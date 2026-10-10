//! The remote-interactivity PROBE SWEEP — plan
//! `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
//! Phase A3, runner half.
//!
//! Traffic alone leaves most sessions unmeasured: a session nobody has
//! attached to in the last half hour has no fresh fact on either half, so the
//! fleet row reads `unknown` and the metric can only bound itself. The sweep
//! turns those into measured facts WITHOUT touching the session:
//!
//! 1. **mint** an attach grant from coord (the same door a tab uses);
//! 2. **attach** with NO `TerminalSession` and no tab — a probe-only pane over a
//!    [`ProbeFrameSink`] that refuses, by construction, to carry a single input
//!    byte;
//! 3. **read probe** — a `remote_terminal_buffer` request for the EMPTY range
//!    at `total_bytes_produced`: the target answers it under the grant, which
//!    proves mint → directive → relay → target gate → session resolution →
//!    return route → source decode, and moves no byte of the session;
//! 4. **write probe** — a zero-byte `probe: true` input, sent ONLY when the
//!    target is positively known to acknowledge input. A target that predates
//!    A1 ignores `probe` and would write the frame — a 0-byte observation in
//!    the session's `last_input`, which the phantom-turn detector reads as
//!    operator input. A probe-only attach has received no ack, and coord does
//!    not serve an input-ack readiness yet, so today the capability is UNKNOWN
//!    and no write probe is sent: the row's write half stays whatever coord
//!    already serves (never overwritten with an untested `unknown`);
//! 5. **detach** honestly — the 09-16 [`DetachOutcome`], checked and reported;
//! 6. **report** through the coalescing reporter (`read` via `probe`).
//!
//! Rows are probed SEQUENTIALLY, one grant at a time, each bounded by
//! [`ATTACH_TIMEOUT`]. A row with a fresh `via: traffic` fact is skipped (a
//! live holder already measures it, and probing would only collide with its
//! binding); a relay refusal because another source holds the terminal is
//! `unknown/held_by_other_source`, never a failure.
//!
//! # Scheduling
//!
//! (a) the Fleet view runs it for each remote device it loads (trigger
//! `fleet_view`, which also stamps that device's "opened at"); (b) the runner's
//! own scheduler ([`ensure_probe_scheduler`]) re-runs it every [`PROBE_EVERY`]
//! for every device whose Fleet view was opened in the last
//! [`FLEET_VIEW_RECENT`]. Compiled constants, no enable flag: the one off
//! switch is the TARGET's `accept_remote_attach: off`, which surfaces as
//! `failed/remote_attach_disabled` — not as silence.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use super::remote_attach::{
    coord_places_session_on, AttachGrantResponse, AttachWaitObserver, GrantPresenter,
    RelayPresenter,
};
use crate::mcp::remote_interactivity::{
    classify_refusal, FactState, Half, Observation, Role, SourceReportContext, Via, FRESH_FOR_SECS,
    INPUT_ACK_DEADLINE, PROBE_EVERY, REASON_TARGET_PREDATES_INPUT_ACK, REASON_TARGET_UNREACHABLE,
};
use crate::mcp::remote_terminal::{AttachError, AttachedReply, ATTACH_TIMEOUT};
use crate::terminal::pane_io::PaneIo;
use crate::terminal::remote_pane_io::{DetachOutcome, RemoteFrameSink, RemotePaneIo};

/// A device whose Fleet view has not been opened for this long is no longer
/// swept by the scheduler.
pub const FLEET_VIEW_RECENT: Duration = Duration::from_secs(7 * 24 * 3600);

/// Delay before the scheduler's first sweep after the relay first connects —
/// long enough that a reconnect storm at startup does not also carry a sweep.
const FIRST_SCHEDULED_SWEEP_DELAY: Duration = Duration::from_secs(60);

/// The probe pane's announced size. Nothing is resized on the target by an
/// attach; the value only has to be valid.
const PROBE_COLS: u16 = 80;
const PROBE_ROWS: u16 = 24;

/// Page size and page bound for the fleet walk behind one sweep.
const FLEET_PAGE_LIMIT: i64 = 200;
const MAX_FLEET_PAGES: usize = 20;

/// How often the write probe polls the pane for its ack.
const ACK_POLL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// The probe-only sink
// ---------------------------------------------------------------------------

/// The sink every probe pane writes through. It records each frame and
/// REFUSES any `remote_terminal_input` that is not a zero-byte `probe: true`
/// frame — so even a bug upstream cannot make a probe type into a live
/// session. The recorded frames are the sweep's evidence of that (and the
/// test's).
pub(crate) struct ProbeFrameSink {
    inner: Arc<dyn RemoteFrameSink>,
    sent: Mutex<Vec<Value>>,
}

impl ProbeFrameSink {
    pub(crate) fn new(inner: Arc<dyn RemoteFrameSink>) -> Self {
        Self {
            inner,
            sent: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn sent(&self) -> Vec<Value> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// True for an input frame a probe may send: zero bytes, `probe: true`.
fn is_probe_only_input(frame: &Value) -> bool {
    let empty = frame
        .get("data")
        .and_then(|v| v.as_str())
        .is_some_and(str::is_empty);
    let probe = frame.get("probe").and_then(|v| v.as_bool()) == Some(true);
    empty && probe
}

impl RemoteFrameSink for ProbeFrameSink {
    fn send_frame(&self, frame: Value) -> Result<(), String> {
        if frame.get("type").and_then(|v| v.as_str()) == Some("remote_terminal_input")
            && !is_probe_only_input(&frame)
        {
            return Err(
                "remote_interactivity_probe: a probe pane never carries input bytes — refused"
                    .to_string(),
            );
        }
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(frame.clone());
        self.inner.send_frame(frame)
    }
}

// ---------------------------------------------------------------------------
// The doors
// ---------------------------------------------------------------------------

/// Everything the sweep touches outside itself — coord, the relay, the
/// routing table, the reporter — so the sweep runs against an in-process
/// recorder target in tests.
#[async_trait]
pub(crate) trait ProbeDoors: Send + Sync {
    /// One page of `GET /coord/sessions/fleet?device_id=…`.
    async fn fleet_page(&self, device_id: &str, cursor: Option<String>) -> Result<Value, String>;
    /// `POST /coord/sessions/{id}/attach-grants`.
    async fn mint(&self, session_id: Uuid) -> Result<AttachGrantResponse, String>;
    /// Present the grant through the relay; the target's ring or a typed refusal.
    async fn attach(&self, grant: &str, cols: u16, rows: u16)
        -> Result<AttachedReply, AttachError>;
    /// Where the probe pane's frames go (wrapped in a [`ProbeFrameSink`]).
    fn sink(&self) -> Arc<dyn RemoteFrameSink>;
    /// Route inbound frames for the pane's grant to it.
    fn register(&self, pane: Arc<RemotePaneIo>);
    /// The read probe: the ring range `[offset, offset)`.
    async fn read_probe(
        &self,
        pane: &RemotePaneIo,
        offset: u64,
    ) -> Result<AttachedReply, AttachError>;
    /// Stop routing to the pane.
    fn forget(&self, grant_jti: &str);
    /// Hand one observation to the reporter.
    fn report(&self, obs: Observation);
    /// This device, when the mint response does not name it.
    fn local_device_id(&self) -> Option<Uuid>;
    /// Whether a relay connection holds the outbound pump right now — a frame
    /// queued otherwise is discarded, so a sweep mints nothing without one.
    fn relay_connected(&self) -> bool;
    /// Where this runner remembers its probe attempts (process-wide in
    /// production; per test otherwise).
    fn attempts(&self) -> &ProbeAttempts;
    /// Whether THIS runner already has a live tab onto `session_id`. Such a
    /// session is measured by that tab's traffic; probing it would only be
    /// refused as held — by us.
    fn live_tab_here(&self, session_id: &str) -> bool;
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// What the sweep read off coord's fleet response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetFlags {
    /// `interactivityEventsPresent` as served; `None` = coord predates the
    /// field (and the observation door), so nothing is probed.
    pub interactivity_events_present: Option<bool>,
    /// The freshness window used — coord's `freshForSecs`, or the compiled
    /// fallback when absent (`freshForSecsServed: false`).
    pub fresh_for_secs: i64,
    pub fresh_for_secs_served: bool,
    pub caller_device_id: Option<String>,
    pub rows_read: usize,
    pub pages: usize,
    /// `false` when the walk stopped at [`MAX_FLEET_PAGES`] with a cursor left.
    pub complete: bool,
}

/// One half's outcome for one row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HalfOutcome {
    /// `ok` / `failed` / `unknown`.
    pub state: String,
    pub reason: Option<String>,
    pub via: Option<String>,
    /// Whether THIS sweep filed it with coord. A `write: ok` is never filed
    /// here — the target measures that half and files it itself.
    pub filed: bool,
    pub note: Option<String>,
}

impl HalfOutcome {
    fn new(state: FactState, reason: Option<&str>, filed: bool, note: Option<&str>) -> Self {
        Self {
            state: state.as_str().to_string(),
            reason: reason.map(str::to_string),
            via: Some(Via::Probe.as_str().to_string()),
            filed,
            note: note.map(str::to_string),
        }
    }
}

/// One fleet row's outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowOutcome {
    pub session_id: String,
    /// `probed` or `skipped`.
    pub decision: String,
    pub skip_reason: Option<String>,
    pub grant_jti: Option<String>,
    /// The mint's refusal, when coord would not grant. Not filed: coord
    /// admits a SOURCE report only under a grant it minted.
    pub mint_error: Option<String>,
    pub attach_error: Option<String>,
    pub read: Option<HalfOutcome>,
    pub write: Option<HalfOutcome>,
    /// `queued`, `failed: …` or `not_attempted` — the 09-16 detach outcome.
    pub detach: Option<String>,
    /// Frames the probe PANE itself put on the wire through its
    /// [`ProbeFrameSink`]: the detach and, at most, one zero-byte probe input.
    /// (The attach presentation and the read probe go through the relay
    /// client's own queue and carry no input by construction.)
    pub frames_sent: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeSweepReport {
    pub device_id: String,
    pub trigger: String,
    pub flags: FleetFlags,
    pub outcomes: Vec<RowOutcome>,
}

/// A sweep that could not run at all, naming the door that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeSweepError {
    pub door: &'static str,
    pub message: String,
}

impl std::fmt::Display for ProbeSweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "remote_interactivity_probe:{}: {}",
            self.door, self.message
        )
    }
}

// ---------------------------------------------------------------------------
// Row selection (pure)
// ---------------------------------------------------------------------------

/// Whether a fleet row's fact is fresh: a MEASURED state (`ok`/`failed`)
/// observed within `fresh_for_secs`. `unknown` is never fresh.
fn fact_is_fresh(fact: Option<&Value>, fresh_for_secs: i64, now: DateTime<Utc>) -> bool {
    let Some(f) = fact else { return false };
    let measured = matches!(
        f.get("state").and_then(|v| v.as_str()),
        Some("ok" | "failed")
    );
    let at = f
        .get("observedAt")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    measured && at.is_some_and(|at| now.signed_duration_since(at).num_seconds() <= fresh_for_secs)
}

/// How long after a session starts the sweep leaves it alone: the target's
/// remote-create attach deadline plus [`REPROBE_SLACK`], so a probe can never
/// be the attach that disarms the reaper for an orphaned remote create.
pub(crate) const ATTACH_DEADLINE_GUARD: Duration = Duration::from_secs(
    crate::mcp::remote_create_reaper::REMOTE_CREATE_ATTACH_DEADLINE.as_secs()
        + REPROBE_SLACK.as_secs(),
);

/// `true` when the row's `startedAt` parses and lies within `window` of `now`
/// (a start in the future counts as young). Absent or unparseable is `false`.
fn started_within(row: &Value, window: Duration, now: DateTime<Utc>) -> bool {
    row.get("startedAt")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .is_some_and(|at| now.signed_duration_since(at).num_seconds() < window.as_secs() as i64)
}

fn is_fresh_traffic_ok(fact: Option<&Value>, fresh_for_secs: i64, now: DateTime<Utc>) -> bool {
    fact.is_some_and(|f| {
        f.get("state").and_then(|v| v.as_str()) == Some("ok")
            && f.get("via").and_then(|v| v.as_str()) == Some("traffic")
    }) && fact_is_fresh(fact, fresh_for_secs, now)
}

/// `None` = probe it; `Some(reason)` = skip.
/// `own_probe`: `Some` when THIS runner holds an attempt stamp for the row —
/// `recent` = inside the re-probe window at the sweep's start, `device` = this
/// runner's device. The stamp decides only for a probe fact THIS device filed
/// (`sourceDeviceId == device`); another device's probe fact, or any probe
/// fact when there is no stamp, is judged by coord's `observedAt` against the
/// re-probe window.
pub(crate) fn skip_reason(
    row: &Value,
    fresh_for_secs: i64,
    now: DateTime<Utc>,
    own_probe: Option<OwnProbe>,
) -> Option<&'static str> {
    if row.get("isCallerDevice").and_then(|v| v.as_bool()) == Some(true) {
        return Some("caller_device");
    }
    if row.get("closedAt").is_some_and(|v| !v.is_null()) {
        return Some("closed");
    }
    // coord's `NON_LIVE_STATES` plus `closed`: not a live PTY, so nothing to
    // measure (and coord excludes them from the metric's denominator).
    if let Some(state) = row
        .get("state")
        .and_then(|v| v.as_str())
        .filter(|s| NON_LIVE_STATES.contains(s))
    {
        return Some(match state {
            "closed" => "closed",
            "stale" => "stale",
            _ => "expected",
        });
    }
    if row.get("interactiveSurface").and_then(|v| v.as_str()) == Some("none") {
        return Some("not_interactive");
    }
    // A session younger than the target's attach-deadline (plus slack) is left
    // to the target's `remote_create_reaper` first: a probe attach is an
    // ordinary grant attach, the target marks the terminal ATTACHED on it, and
    // a remote-created terminal whose creator's reply was lost would then never
    // be reaped. Past the deadline the reaper has decided (an orphan is gone,
    // an owned terminal is unaffected), so the next sweep measures it. A row
    // with no `startedAt` is probed: the age is unknown, not young.
    if started_within(row, ATTACH_DEADLINE_GUARD, now) {
        return Some("attach_deadline_pending");
    }
    let read = row.get("readableRemotely");
    let write = row.get("writableRemotely");
    if is_fresh_traffic_ok(read, fresh_for_secs, now)
        || is_fresh_traffic_ok(write, fresh_for_secs, now)
    {
        return Some("fresh_traffic");
    }
    // "Current" is per source. A traffic fact is good for freshFor. A PROBE
    // fact only for the re-probe window — the sweep must renew it before it
    // goes stale, so it cannot be allowed to hold a row back past the next
    // tick; and when this runner stamped the probe itself, the stamp (taken
    // at the sweep's start) decides, not coord's filing time, which lands
    // mid-sweep and would otherwise skip every other tick.
    let current = |fact: Option<&Value>| -> bool {
        let via_probe =
            fact.is_some_and(|f| f.get("via").and_then(|v| v.as_str()) == Some("probe"));
        let ours = own_probe.filter(|own| {
            fact.and_then(|f| f.get("sourceDeviceId"))
                .and_then(|v| v.as_str())
                .and_then(|d| Uuid::parse_str(d.trim()).ok())
                == Some(own.device)
        });
        match (via_probe, ours) {
            (true, Some(own)) => own.recent && fact_is_fresh(fact, fresh_for_secs, now),
            (true, None) => fact_is_fresh(fact, reprobe_window_secs(fresh_for_secs), now),
            (false, _) => fact_is_fresh(fact, fresh_for_secs, now),
        }
    };
    if current(read) && current(write) {
        return Some("fresh");
    }
    // Convergence. A probe measures the READ half; the write half it can only
    // measure against a target known to acknowledge input. So a row whose read
    // is current and whose write coord records as unmeasurable (never probed,
    // or the target predates acks) would be re-probed every sweep to learn
    // nothing new.
    if current(read) && write_is_unmeasurable(write) {
        return Some("write_unmeasurable");
    }
    // And a probe read that is still current is not re-probed, whatever the
    // write half says.
    if read.is_some_and(|f| f.get("via").and_then(|v| v.as_str()) == Some("probe")) && current(read)
    {
        return Some("recently_probed");
    }
    None
}

/// This runner's own attempt stamp for a row, as [`skip_reason`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnProbe {
    /// The stamp is inside the re-probe window at the sweep's start.
    pub recent: bool,
    /// This runner's device — only a fact filed by it is judged by the stamp.
    pub device: Uuid,
}

/// Slack taken off the re-probe window. Attempts are stamped with the SWEEP's
/// start (see [`run_probe_sweep`]), so time spent inside a sweep cannot push a
/// row past the next tick; the slack covers what stamping cannot — scheduler
/// jitter, and coord's facts, which carry the time they were FILED (mid-sweep).
/// Without it a row looked at a little after one tick would still be "recent"
/// at the next and be probed only every OTHER period, past freshness.
const REPROBE_SLACK: Duration = Duration::from_secs(120);

/// How long after a probe a row is not probed again, in seconds:
/// `w - min(REPROBE_SLACK, w / 2)` with `w = min(PROBE_EVERY, freshFor)` — so a
/// short freshness window keeps half of itself instead of collapsing to 0.
fn reprobe_window_secs(fresh_for_secs: i64) -> i64 {
    let w = (PROBE_EVERY.as_secs() as i64).min(fresh_for_secs).max(0);
    w - (REPROBE_SLACK.as_secs() as i64).min(w / 2)
}

/// In-process memory of when this runner last PROBED each session — noted
/// once a grant was minted (a refused or failed mint is NOT noted, so it is
/// retried next sweep), stamped with the sweep's start, whatever came of the
/// probe after that. Coord only remembers what was filed, and several outcomes
/// file nothing that marks the row as recently looked at (a target mismatch,
/// an unreachable or busy target): without this, every sweep would re-spend a
/// grant on them. Bounded; the oldest entries go first.
#[derive(Default)]
pub(crate) struct ProbeAttempts {
    last: Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

/// Bound on [`ProbeAttempts`].
const PROBE_ATTEMPTS_CAP: usize = 4096;

impl ProbeAttempts {
    pub(crate) fn note(&self, session_id: &str, at: std::time::Instant) {
        let mut g = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let key = session_id.trim().to_ascii_lowercase();
        if !g.contains_key(&key) && g.len() >= PROBE_ATTEMPTS_CAP {
            g.retain(|_, t| at.saturating_duration_since(*t) < PROBE_EVERY);
            while g.len() >= PROBE_ATTEMPTS_CAP {
                let Some(oldest) = g.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone()) else {
                    break;
                };
                g.remove(&oldest);
            }
        }
        g.insert(key, at);
    }

    /// Whether `session_id` was probed within the re-probe window before `now`.
    #[cfg(test)]
    pub(crate) fn recently(
        &self,
        session_id: &str,
        fresh_for_secs: i64,
        now: std::time::Instant,
    ) -> bool {
        self.recently_if_stamped(session_id, fresh_for_secs, now)
            .unwrap_or(false)
    }

    /// `None` when there is no stamp for `session_id`; else whether it falls
    /// inside the re-probe window before `now`.
    pub(crate) fn recently_if_stamped(
        &self,
        session_id: &str,
        fresh_for_secs: i64,
        now: std::time::Instant,
    ) -> Option<bool> {
        let g = self.last.lock().unwrap_or_else(|e| e.into_inner());
        g.get(&session_id.trim().to_ascii_lowercase()).map(|t| {
            (now.saturating_duration_since(*t).as_secs() as i64)
                < reprobe_window_secs(fresh_for_secs)
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.last.lock().unwrap().len()
    }

    #[cfg(test)]
    fn get(&self, session_id: &str) -> Option<std::time::Instant> {
        self.last
            .lock()
            .unwrap()
            .get(&session_id.trim().to_ascii_lowercase())
            .copied()
    }
}

static PROBE_ATTEMPTS: OnceLock<ProbeAttempts> = OnceLock::new();

/// coord's `NON_LIVE_STATES` (`stale`, `expected`) plus `closed`.
const NON_LIVE_STATES: [&str; 3] = ["stale", "expected", "closed"];

/// The write fact is one a probe cannot move: `unknown` because it was never
/// probed, or because the target predates input acknowledgements.
fn write_is_unmeasurable(write: Option<&Value>) -> bool {
    write.is_some_and(|f| {
        f.get("state").and_then(|v| v.as_str()) == Some("unknown")
            && matches!(
                f.get("reason").and_then(|v| v.as_str()),
                Some("unprobed" | "target_predates_input_ack")
            )
    })
}

/// What is known about the target acknowledging input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteCapability {
    /// Positively known — coord said `supports`, or this attachment was acked.
    Supports,
    /// Coord positively said the target predates acknowledgements.
    Predates,
    /// Nobody has said. No write probe.
    Unknown,
}

pub(crate) fn write_capability(
    minted: &AttachGrantResponse,
    pane: &RemotePaneIo,
) -> WriteCapability {
    if pane.interactivity().acks_since_attach > 0 {
        return WriteCapability::Supports;
    }
    match minted
        .target_runner
        .as_ref()
        .and_then(|t| t.input_ack.as_ref())
        .map(|c| c.state.as_str())
    {
        Some("supports") => WriteCapability::Supports,
        Some("predates") => WriteCapability::Predates,
        _ => WriteCapability::Unknown,
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// Walk the device's fleet rows and return them with the flags.
async fn read_fleet(
    doors: &dyn ProbeDoors,
    device_id: &str,
) -> Result<(FleetFlags, Vec<Value>), ProbeSweepError> {
    let mut flags = FleetFlags {
        fresh_for_secs: FRESH_FOR_SECS,
        complete: true,
        ..Default::default()
    };
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_FLEET_PAGES {
        let body = doors
            .fleet_page(device_id, cursor.clone())
            .await
            .map_err(|message| ProbeSweepError {
                door: "coord_fleet",
                message,
            })?;
        let Some(sessions) = body.get("sessions").and_then(|v| v.as_array()) else {
            return Err(ProbeSweepError {
                door: "coord_fleet",
                message: "the fleet response carries no `sessions` array".to_string(),
            });
        };
        if page == 0 {
            flags.interactivity_events_present = body
                .get("interactivityEventsPresent")
                .and_then(|v| v.as_bool());
            if let Some(f) = body.get("freshForSecs").and_then(|v| v.as_i64()) {
                flags.fresh_for_secs = f;
                flags.fresh_for_secs_served = true;
            }
            flags.caller_device_id = body
                .get("callerDeviceId")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        flags.pages = page + 1;
        rows.extend(
            sessions
                .iter()
                .filter(|r| {
                    r.get("deviceId")
                        .and_then(|v| v.as_str())
                        .is_some_and(|d| d.eq_ignore_ascii_case(device_id))
                })
                .cloned(),
        );
        let next = body
            .get("nextCursor")
            .and_then(|v| v.as_str())
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string);
        match next {
            // A cursor handed back unchanged would re-serve the same page.
            Some(c) if cursor.as_deref() != Some(c.as_str()) => cursor = Some(c),
            _ => {
                cursor = None;
                break;
            }
        }
    }
    if cursor.is_some() {
        flags.complete = false;
    }
    flags.rows_read = rows.len();
    Ok((flags, rows))
}

/// Run one sweep for `device_id`. `Err` only when the fleet read itself
/// failed; every per-row failure is an outcome.
pub(crate) async fn run_probe_sweep(
    doors: &dyn ProbeDoors,
    device_id: &str,
    trigger: &str,
) -> Result<ProbeSweepReport, ProbeSweepError> {
    if !doors.relay_connected() {
        return Err(ProbeSweepError {
            door: "relay",
            message: "this runner's relay is not connected — no grant was minted; the sweep \
                      runs once the relay reconnects"
                .to_string(),
        });
    }
    // ONE clock reading for the whole sweep: every skip decision (coord facts
    // and the attempt memory) is taken against the sweep's start, and every
    // attempt is stamped with it — so a slow row ahead of another cannot make
    // the later row look probed later than the sweep that probed it.
    let started = std::time::Instant::now();
    let started_utc = Utc::now();
    let (flags, rows) = read_fleet(doors, device_id).await?;
    let mut outcomes = Vec::with_capacity(rows.len());
    for row in &rows {
        let session_id = row
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if flags.interactivity_events_present.is_none() {
            // coord serves no interactivity facts, so it has no door to record
            // into either: probing would spend grants to file nothing.
            outcomes.push(RowOutcome {
                session_id,
                decision: "skipped".into(),
                skip_reason: Some("coord_predates_interactivity".into()),
                ..Default::default()
            });
            continue;
        }
        // A MANUAL sweep (an explicit `manual` — an absent trigger is
        // `unspecified`) is someone asking now: this runner's attempt memory
        // does not hold it back. The per-device throttle still applies.
        let manual = trigger == "manual";
        let stamped = if manual {
            None
        } else {
            doors
                .attempts()
                .recently_if_stamped(&session_id, flags.fresh_for_secs, started)
        };
        let own_probe = stamped.and_then(|recent| {
            doors
                .local_device_id()
                .map(|device| OwnProbe { recent, device })
        });
        let reason = skip_reason(row, flags.fresh_for_secs, started_utc, own_probe)
            .or_else(|| doors.live_tab_here(&session_id).then_some("live_tab_here"))
            .or_else(|| (stamped == Some(true)).then_some("recently_attempted"));
        if let Some(reason) = reason {
            outcomes.push(RowOutcome {
                session_id,
                decision: "skipped".into(),
                skip_reason: Some(reason.into()),
                ..Default::default()
            });
            continue;
        }
        outcomes.push(probe_row(doors, device_id, &session_id, started).await);
    }
    Ok(ProbeSweepReport {
        device_id: device_id.to_string(),
        trigger: trigger.to_string(),
        flags,
        outcomes,
    })
}

/// File each observation, returning whether any was handed to the reporter.
fn file(doors: &dyn ProbeDoors, obs: Vec<Observation>) -> bool {
    let any = !obs.is_empty();
    for o in obs {
        doors.report(o);
    }
    any
}

/// The outcome for one half from a refusal code, filing it when a context is
/// available.
fn refusal_half(
    doors: &dyn ProbeDoors,
    ctx: Option<&SourceReportContext>,
    code: &str,
    half: Half,
) -> HalfOutcome {
    match classify_refusal(code) {
        Some((state, reason)) => {
            let filed = ctx.is_some_and(|c| file(doors, c.refusal(code, &[half], Utc::now())));
            HalfOutcome::new(state, Some(reason), filed, None)
        }
        None => HalfOutcome::new(
            FactState::Unknown,
            Some(REASON_TARGET_UNREACHABLE),
            false,
            Some("a local failure on this runner — nothing about the target was observed, so nothing was filed"),
        ),
    }
}

async fn probe_row(
    doors: &dyn ProbeDoors,
    device_id: &str,
    session_id: &str,
    sweep_started: std::time::Instant,
) -> RowOutcome {
    let mut out = RowOutcome {
        session_id: session_id.to_string(),
        decision: "probed".into(),
        ..Default::default()
    };
    let Ok(session) = Uuid::parse_str(session_id.trim()) else {
        out.mint_error = Some("session id is not a uuid".into());
        return out;
    };

    // 1. mint
    let minted = match doors.mint(session).await {
        Ok(m) => m,
        Err(e) => {
            out.mint_error = Some(e);
            let note = "no grant was minted, and coord admits a source report only under a grant \
                        it minted — nothing filed";
            out.read = Some(HalfOutcome::new(
                FactState::Unknown,
                Some("unprobed"),
                false,
                Some(note),
            ));
            out.write = Some(HalfOutcome::new(
                FactState::Unknown,
                Some("unprobed"),
                false,
                Some(note),
            ));
            return out;
        }
    };
    out.grant_jti = Some(minted.grant_jti.clone());
    // Noted only once a grant exists — a local or coord fault at the mint must
    // not hold the row back — and before anything else can fail or be
    // cancelled, stamped with the sweep's start.
    doors.attempts().note(session_id, sweep_started);
    // From the mint until the pane guard takes over: a DROPPED sweep future
    // (cancelled mid-attach) must not leave the grant's pending-output slot,
    // or a half-registered route, behind.
    let mut grant_guard = GrantGuard {
        doors,
        grant_jti: minted.grant_jti.clone(),
        armed: true,
    };
    if !coord_places_session_on(device_id, minted.target_device_id.as_deref()) {
        out.attach_error = Some(format!(
            "target_mismatch: coord placed the session on {:?}, not {device_id}",
            minted.target_device_id
        ));
        return out;
    }
    let ctx = SourceReportContext::parse(
        session_id,
        &minted.grant_jti,
        minted.source_device_id.as_deref(),
        Via::Probe,
    )
    .or_else(|| {
        Some(SourceReportContext {
            session_id: session,
            grant_jti: Uuid::parse_str(minted.grant_jti.trim()).ok()?,
            source_device_id: doors.local_device_id()?,
            via: Via::Probe,
        })
    });

    // 2. attach
    let reply = match doors.attach(&minted.grant, PROBE_COLS, PROBE_ROWS).await {
        Ok(r) => r,
        Err(e) => {
            out.attach_error = Some(e.to_string());
            out.read = Some(refusal_half(doors, ctx.as_ref(), &e.code, Half::Read));
            out.write = Some(refusal_half(doors, ctx.as_ref(), &e.code, Half::Write));
            return out;
        }
    };
    if reply.grant_jti != minted.grant_jti {
        doors.forget(&reply.grant_jti);
        out.attach_error = Some(format!(
            "grant_mismatch: the target answered for {} but {} was presented",
            reply.grant_jti, minted.grant_jti
        ));
        return out;
    }
    let sink = Arc::new(ProbeFrameSink::new(doors.sink()));
    let pane = Arc::new(RemotePaneIo::new(
        minted.grant_jti.clone(),
        reply.terminal_id.clone(),
        minted.grant.clone(),
        sink.clone(),
        PROBE_COLS,
        PROBE_ROWS,
        reply.ring.clone(),
    ));
    doors.register(pane.clone());
    // From here the pane is in the routing table and holds the target's
    // binding. If this future is DROPPED (a cancelled command, a timeout
    // wrapped around the sweep), the guard detaches it and takes it out of
    // routing — otherwise the binding and the pane's unbounded output channel
    // would outlive the probe.
    let mut guard = ProbePaneGuard {
        doors,
        pane: pane.clone(),
        armed: true,
    };
    // The pane guard now owns the teardown (it forgets the same grant).
    grant_guard.armed = false;

    // 3. read probe — the empty range at the end of the target's ring.
    let at = reply.ring.total_bytes_produced;
    out.read = Some(match doors.read_probe(&pane, at).await {
        Ok(_) => {
            let filed = ctx
                .as_ref()
                .is_some_and(|c| file(doors, vec![c.read_ok(Utc::now())]));
            HalfOutcome::new(FactState::Ok, None, filed, None)
        }
        Err(e) => refusal_half(doors, ctx.as_ref(), &e.code, Half::Read),
    });

    // 4. write probe — only against a target known to acknowledge input.
    out.write = Some(write_probe(doors, ctx.as_ref(), &minted, &pane).await);

    // 5. detach, honestly.
    guard.armed = false;
    let _ = pane.kill(Duration::ZERO);
    out.detach = Some(match pane.detach_outcome() {
        DetachOutcome::Queued => "queued".to_string(),
        DetachOutcome::Failed(e) => format!("failed: {e}"),
        DetachOutcome::NotAttempted => "not_attempted".to_string(),
    });
    doors.forget(&minted.grant_jti);
    out.frames_sent = sink.sent().len();
    out
}

/// Forgets a minted grant's routing state (the pending-output slot an
/// `attached` reply opens) if the probe future is dropped between the mint and
/// the pane's registration. Harmless on the paths that return normally: a
/// forget of a jti nothing routes is a no-op.
struct GrantGuard<'a> {
    doors: &'a dyn ProbeDoors,
    grant_jti: String,
    armed: bool,
}

impl Drop for GrantGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.doors.forget(&self.grant_jti);
        }
    }
}

/// Tears a probe pane down if the probe never reached its own detach — the
/// sweep future was dropped mid-row. Disarmed on the normal path, which
/// detaches explicitly and reports the outcome.
struct ProbePaneGuard<'a> {
    doors: &'a dyn ProbeDoors,
    pane: Arc<RemotePaneIo>,
    armed: bool,
}

impl Drop for ProbePaneGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // `release` closes the output channel (nothing reads a probe pane's
        // output) and queues the detach; `kill` settles `wait` so the client's
        // sweep treats the pane as finished.
        let _ = self.pane.release(Duration::ZERO);
        let _ = self.pane.kill(Duration::ZERO);
        self.doors.forget(self.pane.grant_jti());
        warn!(
            grant_jti = %self.pane.grant_jti(),
            detach = ?self.pane.detach_outcome(),
            "remote interactivity probe: sweep cancelled mid-probe — probe pane detached and \
             dropped from routing"
        );
    }
}

async fn write_probe(
    doors: &dyn ProbeDoors,
    ctx: Option<&SourceReportContext>,
    minted: &AttachGrantResponse,
    pane: &RemotePaneIo,
) -> HalfOutcome {
    match write_capability(minted, pane) {
        WriteCapability::Unknown => HalfOutcome::new(
            FactState::Unknown,
            Some("unprobed"),
            false,
            Some(
                "the target is not known to acknowledge input (no ack on this attachment, and \
                 coord states no input-ack readiness), so no write probe was sent — an older \
                 target would write it into the session; coord's write fact is left as served",
            ),
        ),
        WriteCapability::Predates => {
            let obs = ctx.and_then(|c| {
                Observation::new(
                    Role::Source,
                    c.session_id,
                    Half::Write,
                    FactState::Unknown,
                    c.source_device_id,
                    Some(Via::Probe),
                    Some(REASON_TARGET_PREDATES_INPUT_ACK),
                    Some(c.grant_jti),
                    Utc::now(),
                )
                .ok()
            });
            let filed = obs.is_some_and(|o| file(doors, vec![o]));
            HalfOutcome::new(
                FactState::Unknown,
                Some(REASON_TARGET_PREDATES_INPUT_ACK),
                filed,
                None,
            )
        }
        WriteCapability::Supports => {
            let seq = match pane.send_input_probe_given(true) {
                Ok(seq) => seq,
                Err(e) => {
                    return HalfOutcome::new(
                        FactState::Unknown,
                        Some("unprobed"),
                        false,
                        Some(&format!("the write probe could not be queued: {e}")),
                    )
                }
            };
            let deadline = tokio::time::Instant::now() + INPUT_ACK_DEADLINE;
            loop {
                if let Some(ack) = pane
                    .interactivity()
                    .last_probe_acked
                    .filter(|a| a.seq == Some(seq))
                {
                    let note = "measured by the TARGET, which files the write half itself";
                    return if ack.accepted {
                        HalfOutcome::new(FactState::Ok, None, false, Some(note))
                    } else {
                        HalfOutcome::new(
                            FactState::Failed,
                            ack.error.as_deref().or(Some("input_write_failed")),
                            false,
                            Some(note),
                        )
                    };
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(ACK_POLL).await;
            }
            let filed = ctx.is_some_and(|c| {
                Observation::new(
                    Role::Source,
                    c.session_id,
                    Half::Write,
                    FactState::Unknown,
                    c.source_device_id,
                    Some(Via::Probe),
                    Some(REASON_TARGET_UNREACHABLE),
                    Some(c.grant_jti),
                    Utc::now(),
                )
                .map(|o| file(doors, vec![o]))
                .unwrap_or(false)
            });
            HalfOutcome::new(
                FactState::Unknown,
                Some(REASON_TARGET_UNREACHABLE),
                filed,
                Some(
                    "no acknowledgement within INPUT_ACK_DEADLINE from a target known to send one",
                ),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// One sweep per device at a time
// ---------------------------------------------------------------------------

static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

struct InFlight(String);

impl InFlight {
    fn claim(device_id: &str) -> Option<InFlight> {
        let key = device_id.trim().to_ascii_lowercase();
        let set = IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()));
        let mut g = set.lock().unwrap_or_else(|e| e.into_inner());
        // NOT `then_some(InFlight(key))`: that builds the guard eagerly, and
        // dropping the unused one on a refusal would re-lock this mutex (and
        // release the OTHER sweep's claim).
        if g.insert(key.clone()) {
            Some(InFlight(key))
        } else {
            None
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Some(set) = IN_FLIGHT.get() {
            set.lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.0);
        }
    }
}

/// Minimum gap between two FINISHED sweeps of one device. Back-to-back calls
/// (a Fleet view re-mounted, a headless caller looping) inside it are refused
/// as `throttled` rather than spending a grant per row again; the scheduler's
/// own period ([`PROBE_EVERY`]) is far above it.
pub const SWEEP_MIN_INTERVAL: Duration = crate::mcp::remote_interactivity::REPORT_EVERY;

static LAST_FINISHED: OnceLock<Mutex<std::collections::HashMap<String, std::time::Instant>>> =
    OnceLock::new();

/// `Some(remaining)` when `device_id` finished a sweep less than
/// [`SWEEP_MIN_INTERVAL`] before `now`.
fn throttled_for(device_id: &str, now: std::time::Instant) -> Option<Duration> {
    let map = LAST_FINISHED.get()?;
    let g = map.lock().unwrap_or_else(|e| e.into_inner());
    let last = *g.get(&device_id.trim().to_ascii_lowercase())?;
    let since = now.saturating_duration_since(last);
    (since < SWEEP_MIN_INTERVAL).then(|| SWEEP_MIN_INTERVAL - since)
}

fn note_finished(device_id: &str, now: std::time::Instant) {
    let map = LAST_FINISHED.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|_, at| now.saturating_duration_since(*at) < SWEEP_MIN_INTERVAL);
    g.insert(device_id.trim().to_ascii_lowercase(), now);
}

/// [`run_probe_sweep`], refusing a second concurrent sweep of one device and
/// a sweep inside [`SWEEP_MIN_INTERVAL`] of the last one that finished.
pub(crate) async fn run_guarded(
    doors: &dyn ProbeDoors,
    device_id: &str,
    trigger: &str,
) -> Result<ProbeSweepReport, ProbeSweepError> {
    let Some(_claim) = InFlight::claim(device_id) else {
        return Err(ProbeSweepError {
            door: "sweep",
            message: format!("a sweep of device {device_id} is already running"),
        });
    };
    if let Some(wait) = throttled_for(device_id, std::time::Instant::now()) {
        return Err(ProbeSweepError {
            door: THROTTLED_DOOR,
            message: format!(
                "device {device_id} was swept less than {}s ago; next sweep allowed in {}s",
                SWEEP_MIN_INTERVAL.as_secs(),
                wait.as_secs()
            ),
        });
    }
    let report = run_probe_sweep(doors, device_id, trigger).await?;
    note_finished(device_id, std::time::Instant::now());
    Ok(report)
}

/// The `door` of a throttled refusal — the Fleet view treats it as "already
/// measured", not as a failure.
pub const THROTTLED_DOOR: &str = "throttled";

// ---------------------------------------------------------------------------
// Fleet-view recency (pure) and its store
// ---------------------------------------------------------------------------

/// Record `device_id` opened at `now` and prune entries past
/// [`FLEET_VIEW_RECENT`] (and any that do not parse).
pub(crate) fn record_fleet_view_opened(
    map: &mut BTreeMap<String, String>,
    device_id: &str,
    now: DateTime<Utc>,
) {
    map.insert(
        device_id.trim().to_ascii_lowercase(),
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    map.retain(|_, at| opened_within_recent(at, now));
}

fn opened_within_recent(at: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(at).is_ok_and(|at| {
        let age = now.signed_duration_since(at.with_timezone(&Utc));
        age.num_seconds() <= FLEET_VIEW_RECENT.as_secs() as i64
    })
}

/// How old a Fleet-view stamp must be before it is rewritten: re-opening the
/// view every minute must not rewrite the settings file every minute.
pub const FLEET_VIEW_RESTAMP_AFTER: Duration = Duration::from_secs(3600);

/// Whether `device_id`'s stamp is absent, unreadable, or older than
/// [`FLEET_VIEW_RESTAMP_AFTER`].
pub(crate) fn needs_restamp(
    map: &BTreeMap<String, String>,
    device_id: &str,
    now: DateTime<Utc>,
) -> bool {
    match map
        .get(&device_id.trim().to_ascii_lowercase())
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
    {
        None => true,
        Some(at) => {
            now.signed_duration_since(at.with_timezone(&Utc))
                .num_seconds()
                >= FLEET_VIEW_RESTAMP_AFTER.as_secs() as i64
        }
    }
}

/// The devices the scheduler sweeps: opened within [`FLEET_VIEW_RECENT`].
pub(crate) fn recent_fleet_view_devices(
    map: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> Vec<String> {
    map.iter()
        .filter(|(_, at)| opened_within_recent(at, now))
        .map(|(d, _)| d.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Production doors, the command body and the scheduler
// ---------------------------------------------------------------------------

/// Re-presentation observer that only logs: the probe has no tab to animate.
struct SilentWait;

impl AttachWaitObserver for SilentWait {
    fn waiting(&self, attempt: u32, elapsed: Duration, _window: Duration) {
        tracing::debug!(
            attempt,
            elapsed_ms = elapsed.as_millis() as u64,
            "remote interactivity probe: target has not recorded the grant yet — re-presenting"
        );
    }
}

pub(crate) struct RunnerDoors {
    coord_base: String,
    app: tauri::AppHandle,
}

impl RunnerDoors {
    pub(crate) fn new(app: &tauri::AppHandle) -> Self {
        let coord_base = super::remote_attach::coord_base_for(app);
        crate::mcp::remote_interactivity::set_coord_base(&coord_base);
        Self {
            coord_base,
            app: app.clone(),
        }
    }
}

#[async_trait]
impl ProbeDoors for RunnerDoors {
    async fn fleet_page(&self, device_id: &str, cursor: Option<String>) -> Result<Value, String> {
        super::fleet_sessions::fleet_sessions_list(super::fleet_sessions::FleetSessionsArgs {
            device_id: Some(device_id.to_string()),
            state: None,
            // The probe sweeps every live session regardless of its work axis.
            session_status: None,
            include_closed: false,
            limit: Some(FLEET_PAGE_LIMIT),
            cursor,
        })
        .await
    }

    async fn mint(&self, session_id: Uuid) -> Result<AttachGrantResponse, String> {
        super::remote_attach::mint_attach_grant(&self.coord_base, session_id).await
    }

    async fn attach(
        &self,
        grant: &str,
        cols: u16,
        rows: u16,
    ) -> Result<AttachedReply, AttachError> {
        // Re-present the SAME grant across the target's learn race, bounded
        // by ATTACH_TIMEOUT of re-presentation (each presentation carries its
        // own ATTACH_TIMEOUT too).
        super::remote_attach::present_grant_until_target_records_it(
            &RelayPresenter as &dyn GrantPresenter,
            grant,
            cols,
            rows,
            ATTACH_TIMEOUT,
            super::remote_attach::GRANT_REPRESENT_INTERVAL,
            &SilentWait,
        )
        .await
    }

    fn sink(&self) -> Arc<dyn RemoteFrameSink> {
        crate::mcp::remote_terminal::client().sink()
    }

    fn register(&self, pane: Arc<RemotePaneIo>) {
        crate::mcp::remote_terminal::client().register_pane(pane);
    }

    async fn read_probe(
        &self,
        pane: &RemotePaneIo,
        offset: u64,
    ) -> Result<AttachedReply, AttachError> {
        crate::mcp::remote_terminal::client()
            .request_history(pane, offset, offset, ATTACH_TIMEOUT)
            .await
    }

    fn forget(&self, grant_jti: &str) {
        crate::mcp::remote_terminal::client().forget_pane(grant_jti);
    }

    fn report(&self, obs: Observation) {
        crate::mcp::remote_interactivity::reporter().observe(obs);
    }

    fn local_device_id(&self) -> Option<Uuid> {
        crate::agent_runtime::load_local_device_id()
    }

    fn relay_connected(&self) -> bool {
        crate::mcp::remote_terminal::client()
            .outbound_pump_state()
            .0
    }

    fn attempts(&self) -> &ProbeAttempts {
        PROBE_ATTEMPTS.get_or_init(ProbeAttempts::default)
    }

    fn live_tab_here(&self, session_id: &str) -> bool {
        use tauri::Manager as _;
        let Some(tm) = self
            .app
            .try_state::<Arc<crate::terminal::TerminalManager>>()
        else {
            return false;
        };
        let client = crate::mcp::remote_terminal::client();
        tm.remote_identities().values().any(|id| {
            id.session_id.eq_ignore_ascii_case(session_id)
                && client.pane(&id.grant_jti).is_some_and(|p| !p.is_finished())
        })
    }
}

/// The body of the `remote_interactivity_probe` command (Tauri and the
/// headless proxy both land here). `trigger = "fleet_view"` also stamps the
/// device's Fleet-view "opened at", which is what keeps the scheduler
/// sweeping it.
pub(crate) async fn run_probe_command(
    app: &tauri::AppHandle,
    device_id: &str,
    trigger: Option<&str>,
) -> Result<Value, String> {
    let device = Uuid::parse_str(device_id.trim()).map_err(|e| {
        format!("remote_interactivity_probe:invalid_device_id: {device_id:?} is not a uuid: {e}")
    })?;
    let device_id = device.to_string();
    let trigger = normalize_trigger(trigger);
    if trigger == "fleet_view" {
        // Settings I/O is blocking file work: off the async runtime, and only
        // when the stamp is actually due.
        let device = device_id.clone();
        let stamped = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
            let now = Utc::now();
            if !needs_restamp(&crate::settings::get_fleet_view_opened_at(), &device, now) {
                return Ok(());
            }
            crate::settings::update_fleet_view_opened_at(|map| {
                record_fleet_view_opened(map, &device, now)
            })
        })
        .await
        .map_err(|e| format!("join: {e}"))
        .and_then(|r| r);
        if let Err(e) = stamped {
            warn!(
                device_id = %device_id,
                error = %e,
                "remote interactivity probe: could not persist the Fleet-view opened-at stamp \
                 (the scheduler will not sweep this device until it is recorded)"
            );
        }
    }
    let doors = RunnerDoors::new(app);
    let report = run_guarded(&doors, &device_id, trigger)
        .await
        .map_err(|e| e.to_string())?;
    log_report(&report);
    serde_json::to_value(&report).map_err(|e| e.to_string())
}

/// A command's trigger: absent or blank means UNSPECIFIED, not manual — only
/// an explicit `manual` bypasses the attempt memory.
pub(crate) fn normalize_trigger(trigger: Option<&str>) -> &str {
    trigger
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("unspecified")
}

fn log_report(r: &ProbeSweepReport) {
    let probed = r.outcomes.iter().filter(|o| o.decision == "probed").count();
    info!(
        device_id = %r.device_id,
        trigger = %r.trigger,
        rows = r.outcomes.len(),
        probed,
        skipped = r.outcomes.len() - probed,
        interactivity_events_present = ?r.flags.interactivity_events_present,
        complete = r.flags.complete,
        "remote interactivity probe: sweep done"
    );
}

static SCHEDULER_STARTED: AtomicBool = AtomicBool::new(false);

/// Start the runner's own probe scheduler, once per process. Called on every
/// relay connect (the sweep rides the relay); only the first call starts it.
pub fn ensure_probe_scheduler(app: &tauri::AppHandle) {
    if SCHEDULER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_SCHEDULED_SWEEP_DELAY).await;
        let mut tick = tokio::time::interval(PROBE_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            scheduled_sweep(&app).await;
        }
    });
}

async fn scheduled_sweep(app: &tauri::AppHandle) {
    let map = crate::settings::get_fleet_view_opened_at();
    let devices = recent_fleet_view_devices(&map, Utc::now());
    if devices.is_empty() {
        return;
    }
    let doors = RunnerDoors::new(app);
    for device in devices {
        match run_guarded(&doors, &device, "scheduler").await {
            Ok(report) => log_report(&report),
            Err(e) if e.door == THROTTLED_DOOR => tracing::debug!(
                device_id = %device,
                "remote interactivity probe: scheduled sweep skipped — already measured ({e})"
            ),
            Err(e) => {
                warn!(device_id = %device, error = %e, "remote interactivity probe: scheduled sweep failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::remote_attach::{CapabilityReadiness, TargetRunner};
    use crate::terminal::remote_pane_io::AttachedRing;
    use serde_json::json;

    const DEVICE: &str = "84c02292-32cb-4983-be85-d00f868b7003";
    const ME: &str = "11111111-1111-4111-8111-111111111111";

    /// `skip_reason` for a runner holding no attempt stamp for the row.
    fn skip_reason_no_stamp(row: &Value, f: i64, now: DateTime<Utc>) -> Option<&'static str> {
        skip_reason(row, f, now, None)
    }

    /// A probe attach would mark a remote-created terminal attached on the
    /// target and disarm its attach-deadline reaper, so a session younger than
    /// that deadline is not probed; past it, or with no start time, it is.
    #[test]
    fn young_session_is_left_to_the_attach_deadline_reaper() {
        let now = Utc::now();
        let guard = ATTACH_DEADLINE_GUARD.as_secs() as i64;
        assert!(
            ATTACH_DEADLINE_GUARD > crate::mcp::remote_create_reaper::REMOTE_CREATE_ATTACH_DEADLINE,
            "the guard must outlast the reaper's deadline"
        );
        let started = |age: i64| {
            row(
                900,
                json!({"startedAt": (now - chrono::Duration::seconds(age)).to_rfc3339()}),
            )
        };
        assert_eq!(
            skip_reason_no_stamp(&started(60), 1800, now),
            Some("attach_deadline_pending")
        );
        assert_eq!(
            skip_reason_no_stamp(&started(guard - 1), 1800, now),
            Some("attach_deadline_pending")
        );
        assert_eq!(skip_reason_no_stamp(&started(guard + 1), 1800, now), None);
        assert_eq!(skip_reason_no_stamp(&row(901, json!({})), 1800, now), None);
        assert_eq!(
            skip_reason_no_stamp(&row(902, json!({"startedAt": "not-a-time"})), 1800, now),
            None
        );
    }

    fn sid(n: u128) -> String {
        Uuid::from_u128(0x5e55_0000 + n).to_string()
    }

    fn jti(n: u128) -> String {
        Uuid::from_u128(0x0a77_0000 + n).to_string()
    }

    fn fact(state: &str, via: Option<&str>, age_secs: i64, reason: Option<&str>) -> Value {
        json!({
            "state": state,
            "observedAt": (Utc::now() - chrono::Duration::seconds(age_secs)).to_rfc3339(),
            "sourceDeviceId": ME,
            "via": via,
            "reason": reason,
        })
    }

    fn row(n: u128, extra: Value) -> Value {
        let mut r = json!({
            "sessionId": sid(n),
            "deviceId": DEVICE,
            "isCallerDevice": false,
            "state": "active",
            "closedAt": null,
            "interactiveSurface": "runner_pty",
            "readableRemotely": {"state": "unknown", "observedAt": null, "sourceDeviceId": null, "via": null, "reason": "unprobed"},
            "writableRemotely": {"state": "unknown", "observedAt": null, "sourceDeviceId": null, "via": null, "reason": "unprobed"},
        });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                r[k] = v;
            }
        }
        r
    }

    /// An in-process recorder TARGET: answers the fleet read, mints grants,
    /// answers attaches and buffer requests, and records every frame any probe
    /// pane put on the wire.
    struct RecorderTarget {
        rows: Vec<Value>,
        /// Absent fields = an older coord.
        page_extra: Value,
        attach_refusal: Mutex<std::collections::HashMap<String, String>>,
        input_ack: Option<&'static str>,
        wire: Arc<WireRecorder>,
        reported: Mutex<Vec<Observation>>,
        registered: Mutex<Vec<Arc<RemotePaneIo>>>,
        forgotten: Mutex<Vec<String>>,
        minted: Mutex<u128>,
        /// A session this runner already has a live tab onto.
        live_tab: Option<String>,
        relay_up: bool,
        /// The read probe never answers (to cancel a sweep mid-probe).
        read_probe_hangs: bool,
        /// The attach never answers (to cancel a sweep mid-attach).
        attach_hangs: bool,
        /// Sessions whose mint takes this long (a slow row).
        mint_delay: Option<(String, Duration)>,
        /// Sessions whose mint fails.
        mint_fails: Option<String>,
        attempts: ProbeAttempts,
    }

    /// Every frame the relay would carry, and — for a zero-byte probe — the
    /// target's ack back into the pane, as the real target would send it.
    #[derive(Default)]
    struct WireRecorder {
        frames: Mutex<Vec<Value>>,
        panes: Mutex<Vec<Arc<RemotePaneIo>>>,
    }

    impl RemoteFrameSink for WireRecorder {
        fn send_frame(&self, frame: Value) -> Result<(), String> {
            self.frames.lock().unwrap().push(frame.clone());
            if frame["type"] == "remote_terminal_input" && frame["probe"] == true {
                let jti = frame["grant_jti"].as_str().unwrap_or("");
                if let Some(p) = self
                    .panes
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|p| p.grant_jti() == jti)
                {
                    // Delivered off this call, as the relay would: the pane
                    // holds its interactivity lock while it queues the probe.
                    let (p, seq) = (p.clone(), frame["seq"].clone());
                    std::thread::spawn(move || {
                        p.record_input_ack(&json!({
                            "seq": seq, "bytes": 0, "accepted": true, "via": "probe",
                        }));
                    });
                }
            }
            Ok(())
        }
    }

    impl RecorderTarget {
        fn new(rows: Vec<Value>) -> Self {
            Self {
                rows,
                page_extra: json!({"interactivityEventsPresent": true, "freshForSecs": 1800}),
                attach_refusal: Mutex::new(Default::default()),
                input_ack: None,
                wire: Arc::new(WireRecorder::default()),
                reported: Mutex::new(Vec::new()),
                registered: Mutex::new(Vec::new()),
                forgotten: Mutex::new(Vec::new()),
                minted: Mutex::new(0),
                live_tab: None,
                relay_up: true,
                read_probe_hangs: false,
                attach_hangs: false,
                mint_delay: None,
                mint_fails: None,
                attempts: ProbeAttempts::default(),
            }
        }

        fn refuse_attach(&self, session: &str, code: &str) {
            self.attach_refusal
                .lock()
                .unwrap()
                .insert(session.to_string(), code.to_string());
        }

        fn reported(&self) -> Vec<Observation> {
            self.reported.lock().unwrap().clone()
        }

        /// The acceptance assertion: across the whole sweep, no input frame
        /// carried a single byte.
        fn assert_no_input_byte_was_ever_sent(&self) {
            for f in self.wire.frames.lock().unwrap().iter() {
                if f["type"] == "remote_terminal_input" {
                    assert_eq!(f["data"], "", "a probe typed into a live session: {f}");
                    assert_eq!(f["probe"], true, "{f}");
                }
            }
        }

        fn input_frames(&self) -> usize {
            self.wire
                .frames
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f["type"] == "remote_terminal_input")
                .count()
        }
    }

    #[async_trait]
    impl ProbeDoors for RecorderTarget {
        async fn fleet_page(
            &self,
            device_id: &str,
            cursor: Option<String>,
        ) -> Result<Value, String> {
            assert_eq!(device_id, DEVICE);
            assert!(cursor.is_none());
            // Serve what coord would: each row's facts are the newest
            // observation filed for its session and half.
            let mut rows = self.rows.clone();
            for obs in self.reported() {
                for r in rows.iter_mut() {
                    if r["sessionId"] == obs.session_id.to_string() {
                        let key = match obs.half {
                            Half::Read => "readableRemotely",
                            Half::Write => "writableRemotely",
                        };
                        r[key] = json!({
                            "state": obs.state.as_str(),
                            "observedAt": obs.observed_at.to_rfc3339(),
                            "sourceDeviceId": obs.source_device_id.to_string(),
                            "via": obs.via.map(Via::as_str),
                            "reason": obs.reason,
                        });
                    }
                }
            }
            let mut body = json!({
                "sessions": rows,
                "callerDeviceId": ME,
                "nextCursor": null,
            });
            if let Value::Object(m) = &self.page_extra {
                for (k, v) in m {
                    body[k] = v.clone();
                }
            }
            Ok(body)
        }

        async fn mint(&self, session_id: Uuid) -> Result<AttachGrantResponse, String> {
            if let Some((s, d)) = &self.mint_delay {
                if *s == session_id.to_string() {
                    tokio::time::sleep(*d).await;
                }
            }
            if self.mint_fails.as_deref() == Some(session_id.to_string().as_str()) {
                return Err("remote_attach:coord_unreachable: POST …: connection refused".into());
            }
            let mut n = self.minted.lock().unwrap();
            *n += 1;
            Ok(AttachGrantResponse {
                grant: format!("grant.jwt.{session_id}"),
                grant_jti: jti(*n),
                source_device_id: Some(ME.to_string()),
                target_device_id: Some(DEVICE.to_string()),
                expires_at: None,
                target_runner: self.input_ack.map(|state| TargetRunner {
                    state: "supports".into(),
                    input_ack: Some(CapabilityReadiness {
                        state: state.into(),
                        reason: None,
                    }),
                    ..Default::default()
                }),
            })
        }

        async fn attach(
            &self,
            grant: &str,
            _cols: u16,
            _rows: u16,
        ) -> Result<AttachedReply, AttachError> {
            if self.attach_hangs {
                std::future::pending::<()>().await;
            }
            let session = grant.trim_start_matches("grant.jwt.");
            if let Some(code) = self.attach_refusal.lock().unwrap().get(session) {
                return Err(AttachError {
                    code: code.clone(),
                    message: "refused".into(),
                });
            }
            let n = *self.minted.lock().unwrap();
            Ok(AttachedReply {
                grant_jti: jti(n),
                terminal_id: format!("term-{session}"),
                ring: AttachedRing {
                    buffer: b"$ claude\r\n> ".to_vec(),
                    start_offset: 1_000,
                    total_bytes_produced: 1_012,
                    history_start: Some(0),
                },
            })
        }

        fn sink(&self) -> Arc<dyn RemoteFrameSink> {
            self.wire.clone()
        }

        fn register(&self, pane: Arc<RemotePaneIo>) {
            self.wire.panes.lock().unwrap().push(pane.clone());
            self.registered.lock().unwrap().push(pane);
        }

        async fn read_probe(
            &self,
            pane: &RemotePaneIo,
            offset: u64,
        ) -> Result<AttachedReply, AttachError> {
            if self.read_probe_hangs {
                std::future::pending::<()>().await;
            }
            // Record the buffer request as the relay would carry it.
            self.wire.frames.lock().unwrap().push(json!({
                "type": "remote_terminal_buffer",
                "grant_jti": pane.grant_jti(),
                "from_offset": offset,
                "to_offset": offset,
            }));
            assert_eq!(offset, 1_012, "the empty range at total_bytes_produced");
            Ok(AttachedReply {
                grant_jti: pane.grant_jti().to_string(),
                terminal_id: pane.terminal_id().to_string(),
                ring: AttachedRing {
                    buffer: Vec::new(),
                    start_offset: offset,
                    total_bytes_produced: offset,
                    history_start: Some(0),
                },
            })
        }

        fn forget(&self, grant_jti: &str) {
            self.forgotten.lock().unwrap().push(grant_jti.to_string());
        }

        fn report(&self, obs: Observation) {
            self.reported.lock().unwrap().push(obs);
        }

        fn local_device_id(&self) -> Option<Uuid> {
            Uuid::parse_str(ME).ok()
        }

        fn live_tab_here(&self, session_id: &str) -> bool {
            self.live_tab.as_deref() == Some(session_id)
        }

        fn relay_connected(&self) -> bool {
            self.relay_up
        }

        fn attempts(&self) -> &ProbeAttempts {
            &self.attempts
        }
    }

    /// Acceptance (3): a sweep against a recorder target never emits a
    /// non-empty input frame — and with the capability UNKNOWN (today's coord)
    /// it emits no input frame at all; the read half is filed `ok via probe`
    /// and the write half is left as coord serves it.
    #[tokio::test]
    async fn the_sweep_never_types_and_unknown_capability_sends_no_write_probe() {
        let target = RecorderTarget::new(vec![row(1, json!({})), row(2, json!({}))]);
        let report = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(report.outcomes.len(), 2);
        target.assert_no_input_byte_was_ever_sent();
        assert_eq!(
            target.input_frames(),
            0,
            "unknown capability ⇒ no write probe"
        );
        for o in &report.outcomes {
            assert_eq!(o.decision, "probed");
            let read = o.read.as_ref().unwrap();
            assert_eq!((read.state.as_str(), read.filed), ("ok", true));
            let write = o.write.as_ref().unwrap();
            assert_eq!(write.state, "unknown");
            assert_eq!(write.reason.as_deref(), Some("unprobed"));
            assert!(
                !write.filed,
                "an untested half is never filed over coord's value"
            );
            assert_eq!(o.detach.as_deref(), Some("queued"), "honest detach checked");
        }
        let reported = target.reported();
        assert_eq!(
            reported.len(),
            2,
            "one read-ok per probed row: {reported:?}"
        );
        for obs in &reported {
            assert_eq!(
                (obs.half, obs.state, obs.role),
                (Half::Read, FactState::Ok, Role::Source)
            );
            assert_eq!(obs.via, Some(Via::Probe));
            assert!(obs.grant_jti.is_some());
            assert_eq!(obs.source_device_id.to_string(), ME);
        }
        // Every probe pane was detached and dropped from routing.
        assert_eq!(target.forgotten.lock().unwrap().len(), 2);
        let detaches = target
            .wire
            .frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["type"] == "remote_terminal_detach")
            .count();
        assert_eq!(detaches, 2);
        for pane in target.registered.lock().unwrap().iter() {
            assert!(pane.is_finished());
        }
    }

    /// With coord POSITIVELY saying the target acks input, exactly one
    /// zero-byte `probe: true` frame goes per row, and its ack comes back.
    /// Still never a byte, and the write fact is left for the target to file.
    #[tokio::test]
    async fn a_known_acker_gets_one_zero_byte_probe_per_row() {
        let mut target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.input_ack = Some("supports");
        let report = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        target.assert_no_input_byte_was_ever_sent();
        assert_eq!(target.input_frames(), 1);
        let w = report.outcomes[0].write.as_ref().unwrap();
        assert_eq!(w.state, "ok");
        assert!(!w.filed, "the target files the write half");
        assert!(target.reported().iter().all(|o| o.half == Half::Read));
    }

    /// Coord saying `predates` files `write: unknown/target_predates_input_ack`
    /// and sends nothing.
    #[tokio::test]
    async fn a_predating_target_is_reported_as_such_and_not_probed() {
        let mut target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.input_ack = Some("predates");
        let report = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(target.input_frames(), 0);
        let w = report.outcomes[0].write.as_ref().unwrap();
        assert_eq!(w.reason.as_deref(), Some("target_predates_input_ack"));
        assert!(w.filed);
        assert!(target
            .reported()
            .iter()
            .any(|o| o.half == Half::Write
                && o.reason.as_deref() == Some("target_predates_input_ack")));
    }

    /// Acceptance (4): a terminal held by another source is
    /// `unknown/held_by_other_source` on both halves — never `failed` — and a
    /// live holder's fresh traffic fact makes the row a skip, so the holder's
    /// `ok` is not buried.
    #[tokio::test]
    async fn a_held_terminal_is_unknown_held_by_other_source() {
        let target = RecorderTarget::new(vec![
            row(1, json!({})),
            row(
                2,
                json!({"readableRemotely": fact("ok", Some("traffic"), 60, None)}),
            ),
        ]);
        target.refuse_attach(&sid(1), "attach_terminal_busy");
        let report = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        let held = &report.outcomes[0];
        for h in [held.read.as_ref().unwrap(), held.write.as_ref().unwrap()] {
            assert_eq!(h.state, "unknown");
            assert_eq!(h.reason.as_deref(), Some("held_by_other_source"));
            assert!(h.filed);
        }
        assert_eq!(
            report.outcomes[1].skip_reason.as_deref(),
            Some("fresh_traffic")
        );
        let reported = target.reported();
        assert_eq!(reported.len(), 2);
        assert!(reported.iter().all(|o| o.state == FactState::Unknown));
        target.assert_no_input_byte_was_ever_sent();
    }

    /// A session this runner already has a live tab onto is measured by that
    /// tab's traffic; the sweep does not probe it (the relay would refuse it
    /// as held — by us).
    #[tokio::test]
    async fn a_session_with_a_live_tab_here_is_not_probed() {
        let mut target = RecorderTarget::new(vec![row(1, json!({})), row(2, json!({}))]);
        target.live_tab = Some(sid(1));
        let report = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(
            report.outcomes[0].skip_reason.as_deref(),
            Some("live_tab_here")
        );
        assert_eq!(report.outcomes[1].decision, "probed");
        assert_eq!(*target.minted.lock().unwrap(), 1);
    }

    /// Convergence: a second sweep straight after the first measures nothing
    /// new — the read is fresh and the write half is unmeasurable (unknown
    /// capability) — so it probes no row and spends no grant.
    #[tokio::test]
    async fn a_second_sweep_right_after_the_first_skips_the_row() {
        let target = RecorderTarget::new(vec![row(1, json!({}))]);
        let first = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(first.outcomes[0].decision, "probed");
        assert_eq!(*target.minted.lock().unwrap(), 1);
        let second = run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        assert_eq!(second.outcomes[0].decision, "skipped");
        assert_eq!(
            second.outcomes[0].skip_reason.as_deref(),
            Some("write_unmeasurable")
        );
        assert_eq!(*target.minted.lock().unwrap(), 1, "no second grant");
    }

    /// Convergence on the FAILURE paths: a row whose probe met a busy
    /// terminal (filed `unknown/held_by_other_source`, which does not mark the
    /// row as recently probed in coord's facts) is not re-probed by the next
    /// sweep.
    #[tokio::test]
    async fn a_row_whose_probe_failed_is_not_reprobed_by_the_next_sweep() {
        let target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.refuse_attach(&sid(1), "attach_terminal_busy");
        let first = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(first.outcomes[0].decision, "probed");
        let second = run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        assert_eq!(
            second.outcomes[0].skip_reason.as_deref(),
            Some("recently_attempted")
        );
        assert_eq!(*target.minted.lock().unwrap(), 1, "no second grant");
    }

    /// Attempts are stamped with the SWEEP's start: a slow row ahead of the
    /// row under test does not push that row's stamp later, so the next tick
    /// (PROBE_EVERY after this sweep started) finds it due.
    #[tokio::test]
    async fn attempts_are_stamped_with_the_sweep_start() {
        let mut target = RecorderTarget::new(vec![row(1, json!({})), row(2, json!({}))]);
        target.mint_delay = Some((sid(1), Duration::from_millis(400)));
        let before = std::time::Instant::now();
        run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        let after = std::time::Instant::now();
        assert!(after.duration_since(before) >= Duration::from_millis(400));
        let stamped = target.attempts.get(&sid(2)).expect("row 2 was attempted");
        assert!(
            stamped.duration_since(before) < Duration::from_millis(100),
            "row 2 stamped {:?} after the sweep began — not the sweep start",
            stamped.duration_since(before)
        );
        // The next tick, PROBE_EVERY after this sweep started, finds it due.
        assert!(!target
            .attempts
            .recently(&sid(2), 1800, before + PROBE_EVERY));
    }

    /// A failed mint notes nothing; a manual sweep ignores the attempt memory.
    #[tokio::test]
    async fn a_failed_mint_is_not_noted_and_manual_ignores_the_memory() {
        let mut target = RecorderTarget::new(vec![row(1, json!({})), row(2, json!({}))]);
        target.mint_fails = Some(sid(1));
        target.refuse_attach(&sid(2), "attach_terminal_busy");
        run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        assert!(
            target.attempts.get(&sid(1)).is_none(),
            "mint fault not noted"
        );
        assert!(target.attempts.get(&sid(2)).is_some());
        let again = run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        assert_eq!(
            again.outcomes[0].decision, "probed",
            "mint fault is retried"
        );
        assert_eq!(
            again.outcomes[1].skip_reason.as_deref(),
            Some("recently_attempted")
        );
        let manual = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(
            manual.outcomes[1].decision, "probed",
            "manual is not held back"
        );
    }

    #[test]
    fn a_short_fresh_window_keeps_half_of_itself() {
        assert_eq!(reprobe_window_secs(60), 30);
        assert_eq!(reprobe_window_secs(200), 100);
        assert_eq!(reprobe_window_secs(1800), 1200 - 120);
        assert_eq!(reprobe_window_secs(0), 0);
    }

    /// W1: when this runner stamped the probe, its stamp decides — a fact that
    /// coord filed 300 s after the previous sweep STARTED is still re-probed at
    /// the next PROBE_EVERY tick. Without a stamp the filing time is all there
    /// is, and the same fact reads as recently probed.
    #[test]
    fn this_runners_stamp_decides_over_coords_filing_time() {
        let now = Utc::now();
        // Previous sweep started PROBE_EVERY ago; its fact was filed 300 s in.
        let age = PROBE_EVERY.as_secs() as i64 - 300;
        let r = row(
            1,
            json!({
                "readableRemotely": fact("ok", Some("probe"), age, None),
                "writableRemotely": fact("unknown", None, 0, Some("unprobed")),
            }),
        );
        assert_eq!(
            skip_reason(
                &r,
                1800,
                now,
                Some(OwnProbe {
                    recent: false,
                    device: Uuid::parse_str(ME).unwrap(),
                }),
            ),
            None,
            "due at the tick"
        );
        assert_eq!(
            skip_reason(&r, 1800, now, None),
            Some("write_unmeasurable"),
            "no stamp: the filing time is all there is"
        );
        // And with the stamp inside the window the row is skipped.
        assert!(skip_reason(
            &r,
            1800,
            now,
            Some(OwnProbe {
                recent: true,
                device: Uuid::parse_str(ME).unwrap(),
            })
        )
        .is_some());
        // End to end: the attempt stamp at the previous sweep's start makes the
        // next tick's sweep probe it.
        let a = ProbeAttempts::default();
        let prev = std::time::Instant::now();
        a.note(&sid(1), prev);
        assert_eq!(
            a.recently_if_stamped(&sid(1), 1800, prev + PROBE_EVERY),
            Some(false)
        );
    }

    /// Two runners: this runner's stale stamp does NOT override another
    /// device's recent probe fact — that one is judged by its observedAt.
    #[test]
    fn another_devices_probe_fact_is_judged_by_its_filing_time() {
        let now = Utc::now();
        let other = "22222222-2222-4222-8222-222222222222";
        let mut theirs = fact("ok", Some("probe"), 300, None);
        theirs["sourceDeviceId"] = json!(other);
        let r = row(
            1,
            json!({
                "readableRemotely": theirs,
                "writableRemotely": fact("unknown", None, 0, Some("unprobed")),
            }),
        );
        let mine_stale = Some(OwnProbe {
            recent: false,
            device: Uuid::parse_str(ME).unwrap(),
        });
        assert_eq!(
            skip_reason(&r, 1800, now, mine_stale),
            Some("write_unmeasurable"),
            "the other runner probed 300 s ago — not due"
        );
    }

    /// Sweep level: a row whose OWN stamp is past the window is probed, even
    /// though coord's probe fact (filed mid-way through that earlier sweep) is
    /// younger than the window.
    #[tokio::test]
    async fn a_row_whose_own_stamp_is_past_the_window_is_probed() {
        let target = RecorderTarget::new(vec![row(
            1,
            json!({
                "readableRemotely": fact("ok", Some("probe"), PROBE_EVERY.as_secs() as i64 - 300, None),
                "writableRemotely": fact("unknown", None, 0, Some("unprobed")),
            }),
        )]);
        let prev = std::time::Instant::now()
            .checked_sub(PROBE_EVERY)
            .expect("the box has been up longer than PROBE_EVERY");
        target.attempts.note(&sid(1), prev);
        let report = run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        assert_eq!(
            report.outcomes[0].decision, "probed",
            "{:?}",
            report.outcomes[0]
        );
    }

    #[test]
    fn an_absent_trigger_is_unspecified_not_manual() {
        assert_eq!(normalize_trigger(None), "unspecified");
        assert_eq!(normalize_trigger(Some("  ")), "unspecified");
        assert_eq!(normalize_trigger(Some(" manual ")), "manual");
        assert_eq!(normalize_trigger(Some("fleet_view")), "fleet_view");
    }

    /// S4: probe facts are current only for the re-probe window (so a probed
    /// row is renewed every PROBE_EVERY and never goes stale between T+1800 and
    /// T+2400); traffic facts keep freshFor.
    #[test]
    fn probe_facts_are_current_for_the_reprobe_window_traffic_for_fresh_for() {
        let now = Utc::now();
        let age = reprobe_window_secs(1800) + 20; // past the window, inside freshFor
        assert!(age < 1800);
        let probed = row(
            1,
            json!({
                "readableRemotely": fact("ok", Some("probe"), age, None),
                "writableRemotely": fact("failed", Some("probe"), age, Some("terminal_exited")),
            }),
        );
        assert_eq!(skip_reason(&probed, 1800, now, None), None);
        let trafficked = row(
            1,
            json!({
                "readableRemotely": fact("failed", Some("traffic"), age, Some("session_not_local")),
                "writableRemotely": fact("failed", Some("traffic"), age, Some("terminal_exited")),
            }),
        );
        assert_eq!(skip_reason(&trafficked, 1800, now, None), Some("fresh"));
    }

    /// The attempt memory is bounded and honours the slack.
    #[test]
    fn probe_attempts_are_bounded_and_windowed() {
        let a = ProbeAttempts::default();
        let t = std::time::Instant::now();
        for n in 0..(PROBE_ATTEMPTS_CAP + 10) {
            a.note(&format!("s{n}"), t + Duration::from_millis(n as u64));
        }
        assert!(a.len() <= PROBE_ATTEMPTS_CAP);
        let last = format!("s{}", PROBE_ATTEMPTS_CAP + 9);
        assert!(a.recently(&last, 1800, t + Duration::from_secs(60)));
        // The window is min(PROBE_EVERY, freshFor) - slack: a row attempted a
        // few seconds into one 20-minute tick is due again at the next.
        assert!(!a.recently(
            &last,
            1800,
            // `last` was noted ~4.1 s after `t`.
            t + PROBE_EVERY - REPROBE_SLACK + Duration::from_secs(10)
        ));
        assert!(
            !a.recently("s0", 1800, t + Duration::from_secs(60)),
            "evicted"
        );
    }

    /// A sweep cancelled mid-ATTACH (after the mint, before any pane) forgets
    /// the minted grant's routing state.
    #[tokio::test]
    async fn a_sweep_cancelled_mid_attach_forgets_the_grant() {
        let mut target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.attach_hangs = true;
        let r = tokio::time::timeout(
            Duration::from_millis(100),
            run_probe_sweep(&target, DEVICE, "manual"),
        )
        .await;
        assert!(r.is_err());
        assert!(target.registered.lock().unwrap().is_empty(), "no pane");
        assert_eq!(target.forgotten.lock().unwrap().clone(), vec![jti(1)]);
    }

    /// No relay: a typed `relay` error, before the fleet walk and before any
    /// mint.
    #[tokio::test]
    async fn no_relay_is_a_typed_error_and_mints_nothing() {
        let mut target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.relay_up = false;
        let e = run_probe_sweep(&target, DEVICE, "manual")
            .await
            .unwrap_err();
        assert_eq!(e.door, "relay");
        assert_eq!(*target.minted.lock().unwrap(), 0);
    }

    /// A sweep future dropped mid-probe (after register, while the read probe
    /// waits) does not leak the probe pane: it is detached, finished, and out
    /// of routing.
    #[tokio::test]
    async fn a_cancelled_sweep_tears_the_probe_pane_down() {
        let mut target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.read_probe_hangs = true;
        let r = tokio::time::timeout(
            Duration::from_millis(100),
            run_probe_sweep(&target, DEVICE, "manual"),
        )
        .await;
        assert!(r.is_err(), "the sweep was cancelled mid-probe");
        let panes = target.registered.lock().unwrap().clone();
        assert_eq!(panes.len(), 1);
        let pane = &panes[0];
        assert!(pane.is_finished(), "wait settled");
        assert_eq!(pane.detach_outcome(), DetachOutcome::Queued);
        assert_eq!(
            target.forgotten.lock().unwrap().clone(),
            vec![pane.grant_jti().to_string()],
            "dropped from routing"
        );
        let detaches = target
            .wire
            .frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f["type"] == "remote_terminal_detach")
            .count();
        assert_eq!(
            detaches, 1,
            "exactly one detach even though release+kill both ran"
        );
    }

    /// Back-to-back sweeps of one device are throttled for SWEEP_MIN_INTERVAL.
    #[test]
    fn finished_sweeps_throttle_the_next_one() {
        let dev = "throttle-test-device";
        let t0 = std::time::Instant::now();
        assert!(throttled_for(dev, t0).is_none());
        note_finished(dev, t0);
        assert!(throttled_for(dev, t0 + Duration::from_secs(10)).is_some());
        assert!(throttled_for(&dev.to_uppercase(), t0 + Duration::from_secs(10)).is_some());
        assert!(throttled_for(dev, t0 + SWEEP_MIN_INTERVAL).is_none());
    }

    #[test]
    fn the_fleet_view_stamp_is_rewritten_only_when_due() {
        let now = Utc::now();
        let mut map = BTreeMap::new();
        assert!(needs_restamp(&map, "Dev-A", now));
        record_fleet_view_opened(&mut map, "Dev-A", now);
        assert!(!needs_restamp(
            &map,
            "dev-a",
            now + chrono::Duration::minutes(59)
        ));
        assert!(needs_restamp(
            &map,
            "dev-a",
            now + chrono::Duration::minutes(61)
        ));
        map.insert("dev-b".into(), "garbage".into());
        assert!(needs_restamp(&map, "dev-b", now));
    }

    /// The off switch: a target with `accept_remote_attach: off` refuses the
    /// attach, and that is filed as the true statement `failed/remote_attach_disabled`.
    #[tokio::test]
    async fn remote_attach_off_is_failed_remote_attach_disabled() {
        let target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.refuse_attach(&sid(1), "remote_attach_disabled");
        let report = run_probe_sweep(&target, DEVICE, "scheduler").await.unwrap();
        let r = report.outcomes[0].read.as_ref().unwrap();
        assert_eq!(
            (r.state.as_str(), r.reason.as_deref()),
            ("failed", Some("remote_attach_disabled"))
        );
        assert!(target
            .reported()
            .iter()
            .all(|o| o.reason.as_deref() == Some("remote_attach_disabled")));
    }

    /// Skips: caller device, closed, not interactive, fresh traffic, both
    /// fresh. An unknown or stale fact is never fresh.
    #[test]
    fn row_skip_rules() {
        let now = Utc::now();
        let f = 1800;
        assert_eq!(
            skip_reason_no_stamp(&row(1, json!({"isCallerDevice": true})), f, now),
            Some("caller_device")
        );
        assert_eq!(
            skip_reason_no_stamp(&row(1, json!({"state": "closed"})), f, now),
            Some("closed")
        );
        assert_eq!(
            skip_reason_no_stamp(&row(1, json!({"interactiveSurface": "none"})), f, now),
            Some("not_interactive")
        );
        assert_eq!(
            skip_reason_no_stamp(
                &row(
                    1,
                    json!({"writableRemotely": fact("ok", Some("traffic"), 10, None)})
                ),
                f,
                now
            ),
            Some("fresh_traffic")
        );
        assert_eq!(
            skip_reason_no_stamp(
                &row(
                    1,
                    json!({
                        "readableRemotely": fact("ok", Some("probe"), 10, None),
                        "writableRemotely": fact("failed", Some("probe"), 10, Some("terminal_exited")),
                    })
                ),
                f,
                now
            ),
            Some("fresh")
        );
        // One half fresh is not enough while the write half is measurable (a
        // STALE write fact, which a probe against an acking target can move);
        // an aged fact is not fresh; unknown never is.
        assert_eq!(
            skip_reason_no_stamp(
                &row(
                    1,
                    json!({
                        "readableRemotely": fact("failed", Some("traffic"), 1500, Some("session_not_local")),
                        "writableRemotely": fact("unknown", Some("traffic"), 4000, Some("stale")),
                    })
                ),
                f,
                now
            ),
            None
        );
        // Convergence: a fresh read with an unmeasurable write is skipped...
        for reason in ["unprobed", "target_predates_input_ack"] {
            assert_eq!(
                skip_reason_no_stamp(
                    &row(
                        1,
                        json!({
                            "readableRemotely": fact("ok", Some("probe"), 600, None),
                            "writableRemotely": fact("unknown", None, 0, Some(reason)),
                        })
                    ),
                    f,
                    now
                ),
                Some("write_unmeasurable"),
                "{reason}"
            );
        }
        // ...and so is a read any probe filed within the re-probe window.
        assert_eq!(
            skip_reason_no_stamp(
                &row(
                    1,
                    json!({
                        "readableRemotely": fact("ok", Some("probe"), 10, None),
                        "writableRemotely": fact("unknown", Some("traffic"), 4000, Some("stale")),
                    })
                ),
                f,
                now
            ),
            Some("recently_probed")
        );
        // Non-live states are skipped like coord's NON_LIVE_STATES.
        for (state, why) in [
            ("stale", "stale"),
            ("expected", "expected"),
            ("closed", "closed"),
        ] {
            assert_eq!(
                skip_reason_no_stamp(&row(1, json!({"state": state})), f, now),
                Some(why)
            );
        }
        assert_eq!(
            skip_reason_no_stamp(
                &row(
                    1,
                    json!({"readableRemotely": fact("ok", Some("traffic"), 1900, None)})
                ),
                f,
                now
            ),
            None
        );
        // not_runner_hosted and unknown surfaces are still probed.
        assert_eq!(
            skip_reason_no_stamp(
                &row(1, json!({"interactiveSurface": "not_runner_hosted"})),
                f,
                now
            ),
            None
        );
        // An older coord with no fact fields: probed (but see the sweep's
        // coord_predates_interactivity arm).
        let mut bare = row(1, json!({}));
        for k in ["readableRemotely", "writableRemotely", "interactiveSurface"] {
            bare.as_object_mut().unwrap().remove(k);
        }
        assert_eq!(skip_reason_no_stamp(&bare, f, now), None);
    }

    /// A coord that predates the interactivity facts gets no probes at all:
    /// it has no door to record them in.
    #[tokio::test]
    async fn an_older_coord_is_not_probed() {
        let mut target = RecorderTarget::new(vec![row(1, json!({}))]);
        target.page_extra = json!({});
        let report = run_probe_sweep(&target, DEVICE, "manual").await.unwrap();
        assert_eq!(report.flags.interactivity_events_present, None);
        assert!(!report.flags.fresh_for_secs_served);
        assert_eq!(
            report.outcomes[0].skip_reason.as_deref(),
            Some("coord_predates_interactivity")
        );
        assert_eq!(*target.minted.lock().unwrap(), 0, "no grant spent");
    }

    /// The fleet read failing is a typed error naming the door.
    #[tokio::test]
    async fn a_failed_fleet_read_names_the_door() {
        struct Down;
        #[async_trait]
        impl ProbeDoors for Down {
            async fn fleet_page(&self, _: &str, _: Option<String>) -> Result<Value, String> {
                Err("GET /coord/sessions/fleet returned 503".into())
            }
            async fn mint(&self, _: Uuid) -> Result<AttachGrantResponse, String> {
                unreachable!()
            }
            async fn attach(&self, _: &str, _: u16, _: u16) -> Result<AttachedReply, AttachError> {
                unreachable!()
            }
            fn sink(&self) -> Arc<dyn RemoteFrameSink> {
                unreachable!()
            }
            fn register(&self, _: Arc<RemotePaneIo>) {}
            async fn read_probe(
                &self,
                _: &RemotePaneIo,
                _: u64,
            ) -> Result<AttachedReply, AttachError> {
                unreachable!()
            }
            fn forget(&self, _: &str) {}
            fn report(&self, _: Observation) {}
            fn local_device_id(&self) -> Option<Uuid> {
                None
            }
            fn live_tab_here(&self, _: &str) -> bool {
                false
            }
            fn relay_connected(&self) -> bool {
                true
            }
            fn attempts(&self) -> &ProbeAttempts {
                unreachable!()
            }
        }
        let e = run_probe_sweep(&Down, DEVICE, "manual").await.unwrap_err();
        assert_eq!(e.door, "coord_fleet");
        assert!(e
            .to_string()
            .starts_with("remote_interactivity_probe:coord_fleet:"));
    }

    /// The probe sink refuses any input with bytes, or without the probe flag.
    #[test]
    fn the_probe_sink_refuses_real_input() {
        let wire = Arc::new(WireRecorder::default());
        let sink = ProbeFrameSink::new(wire.clone());
        assert!(sink
            .send_frame(json!({"type": "remote_terminal_input", "data": "aA==", "probe": true}))
            .is_err());
        assert!(sink
            .send_frame(json!({"type": "remote_terminal_input", "data": ""}))
            .is_err());
        assert!(sink
            .send_frame(json!({"type": "remote_terminal_input", "data": "", "probe": true}))
            .is_ok());
        assert!(sink
            .send_frame(json!({"type": "remote_terminal_detach"}))
            .is_ok());
        assert_eq!(
            wire.frames.lock().unwrap().len(),
            2,
            "refused frames never reach the wire"
        );
        // And a pane writer over it cannot type either.
        let pane = RemotePaneIo::new(
            "j",
            "t",
            "g",
            Arc::new(sink),
            80,
            24,
            AttachedRing::default(),
        );
        let mut w = pane.writer().unwrap();
        assert!(std::io::Write::write(&mut w, b"x").is_err());
    }

    /// One sweep per device at a time.
    #[test]
    fn in_flight_claims_are_exclusive_per_device() {
        let a = InFlight::claim("DEV-X").expect("first");
        assert!(InFlight::claim("dev-x").is_none(), "case-insensitive");
        assert!(InFlight::claim("dev-y").is_some());
        drop(a);
        assert!(InFlight::claim("dev-x").is_some());
    }

    #[test]
    fn fleet_view_recency_records_prunes_and_selects() {
        let now = Utc::now();
        let mut map = BTreeMap::new();
        map.insert(
            "old".to_string(),
            (now - chrono::Duration::days(8)).to_rfc3339(),
        );
        map.insert("garbage".to_string(), "not a time".to_string());
        map.insert(
            "recent".to_string(),
            (now - chrono::Duration::days(6)).to_rfc3339(),
        );
        record_fleet_view_opened(&mut map, "  NEW-Device ", now);
        assert!(!map.contains_key("old"), "pruned past seven days");
        assert!(!map.contains_key("garbage"));
        assert!(map.contains_key("new-device"));
        assert_eq!(
            recent_fleet_view_devices(&map, now),
            vec!["new-device".to_string(), "recent".to_string()]
        );
        // A week later only the stamp renewed then survives.
        assert!(recent_fleet_view_devices(&map, now + chrono::Duration::days(8)).is_empty());
    }
}
