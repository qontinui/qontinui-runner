//! Remote session interactivity — the runner's REPORTER to coord's recording
//! door (plan
//! `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
//! Phase A2, runner half).
//!
//! # The two facts, and which end measures which
//!
//! * `read` — bytes a session produced reached a SOURCE device's pane. The
//!   SOURCE measures it: its [`crate::terminal::remote_pane_io::RemotePaneIo`]
//!   spliced an `attached` / `output` / `buffer` frame under the grant.
//! * `write` — bytes a source sent were written into the session's PTY. The
//!   TARGET measures it: `apply_terminal_input` passed the grant gate and the
//!   sink (or `probe_writable`) said `Ok`, which is exactly when it emits an
//!   accepted `terminal_input_ack`.
//!
//! Coord enforces that binding (`403 half_not_measured_by_reporter`): only the
//! source may file `read: ok`, only the target `write: ok`; either may file
//! `failed` / `unknown` for the other half. [`Observation::new`] enforces the
//! same rule HERE, so a report coord would refuse is never queued.
//!
//! # Telemetry must never fail or slow the call it observes
//!
//! Modelled on `session::coord_transport_rung` — the same posture, a different
//! transport. [`Reporter::observe`] is called on the relay's inbound path (a
//! spliced output frame, a keystroke's ack), so it:
//!
//! * never awaits and never does I/O — it takes one short `std` lock, decides,
//!   and returns;
//! * COALESCES before anything is queued: an observation is kept only when its
//!   `(state, reason)` differs from the last one kept for the same
//!   `(session, half, source device, role)` — a TRANSITION — or when
//!   [`REPORT_EVERY`] has passed since that one. A burst of output frames or
//!   keystrokes is therefore one row per five minutes, never one per frame;
//! * queues into a BOUNDED deque ([`REPORT_QUEUE_CAP`]); a full queue drops its
//!   OLDEST entry (counted), so a coord outage can cost old observations but
//!   never memory and never the caller's time;
//! * is drained by ONE background worker, one POST at a time, each bounded by
//!   [`POST_TIMEOUT`], with NO retry: a failed POST is logged and dropped. The
//!   coalescer already recorded the attempt, so an outage produces at most one
//!   attempt per key per [`REPORT_EVERY`] — there is no retry storm to bound.
//!
//! # Wire (coord `session_interactivity.rs`, matched exactly)
//!
//! `POST /coord/sessions/{session_id}/interactivity-observations` with the
//! runner's device JWT (`coord_http::coord_post`) and the body
//! `{half, state, source_device_id, via, reason, observed_at, grant_jti}`.
//! `grant_jti` must be a UUID (coord mints UUIDv7 jtis); a SOURCE report MUST
//! carry it — coord authorises a source only against the grant row it minted.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tracing::{debug, warn};
use uuid::Uuid;

/// A fact older than this is served by coord as `unknown/stale`. Echoed on the
/// fleet response as `freshForSecs`; this is the fallback when a coord
/// response does not carry it.
pub const FRESH_FOR_SECS: i64 = 1800;

/// Coalescing interval: at most one report per `(session, half, source,
/// role)` per this, unless the state changes.
pub const REPORT_EVERY: Duration = Duration::from_secs(300);

/// How often the runner's own scheduler re-probes a device whose Fleet view
/// was opened recently. Under [`FRESH_FOR_SECS`], so a probed device's facts
/// never go stale between sweeps.
pub const PROBE_EVERY: Duration = Duration::from_secs(1200);

/// How long a write probe waits for the target's `terminal_input_ack`.
pub const INPUT_ACK_DEADLINE: Duration = Duration::from_secs(5);

/// Bound on queued, not-yet-POSTed observations. Drop-oldest past it.
pub const REPORT_QUEUE_CAP: usize = 256;

/// Per-POST deadline. A slow coord costs the worker, never a caller.
pub const POST_TIMEOUT: Duration = Duration::from_secs(5);

/// HARD bound on the coalescer's memory. Past it, entries older than two
/// [`REPORT_EVERY`] windows are forgotten first (they could not suppress
/// anything anyway); if that frees nothing, the OLDEST entry is evicted — the
/// worst it costs is one extra report for that key.
pub const COALESCER_KEYS_CAP: usize = 4096;

/// Coord's closed `unknown` vocabulary (`session_interactivity::UNKNOWN_REASONS`).
pub const UNKNOWN_REASONS: [&str; 6] = [
    "unprobed",
    "stale",
    "held_by_other_source",
    "target_unreachable",
    "events_unreadable",
    "target_predates_input_ack",
];

pub const REASON_HELD_BY_OTHER_SOURCE: &str = "held_by_other_source";
pub const REASON_TARGET_UNREACHABLE: &str = "target_unreachable";
pub const REASON_TARGET_PREDATES_INPUT_ACK: &str = "target_predates_input_ack";

/// A `failed` reason is a wire refusal code carried verbatim — shape
/// `^[a-z][a-z0-9_]{0,63}$`, coord's `is_wire_refusal_code`.
pub fn is_wire_refusal_code(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Half {
    Read,
    Write,
}

impl Half {
    pub const BOTH: [Half; 2] = [Half::Read, Half::Write];

    pub fn as_str(self) -> &'static str {
        match self {
            Half::Read => "read",
            Half::Write => "write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactState {
    Ok,
    Failed,
    Unknown,
}

impl FactState {
    pub fn as_str(self) -> &'static str {
        match self {
            FactState::Ok => "ok",
            FactState::Failed => "failed",
            FactState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Via {
    Traffic,
    Probe,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Traffic => "traffic",
            Via::Probe => "probe",
        }
    }

    /// The ack's `via`. Anything but `probe` is traffic — the target spells
    /// only these two.
    pub fn from_wire(s: Option<&str>) -> Via {
        match s {
            Some("probe") => Via::Probe,
            _ => Via::Traffic,
        }
    }
}

/// Which end of the attach this runner is, for this report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// This runner opened the attach (the pane is here).
    Source,
    /// This runner hosts the session's PTY.
    Target,
}

impl Role {
    /// The only half this role may call `ok` — coord's `ReporterRole::measures`.
    pub fn measures(self) -> Half {
        match self {
            Role::Source => Half::Read,
            Role::Target => Half::Write,
        }
    }
}

/// One observation, validated against coord's door before it is queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub session_id: Uuid,
    pub half: Half,
    pub state: FactState,
    pub source_device_id: Uuid,
    pub via: Option<Via>,
    pub reason: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub grant_jti: Option<Uuid>,
    pub role: Role,
}

impl Observation {
    /// Build an observation, refusing any body coord's door would refuse, so
    /// the queue only ever holds reports that can land:
    ///
    /// * `ok` carries no reason; `unknown` needs a reason from
    ///   [`UNKNOWN_REASONS`]; `failed` needs a wire-refusal-shaped code.
    /// * `ok` only for the half the role measures.
    /// * a SOURCE report must carry `grant_jti`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        role: Role,
        session_id: Uuid,
        half: Half,
        state: FactState,
        source_device_id: Uuid,
        via: Option<Via>,
        reason: Option<&str>,
        grant_jti: Option<Uuid>,
        observed_at: DateTime<Utc>,
    ) -> Result<Observation, String> {
        let reason = reason.map(str::trim).filter(|r| !r.is_empty());
        match (state, reason) {
            (FactState::Ok, Some(r)) => {
                return Err(format!("state=ok carries no reason (got {r:?})"))
            }
            (FactState::Unknown, r) if !r.is_some_and(|r| UNKNOWN_REASONS.contains(&r)) => {
                return Err(format!(
                    "state=unknown needs a reason from {UNKNOWN_REASONS:?} (got {r:?})"
                ))
            }
            (FactState::Failed, r) if !r.is_some_and(is_wire_refusal_code) => {
                return Err(format!(
                    "state=failed needs a wire refusal code (got {r:?})"
                ))
            }
            _ => {}
        }
        if state == FactState::Ok && half != role.measures() {
            return Err(format!(
                "a {role:?} may not report the {} half ok — only the end that measures it",
                half.as_str()
            ));
        }
        if role == Role::Source && grant_jti.is_none() {
            return Err("a source report must carry the grant_jti it observed under".to_string());
        }
        Ok(Observation {
            session_id,
            half,
            state,
            source_device_id,
            via,
            reason: reason.map(str::to_string),
            observed_at,
            grant_jti,
            role,
        })
    }

    /// The door's request body.
    pub fn body(&self) -> Value {
        json!({
            "half": self.half.as_str(),
            "state": self.state.as_str(),
            "source_device_id": self.source_device_id.to_string(),
            "via": self.via.map(Via::as_str),
            "reason": self.reason,
            "observed_at": self.observed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "grant_jti": self.grant_jti.map(|j| j.to_string()),
        })
    }

    fn key(&self) -> CoalesceKey {
        CoalesceKey {
            session_id: self.session_id,
            half: self.half,
            source_device_id: self.source_device_id,
            role: self.role,
        }
    }
}

// ---------------------------------------------------------------------------
// Coalescing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CoalesceKey {
    session_id: Uuid,
    half: Half,
    source_device_id: Uuid,
    role: Role,
}

#[derive(Debug, Clone)]
struct LastKept {
    state: FactState,
    reason: Option<String>,
    via: Option<Via>,
    at: Instant,
}

/// Decides which observations are worth a row: a transition, or the first
/// one after [`REPORT_EVERY`] of the same.
#[derive(Debug)]
pub struct Coalescer {
    every: Duration,
    cap: usize,
    last: HashMap<CoalesceKey, LastKept>,
}

impl Coalescer {
    pub fn new(every: Duration) -> Self {
        Self::with_cap(every, COALESCER_KEYS_CAP)
    }

    pub fn with_cap(every: Duration, cap: usize) -> Self {
        Self {
            every,
            cap: cap.max(1),
            last: HashMap::new(),
        }
    }

    /// `true` = keep (and remember) this observation; `false` = coalesced.
    pub fn admit(&mut self, obs: &Observation, now: Instant) -> bool {
        let key = obs.key();
        let keep = match self.last.get(&key) {
            None => true,
            Some(prev) => {
                // A change of `via` is a transition too: "measured by a probe"
                // and "measured by real traffic" are different statements.
                prev.state != obs.state
                    || prev.reason != obs.reason
                    || prev.via != obs.via
                    || now.saturating_duration_since(prev.at) >= self.every
            }
        };
        if keep {
            if !self.last.contains_key(&key) && self.last.len() >= self.cap {
                let horizon = self.every * 2;
                self.last
                    .retain(|_, v| now.saturating_duration_since(v.at) < horizon);
                while self.last.len() >= self.cap {
                    let oldest = self.last.iter().min_by_key(|(_, v)| v.at).map(|(k, _)| *k);
                    match oldest {
                        Some(k) => {
                            self.last.remove(&k);
                        }
                        None => break,
                    }
                }
            }
            self.last.insert(
                key,
                LastKept {
                    state: obs.state,
                    reason: obs.reason.clone(),
                    via: obs.via,
                    at: now,
                },
            );
        }
        keep
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.last.len()
    }
}

// ---------------------------------------------------------------------------
// The reporter
// ---------------------------------------------------------------------------

/// Where kept observations go. Production is [`HttpObservationSender`]; tests
/// hand in a recorder.
#[async_trait]
pub trait ObservationSender: Send + Sync {
    async fn send(&self, obs: &Observation) -> Result<(), String>;
}

/// What [`Reporter::observe`] did with an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    /// Kept and queued for the worker.
    Queued,
    /// Kept and queued, and the queue was full so its OLDEST entry was dropped.
    QueuedDroppingOldest,
    /// Suppressed by the coalescer (same state inside [`REPORT_EVERY`]).
    Coalesced,
}

/// Counters, for logs and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReporterStats {
    pub queued: usize,
    pub coalesced: u64,
    pub dropped_oldest: u64,
    pub sent: u64,
    pub failed: u64,
}

struct Inner {
    coalescer: Coalescer,
    queue: VecDeque<Observation>,
    stats: ReporterStats,
}

pub struct Reporter {
    inner: Mutex<Inner>,
    cap: usize,
    notify: tokio::sync::Notify,
    sender: Arc<dyn ObservationSender>,
    /// Whether [`Self::observe`] should start the background worker. Off for a
    /// test reporter, which is drained by hand with [`Self::drain_once`].
    spawn_worker: bool,
    worker_started: AtomicBool,
}

impl Reporter {
    pub fn new(
        sender: Arc<dyn ObservationSender>,
        every: Duration,
        cap: usize,
        spawn_worker: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                coalescer: Coalescer::new(every),
                queue: VecDeque::new(),
                stats: ReporterStats::default(),
            }),
            cap: cap.max(1),
            notify: tokio::sync::Notify::new(),
            sender,
            spawn_worker,
            worker_started: AtomicBool::new(false),
        })
    }

    /// Offer one observation. Never awaits, never does I/O.
    pub fn observe(self: &Arc<Self>, obs: Observation) -> Enqueued {
        self.observe_at(obs, Instant::now())
    }

    fn observe_at(self: &Arc<Self>, obs: Observation, now: Instant) -> Enqueued {
        let outcome = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !g.coalescer.admit(&obs, now) {
                g.stats.coalesced += 1;
                return Enqueued::Coalesced;
            }
            let mut outcome = Enqueued::Queued;
            if g.queue.len() >= self.cap {
                g.queue.pop_front();
                g.stats.dropped_oldest += 1;
                outcome = Enqueued::QueuedDroppingOldest;
            }
            g.queue.push_back(obs);
            g.stats.queued = g.queue.len();
            outcome
        };
        if outcome == Enqueued::QueuedDroppingOldest {
            warn!(
                cap = self.cap,
                "remote interactivity: report queue full — dropped the oldest observation"
            );
        }
        self.notify.notify_one();
        if self.spawn_worker && !self.worker_started.swap(true, Ordering::AcqRel) {
            let me = self.clone();
            tauri::async_runtime::spawn(async move { me.run_worker().await });
        }
        outcome
    }

    pub fn stats(&self) -> ReporterStats {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).stats
    }

    fn pop(&self) -> Option<Observation> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let obs = g.queue.pop_front();
        g.stats.queued = g.queue.len();
        obs
    }

    /// Send the oldest queued observation, if any. `None` when the queue was
    /// empty. Never retried: a failure is counted, logged and dropped.
    pub async fn drain_once(&self) -> Option<Result<(), String>> {
        let obs = self.pop()?;
        let result = self.sender.send(&obs).await;
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &result {
            Ok(()) => g.stats.sent += 1,
            Err(e) => {
                g.stats.failed += 1;
                warn!(
                    session_id = %obs.session_id,
                    half = obs.half.as_str(),
                    state = obs.state.as_str(),
                    error = %e,
                    "remote interactivity: observation not recorded by coord (dropped, not retried)"
                );
            }
        }
        Some(result)
    }

    async fn run_worker(self: Arc<Self>) {
        loop {
            while self.drain_once().await.is_some() {}
            self.notify.notified().await;
        }
    }
}

/// The production sender: the device-authed coord door.
pub struct HttpObservationSender;

#[async_trait]
impl ObservationSender for HttpObservationSender {
    async fn send(&self, obs: &Observation) -> Result<(), String> {
        let Some(http) = crate::coord_http::coord_client() else {
            return Err("coord HTTP client unavailable".to_string());
        };
        let url = format!(
            "{}/coord/sessions/{}/interactivity-observations",
            coord_base(),
            obs.session_id
        );
        // coord-tenant-scope(device): the door authorises the REPORTER DEVICE
        // (the session's own device, or the source of a grant coord minted to
        // this device) and derives nothing from the credential's tenant claim
        // — `session_interactivity.rs` says so in terms, because a dual-tenant
        // device's JWT may carry the other binding. The default binding's
        // device JWT names the right device by construction.
        let resp = crate::coord_http::coord_post(http, &url)
            .timeout(POST_TIMEOUT)
            .json(&obs.body())
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            debug!(
                session_id = %obs.session_id,
                half = obs.half.as_str(),
                state = obs.state.as_str(),
                "remote interactivity: observation recorded"
            );
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(format!(
            "POST {url} answered {}: {}",
            status.as_u16(),
            crate::str_utils::truncate_str_ellipsis(&body, 300)
        ))
    }
}

static COORD_BASE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

/// Record the coord base this runner's attach grants are minted against, so
/// observations land on the same coord that holds the grant rows. Called on
/// every relay connect and every attach.
pub fn set_coord_base(base: &str) {
    let slot = COORD_BASE.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = slot.lock() {
        *g = Some(base.trim_end_matches('/').to_string());
    }
}

fn coord_base() -> String {
    COORD_BASE
        .get()
        .and_then(|m| m.lock().ok().and_then(|g| g.clone()))
        .unwrap_or_else(|| {
            qontinui_runner_lib::profiles::coord_base_with_source()
                .0
                .trim_end_matches('/')
                .to_string()
        })
}

static REPORTER: OnceLock<Arc<Reporter>> = OnceLock::new();

/// The process-wide reporter.
pub fn reporter() -> &'static Arc<Reporter> {
    REPORTER.get_or_init(|| {
        Reporter::new(
            Arc::new(HttpObservationSender),
            REPORT_EVERY,
            REPORT_QUEUE_CAP,
            true,
        )
    })
}

// ---------------------------------------------------------------------------
// Role mappings — pure
// ---------------------------------------------------------------------------

/// What a SOURCE knows about the attach a pane (or a probe) runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReportContext {
    /// The coord session the attach is to.
    pub session_id: Uuid,
    /// The coord-minted grant the attach runs under.
    pub grant_jti: Uuid,
    /// THIS device — the one coord minted the grant to.
    pub source_device_id: Uuid,
    /// `traffic` for a tab, `probe` for the probe sweep.
    pub via: Via,
}

impl SourceReportContext {
    /// Build from the strings the attach flow carries. `None` (and nothing is
    /// reported) when any id is not a UUID — coord would refuse the report.
    pub fn parse(
        session_id: &str,
        grant_jti: &str,
        source_device_id: Option<&str>,
        via: Via,
    ) -> Option<SourceReportContext> {
        let source = source_device_id
            .and_then(|s| Uuid::parse_str(s.trim()).ok())
            .or_else(crate::agent_runtime::load_local_device_id)?;
        Some(SourceReportContext {
            session_id: Uuid::parse_str(session_id.trim()).ok()?,
            grant_jti: Uuid::parse_str(grant_jti.trim()).ok()?,
            source_device_id: source,
            via,
        })
    }

    /// `read: ok` — a frame from the target was spliced here.
    pub fn read_ok(&self, at: DateTime<Utc>) -> Observation {
        Observation::new(
            Role::Source,
            self.session_id,
            Half::Read,
            FactState::Ok,
            self.source_device_id,
            Some(self.via),
            None,
            Some(self.grant_jti),
            at,
        )
        .expect("a source read-ok with a grant is always a valid observation")
    }

    /// Observations for the half (or halves) a refusal speaks to.
    pub fn refusal(&self, code: &str, halves: &[Half], at: DateTime<Utc>) -> Vec<Observation> {
        let Some((state, reason)) = classify_refusal(code) else {
            return Vec::new();
        };
        halves
            .iter()
            .filter_map(|half| {
                Observation::new(
                    Role::Source,
                    self.session_id,
                    *half,
                    state,
                    self.source_device_id,
                    Some(self.via),
                    Some(reason),
                    Some(self.grant_jti),
                    at,
                )
                .ok()
            })
            .collect()
    }
}

/// Codes that describe THIS runner's own relay or bookkeeping, not the target:
/// nothing about the remote session was observed, so nothing is filed.
const LOCAL_ONLY_CODES: &[&str] = &[
    "relay_unavailable",
    "relay_disconnected",
    "attach_canceled",
    "history_canceled",
    "grant_mismatch",
    "target_mismatch",
    "session_spawn_failed",
];

/// Codes that mean the TARGET could not be reached (or the relay could not
/// carry the attempt), which is `unknown/target_unreachable` — not a refusal.
const UNREACHABLE_CODES: &[&str] = &[
    "timeout",
    "target_not_connected",
    "attach_verifier_unavailable",
    "attach_registry_unavailable",
    "listener_lost",
];

/// The relay's one-holder binding refused: someone else holds the terminal.
/// Not a failure of the session.
const HELD_CODES: &[&str] = &["attach_terminal_busy"];

/// Map a wire refusal code to the fact it establishes. `None` = nothing about
/// the target was observed (a local code, or one that is not a wire code).
pub fn classify_refusal(code: &str) -> Option<(FactState, &str)> {
    let code = code.trim();
    // `attach_grant_*` (expired / unknown / consumed / wrong_source / invalid)
    // describe THIS source's credential, not the session: a grant that aged
    // out or was never recorded says nothing about whether the session is
    // reachable. Nothing is filed for them.
    if LOCAL_ONLY_CODES.contains(&code)
        || code.starts_with("coord_")
        || code.starts_with("attach_grant_")
    {
        return None;
    }
    if HELD_CODES.contains(&code) {
        return Some((FactState::Unknown, REASON_HELD_BY_OTHER_SOURCE));
    }
    if UNREACHABLE_CODES.contains(&code) {
        return Some((FactState::Unknown, REASON_TARGET_UNREACHABLE));
    }
    is_wire_refusal_code(code).then_some((FactState::Failed, code))
}

/// The TARGET's report for one `terminal_input_ack` it is about to send:
/// `write: ok` for an accepted ack, `write: failed` with the bare code for a
/// refused one. `source_device_id` and `session_id` come from the admitted
/// grant row (coord's directive), never from the frame. `None` for anything
/// that is not an ack, or a grant whose ids are not UUIDs.
pub fn target_ack_observation(
    ack: &Value,
    grant: &crate::mcp::remote_terminal::AttachGrant,
    at: DateTime<Utc>,
) -> Option<Observation> {
    if ack.get("type").and_then(|v| v.as_str()) != Some("terminal_input_ack") {
        return None;
    }
    let source = Uuid::parse_str(grant.source_device_id.trim()).ok()?;
    let via = Via::from_wire(ack.get("via").and_then(|v| v.as_str()));
    let accepted = ack
        .get("accepted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (state, reason) = if accepted {
        (FactState::Ok, None)
    } else {
        let code = ack
            .get("error")
            .and_then(|v| v.as_str())
            .filter(|c| is_wire_refusal_code(c))
            .unwrap_or(crate::mcp::remote_terminal::INPUT_ACK_INPUT_WRITE_FAILED);
        (FactState::Failed, Some(code))
    };
    Observation::new(
        Role::Target,
        grant.session_id,
        Half::Write,
        state,
        source,
        Some(via),
        reason,
        // Optional for the target role; coord cross-checks it when present.
        Uuid::parse_str(grant.grant_jti.trim()).ok(),
        at,
    )
    .ok()
}

/// Target side entry point: report the ack `apply_terminal_input` returned
/// for an admitted remote input. Looks the grant up in the target's own table
/// (the row coord published), so the source device is coord's, not the
/// frame's. Never blocks.
pub fn report_target_ack(ack: &Value) {
    let Some(jti) = ack.get("grant_jti").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(grant) = crate::mcp::remote_terminal::grants().get(jti) else {
        return;
    };
    if let Some(obs) = target_ack_observation(ack, &grant, Utc::now()) {
        reporter().observe(obs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::remote_terminal::AttachGrant;

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn ctx() -> SourceReportContext {
        SourceReportContext {
            session_id: u(1),
            grant_jti: u(2),
            source_device_id: u(3),
            via: Via::Traffic,
        }
    }

    #[derive(Default)]
    struct RecordingSender {
        sent: Mutex<Vec<Observation>>,
        fail: bool,
    }

    #[async_trait]
    impl ObservationSender for RecordingSender {
        async fn send(&self, obs: &Observation) -> Result<(), String> {
            self.sent.lock().unwrap().push(obs.clone());
            if self.fail {
                Err("coord said no".into())
            } else {
                Ok(())
            }
        }
    }

    /// A sender that never completes — a coord that hangs forever.
    struct HangingSender;

    #[async_trait]
    impl ObservationSender for HangingSender {
        async fn send(&self, _obs: &Observation) -> Result<(), String> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    fn test_reporter(sender: Arc<dyn ObservationSender>, cap: usize) -> Arc<Reporter> {
        Reporter::new(sender, REPORT_EVERY, cap, false)
    }

    #[test]
    fn the_body_is_the_doors_shape() {
        let obs = ctx().read_ok(Utc::now());
        let b = obs.body();
        assert_eq!(b["half"], "read");
        assert_eq!(b["state"], "ok");
        assert_eq!(b["via"], "traffic");
        assert!(b["reason"].is_null());
        assert_eq!(b["grant_jti"], u(2).to_string());
        assert_eq!(b["source_device_id"], u(3).to_string());
        assert!(DateTime::parse_from_rfc3339(b["observed_at"].as_str().unwrap()).is_ok());
        assert!(
            b.get("session_id").is_none(),
            "the session is the path, not the body"
        );
    }

    #[test]
    fn observation_new_refuses_what_coord_would_refuse() {
        let now = Utc::now();
        let mk = |role, half, state, reason: Option<&str>, jti: Option<Uuid>| {
            Observation::new(role, u(1), half, state, u(3), None, reason, jti, now)
        };
        // ok with a reason
        assert!(mk(
            Role::Source,
            Half::Read,
            FactState::Ok,
            Some("x"),
            Some(u(2))
        )
        .is_err());
        // unknown outside the vocabulary, and with none
        assert!(mk(
            Role::Source,
            Half::Read,
            FactState::Unknown,
            Some("nope"),
            Some(u(2))
        )
        .is_err());
        assert!(mk(
            Role::Source,
            Half::Read,
            FactState::Unknown,
            None,
            Some(u(2))
        )
        .is_err());
        // failed with a non-wire code
        assert!(mk(
            Role::Source,
            Half::Read,
            FactState::Failed,
            Some("Bad Code"),
            Some(u(2))
        )
        .is_err());
        // the role/half binding
        assert!(mk(Role::Source, Half::Write, FactState::Ok, None, Some(u(2))).is_err());
        assert!(mk(Role::Target, Half::Read, FactState::Ok, None, None).is_err());
        // a source report with no grant
        assert!(mk(
            Role::Source,
            Half::Read,
            FactState::Failed,
            Some("session_not_local"),
            None
        )
        .is_err());
        // what each role MAY file
        assert!(mk(
            Role::Source,
            Half::Write,
            FactState::Failed,
            Some("session_not_local"),
            Some(u(2))
        )
        .is_ok());
        assert!(mk(Role::Target, Half::Write, FactState::Ok, None, None).is_ok());
        assert!(mk(
            Role::Target,
            Half::Read,
            FactState::Unknown,
            Some("unprobed"),
            None
        )
        .is_ok());
    }

    /// Coalescing: the first observation and every TRANSITION are kept; a
    /// repeat inside REPORT_EVERY is not; the same state is kept again once
    /// the window has passed.
    #[test]
    fn coalescing_keeps_transitions_and_one_per_window() {
        let mut c = Coalescer::new(REPORT_EVERY);
        let t0 = Instant::now();
        let ok = ctx().read_ok(Utc::now());
        assert!(c.admit(&ok, t0), "first is kept");
        for s in 1..299 {
            assert!(
                !c.admit(&ok, t0 + Duration::from_secs(s)),
                "a repeat inside the window is coalesced (t+{s}s)"
            );
        }
        let failed = ctx()
            .refusal("session_not_local", &[Half::Read], Utc::now())
            .remove(0);
        assert!(
            c.admit(&failed, t0 + Duration::from_secs(10)),
            "a transition is kept at once"
        );
        assert!(c.admit(&ok, t0 + Duration::from_secs(11)), "and back");
        assert!(!c.admit(&ok, t0 + Duration::from_secs(12)));
        assert!(
            c.admit(&ok, t0 + Duration::from_secs(11) + REPORT_EVERY),
            "kept again after REPORT_EVERY"
        );
        // A reason change is a transition too.
        let a = ctx()
            .refusal("session_not_local", &[Half::Write], Utc::now())
            .remove(0);
        let b = ctx()
            .refusal("cross_tenant", &[Half::Write], Utc::now())
            .remove(0);
        assert!(c.admit(&a, t0));
        assert!(c.admit(&b, t0 + Duration::from_secs(1)));
    }

    /// A change of `via` (traffic after probe, or back) is a transition.
    #[test]
    fn a_via_change_is_a_transition() {
        let mut c = Coalescer::new(REPORT_EVERY);
        let t = Instant::now();
        let traffic = ctx().read_ok(Utc::now());
        let mut probe_ctx = ctx();
        probe_ctx.via = Via::Probe;
        let probe = probe_ctx.read_ok(Utc::now());
        assert!(c.admit(&traffic, t));
        assert!(!c.admit(&traffic, t + Duration::from_secs(1)));
        assert!(c.admit(&probe, t + Duration::from_secs(2)));
        assert!(c.admit(&traffic, t + Duration::from_secs(3)));
    }

    /// The map is HARD-capped: when no entry is old enough to forget, the
    /// oldest is evicted, and the map never exceeds the cap.
    #[test]
    fn the_coalescer_map_is_hard_capped() {
        let mut c = Coalescer::with_cap(REPORT_EVERY, 3);
        let t = Instant::now();
        for n in 0..10u128 {
            let mut k = ctx();
            k.session_id = u(500 + n);
            assert!(c.admit(&k.read_ok(Utc::now()), t + Duration::from_secs(n as u64)));
            assert!(c.len() <= 3, "len {} over the cap", c.len());
        }
        // The oldest were evicted, so the first key is admitted again at once;
        // the newest is still remembered.
        let mut first = ctx();
        first.session_id = u(500);
        assert!(c.admit(&first.read_ok(Utc::now()), t + Duration::from_secs(11)));
        let mut newest = ctx();
        newest.session_id = u(509);
        assert!(!c.admit(&newest.read_ok(Utc::now()), t + Duration::from_secs(12)));
    }

    /// Keys are per (session, half, source, role): another session, another
    /// half, another source device, or the other role is its own series.
    #[test]
    fn coalescing_keys_do_not_bleed() {
        let mut c = Coalescer::new(REPORT_EVERY);
        let t = Instant::now();
        let base = ctx();
        assert!(c.admit(&base.read_ok(Utc::now()), t));
        let mut other_session = base.clone();
        other_session.session_id = u(9);
        assert!(c.admit(&other_session.read_ok(Utc::now()), t));
        let mut other_source = base.clone();
        other_source.source_device_id = u(8);
        assert!(c.admit(&other_source.read_ok(Utc::now()), t));
        let write = base
            .refusal("session_not_local", &[Half::Write], Utc::now())
            .remove(0);
        assert!(c.admit(&write, t));
        let target = Observation::new(
            Role::Target,
            u(1),
            Half::Write,
            FactState::Failed,
            u(3),
            None,
            Some("session_not_local"),
            None,
            Utc::now(),
        )
        .unwrap();
        assert!(c.admit(&target, t), "the other role is its own series");
        assert_eq!(c.len(), 5);
    }

    /// A burst of 10 000 frames is ONE queued observation, never one row per
    /// frame (or per keystroke).
    #[tokio::test]
    async fn a_burst_is_one_report() {
        let sender = Arc::new(RecordingSender::default());
        let r = test_reporter(sender.clone(), REPORT_QUEUE_CAP);
        for _ in 0..10_000 {
            r.observe(ctx().read_ok(Utc::now()));
        }
        let s = r.stats();
        assert_eq!(s.queued, 1);
        assert_eq!(s.coalesced, 9_999);
        while r.drain_once().await.is_some() {}
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
    }

    /// The queue is bounded and drops its OLDEST entry.
    #[tokio::test]
    async fn the_queue_is_bounded_drop_oldest() {
        let sender = Arc::new(RecordingSender::default());
        let r = test_reporter(sender.clone(), 3);
        let mut outcomes = Vec::new();
        for n in 0..5u128 {
            let mut c = ctx();
            c.session_id = u(100 + n);
            outcomes.push(r.observe(c.read_ok(Utc::now())));
        }
        assert_eq!(
            outcomes,
            vec![
                Enqueued::Queued,
                Enqueued::Queued,
                Enqueued::Queued,
                Enqueued::QueuedDroppingOldest,
                Enqueued::QueuedDroppingOldest,
            ]
        );
        let s = r.stats();
        assert_eq!((s.queued, s.dropped_oldest), (3, 2));
        while r.drain_once().await.is_some() {}
        let sent: Vec<Uuid> = sender
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|o| o.session_id)
            .collect();
        assert_eq!(
            sent,
            vec![u(102), u(103), u(104)],
            "the two oldest were dropped"
        );
    }

    /// `observe` never blocks on the sender: with a coord that hangs forever
    /// and a worker stuck inside it, 1 000 observations still return at once.
    #[tokio::test]
    async fn observe_never_blocks_on_a_hanging_coord() {
        let r = test_reporter(Arc::new(HangingSender), 8);
        let r2 = r.clone();
        // A worker stuck inside the hanging POST.
        r.observe(ctx().read_ok(Utc::now()));
        let stuck = tokio::spawn(async move { r2.drain_once().await });
        tokio::task::yield_now().await;
        let started = Instant::now();
        for n in 0..1_000u128 {
            let mut c = ctx();
            c.session_id = u(1_000 + n);
            r.observe(c.read_ok(Utc::now()));
        }
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "observe must not wait on the sender: {:?}",
            started.elapsed()
        );
        assert_eq!(r.stats().queued, 8, "bounded while coord hangs");
        stuck.abort();
    }

    /// A failed POST is counted and dropped — not re-queued, not retried.
    #[tokio::test]
    async fn a_failed_post_is_not_retried() {
        let sender = Arc::new(RecordingSender {
            fail: true,
            ..Default::default()
        });
        let r = test_reporter(sender.clone(), 8);
        r.observe(ctx().read_ok(Utc::now()));
        assert!(matches!(r.drain_once().await, Some(Err(_))));
        assert!(r.drain_once().await.is_none(), "nothing re-queued");
        // And the coalescer still holds the attempt: a repeat is coalesced.
        assert_eq!(r.observe(ctx().read_ok(Utc::now())), Enqueued::Coalesced);
        let s = r.stats();
        assert_eq!((s.sent, s.failed), (0, 1));
    }

    #[test]
    fn refusal_classification() {
        assert_eq!(
            classify_refusal("attach_terminal_busy"),
            Some((FactState::Unknown, "held_by_other_source"))
        );
        assert_eq!(
            classify_refusal("timeout"),
            Some((FactState::Unknown, "target_unreachable"))
        );
        assert_eq!(
            classify_refusal("target_not_connected"),
            Some((FactState::Unknown, "target_unreachable"))
        );
        for code in [
            "cross_tenant",
            "session_not_local",
            "target_runner_predates_remote_attach",
            "remote_attach_disabled",
            "attach_terminal_mismatch",
        ] {
            assert_eq!(
                classify_refusal(code),
                Some((FactState::Failed, code)),
                "{code}"
            );
        }
        for local in [
            "relay_unavailable",
            "relay_disconnected",
            "coord_unreachable",
            "Not A Code",
            // The source's own credential, not the session.
            "attach_grant_unknown",
            "attach_grant_expired",
            "attach_grant_consumed",
            "attach_grant_wrong_source",
            "attach_grant_invalid",
        ] {
            assert_eq!(classify_refusal(local), None, "{local}");
        }
    }

    /// SOURCE mapping: a refusal is filed on each half asked for, with the
    /// grant and this device, and a local code files nothing.
    #[test]
    fn source_refusal_mapping() {
        let now = Utc::now();
        let obs = ctx().refusal("session_not_local", &Half::BOTH, now);
        assert_eq!(obs.len(), 2);
        for o in &obs {
            assert_eq!(o.role, Role::Source);
            assert_eq!(o.state, FactState::Failed);
            assert_eq!(o.reason.as_deref(), Some("session_not_local"));
            assert_eq!(o.grant_jti, Some(u(2)));
            assert_eq!(o.source_device_id, u(3));
            assert_eq!(o.via, Some(Via::Traffic));
        }
        assert_eq!(obs[0].half, Half::Read);
        assert_eq!(obs[1].half, Half::Write);
        let held = ctx().refusal("attach_terminal_busy", &[Half::Read], now);
        assert_eq!(held[0].state, FactState::Unknown);
        assert_eq!(held[0].reason.as_deref(), Some("held_by_other_source"));
        assert!(ctx()
            .refusal("relay_disconnected", &Half::BOTH, now)
            .is_empty());
    }

    #[test]
    fn source_context_parse_requires_uuids() {
        let s = u(1).to_string();
        let j = u(2).to_string();
        let d = u(3).to_string();
        assert!(SourceReportContext::parse(&s, &j, Some(&d), Via::Traffic).is_some());
        assert!(SourceReportContext::parse("not-a-uuid", &j, Some(&d), Via::Traffic).is_none());
        assert!(SourceReportContext::parse(&s, "jti-1", Some(&d), Via::Traffic).is_none());
    }

    fn grant() -> AttachGrant {
        AttachGrant {
            grant_jti: u(2).to_string(),
            source_device_id: u(3).to_string(),
            session_id: u(1),
            terminal_id: Some("term-A".into()),
            expires_at: 0,
        }
    }

    /// TARGET mapping: an accepted ack is `write: ok` (via echoed); a refused
    /// one is `write: failed` with the bare code; the session and source come
    /// from the grant row.
    #[test]
    fn target_ack_mapping() {
        let now = Utc::now();
        let ok = target_ack_observation(
            &json!({"type": "terminal_input_ack", "grant_jti": u(2).to_string(),
                    "accepted": true, "via": "probe", "bytes": 0}),
            &grant(),
            now,
        )
        .unwrap();
        assert_eq!(ok.role, Role::Target);
        assert_eq!((ok.half, ok.state), (Half::Write, FactState::Ok));
        assert_eq!(ok.via, Some(Via::Probe));
        assert_eq!(ok.reason, None);
        assert_eq!(ok.session_id, u(1));
        assert_eq!(ok.source_device_id, u(3));
        assert_eq!(ok.grant_jti, Some(u(2)));

        let refused = target_ack_observation(
            &json!({"type": "terminal_input_ack", "accepted": false,
                    "error": "terminal_exited", "via": "traffic"}),
            &grant(),
            now,
        )
        .unwrap();
        assert_eq!(refused.state, FactState::Failed);
        assert_eq!(refused.reason.as_deref(), Some("terminal_exited"));
        assert_eq!(refused.via, Some(Via::Traffic));

        // An off-shape error code is still a failure, under the catch-all.
        let odd = target_ack_observation(
            &json!({"type": "terminal_input_ack", "accepted": false, "error": "Weird Thing"}),
            &grant(),
            now,
        )
        .unwrap();
        assert_eq!(odd.reason.as_deref(), Some("input_write_failed"));

        // Not an ack, or a grant with non-uuid ids: nothing.
        assert!(target_ack_observation(&json!({"type": "error"}), &grant(), now).is_none());
        let mut bad = grant();
        bad.source_device_id = "src-device".into();
        assert!(target_ack_observation(
            &json!({"type": "terminal_input_ack", "accepted": true}),
            &bad,
            now
        )
        .is_none());
        // A non-uuid jti is simply omitted (optional for the target).
        let mut odd_jti = grant();
        odd_jti.grant_jti = "j1".into();
        let o = target_ack_observation(
            &json!({"type": "terminal_input_ack", "accepted": true}),
            &odd_jti,
            now,
        )
        .unwrap();
        assert_eq!(o.grant_jti, None);
    }
}
