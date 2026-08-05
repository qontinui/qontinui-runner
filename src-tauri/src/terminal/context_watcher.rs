//! Proactive context-exhaustion handoff — the grid-scan watcher + PreCompact
//! signal sink (plan `2026-07-17-session-autonomy-fabric.md`, Phase 7).
//!
//! Two redundant signals, ONE trigger path, one per-session debounce:
//!
//! 1. **Grid-scan watcher** — [`scan_terminals_once`] rides the EXISTING
//!    auto-response grid-scan tick (`auto_response::spawn_grid_scan_loop`,
//!    ~1.5s) and evaluates every live terminal's *rendered* VT grid with the
//!    proven pure predicate
//!    [`qontinui_runner_lib::looping_agent::idle::snapshot_context_low`]
//!    (the `"… until auto-compact: N%"` countdown, threshold-gated; plus the
//!    bare `"context low"` marker). This covers sessions whose hooks died.
//! 2. **PreCompact hook** — Claude Code fires `PreCompact` exactly when
//!    compaction is imminent; the bundled hook script POSTs its payload to
//!    `POST /sessions/{id}/context-low` (`mcp::sessions::routes`), which lands
//!    in [`on_precompact_signal`]. Deterministic; covers grids the scanner
//!    can't parse. Only the `auto` trigger counts — a manual `/compact` is a
//!    deliberate operator action, not exhaustion.
//!
//! ## On trigger
//!
//! [`select_action`]: a terminal whose session is coord-mirrored
//! (`coord_session_id` set — coord holds a restorable handoff-state bundle:
//! intent, claims, scrollback) gets the EXISTING handoff machinery,
//! [`crate::session::handoff::trigger_handoff`] targeted at THIS device (the
//! already-running push receiver materializes the continuation child locally,
//! re-acquires claims, replays scrollback, closes the source). A terminal with
//! no coord mirror has nothing to hand off — it gets a fresh continuation
//! session via the existing `POST /sessions/spawn` handler (loopback), seeded
//! with a handoff-summary prompt. A failed handoff trigger falls back to the
//! spawn path so the session still gets a continuation (the debounce means
//! this is the single recovery attempt either way).
//!
//! A spawned continuation also inherits the exhausted session's
//! [`InheritedContext`] — its working directory and its Claude account. The
//! cwd is the load-bearing half: the summary prompt tells the continuation to
//! `git status` "in the working directory" and inspect worktrees, which means
//! nothing if the session starts in the runner process's cwd (wherever the exe
//! was launched). Both are validated in-process and OMITTED when unusable,
//! because the spawn handler rejects a bad `cwd`/`account` outright and losing
//! the whole continuation over an echoed-back detail would make this recovery
//! path less robust than not carrying them at all.
//!
//! The transition is then reported through the runner's EXISTING coord
//! status-report surface — a `progress` outbox row draining to
//! `PATCH /sessions/:id {progress:{…}}` (`session::coord_sync`). When the
//! session has no coord mirror there IS no per-session report surface; that is
//! logged loudly rather than inventing a new coord route.
//!
//! ## Debounce
//!
//! Fires AT MOST ONCE per session: a process-global fired-marker set keyed by
//! terminal id, shared by BOTH signals and claimed atomically
//! ([`mark_fired`] — claim-first, so a racing grid tick + PreCompact POST
//! cannot double-fire). Entries are deliberately NEVER pruned: the guarantee
//! is strict once-per-session, and the set is bounded by the number of
//! sessions over the process lifetime (a few strings each).
//!
//! ## Flag
//!
//! `QONTINUI_CONTEXT_HANDOFF=off|observe|on`, default **off** — the project's
//! tri-state ramp convention (reuses Phase 1's
//! [`crate::mcp::continuation_verdict::Mode`] parser). `observe` logs the
//! would-fire (once per session, same debounce) without acting. Threshold is
//! `QONTINUI_CONTEXT_HANDOFF_THRESHOLD_PCT` (1–100), defaulting to the
//! looping-agent lifecycle default (15%).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tracing::{debug, info, warn};

use qontinui_runner_lib::looping_agent::idle::snapshot_context_low;
use qontinui_runner_lib::looping_agent::registry::LifecyclePolicy;

use crate::mcp::continuation_verdict::Mode;

/// Tri-state kill-switch env flag. `off` (default) | `observe` | `on`.
pub const FLAG_ENV: &str = "QONTINUI_CONTEXT_HANDOFF";

/// Env override for the context-low percentage threshold (1–100).
pub const THRESHOLD_ENV: &str = "QONTINUI_CONTEXT_HANDOFF_THRESHOLD_PCT";

/// Read the live watcher mode from the process env.
pub fn watch_mode() -> Mode {
    Mode::from_flag(std::env::var(FLAG_ENV).ok().as_deref())
}

/// Parse a threshold override; anything unusable falls back to the
/// looping-agent lifecycle default (15%). Pure — the unit-test seam.
pub fn threshold_from(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| (1..=100).contains(&n))
        .unwrap_or_else(|| LifecyclePolicy::default().context_low_threshold_pct)
}

fn threshold_pct() -> u32 {
    threshold_from(std::env::var(THRESHOLD_ENV).ok().as_deref())
}

// ===========================================================================
// Per-session fired-marker (the once-per-session debounce)
// ===========================================================================

/// Terminal ids (or PreCompact session keys) that have already fired.
/// Option-wrapped so the static is const-initializable (mirrors
/// `auto_response::GRID_EDGE`). NEVER pruned — see module docs.
static FIRED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Per-terminal output-byte watermarks so a tick skips sessions whose rendered
/// screen cannot have changed since the previous pass (see
/// [`super::scan_gate`]). Own gate per scanner — see the note on
/// `usage_limit::SCAN_GATE`.
static SCAN_GATE: Mutex<Option<super::scan_gate::ScanGate>> = Mutex::new(None);

/// Has this session already fired? (Read-only peek for the decision seam.)
fn already_fired(key: &str) -> bool {
    let guard = FIRED.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(|s| s.contains(key)).unwrap_or(false)
}

/// Atomically claim the fired-marker for `key`. Returns `true` iff THIS call
/// claimed it (claim-first: the single winner acts; every later caller —
/// including a racing second signal — gets `false` and does nothing).
fn mark_fired(key: &str) -> bool {
    let mut guard = FIRED.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashSet::new)
        .insert(key.to_string())
}

// ===========================================================================
// Pure decision cores (the unit-test surface)
// ===========================================================================

/// What one watcher evaluation should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchDecision {
    /// Nothing to do (flag off / context healthy / already fired).
    Skip,
    /// `observe` mode: log the would-fire (and consume the debounce so the
    /// log appears once per session), act on nothing.
    WouldFire,
    /// `on` mode: run the trigger.
    Fire,
}

/// The deterministic watcher rule: fire ⇔ flag armed AND the context-low
/// signal is present AND this session has never fired. Pure.
pub fn decide(mode: Mode, context_low: bool, already_fired: bool) -> WatchDecision {
    if mode == Mode::Off || !context_low || already_fired {
        return WatchDecision::Skip;
    }
    match mode {
        Mode::Observe => WatchDecision::WouldFire,
        Mode::On => WatchDecision::Fire,
        Mode::Off => unreachable!("handled above"),
    }
}

/// Which recovery lever a fired trigger pulls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerAction {
    /// The session is coord-mirrored ⇒ coord holds a restorable
    /// handoff-state bundle ⇒ use the existing handoff machinery.
    Handoff,
    /// No coord mirror ⇒ nothing to hand off ⇒ spawn a fresh continuation
    /// session seeded with a handoff-summary prompt.
    SpawnContinuation,
}

/// Handoff when (and only when) the session has restorable coord state.
pub fn select_action(has_coord_session: bool) -> TriggerAction {
    if has_coord_session {
        TriggerAction::Handoff
    } else {
        TriggerAction::SpawnContinuation
    }
}

/// Is a PreCompact payload the AUTO (context-exhaustion) kind? A manual
/// `/compact` (`"trigger":"manual"`) is a deliberate operator action and never
/// fires the watcher. A missing/garbled `trigger` reads as auto — PreCompact
/// firing at all already means compaction is imminent.
pub fn precompact_is_auto(payload: &Value) -> bool {
    payload
        .get("trigger")
        .and_then(Value::as_str)
        .map(|t| !t.trim().eq_ignore_ascii_case("manual"))
        .unwrap_or(true)
}

/// The handoff-summary prompt a spawned continuation session starts with.
/// Pure so the test can pin the load-bearing content.
pub fn handoff_summary_prompt(
    terminal_id: &str,
    title: Option<&str>,
    coord_session_id: Option<uuid::Uuid>,
) -> String {
    let title_part = title
        .map(|t| format!("\"{t}\""))
        .unwrap_or_else(|| "an unnamed session".to_string());
    let coord_part = coord_session_id
        .map(|id| format!(" Its coord session id was {id}."))
        .unwrap_or_default();
    format!(
        "You are the CONTINUATION of {title_part} (runner terminal {terminal_id}), \
         which was about to exhaust its context window (auto-compact imminent).{coord_part} \
         Recover the work in progress: run `git status` in the working directory and \
         inspect worktrees for uncommitted work, read any session journal or plan \
         documents it left behind, then continue the task to completion. When the work \
         is done, report status via coord_report_status."
    )
}

// ===========================================================================
// Grid-scan watcher (signal 1 — rides the auto_response grid tick)
// ===========================================================================

/// One watcher pass over every live terminal's rendered grid. Called from the
/// auto-response grid-scan loop each tick; a dark flag is a zero-cost
/// early-out (no grid locks taken).
pub fn scan_terminals_once() {
    use tauri::Manager;

    let mode = watch_mode();
    if mode == Mode::Off {
        return;
    }
    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    let threshold = threshold_pct();

    let sessions = tm.sessions_snapshot();
    {
        // Prune watermarks for terminals that have gone away before the sweep,
        // so a dead terminal's entry cannot outlive it. The gate lock is
        // released before any grid work.
        let live: HashSet<&String> = sessions.iter().map(|(t, _)| t).collect();
        let mut gate_guard = SCAN_GATE.lock().unwrap_or_else(|e| e.into_inner());
        gate_guard
            .get_or_insert_with(super::scan_gate::ScanGate::new)
            .retain_live(&live);
    }

    for (tid, session) in sessions {
        // Skip sessions whose grid has not been mutated since the last pass:
        // the rendered screen is byte-identical, so the (pure) context-low
        // predicate would return exactly what it returned last tick. One
        // atomic load instead of a grid lock + full screen render.
        {
            let mut gate_guard = SCAN_GATE.lock().unwrap_or_else(|e| e.into_inner());
            let gate = gate_guard.get_or_insert_with(super::scan_gate::ScanGate::new);
            if !gate.should_scan(&tid, session.grid_generation()) {
                continue;
            }
        }
        // Rendered screen text (rows joined by `\n`) — same read the
        // auto-response scanner uses; immune to synchronized-output frame
        // batching. `snapshot_context_low` wants per-row lines.
        let text = {
            let grid = session.grid();
            let guard = grid.lock().unwrap_or_else(|e| e.into_inner());
            guard.text_snapshot().text
        };
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let low = snapshot_context_low(&lines, threshold);

        match decide(mode, low, already_fired(&tid)) {
            WatchDecision::Skip => {}
            WatchDecision::WouldFire => {
                if mark_fired(&tid) {
                    info!(
                        terminal = %tid,
                        threshold_pct = threshold,
                        mode = mode.as_str(),
                        "context_watcher: WOULD FIRE (observe) — context-low on rendered grid"
                    );
                }
            }
            WatchDecision::Fire => {
                if mark_fired(&tid) {
                    info!(
                        terminal = %tid,
                        threshold_pct = threshold,
                        "context_watcher: context-low on rendered grid — triggering handoff"
                    );
                    spawn_trigger_task(tid, "grid-scan");
                }
            }
        }
    }
}

// ===========================================================================
// PreCompact signal (signal 2 — the deterministic hook)
// ===========================================================================

/// Wire-facing outcome of one PreCompact signal, returned by the
/// `POST /sessions/{id}/context-low` handler.
#[derive(Debug, Clone)]
pub struct SignalOutcome {
    pub fired: bool,
    pub mode: &'static str,
    pub reason: String,
}

/// Handle one PreCompact hook POST for session key `key` (the runner terminal
/// id the hook script sends, falling back to the Claude session id). Shares
/// the SAME debounce + trigger path as the grid-scan watcher.
pub fn on_precompact_signal(key: &str, payload: &Value) -> SignalOutcome {
    let mode = watch_mode();
    let done = |fired: bool, reason: String| SignalOutcome {
        fired,
        mode: mode.as_str(),
        reason,
    };

    if mode == Mode::Off {
        debug!(key = %key, "context_watcher: PreCompact signal ignored — flag off");
        return done(false, "flag off".to_string());
    }
    if !precompact_is_auto(payload) {
        debug!(key = %key, "context_watcher: PreCompact signal ignored — manual /compact");
        return done(
            false,
            "manual compaction — deliberate, not exhaustion".to_string(),
        );
    }

    match decide(mode, true, already_fired(key)) {
        WatchDecision::Skip => done(
            false,
            "already fired for this session (once-per-session debounce)".to_string(),
        ),
        WatchDecision::WouldFire => {
            if mark_fired(key) {
                info!(
                    key = %key,
                    mode = mode.as_str(),
                    "context_watcher: WOULD FIRE (observe) — PreCompact auto signal"
                );
                done(false, "observe mode — would fire".to_string())
            } else {
                done(false, "lost the debounce race — already fired".to_string())
            }
        }
        WatchDecision::Fire => {
            if mark_fired(key) {
                info!(
                    key = %key,
                    "context_watcher: PreCompact auto signal — triggering handoff"
                );
                spawn_trigger_task(key.to_string(), "precompact-hook");
                done(true, "context-low handoff triggered".to_string())
            } else {
                done(false, "lost the debounce race — already fired".to_string())
            }
        }
    }
}

// ===========================================================================
// The trigger (handoff vs spawn-continuation) + coord status report
// ===========================================================================

/// Detach the trigger so neither the grid tick nor the hook HTTP handler
/// blocks on coord round-trips / session spawning.
fn spawn_trigger_task(terminal_id: String, origin: &'static str) {
    tauri::async_runtime::spawn(async move {
        run_trigger(&terminal_id, origin).await;
    });
}

async fn run_trigger(terminal_id: &str, origin: &'static str) {
    use tauri::Manager;

    // Resolve the live session identity + the session registry (the coord
    // transport). Both are best-effort: a missing piece degrades toward the
    // spawn-continuation path rather than doing nothing.
    let mut coord_session_id: Option<uuid::Uuid> = None;
    let mut title: Option<String> = None;
    let mut registry: Option<Arc<crate::session::SessionRegistry>> = None;
    if let Some(app) = crate::tauri_app_handle::current() {
        if let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() {
            if let Some(session) = tm.get(terminal_id) {
                coord_session_id = session.coord_session_id();
                title = Some(session.title());
            }
        }
        registry = app
            .try_state::<Arc<crate::session::SessionRegistry>>()
            .map(|s| s.inner().clone());
    }

    // Handoff needs BOTH the coord mirror (the restorable state) and the
    // registry (http client + coord base + device id).
    let handoff_parts = match (
        select_action(coord_session_id.is_some()),
        coord_session_id,
        registry.as_ref(),
    ) {
        (TriggerAction::Handoff, Some(cid), Some(reg)) => Some((cid, reg.clone())),
        _ => None,
    };

    match handoff_parts {
        Some((cid, reg)) => {
            // Existing handoff machinery, targeted at THIS device: the
            // already-running push receiver (`handoff::start_receiver_task`)
            // materializes the continuation child locally — claims
            // re-acquired, scrollback replayed, source closed.
            let http = reg.coord_sync().http_client();
            let coord_url = reg.coord_sync().coord_url().to_string();
            match crate::session::handoff::trigger_handoff(&http, &coord_url, cid, reg.machine_id())
                .await
            {
                Ok(()) => {
                    info!(
                        terminal = %terminal_id,
                        coord_session = %cid,
                        origin,
                        "context_watcher: handoff triggered (self-targeted)"
                    );
                    report_transition(&reg, cid, "handoff", origin);
                }
                Err(e) => {
                    warn!(
                        terminal = %terminal_id,
                        coord_session = %cid,
                        origin,
                        error = %e,
                        "context_watcher: handoff trigger failed — falling back to spawn-continuation"
                    );
                    match spawn_continuation(terminal_id, title.as_deref(), coord_session_id).await
                    {
                        Ok(_) => report_transition(
                            &reg,
                            cid,
                            "spawn-continuation (handoff fallback)",
                            origin,
                        ),
                        Err(e) => warn!(
                            terminal = %terminal_id,
                            origin,
                            error = %e,
                            "context_watcher: fallback spawn-continuation ALSO failed — session \
                             gets no continuation (debounce consumed; operator attention needed)"
                        ),
                    }
                }
            }
        }
        None => {
            let spawned = spawn_continuation(terminal_id, title.as_deref(), coord_session_id).await;
            match (spawned, coord_session_id, registry.as_ref()) {
                (Ok(_), Some(cid), Some(reg)) => {
                    report_transition(reg, cid, "spawn-continuation", origin);
                }
                (Ok(_), _, _) => {
                    // LOUD by design: there is no per-session coord
                    // status-report surface for an unmirrored session, and we
                    // do not invent a new coord route.
                    warn!(
                        terminal = %terminal_id,
                        origin,
                        "context_watcher: continuation spawned, but session has NO coord mirror — \
                         transition NOT reported to coord (no status-report surface for this session)"
                    );
                }
                (Err(e), _, _) => {
                    warn!(
                        terminal = %terminal_id,
                        origin,
                        error = %e,
                        "context_watcher: spawn-continuation failed — session gets no continuation \
                         (debounce consumed; operator attention needed)"
                    );
                }
            }
        }
    }
}

/// Report the transition through the runner's EXISTING coord status-report
/// surface: a `progress` outbox row that drains to
/// `PATCH /sessions/:id {progress:{progress_detail}}` (at-least-once, offline
/// tolerant — `session::coord_sync`). Best-effort.
fn report_transition(
    registry: &Arc<crate::session::SessionRegistry>,
    coord_session_id: uuid::Uuid,
    action: &str,
    origin: &str,
) {
    let payload = json!({
        "progress_detail": format!(
            "context-exhaustion watcher fired ({origin}): {action} triggered before auto-compact"
        ),
    });
    if let Err(e) = registry.coord_sync().outbox().record(
        registry.machine_id(),
        coord_session_id,
        crate::session::SessionEventKind::Progress,
        payload,
    ) {
        warn!(
            coord_session = %coord_session_id,
            error = %e,
            "context_watcher: coord progress report enqueue failed"
        );
    }
}

/// What a continuation inherits from the session it replaces: WHERE that
/// session was working, and WHICH Claude account it was working as.
///
/// Both are resolved against live in-process state and are `None` when
/// unusable, because this is a best-effort recovery path: the spawn handler
/// 400s an unusable `cwd` and 400/409s an unresolvable `account`, and losing
/// the whole continuation over an echoed-back detail would make the feature
/// LESS robust than not carrying them at all. Validate here, omit on doubt.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InheritedContext {
    /// The source terminal's working directory, when it still exists.
    pub cwd: Option<String>,
    /// The source session's pinned Claude `config_dir`, when it still resolves
    /// to a logged-in roster account.
    pub account: Option<String>,
}

/// Keep a source terminal's working directory only if it still names a live
/// directory. The unit-test seam for the cwd half of [`InheritedContext`] —
/// it touches the filesystem (one `is_dir`), so it takes the path as an
/// argument rather than reaching for the terminal itself.
///
/// Blank is what a terminal spawned with no explicit cwd carries; a path that
/// no longer resolves is typically a worktree the platform already reclaimed.
pub fn usable_cwd(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !std::path::Path::new(trimmed).is_dir() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Resolve what the continuation inherits from the terminal it replaces.
///
/// Called only on the spawn path — the handoff path carries this state through
/// coord's restorable bundle instead, so it must not pay for these lookups.
fn inherited_context(terminal_id: &str) -> InheritedContext {
    use tauri::Manager;

    let cwd = crate::tauri_app_handle::current()
        .and_then(|app| {
            app.try_state::<Arc<crate::terminal::TerminalManager>>()
                .and_then(|tm| tm.get(terminal_id))
        })
        .and_then(|session| usable_cwd(session.working_dir()));

    InheritedContext {
        cwd,
        account: source_account(terminal_id),
    }
}

/// The Claude account the exhausted session was running as, as a roster
/// `config_dir` the spawn handler will accept.
///
/// Read from the session-lifecycle store's spawn-time record (`config_dir` is
/// STICKY there) and then RE-RESOLVED through the same roster validation the
/// spawn handler applies, so an account that has since been removed from the
/// roster or logged out degrades to `None` — global resolution — instead of
/// 400/409-ing the continuation away.
fn source_account(terminal_id: &str) -> Option<String> {
    use tauri::Manager;

    let app = crate::tauri_app_handle::current()?;
    let store =
        app.try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()?;
    let config_dir = store
        .find_open_by_terminal(terminal_id)?
        .config_dir
        .filter(|d| !d.trim().is_empty())?;

    match crate::ai_provider::resolve_requested_account(&config_dir) {
        Ok(resolved) => Some(resolved.config_dir),
        Err(e) => {
            debug!(
                terminal = %terminal_id,
                config_dir = %config_dir,
                error = %e.message(),
                "context_watcher: source account no longer resolvable — continuation falls back \
                 to global account resolution"
            );
            None
        }
    }
}

/// The exact `POST /sessions/spawn` body a continuation is spawned with. Pure,
/// so the wire contract is pinned by a test instead of by a live spawn — the
/// field names here are load-bearing and have silently drifted once already
/// (an earlier revision sent `initial_prompt`, which deserialized to `None`
/// and handed the continuation the generic greeting instead of its summary).
///
/// `cwd` and `account` are OMITTED rather than sent as `null` when unknown,
/// so the handler's own defaults apply untouched.
pub fn spawn_body(
    terminal_id: &str,
    title: Option<&str>,
    coord_session_id: Option<uuid::Uuid>,
    inherited: &InheritedContext,
) -> Value {
    let mut body = json!({
        "task_name": format!(
            "Continuation of {}",
            title.filter(|t| !t.trim().is_empty()).unwrap_or(terminal_id)
        ),
        // Field name is the spawn handler's free-form `prompt` (no `role`, so
        // the prompt is used verbatim as the first user message).
        "prompt": handoff_summary_prompt(terminal_id, title, coord_session_id),
    });
    let map = body.as_object_mut().expect("json! built an object");
    // The continuation's whole job is to recover work in a specific
    // directory — the summary prompt tells it to `git status` there — so the
    // source terminal's cwd is the load-bearing half of this body.
    if let Some(cwd) = &inherited.cwd {
        map.insert("cwd".to_string(), json!(cwd));
    }
    // Continue as the SAME Claude account, so a fleet running several accounts
    // does not silently migrate the work onto whichever one global resolution
    // happens to pick.
    if let Some(account) = &inherited.account {
        map.insert("account".to_string(), json!(account));
    }
    body
}

/// Spawn a continuation session through the EXISTING `POST /sessions/spawn`
/// handler (loopback — the same code path agents use), seeded with the
/// handoff-summary prompt and the exhausted session's inherited context.
/// Returns the new `taskRunId`.
async fn spawn_continuation(
    terminal_id: &str,
    title: Option<&str>,
    coord_session_id: Option<uuid::Uuid>,
) -> Result<String, String> {
    let port = crate::mcp::types::get_mcp_api_port();
    let url = format!("http://127.0.0.1:{port}/sessions/spawn");
    let inherited = inherited_context(terminal_id);
    let body = spawn_body(terminal_id, title, coord_session_id, &inherited);
    let client = reqwest::Client::builder()
        // The spawn handler blocks through the CLI init handshake; give it
        // room.
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("decode spawn response: {e}"))?;
    if !status.is_success() {
        return Err(format!("spawn returned {status}: {v}"));
    }
    if v.get("state").and_then(Value::as_str) == Some("error") {
        return Err(format!("spawn handler reported error state: {v}"));
    }
    let task_run_id = v
        .get("taskRunId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    info!(
        terminal = %terminal_id,
        continuation = %task_run_id,
        cwd = inherited.cwd.as_deref().unwrap_or("<runner default>"),
        account = inherited.account.as_deref().unwrap_or("<global resolution>"),
        "context_watcher: continuation session spawned with handoff-summary prompt"
    );
    Ok(task_run_id)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── decide(): the mode × signal × debounce matrix ────────────────────

    #[test]
    fn off_never_fires() {
        assert_eq!(decide(Mode::Off, true, false), WatchDecision::Skip);
        assert_eq!(decide(Mode::Off, true, true), WatchDecision::Skip);
        assert_eq!(decide(Mode::Off, false, false), WatchDecision::Skip);
    }

    #[test]
    fn healthy_context_never_fires() {
        assert_eq!(decide(Mode::On, false, false), WatchDecision::Skip);
        assert_eq!(decide(Mode::Observe, false, false), WatchDecision::Skip);
    }

    #[test]
    fn debounce_never_fires_twice() {
        // The load-bearing invariant: once fired, NEVER again — in any mode.
        assert_eq!(decide(Mode::On, true, true), WatchDecision::Skip);
        assert_eq!(decide(Mode::Observe, true, true), WatchDecision::Skip);
    }

    #[test]
    fn observe_would_fire_and_on_fires() {
        assert_eq!(decide(Mode::Observe, true, false), WatchDecision::WouldFire);
        assert_eq!(decide(Mode::On, true, false), WatchDecision::Fire);
    }

    // ── fired-marker registry (claim-first debounce) ─────────────────────

    #[test]
    fn mark_fired_claims_exactly_once_per_key() {
        // Unique keys — the registry is process-global and tests run in
        // parallel.
        let key = format!("term-{}", uuid::Uuid::new_v4());
        assert!(!already_fired(&key));
        assert!(mark_fired(&key), "first claim wins");
        assert!(already_fired(&key));
        assert!(!mark_fired(&key), "second claim loses — never twice");
        assert!(!mark_fired(&key), "still never");
    }

    #[test]
    fn fired_marker_is_per_terminal() {
        let a = format!("term-{}", uuid::Uuid::new_v4());
        let b = format!("term-{}", uuid::Uuid::new_v4());
        assert!(mark_fired(&a));
        // A different terminal (= different session) is unaffected.
        assert!(!already_fired(&b));
        assert!(mark_fired(&b));
    }

    // ── flag + threshold parsing ─────────────────────────────────────────

    #[test]
    fn threshold_defaults_to_lifecycle_policy_default() {
        let default = LifecyclePolicy::default().context_low_threshold_pct;
        assert_eq!(default, 15, "registry default is the 15% contract");
        assert_eq!(threshold_from(None), default);
        assert_eq!(threshold_from(Some("")), default);
        assert_eq!(threshold_from(Some("garbage")), default);
        assert_eq!(threshold_from(Some("0")), default, "0 is out of range");
        assert_eq!(threshold_from(Some("101")), default, "101 is out of range");
        assert_eq!(threshold_from(Some("8")), 8);
        assert_eq!(threshold_from(Some(" 25 ")), 25);
        assert_eq!(threshold_from(Some("100")), 100);
    }

    #[test]
    fn flag_parses_with_phase1_tristate_convention() {
        // Reuses Phase 1's Mode parser — pin the contract here too.
        assert_eq!(Mode::from_flag(None), Mode::Off);
        assert_eq!(Mode::from_flag(Some("observe")), Mode::Observe);
        assert_eq!(Mode::from_flag(Some("on")), Mode::On);
        assert_eq!(Mode::from_flag(Some("bogus")), Mode::Off);
    }

    // ── PreCompact payload parsing ───────────────────────────────────────

    #[test]
    fn precompact_auto_trigger_counts() {
        assert!(precompact_is_auto(&json!({"trigger": "auto"})));
        assert!(precompact_is_auto(&json!({"trigger": "Auto"})));
    }

    #[test]
    fn precompact_manual_trigger_is_ignored() {
        assert!(!precompact_is_auto(&json!({"trigger": "manual"})));
        assert!(!precompact_is_auto(&json!({"trigger": " MANUAL "})));
    }

    #[test]
    fn precompact_missing_or_garbled_trigger_reads_as_auto() {
        // PreCompact firing at all means compaction is imminent.
        assert!(precompact_is_auto(&json!({})));
        assert!(precompact_is_auto(&Value::Null));
        assert!(precompact_is_auto(&json!({"trigger": 42})));
    }

    // ── on_precompact_signal: flag gating + debounce end to end ──────────

    #[test]
    fn precompact_signal_respects_manual_and_debounce() {
        // NOTE: mode comes from env (default Off in the test harness — the
        // flag is never set here), so the flag-off gate is what we can pin
        // end-to-end without global env mutation (which races the parallel
        // harness). The armed paths are covered via `decide` + `mark_fired`.
        let key = format!("term-{}", uuid::Uuid::new_v4());
        let out = on_precompact_signal(&key, &json!({"trigger": "auto"}));
        assert!(!out.fired);
        assert_eq!(out.mode, "off");
        assert!(!already_fired(&key), "flag off consumes NO debounce");
    }

    // ── trigger selection ────────────────────────────────────────────────

    #[test]
    fn coord_mirrored_session_gets_handoff_else_spawn() {
        assert_eq!(select_action(true), TriggerAction::Handoff);
        assert_eq!(select_action(false), TriggerAction::SpawnContinuation);
    }

    // ── handoff-summary prompt ───────────────────────────────────────────

    #[test]
    fn handoff_summary_prompt_carries_identity_and_recovery_steps() {
        let cid = uuid::Uuid::new_v4();
        let p = handoff_summary_prompt("term-9", Some("fix auth bug"), Some(cid));
        assert!(p.contains("CONTINUATION"));
        assert!(p.contains("fix auth bug"));
        assert!(p.contains("term-9"));
        assert!(p.contains(&cid.to_string()));
        assert!(p.contains("git status"));
        assert!(p.contains("coord_report_status"));

        // Sparse identity still produces a usable prompt.
        let p = handoff_summary_prompt("term-9", None, None);
        assert!(p.contains("unnamed session"));
        assert!(p.contains("term-9"));
    }

    // ── inherited context: the continuation's cwd + account ──────────────

    #[test]
    fn usable_cwd_keeps_only_live_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        assert_eq!(usable_cwd(&path), Some(path.clone()));
        // Surrounding whitespace is trimmed, not rejected.
        assert_eq!(usable_cwd(&format!("  {path}  ")), Some(path.clone()));

        // A terminal spawned with no explicit cwd carries a blank string.
        assert_eq!(usable_cwd(""), None);
        assert_eq!(usable_cwd("   "), None);
        // A worktree the platform already reclaimed.
        assert_eq!(
            usable_cwd(&format!("{path}/gone-{}", uuid::Uuid::new_v4())),
            None
        );
        // A FILE is not a working directory.
        let file = dir.path().join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(usable_cwd(&file.to_string_lossy()), None);
    }

    #[test]
    fn spawn_body_pins_the_wire_contract() {
        // The field names here are the contract with
        // `mcp::sessions::SpawnSessionRequest`. `prompt` (NOT `initial_prompt`)
        // is the one that has already drifted once and silently handed a
        // continuation the generic greeting; the rest ride along with it.
        let cid = uuid::Uuid::new_v4();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let inherited = InheritedContext {
            cwd: Some(cwd.clone()),
            account: Some("C:/roster/hotmail".to_string()),
        };

        let body = spawn_body("term-9", Some("fix auth bug"), Some(cid), &inherited);
        assert_eq!(body["task_name"], json!("Continuation of fix auth bug"));
        assert_eq!(
            body["prompt"],
            json!(handoff_summary_prompt(
                "term-9",
                Some("fix auth bug"),
                Some(cid)
            ))
        );
        assert_eq!(body["cwd"], json!(cwd));
        assert_eq!(body["account"], json!("C:/roster/hotmail"));
        // No `role`: the handler 400s role+prompt together.
        assert!(body.get("role").is_none());
    }

    #[test]
    fn spawn_body_omits_unknown_context_rather_than_nulling_it() {
        // A `null` cwd/account would have to be special-cased by the handler;
        // an ABSENT field lets its own `#[serde(default)]` defaults apply.
        let body = spawn_body("term-9", None, None, &InheritedContext::default());
        assert!(body.get("cwd").is_none(), "unknown cwd is omitted");
        assert!(body.get("account").is_none(), "unknown account is omitted");
        // Falls back to the terminal id when the session has no title.
        assert_eq!(body["task_name"], json!("Continuation of term-9"));

        // A blank title is treated as absent, same as the prompt builder.
        let body = spawn_body("term-9", Some("   "), None, &InheritedContext::default());
        assert_eq!(body["task_name"], json!("Continuation of term-9"));
    }
}
