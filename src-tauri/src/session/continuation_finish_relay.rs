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

use crate::session::session_lifecycle_store::{FinishSync, SessionLifecycleStore};
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

/// What survives between ticks: when each gate was last read and held.
#[derive(Debug, Default)]
struct RelayState {
    last_read: HashMap<uuid::Uuid, std::time::Instant>,
}

async fn run_loop() {
    tokio::time::sleep(BOOT_SETTLE_DELAY).await;
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut state = RelayState::default();
    loop {
        ticker.tick().await;
        tick_once(&mut state).await;
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
                Some(Ok(r)) => r,
                Some(Err(e)) => {
                    debug!(error = %e, "continuation_finish_relay: no coord reader this tick");
                    return;
                }
                None => return,
            };
            state.last_read.insert(gate_id, now);
            fetch_gate_outcome(r, gate_id).await
        };
        match decide(grid, sideband, now_ms, grace, &gate) {
            RelayDecision::Hold(why) => {
                debug!(terminal_id = %terminal_id, gate_id = %gate_id, reason = %why, "continuation_finish_relay: hold");
            }
            RelayDecision::Relay(token) => {
                relay_one(&store, &terminal_id, gate_id, &claude_session_id, token);
            }
        }
    }
}

fn relay_one(
    store: &SessionLifecycleStore,
    terminal_id: &str,
    gate_id: uuid::Uuid,
    claude_session_id: &str,
    token: &'static str,
) {
    let reason = format!("{REASON_PREFIX}{token}");
    match store.set_finished(claude_session_id, true, Some(reason.clone())) {
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
}
