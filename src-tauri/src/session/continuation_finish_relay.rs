//! Continuation finish RELAY — a gate continuation that already said its work
//! is over gets `session_status: finished` recorded for it (plan
//! `2026-10-28-finished-gate-continuations-never-free-a-slot-so-the-box-deadlocks-at-the-continuation-cap`,
//! Phase 2, design decision D3 "relay, never inference").
//!
//! ## Why
//!
//! A `run_skill` gate continuation is an interactive `claude` that sits at its
//! prompt once its skill ends. The close path (wind-down) may only close a
//! session whose coord row reads `finished` (served `finished-criteria`
//! `the-product-closes-only-a-finished-custody-clean-session`), so a
//! continuation that never wrote its session status holds its continuation
//! slot forever. Some continuations DO report their work outcome on their own
//! gate (`work_completed` / `work_abandoned`, through `QONTINUI_GATE_ID`) and
//! stop there. That report is the session's own statement that its single
//! assignment is over; this module relays it onto the session axis.
//!
//! ## What one tick does
//!
//! Every [`TICK`], on its OWN ticker — never the drain-gated wind-down
//! executor, which acts only while coord holds the device drained:
//!
//! 1. Snapshot the live gate continuations from the runner's own registry
//!    ([`crate::agent_runtime::live_gate_continuations`]). None ⇒ nothing else
//!    happens, no coord traffic.
//! 2. Resolve each pane's open lifecycle record (its harness session id). A
//!    record already marked finished is skipped.
//! 3. Observe each pane's grid ([`wind_down_observer::observe_terminals`], the
//!    same tracker wind-down uses — this tick also keeps those idle windows
//!    advancing). Only a pane idle continuously for the wind-down grace
//!    ([`wind_down::grace_from_env`]) goes further.
//! 4. Read that gate's `continuation_consumed_outcome` over the device-authed
//!    `GET /coord/agent-gates?gate_id=…` door. Only a `work_completed` or a
//!    `work_abandoned[: …]` is relayed, through
//!    [`SessionLifecycleStore::set_finished`] with reason
//!    `relayed_gate_outcome:<outcome>`.
//!
//! ## What it never does
//!
//! - It never relays `spawned`, `spawn_failed`, `work_unreported`, a null
//!   outcome, a missing row, a non-2xx or an undecodable answer: a session that
//!   said nothing stays open (served `verification-and-evidence`
//!   `unknown-must-not-render-as-a-default`).
//! - It never closes anything. Closing stays with wind-down under the four
//!   served conditions; this only records what the session already stated.
//! - It never reports success it did not get: a `set_finished` whose coord
//!   write was not queued (`no_coord_session`, …) is logged UNRELAYED.
//! - It never holds silently. Every per-gate hold is `debug`, so on its own a
//!   relay that can never read coord would look exactly like one with nothing
//!   to relay. The relay keeps each held gate's LAST read ([`LastRead`]) and
//!   the last credential result, folds them into one [`Reading`], and logs its
//!   [`Reading::headline`] whenever it changes: at `warn` while it is degraded
//!   (a gate unreadable, or no credential), at `info` otherwise. A degraded
//!   headline that does not change is logged again every [`DEGRADED_RELOG`],
//!   so a failure that never clears is never just one old line in the log.
//!
//! The live children hint (`has_live_children`) is deliberately NOT a
//! condition here. Every fleet session holds its boot-time `coord-mcp-shim.py`
//! child, so gating on it would make the relay inert; children are the CLOSE
//! path's concern, not the session-status one.
//!
//! Kill switch: [`ENABLE_ENV`]`=0`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use qontinui_runner_lib::wind_down::{self, GridIdle, Sideband, SidebandState};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::session::session_lifecycle_store::{FinishChange, FinishSync, SessionLifecycleStore};
use crate::session::wind_down_observer;

/// How often the relay looks. The grace window it waits out is ten minutes by
/// default, so a one-minute tick costs at most a minute of extra latency.
pub const TICK: Duration = Duration::from_secs(60);

/// Wait this long after boot before the first tick, so the registry and the
/// lifecycle store have settled.
const BOOT_SETTLE_DELAY: Duration = Duration::from_secs(90);

/// Kill switch: exactly `0` disables the relay on this machine.
pub const ENABLE_ENV: &str = "QONTINUI_CONTINUATION_FINISH_RELAY";

/// Bound on resolving the device JWT (it may reach the OS keychain).
const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(3);

/// The reason prefix recorded on the lifecycle record and forwarded to coord.
pub const REASON_PREFIX: &str = "relayed_gate_outcome:";

fn enabled() -> bool {
    std::env::var(ENABLE_ENV).ok().as_deref().map(str::trim) != Some("0")
}

// ===========================================================================
// Pure decision surface
// ===========================================================================

/// The gate read, as the relay sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcomeRead {
    /// coord served the gate row; `None` = no outcome recorded yet.
    Outcome(Option<String>),
    /// The row could not be read (no credential, transport, non-2xx, bad body,
    /// no such row). UNKNOWN — never relayed.
    Unreadable(String),
}

/// What the relay decided for one continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayDecision {
    /// Relay this token (`work_completed` / `work_abandoned`).
    Relay(&'static str),
    /// Hold, for this reason (logged at debug).
    Hold(String),
}

/// What a held gate's last read said. It is kept per gate between ticks, so
/// [`Reading`] describes every gate held after a read, not only the ones whose
/// [`RE_READ_AFTER`] clock happened to fall due this tick.
///
/// It is LAST-KNOWN, not current. A gate whose pane has gone busy is not
/// re-read, so it keeps its last read until it is read again or its session
/// leaves the candidate set. A gate never read (its pane has been busy since
/// the continuation opened) has no entry and is not counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastRead {
    /// The gate carries no outcome: the session said nothing yet.
    NoOutcome,
    /// An outcome only the dispatcher writes (`spawned`, `work_unreported`, …).
    NotRelayable,
    /// The read failed (transport, non-2xx, no such row, undecodable body).
    Unreadable(String),
}

impl LastRead {
    /// The held class `read` leaves behind, or `None` for a relayable outcome:
    /// that gate's session is about to be marked finished, not held.
    pub fn from_read(read: &GateOutcomeRead) -> Option<Self> {
        match read {
            GateOutcomeRead::Unreadable(why) => Some(LastRead::Unreadable(why.clone())),
            GateOutcomeRead::Outcome(None) => Some(LastRead::NoOutcome),
            GateOutcomeRead::Outcome(Some(raw)) => match relayable_token(raw) {
                Some(_) => None,
                None => Some(LastRead::NotRelayable),
            },
        }
    }
}

/// The relay's standing reading over every gate it holds after a read, plus
/// the last credential result. This is what it logs above `debug`. Pure data.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reading {
    /// Held gates whose last read carried no outcome.
    pub no_outcome: usize,
    /// Held gates whose last read carried a non-relayable outcome.
    pub not_relayable: usize,
    /// Held gates whose last read failed.
    pub unreadable: usize,
    /// The lexicographically first unreadable reason. A fixed pick, so two
    /// gates failing differently cannot make the headline alternate.
    pub first_unreadable: Option<String>,
    /// The last attempt to build a coord reader failed with this; `None`
    /// once one succeeds.
    pub reader_error: Option<String>,
}

impl Reading {
    /// Fold the held gates' last reads and the credential result.
    pub fn from_known<'a>(
        known: impl IntoIterator<Item = &'a LastRead>,
        reader_error: Option<&str>,
    ) -> Self {
        let mut r = Reading {
            reader_error: reader_error.map(str::to_string),
            ..Reading::default()
        };
        for last in known {
            match last {
                LastRead::NoOutcome => r.no_outcome += 1,
                LastRead::NotRelayable => r.not_relayable += 1,
                LastRead::Unreadable(why) => {
                    r.unreadable += 1;
                    if r.first_unreadable
                        .as_deref()
                        .is_none_or(|f| why.as_str() < f)
                    {
                        r.first_unreadable = Some(why.clone());
                    }
                }
            }
        }
        r
    }

    /// Nothing held after a read and no credential failure.
    pub fn is_empty(&self) -> bool {
        *self == Reading::default()
    }

    /// Whether the relay is blind somewhere: a gate unreadable, or no
    /// credential. Logged at `warn`.
    pub fn degraded(&self) -> bool {
        self.reader_error.is_some() || self.unreadable > 0
    }

    /// The one-line summary compared between ticks.
    pub fn headline(&self) -> String {
        let mut h = format!(
            "held_no_outcome={} held_not_relayable={} unreadable={}",
            self.no_outcome, self.not_relayable, self.unreadable
        );
        if let Some(why) = &self.first_unreadable {
            h.push_str(&format!(" first_unreadable=\"{why}\""));
        }
        if let Some(e) = &self.reader_error {
            h.push_str(&format!(" reader_error=\"{e}\""));
        }
        h
    }
}

/// A degraded headline that has not changed is logged again after this long.
pub const DEGRADED_RELOG: Duration = Duration::from_secs(30 * 60);

/// The headline to log now, or `None`. `last` is the last headline logged and
/// when it was logged. Pure: the unit-test surface of the change gate.
///
/// - The first line is withheld until there is something to say, so a box
///   with no held continuation logs nothing at all.
/// - After that, a CHANGED headline is logged. That includes the return to
///   empty, so a recovery is stated rather than left implied.
/// - An unchanged headline is logged again only while degraded, once every
///   [`DEGRADED_RELOG`].
pub fn headline_to_log(
    reading: &Reading,
    last: Option<(&str, std::time::Instant)>,
    now: std::time::Instant,
) -> Option<String> {
    let h = reading.headline();
    match last {
        None => (!reading.is_empty()).then_some(h),
        Some((prev, _)) if prev != h => Some(h),
        Some((_, at)) => {
            (reading.degraded() && now.saturating_duration_since(at) >= DEGRADED_RELOG).then_some(h)
        }
    }
}

/// The relayable token a raw outcome carries, or `None`.
///
/// Delegates to [`crate::agent_runtime::is_session_work_outcome`], the ONE
/// predicate for "an outcome only the session itself can write", shaped to
/// what coord persists: `work_completed` bare, `work_abandoned` bare or as
/// `work_abandoned: <detail>`. A second, looser copy here would drift from it.
pub fn relayable_token(raw: &str) -> Option<&'static str> {
    if !crate::agent_runtime::is_session_work_outcome(raw) {
        return None;
    }
    Some(if raw == "work_completed" {
        "work_completed"
    } else {
        "work_abandoned"
    })
}

/// Re-read a gate that held (no outcome yet, or a non-relayable one) at most
/// this often, so a box full of idle continuations that said nothing costs one
/// coord read per gate per window, not per tick.
pub const RE_READ_AFTER: Duration = Duration::from_secs(5 * 60);

/// Whether a gate last read at `last` is due for another read at `now`.
pub fn due_for_read(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= RE_READ_AFTER)
}

/// Whether `grid` has been idle continuously for at least `grace` at `now_ms`.
pub fn idle_past_grace(grid: GridIdle, now_ms: i64, grace: Duration) -> bool {
    match grid {
        GridIdle::Idle { since_ms } => {
            now_ms.saturating_sub(since_ms) >= i64::try_from(grace.as_millis()).unwrap_or(i64::MAX)
        }
        GridIdle::Busy | GridIdle::Unknown => false,
    }
}

/// Whether the pane is quiet enough to relay: idle past grace on the grid,
/// and not self-reporting `working` on its OSC 9999 sideband (a pane can look
/// idle while the session says it is mid-work).
pub fn pane_quiet(grid: GridIdle, sideband: Sideband, now_ms: i64, grace: Duration) -> bool {
    let working = matches!(
        sideband,
        Sideband::Reported {
            state: SidebandState::Working,
            ..
        }
    );
    !working && idle_past_grace(grid, now_ms, grace)
}

/// The whole rule, pure. `gate` is consulted only once the pane is quiet —
/// the async loop does not even read it otherwise.
pub fn decide(
    grid: GridIdle,
    sideband: Sideband,
    now_ms: i64,
    grace: Duration,
    gate: &GateOutcomeRead,
) -> RelayDecision {
    if !pane_quiet(grid, sideband, now_ms, grace) {
        return RelayDecision::Hold(format!(
            "pane not quiet past grace (grid {grid:?}, sideband {sideband:?})"
        ));
    }
    match gate {
        GateOutcomeRead::Unreadable(why) => RelayDecision::Hold(format!("gate unreadable: {why}")),
        GateOutcomeRead::Outcome(None) => {
            RelayDecision::Hold("gate carries no outcome — the session said nothing".to_string())
        }
        GateOutcomeRead::Outcome(Some(raw)) => match relayable_token(raw) {
            Some(token) => RelayDecision::Relay(token),
            None => {
                RelayDecision::Hold(format!("outcome `{raw}` is not a session's own statement"))
            }
        },
    }
}

/// Pull `gate_id`'s outcome out of a `GET /coord/agent-gates` envelope. Pure.
/// A row that is not there is UNREADABLE, never "no outcome".
pub fn gate_outcome_from_body(body: &Value, gate_id: uuid::Uuid) -> GateOutcomeRead {
    let Some(gates) = body.get("gates").and_then(Value::as_array) else {
        return GateOutcomeRead::Unreadable("envelope carries no `gates` array".to_string());
    };
    let want = gate_id.to_string();
    let Some(row) = gates
        .iter()
        .find(|g| g.get("gate_id").and_then(Value::as_str) == Some(want.as_str()))
    else {
        return GateOutcomeRead::Unreadable(format!("no row for gate {want}"));
    };
    GateOutcomeRead::Outcome(
        row.get("continuation_consumed_outcome")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    )
}

// ===========================================================================
// I/O
// ===========================================================================

/// The coord base, device JWT and client one tick's reads share — resolved
/// once per tick (the credential may reach the OS keychain), never per gate.
struct CoordReader {
    base: String,
    jwt: String,
    client: reqwest::Client,
}

async fn coord_reader() -> Result<CoordReader, String> {
    let parts = tokio::time::timeout(
        CREDENTIAL_TIMEOUT,
        tokio::task::spawn_blocking(crate::mcp::continuation_verdict::coord_client_parts),
    )
    .await;
    let (base, jwt) = match parts {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => return Err(format!("no credential ({e})")),
        Ok(Err(e)) => return Err(format!("credential task failed ({e})")),
        Err(_) => return Err("credential resolution timed out".to_string()),
    };
    let client = crate::mcp::continuation_verdict::http_client()?;
    Ok(CoordReader { base, jwt, client })
}

/// Read one gate's outcome over the device-authed list door. Never errors.
async fn fetch_gate_outcome(reader: &CoordReader, gate_id: uuid::Uuid) -> GateOutcomeRead {
    let CoordReader { base, jwt, client } = reader;
    // `include_archived`: a gate the retention worker archived still answers.
    let url = format!("{base}/coord/agent-gates?gate_id={gate_id}&include_archived=true");
    let resp = match client.get(&url).bearer_auth(jwt).send().await {
        Ok(r) => r,
        Err(e) => return GateOutcomeRead::Unreadable(format!("request failed ({e})")),
    };
    let status = resp.status();
    if !status.is_success() {
        return GateOutcomeRead::Unreadable(format!("HTTP {}", status.as_u16()));
    }
    match resp.json::<Value>().await {
        Ok(body) => gate_outcome_from_body(&body, gate_id),
        Err(e) => GateOutcomeRead::Unreadable(format!("undecodable 2xx body ({e})")),
    }
}

/// Start the relay loop (supervised, restarted on panic).
pub fn start() {
    if !enabled() {
        info!("continuation_finish_relay: disabled by {ENABLE_ENV}=0");
        return;
    }
    tauri::async_runtime::spawn(async move {
        crate::mcp::task_supervisor::spawn_supervised_forever(
            "continuation-finish-relay",
            Duration::from_secs(5),
            Duration::from_secs(60),
            Duration::from_secs(120),
            run_loop,
        );
    });
    info!("continuation_finish_relay: started");
}

/// What survives between ticks: when each gate was last read and held, what
/// that read said, the last credential result, and the last headline logged.
#[derive(Debug, Default)]
struct RelayState {
    last_read: HashMap<uuid::Uuid, std::time::Instant>,
    /// Each held gate's last read. A gate leaves it when its session is no
    /// longer a candidate (finished, or its pane gone) or its read relays.
    known: HashMap<uuid::Uuid, LastRead>,
    /// The last ATTEMPT to build a coord reader, if it failed. It is cleared
    /// by the next successful attempt, or once no candidate is held. It is
    /// not re-tested on a tick that reads nothing (every pane busy or not
    /// due), so it is last-known, like [`LastRead`].
    reader_error: Option<String>,
    /// The last headline logged and when: the change gate.
    last_logged: Option<(String, std::time::Instant)>,
}

impl RelayState {
    /// Drop every gate not in `held` (its session finished, its pane or
    /// registry entry gone). With nothing held, nothing is blind, so a stale
    /// credential failure is cleared too.
    fn keep_only(&mut self, held: &HashSet<uuid::Uuid>) {
        self.known.retain(|g, _| held.contains(g));
        if held.is_empty() {
            self.reader_error = None;
        }
    }

    /// Record one attempt to build a coord reader.
    fn record_reader(&mut self, result: Result<(), &str>) {
        self.reader_error = result.err().map(str::to_string);
    }

    /// Record what one gate's read said. A relayable read leaves no entry: its
    /// session is about to be marked finished, not held.
    fn record_read(&mut self, gate_id: uuid::Uuid, read: &GateOutcomeRead) {
        match LastRead::from_read(read) {
            Some(last) => self.known.insert(gate_id, last),
            None => self.known.remove(&gate_id),
        };
    }

    /// The standing reading over everything recorded.
    fn reading(&self) -> Reading {
        Reading::from_known(self.known.values(), self.reader_error.as_deref())
    }

    /// Log the standing [`Reading`] if [`headline_to_log`] says so.
    fn log_reading(&mut self) {
        let reading = self.reading();
        let now = std::time::Instant::now();
        let last = self.last_logged.as_ref().map(|(h, at)| (h.as_str(), *at));
        let Some(h) = headline_to_log(&reading, last, now) else {
            return;
        };
        if reading.degraded() {
            warn!(summary = %h, "continuation_finish_relay: DEGRADED — held continuations whose gate cannot be read, or no coord credential");
        } else {
            info!(summary = %h, "continuation_finish_relay: held continuations");
        }
        self.last_logged = Some((h, now));
    }
}

async fn run_loop() {
    tokio::time::sleep(BOOT_SETTLE_DELAY).await;
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut state = RelayState::default();
    loop {
        ticker.tick().await;
        tick_once(&mut state).await;
        state.log_reading();
    }
}

/// `terminal_id` -> the harness session id of its NEWEST open, unfinished
/// record, from `(terminal_id, claude_session_id, opened_at, finished)` rows. A terminal can hold more than one open record (a stale one from an
/// earlier session id in the same pane); the newest is the live session.
fn unfinished_by_terminal(
    rows: impl IntoIterator<Item = (String, String, i64, bool)>,
) -> HashMap<String, String> {
    // (terminal_id, claude_session_id, opened_at, finished)
    let mut best: HashMap<String, (i64, String)> = HashMap::new();
    for (tid, sid, opened_at, finished) in rows {
        if finished {
            continue;
        }
        match best.get(&tid) {
            Some((kept, _)) if *kept >= opened_at => {
                debug!(terminal_id = %tid, "continuation_finish_relay: a terminal holds more than one open record — keeping the newest");
            }
            _ => {
                best.insert(tid, (opened_at, sid));
            }
        }
    }
    best.into_iter().map(|(tid, (_, sid))| (tid, sid)).collect()
}

async fn tick_once(state: &mut RelayState) {
    use tauri::Manager;

    let continuations = crate::agent_runtime::live_gate_continuations();
    let live_gates: HashSet<uuid::Uuid> = continuations.iter().map(|(_, g)| *g).collect();
    state.last_read.retain(|g, _| live_gates.contains(g));
    state.keep_only(&live_gates);
    if continuations.is_empty() {
        return;
    }
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(store) = app
        .try_state::<Arc<SessionLifecycleStore>>()
        .map(|s| s.inner().clone())
    else {
        return;
    };
    // terminal_id -> its open, not-yet-finished record's harness session id.
    let unfinished = unfinished_by_terminal(store.open_records().into_iter().map(|r| {
        (
            r.terminal_id,
            r.claude_session_id,
            r.opened_at,
            r.finished_at.is_some(),
        )
    }));
    let candidates: Vec<(String, uuid::Uuid, String)> = continuations
        .into_iter()
        .filter_map(|(tid, gate)| unfinished.get(&tid).map(|sid| (tid, gate, sid.clone())))
        .collect();
    // A gate whose session finished (itself, or by this relay) or whose
    // record is gone is no longer held.
    let candidate_gates: HashSet<uuid::Uuid> = candidates.iter().map(|(_, g, _)| *g).collect();
    state.keep_only(&candidate_gates);
    if candidates.is_empty() {
        return;
    }

    let manager = app
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());
    let wanted: HashSet<String> = candidates.iter().map(|(tid, _, _)| tid.clone()).collect();
    let observations = wind_down_observer::observe_terminals(manager.as_deref(), wanted);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let grace = wind_down::grace_from_env();

    let mut reader: Option<Result<CoordReader, String>> = None;
    // The tick's relays, collected first so their net retires run
    // CONCURRENTLY ([`relay_batch`]): one dark coord costs the tick one
    // retire budget, not one per candidate.
    let mut due: Vec<RelayItem> = Vec::new();
    for (terminal_id, gate_id, claude_session_id) in candidates {
        let observation = observations
            .get(&terminal_id)
            .copied()
            .unwrap_or(wind_down_observer::TerminalObservation::UNOBSERVABLE);
        let (grid, sideband) = (observation.grid, observation.sideband);
        // Only a quiet pane whose gate is due costs a coord read; the
        // credential is resolved once per tick, on the first read.
        let now = std::time::Instant::now();
        let gate = if !pane_quiet(grid, sideband, now_ms, grace) {
            GateOutcomeRead::Unreadable("not read — pane not quiet past grace".to_string())
        } else if !due_for_read(state.last_read.get(&gate_id).copied(), now) {
            continue;
        } else {
            if reader.is_none() {
                reader = Some(coord_reader().await);
            }
            let r = match &reader {
                Some(Ok(r)) => {
                    state.record_reader(Ok(()));
                    r
                }
                Some(Err(e)) => {
                    debug!(error = %e, "continuation_finish_relay: no coord reader this tick");
                    state.record_reader(Err(e));
                    break;
                }
                None => break,
            };
            state.last_read.insert(gate_id, now);
            let read = fetch_gate_outcome(r, gate_id).await;
            state.record_read(gate_id, &read);
            read
        };
        match decide(grid, sideband, now_ms, grace, &gate) {
            RelayDecision::Hold(why) => {
                debug!(terminal_id = %terminal_id, gate_id = %gate_id, reason = %why, "continuation_finish_relay: hold");
            }
            RelayDecision::Relay(token) => due.push(RelayItem {
                terminal_id,
                gate_id,
                claude_session_id,
                token,
            }),
        }
    }
    relay_batch(
        &store,
        &crate::session::finish_net_retire::CoordNetRetirer,
        due,
    )
    .await;
}

/// One relay the tick decided on.
struct RelayItem {
    terminal_id: String,
    gate_id: uuid::Uuid,
    claude_session_id: String,
    token: &'static str,
}

/// Relay a tick's decisions: every candidate's implement→ship nets are retired
/// in ONE batch ([`FinishNetRetirer::retire_many`] — the device context and the
/// claims list read once and shared, the per-session gate work concurrent, all
/// within one [`DOOR_BUDGET`]), and only then is each conditional mark written. Retire
/// before mark holds per session; the mark stays conditional
/// (`set_finished_if_unfinished`), so a session that finished itself during
/// the retire keeps its own mark.
///
/// [`DOOR_BUDGET`]: crate::session::finish_net_retire::DOOR_BUDGET
/// [`FinishNetRetirer::retire_many`]: crate::session::finish_net_retire::FinishNetRetirer::retire_many
async fn relay_batch(
    store: &SessionLifecycleStore,
    retirer: &dyn crate::session::finish_net_retire::FinishNetRetirer,
    due: Vec<RelayItem>,
) {
    if due.is_empty() {
        return;
    }
    // ONE batch retire for the tick: the device context and the claims list
    // are read once and shared, each session's gate reads and writes run
    // concurrently, all within one DOOR_BUDGET; every mark waits for it.
    let ids: Vec<String> = due
        .iter()
        .map(|item| item.claude_session_id.clone())
        .collect();
    let nets = crate::session::finish_net_retire::retire_before_conditional_finishes(
        store,
        retirer,
        &ids,
        "the continuation finish relay",
        crate::session::finish_net_retire::DOOR_BUDGET,
    )
    .await;
    for (item, nets) in due.iter().zip(nets) {
        debug!(
            terminal_id = %item.terminal_id,
            claude_session_id = %item.claude_session_id,
            nets = %nets.to_json(),
            "continuation_finish_relay: nets before the relay's mark"
        );
        mark_one(
            store,
            &item.terminal_id,
            item.gate_id,
            &item.claude_session_id,
            item.token,
        );
    }
}

/// The relay's conditional mark for one session (its nets already retired by
/// [`relay_batch`]).
fn mark_one(
    store: &SessionLifecycleStore,
    terminal_id: &str,
    gate_id: uuid::Uuid,
    claude_session_id: &str,
    token: &'static str,
) {
    // The candidate list was read at the start of the tick, before the coord
    // awaits. A session that finished itself meanwhile must keep its own mark:
    // a reason-carrying set_finished would restamp it (finish_synced cleared,
    // its finish_reason replaced), so the write is conditional, decided under
    // the store's own locks.
    let reason = format!("{REASON_PREFIX}{token}");
    match store.set_finished_if_unfinished(claude_session_id, reason.clone()) {
        Ok(outcome) if outcome.changed == FinishChange::None => debug!(
            terminal_id,
            gate_id = %gate_id,
            claude_session_id,
            "continuation_finish_relay: hold — the session finished itself during this tick"
        ),
        Ok(outcome) => match outcome.coord {
            FinishSync::Queued { coord_session_id } => info!(
                terminal_id,
                gate_id = %gate_id,
                claude_session_id,
                coord_session_id = %coord_session_id,
                reason = %reason,
                "continuation_finish_relay: RELAYED the session's own gate outcome as session_status finished (coord write queued)"
            ),
            FinishSync::LocalOnly(why) => warn!(
                terminal_id,
                gate_id = %gate_id,
                claude_session_id,
                coord_reason = why.as_str(),
                reason = %reason,
                "continuation_finish_relay: UNRELAYED — marked finished locally but no coord write was queued"
            ),
        },
        Err(e) => warn!(
            terminal_id,
            gate_id = %gate_id,
            claude_session_id,
            error = ?e,
            "continuation_finish_relay: UNRELAYED — set_finished refused"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const GRACE: Duration = Duration::from_secs(600);
    const NOW: i64 = 10_000_000;

    fn idle_for(ms: i64) -> GridIdle {
        GridIdle::Idle { since_ms: NOW - ms }
    }

    const NR: Sideband = Sideband::NeverReported;

    fn outcome(s: Option<&str>) -> GateOutcomeRead {
        GateOutcomeRead::Outcome(s.map(String::from))
    }

    /// One relay through [`relay_batch`] — the tick's real path.
    async fn relay_one(
        store: &SessionLifecycleStore,
        retirer: &dyn crate::session::finish_net_retire::FinishNetRetirer,
        terminal_id: &str,
        gate_id: uuid::Uuid,
        claude_session_id: &str,
        token: &'static str,
    ) {
        relay_batch(
            store,
            retirer,
            vec![RelayItem {
                terminal_id: terminal_id.to_string(),
                gate_id,
                claude_session_id: claude_session_id.to_string(),
                token,
            }],
        )
        .await;
    }

    /// The relay retires the session's nets BEFORE its conditional mark (plan
    /// 2026-11-23-an-operator-finish-from-the-runner-ui-still-fires-the-finished-sessions-implement-ship-net).
    /// The relay only ever finishes, so there is no unmark case.
    #[tokio::test]
    async fn the_relay_retires_nets_before_the_mark() {
        crate::session::finish_net_retire::tests::door_contract(
            |store, retirer, id, finished| {
                Box::pin(async move {
                    assert!(finished, "the relay only finishes");
                    relay_one(
                        &store,
                        retirer.as_ref(),
                        &format!("term-{id}"),
                        uuid::Uuid::nil(),
                        id,
                        "work_completed",
                    )
                    .await;
                    None
                })
            },
            false,
            false,
        )
        .await;
    }

    /// A retirer that finds no reserve.
    struct NoNets;
    #[async_trait::async_trait]
    impl crate::session::finish_net_retire::FinishNetRetirer for NoNets {
        async fn retire(
            &self,
            _: &str,
            _: &str,
            _: Duration,
        ) -> crate::session::finish_net_retire::NetsOutcome {
            crate::session::finish_net_retire::NetsOutcome::default()
        }
    }

    /// A retirer that takes a while, to show a tick's retires overlap.
    struct SlowNets;
    #[async_trait::async_trait]
    impl crate::session::finish_net_retire::FinishNetRetirer for SlowNets {
        async fn retire(
            &self,
            _: &str,
            _: &str,
            _: Duration,
        ) -> crate::session::finish_net_retire::NetsOutcome {
            tokio::time::sleep(Duration::from_millis(400)).await;
            crate::session::finish_net_retire::NetsOutcome::default()
        }
    }

    /// Records each `retire_many` call, and whether any of its sessions was
    /// already marked when it ran. `retire` must not be used by the relay.
    struct BatchProbe {
        store: Arc<SessionLifecycleStore>,
        calls: std::sync::Mutex<Vec<(Vec<String>, bool)>>,
    }
    #[async_trait::async_trait]
    impl crate::session::finish_net_retire::FinishNetRetirer for BatchProbe {
        async fn retire(
            &self,
            _: &str,
            _: &str,
            _: Duration,
        ) -> crate::session::finish_net_retire::NetsOutcome {
            panic!("the relay retires a tick in ONE batch, never per session");
        }
        async fn retire_many(
            &self,
            ids: &[String],
            _: &str,
            budget: Duration,
        ) -> Vec<crate::session::finish_net_retire::NetsOutcome> {
            assert_eq!(budget, crate::session::finish_net_retire::DOOR_BUDGET);
            let any_marked = ids
                .iter()
                .any(|id| self.store.get(id).is_some_and(|r| r.finished_at.is_some()));
            self.calls.lock().unwrap().push((ids.to_vec(), any_marked));
            ids.iter()
                .map(|id| crate::session::finish_net_retire::NetsOutcome {
                    stems: vec![format!("stem-{id}")],
                    ..Default::default()
                })
                .collect()
        }
    }

    /// A tick's sessions are retired in ONE batch call, before any mark; a
    /// session already finished is left out of the batch.
    #[tokio::test]
    async fn a_tick_retires_its_sessions_in_one_batch_before_any_mark() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        let ids = ["b1", "b2", "done"];
        for id in ids {
            store.record_open(
                serde_json::from_value(json!({
                    "claudeSessionId": id,
                    "pageId": "default",
                    "zoneIndex": 0,
                    "terminalId": format!("term-{id}"),
                    "openedAt": 0,
                    "lastSeenAt": 0,
                    "state": "open",
                }))
                .unwrap(),
            );
        }
        store
            .set_finished("done", true, Some("own".to_string()))
            .unwrap();
        let probe = BatchProbe {
            store: store.clone(),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let due = ids
            .iter()
            .map(|id| RelayItem {
                terminal_id: format!("term-{id}"),
                gate_id: uuid::Uuid::nil(),
                claude_session_id: id.to_string(),
                token: "work_completed",
            })
            .collect();
        relay_batch(&store, &probe, due).await;
        assert_eq!(
            *probe.calls.lock().unwrap(),
            vec![(vec!["b1".to_string(), "b2".to_string()], false)],
            "one batch, the already-finished session left out, before any mark"
        );
        assert!(store.get("b1").unwrap().finished_at.is_some());
        assert!(store.get("b2").unwrap().finished_at.is_some());
        assert_eq!(
            store.get("done").unwrap().finish_reason.as_deref(),
            Some("own")
        );
    }

    /// The batch's outcomes are put back in INPUT order around the skipped
    /// (already-finished) sessions: a skip in the middle must not shift them.
    #[tokio::test]
    async fn batch_outcomes_keep_input_order_around_a_skip() {
        use crate::session::finish_net_retire::{retire_before_conditional_finishes, NetsReport};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap(),
        );
        for id in ["a", "done", "b"] {
            store.record_open(
                serde_json::from_value(json!({
                    "claudeSessionId": id,
                    "pageId": "default",
                    "zoneIndex": 0,
                    "terminalId": format!("term-{id}"),
                    "openedAt": 0,
                    "lastSeenAt": 0,
                    "state": "open",
                }))
                .unwrap(),
            );
        }
        store
            .set_finished("done", true, Some("own".to_string()))
            .unwrap();
        let probe = BatchProbe {
            store: store.clone(),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let ids: Vec<String> = ["a", "done", "b"].iter().map(|s| s.to_string()).collect();
        let reports = retire_before_conditional_finishes(
            &store,
            &probe,
            &ids,
            "test",
            crate::session::finish_net_retire::DOOR_BUDGET,
        )
        .await;
        assert_eq!(
            *probe.calls.lock().unwrap(),
            vec![(vec!["a".to_string(), "b".to_string()], false)],
            "the skipped session is left out of the one batch"
        );
        assert_eq!(reports.len(), 3);
        match &reports[0] {
            NetsReport::Ran(o) => assert_eq!(o.stems, vec!["stem-a".to_string()]),
            other => panic!("reports[0] should be a's outcome, got {other:?}"),
        }
        assert!(
            matches!(reports[1], NetsReport::Skipped(_)),
            "{:?}",
            reports[1]
        );
        match &reports[2] {
            NetsReport::Ran(o) => assert_eq!(o.stems, vec!["stem-b".to_string()]),
            other => panic!("reports[2] should be b's outcome, got {other:?}"),
        }
    }

    /// A tick's retires run CONCURRENTLY, so one dark coord costs the tick one
    /// retire budget, not one per candidate; every mark is still written.
    #[tokio::test]
    async fn a_ticks_retires_run_concurrently_then_every_mark_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let ids = ["c1", "c2", "c3"];
        for id in ids {
            store.record_open(
                serde_json::from_value(json!({
                    "claudeSessionId": id,
                    "pageId": "default",
                    "zoneIndex": 0,
                    "terminalId": format!("term-{id}"),
                    "openedAt": 0,
                    "lastSeenAt": 0,
                    "state": "open",
                }))
                .unwrap(),
            );
        }
        let due = ids
            .iter()
            .map(|id| RelayItem {
                terminal_id: format!("term-{id}"),
                gate_id: uuid::Uuid::nil(),
                claude_session_id: id.to_string(),
                token: "work_completed",
            })
            .collect();
        let started = std::time::Instant::now();
        relay_batch(&store, &SlowNets, due).await;
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "three 400 ms retires took {:?}: they ran serially",
            started.elapsed()
        );
        for id in ids {
            assert!(store.get(id).unwrap().finished_at.is_some());
        }
    }

    /// A session that finished itself between the tick's candidate read and the
    /// relay's write keeps its own mark: no restamp, its reason unchanged.
    #[tokio::test]
    async fn the_relay_never_restamps_a_session_that_finished_itself() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        let open = |csid: &str| -> crate::session::session_lifecycle_store::TerminalSessionRecord {
            serde_json::from_value(json!({
                "claudeSessionId": csid,
                "pageId": "default",
                "zoneIndex": 0,
                "terminalId": format!("term-{csid}"),
                "openedAt": 0,
                "lastSeenAt": 0,
                "state": "open",
            }))
            .unwrap()
        };
        store.record_open(open("self-finished"));
        store
            .set_finished("self-finished", true, Some("own_reason".to_string()))
            .unwrap();
        let before = store.get("self-finished").unwrap();

        relay_one(
            &store,
            &NoNets,
            "term-self-finished",
            uuid::Uuid::nil(),
            "self-finished",
            "work_completed",
        )
        .await;
        let after = store.get("self-finished").unwrap();
        assert_eq!(after.finish_reason.as_deref(), Some("own_reason"));
        assert_eq!(after.finished_at, before.finished_at);

        // An unfinished one is still relayed.
        store.record_open(open("idle-cont"));
        relay_one(
            &store,
            &NoNets,
            "term-idle-cont",
            uuid::Uuid::nil(),
            "idle-cont",
            "work_completed",
        )
        .await;
        let relayed = store.get("idle-cont").unwrap();
        assert!(relayed.finished_at.is_some());
        assert_eq!(
            relayed.finish_reason.as_deref(),
            Some(format!("{REASON_PREFIX}work_completed").as_str())
        );
    }

    #[test]
    fn relays_work_completed_and_work_abandoned_once_idle_past_grace() {
        let grid = idle_for(600_000);
        assert_eq!(
            decide(grid, NR, NOW, GRACE, &outcome(Some("work_completed"))),
            RelayDecision::Relay("work_completed")
        );
        assert_eq!(
            decide(
                grid,
                NR,
                NOW,
                GRACE,
                &outcome(Some("work_abandoned: gave up"))
            ),
            RelayDecision::Relay("work_abandoned")
        );
    }

    #[test]
    fn never_relays_silence_a_process_start_or_an_unreadable_gate() {
        let grid = idle_for(3_600_000);
        for raw in [
            Some("spawned"),
            Some("spawn_failed: boom"),
            Some("work_unreported"),
            Some("work_unreported: pty exited"),
            None,
        ] {
            assert!(
                matches!(
                    decide(grid, NR, NOW, GRACE, &outcome(raw)),
                    RelayDecision::Hold(_)
                ),
                "{raw:?} must not be relayed"
            );
        }
        assert!(matches!(
            decide(
                grid,
                NR,
                NOW,
                GRACE,
                &GateOutcomeRead::Unreadable("HTTP 401".into())
            ),
            RelayDecision::Hold(_)
        ));
    }

    #[test]
    fn holds_until_the_pane_has_been_idle_for_the_whole_grace() {
        let done = outcome(Some("work_completed"));
        assert!(matches!(
            decide(idle_for(599_999), NR, NOW, GRACE, &done),
            RelayDecision::Hold(_)
        ));
        assert!(matches!(
            decide(GridIdle::Busy, NR, NOW, GRACE, &done),
            RelayDecision::Hold(_)
        ));
        assert!(matches!(
            decide(GridIdle::Unknown, NR, NOW, GRACE, &done),
            RelayDecision::Hold(_)
        ));
    }

    #[test]
    fn gate_body_parse_reads_the_named_row_and_a_missing_row_is_unreadable() {
        let g = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let body = json!({"gates": [
            {"gate_id": other.to_string(), "continuation_consumed_outcome": "work_completed"},
            {"gate_id": g.to_string(), "continuation_consumed_outcome": "spawned"}
        ], "total": 2});
        assert_eq!(gate_outcome_from_body(&body, g), outcome(Some("spawned")));

        let null =
            json!({"gates": [{"gate_id": g.to_string(), "continuation_consumed_outcome": null}]});
        assert_eq!(gate_outcome_from_body(&null, g), outcome(None));

        let empty = json!({"gates": [], "total": 0});
        assert!(matches!(
            gate_outcome_from_body(&empty, g),
            GateOutcomeRead::Unreadable(_)
        ));
        assert!(matches!(
            gate_outcome_from_body(&json!({}), g),
            GateOutcomeRead::Unreadable(_)
        ));
    }

    #[test]
    fn relayable_token_matches_what_coord_persists_and_nothing_looser() {
        assert_eq!(relayable_token("work_completed"), Some("work_completed"));
        assert_eq!(relayable_token("work_abandoned"), Some("work_abandoned"));
        assert_eq!(relayable_token("work_abandoned: x"), Some("work_abandoned"));
        // coord stores `work_completed` bare; a detail on it is not its shape.
        assert_eq!(relayable_token("work_completed: x"), None);
        assert_eq!(relayable_token("work_abandoned:x"), None);
        assert_eq!(relayable_token("work_completed_maybe"), None);
        assert_eq!(relayable_token("work_unreported"), None);
    }

    #[test]
    fn a_pane_whose_sideband_says_working_is_not_quiet() {
        let working = Sideband::Reported {
            state: SidebandState::Working,
            set_at_ms: NOW,
        };
        let not_working = Sideband::Reported {
            state: SidebandState::NotWorking,
            set_at_ms: NOW,
        };
        let done = outcome(Some("work_completed"));
        let grid = idle_for(3_600_000);
        assert!(matches!(
            decide(grid, working, NOW, GRACE, &done),
            RelayDecision::Hold(_)
        ));
        assert_eq!(
            decide(grid, not_working, NOW, GRACE, &done),
            RelayDecision::Relay("work_completed")
        );
    }

    #[test]
    fn a_held_gate_is_re_read_only_after_the_backoff() {
        let t0 = std::time::Instant::now();
        assert!(due_for_read(None, t0));
        assert!(!due_for_read(
            Some(t0),
            t0 + RE_READ_AFTER - Duration::from_secs(1)
        ));
        assert!(due_for_read(Some(t0), t0 + RE_READ_AFTER));
    }

    fn row(tid: &str, sid: &str, opened_at: i64, finished: bool) -> (String, String, i64, bool) {
        (tid.to_string(), sid.to_string(), opened_at, finished)
    }

    #[test]
    fn a_terminal_with_two_open_records_maps_to_the_newest() {
        let m = unfinished_by_terminal(vec![
            row("t1", "old", 100, false),
            row("t1", "new", 200, false),
            row("t2", "done", 300, true),
        ]);
        assert_eq!(m.get("t1").map(String::as_str), Some("new"));
        assert!(
            !m.contains_key("t2"),
            "a finished record is never a candidate"
        );
        // Order-independent.
        let m = unfinished_by_terminal(vec![
            row("t1", "new", 200, false),
            row("t1", "old", 100, false),
        ]);
        assert_eq!(m.get("t1").map(String::as_str), Some("new"));
    }

    /// Each read leaves the right held class; a relayable one leaves none.
    #[test]
    fn last_read_classifies_each_read_and_a_relayable_one_is_not_held() {
        assert_eq!(
            LastRead::from_read(&outcome(None)),
            Some(LastRead::NoOutcome)
        );
        assert_eq!(
            LastRead::from_read(&outcome(Some("spawned"))),
            Some(LastRead::NotRelayable)
        );
        assert_eq!(
            LastRead::from_read(&GateOutcomeRead::Unreadable("HTTP 401".into())),
            Some(LastRead::Unreadable("HTTP 401".into()))
        );
        assert_eq!(LastRead::from_read(&outcome(Some("work_completed"))), None);
        assert_eq!(
            LastRead::from_read(&outcome(Some("work_abandoned: x"))),
            None
        );
    }

    /// The reading counts every held gate, picks a FIXED first unreadable
    /// reason whatever the order, and is degraded only when blind somewhere.
    #[test]
    fn the_reading_folds_every_held_gate_and_is_order_independent() {
        let a = [
            LastRead::NoOutcome,
            LastRead::Unreadable("HTTP 500".into()),
            LastRead::NotRelayable,
            LastRead::Unreadable("HTTP 401".into()),
            LastRead::NoOutcome,
        ];
        let mut b = a.clone();
        b.reverse();
        let ra = Reading::from_known(a.iter(), None);
        assert_eq!((ra.no_outcome, ra.not_relayable, ra.unreadable), (2, 1, 2));
        assert_eq!(ra.first_unreadable.as_deref(), Some("HTTP 401"));
        assert_eq!(ra, Reading::from_known(b.iter(), None));
        assert!(ra.degraded());

        let held = Reading::from_known([LastRead::NoOutcome].iter(), None);
        assert!(!held.degraded() && !held.is_empty());

        let blind = Reading::from_known(std::iter::empty(), Some("no credential (x)"));
        assert!(blind.degraded() && !blind.is_empty());
        assert!(blind
            .headline()
            .contains("reader_error=\"no credential (x)\""));
        assert!(Reading::default().is_empty());
    }

    /// The change gate: nothing on an empty first reading; a change (the
    /// return to empty included) is logged; an unchanged reading stays silent,
    /// except a degraded one, which is logged again every DEGRADED_RELOG.
    #[test]
    fn a_headline_is_logged_on_change_and_a_standing_failure_is_re_logged() {
        let t0 = std::time::Instant::now();
        assert_eq!(headline_to_log(&Reading::default(), None, t0), None);

        let held = Reading::from_known([LastRead::NoOutcome].iter(), None);
        let first = headline_to_log(&held, None, t0).expect("first non-empty reading is logged");
        let later = t0 + DEGRADED_RELOG * 3;
        assert_eq!(
            headline_to_log(&held, Some((&first, t0)), later),
            None,
            "an unchanged healthy reading is never re-logged"
        );
        assert!(
            headline_to_log(&Reading::default(), Some((&first, t0)), t0).is_some(),
            "the return to empty is a change and is stated"
        );

        let blind = Reading::from_known(std::iter::empty(), Some("timed out"));
        let h = headline_to_log(&blind, Some((&first, t0)), t0).expect("a new failure is logged");
        assert_eq!(
            headline_to_log(&blind, Some((&h, t0)), t0 + DEGRADED_RELOG / 2),
            None
        );
        assert!(headline_to_log(&blind, Some((&h, t0)), t0 + DEGRADED_RELOG).is_some());
    }

    /// The bookkeeping tick_once drives: reads replace a gate's entry, a
    /// relayable read removes it, a credential result is recorded and cleared,
    /// and dropping out of the held set removes the gate (and, with nothing
    /// held, the credential failure).
    #[test]
    fn relay_state_bookkeeping_tracks_reads_credentials_and_dropouts() {
        let (g1, g2) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let mut st = RelayState::default();
        st.record_read(g1, &GateOutcomeRead::Unreadable("HTTP 500".into()));
        st.record_read(g2, &outcome(None));
        let r = st.reading();
        assert_eq!((r.no_outcome, r.unreadable), (1, 1));
        assert!(r.degraded());

        // A later read of the same gate REPLACES its entry.
        st.record_read(g1, &outcome(Some("spawned")));
        let r = st.reading();
        assert_eq!((r.no_outcome, r.not_relayable, r.unreadable), (1, 1, 0));
        assert!(!r.degraded());

        // A relayable read leaves no entry.
        st.record_read(g1, &outcome(Some("work_completed")));
        assert_eq!(st.reading().not_relayable, 0);
        assert_eq!(st.reading().no_outcome, 1);

        // A credential failure is recorded, and a success clears it.
        st.record_reader(Err("no credential (x)"));
        assert_eq!(
            st.reading().reader_error.as_deref(),
            Some("no credential (x)")
        );
        st.record_reader(Ok(()));
        assert_eq!(st.reading().reader_error, None);

        // Dropping out of the held set removes the gate; an EMPTY held set
        // also clears a standing credential failure.
        st.record_reader(Err("timed out"));
        st.keep_only(&HashSet::from([g1]));
        assert_eq!(st.reading().no_outcome, 0);
        assert!(
            st.reading().reader_error.is_some(),
            "still held ⇒ still blind"
        );
        st.keep_only(&HashSet::new());
        assert!(st.reading().is_empty());
    }
}
