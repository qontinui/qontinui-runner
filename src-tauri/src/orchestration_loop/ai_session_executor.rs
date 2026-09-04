//! Approach-D Conductor/Engine — Phase 2 worker executor.
//!
//! Three concerns live here:
//!
//! 1. **DISPATCH** ([`dispatch_subtask`]) — turn one ready [`Subtask`] into a
//!    live worker AI-session tab. Allocates a worktree (when the subtask
//!    declares a `repo`), re-picks the lowest-utilization Claude account, spawns
//!    an interactive `ClaudeSession` under `page_id = run_id`, submits the brief
//!    (with an appended report-contract instruction block), records the
//!    `task_id ↔ task_run_id` binding, and transitions the subtask
//!    `Submitted → Working`.
//!
//! 2. **SIGNAL** ([`worker_terminal_state`]) — the completion-detection
//!    primitive the Phase-3 reconciler calls each tick. It reads the worker's
//!    FSM state in-process via the [`SessionManager`]. The PRIMARY signal is the
//!    FSM `Result → Ready` edge (`dispatcher.rs` transitions
//!    `Processing → Ready` on a result message); the `[TASK_COMPLETE]` sentinel
//!    is CONFIRM-only and never advances state on its own.
//!
//! 3. **GUARD** ([`can_complete`]) — the contract guard the reconciler enforces
//!    before flipping `Working → Completed`: BOTH the FSM `Ready` signal AND a
//!    readable `artifact` row must exist.
//!
//! The reconciler itself (Phase 3) is stateless over the durable ledger
//! (`orchestration.runs` / `orchestration.subtasks`); this module provides the
//! side-effecting dispatch and the pure read primitives it composes.

use std::sync::Arc;

use tauri::Manager;
use tracing::{info, warn};
use uuid::Uuid;

use crate::claude_session::manager::SessionManager;
use crate::claude_session::state::SessionState;
use crate::orchestration_loop::ledger::{Subtask, SubtaskState};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// How long [`dispatch_subtask`] queues for a parallel fan-out admission slot.
///
/// Near-zero on purpose: the conductor's tick loop already retries, and
/// `apply_tick` is sequential, so a long queue here freezes completions, gate
/// registration and the stall fingerprint for the whole budget — once per
/// queued dispatch. Small but non-zero so a slot released microseconds ago is
/// still picked up in this tick rather than costing a whole 5s interval.
const FANOUT_ADMISSION_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// In-process read of a worker's terminal/FSM state, as observed by the
/// Phase-3 reconciler each tick.
///
/// This is deliberately coarse: the reconciler only needs to know whether the
/// worker is still busy, has gone idle (the `Ready` edge — a completion
/// CANDIDATE, gated by [`can_complete`]), has terminally errored, or has
/// vanished from the [`SessionManager`] (closed / never registered / killed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerSignal {
    /// The worker's CLI session is alive and mid-turn (`Processing` /
    /// `Initializing` / `Interrupting` / `Promoting` / `Created`). Not a
    /// completion candidate.
    Working,
    /// The worker reached `Ready` — idle after a turn. This is the PRIMARY
    /// completion signal (the FSM `Result → Ready` edge), but it is only a
    /// CANDIDATE: a worker that pauses to ask a question is byte-identically
    /// `Ready`/idle, so the reconciler must ALSO see a persisted artifact
    /// (see [`can_complete`]) before flipping `Working → Completed`.
    ReadyIdle,
    /// The worker's session entered a terminal closing/closed state — treat as
    /// a `Working → Failed` trigger (non-zero exit / killed mid-turn).
    Errored,
    /// No session is registered for this `task_run_id` — it closed, was never
    /// registered, or the runner restarted without resuming it. The reconciler
    /// treats a `Gone` worker as a recovery case (re-dispatch or fail past the
    /// stall budget); it is NOT, by itself, a success.
    Gone,
}

/// Read a worker's current signal from the [`SessionManager`], keyed by its
/// `task_run_id`. Pure over the manager — no DB, no mutation — so the
/// reconciler can poll it cheaply every tick.
///
/// Mapping:
/// - `Ready` → [`WorkerSignal::ReadyIdle`] (completion candidate)
/// - `Created` / `Initializing` / `Processing` / `Interrupting` / `Promoting`
///   → [`WorkerSignal::Working`]
/// - `Closing` / `Closed` → [`WorkerSignal::Errored`]
/// - no registered session → [`WorkerSignal::Gone`]
///
/// Looks in BOTH the `ClaudeSession` map and the pty-backed worker map (via
/// [`SessionManager::get_state`]) so it works regardless of which spawn path
/// produced the worker.
pub fn worker_terminal_state(session_mgr: &SessionManager, task_run_id: Uuid) -> WorkerSignal {
    match session_mgr.get_state(&task_run_id.to_string()) {
        Some(state) => signal_from_state(state),
        None => WorkerSignal::Gone,
    }
}

/// Pure mapping from a [`SessionState`] to a [`WorkerSignal`]. Split out so the
/// classification is unit-testable without a live `SessionManager`.
pub fn signal_from_state(state: SessionState) -> WorkerSignal {
    match state {
        SessionState::Ready => WorkerSignal::ReadyIdle,
        SessionState::Created
        | SessionState::Initializing
        | SessionState::Processing
        | SessionState::Interrupting
        | SessionState::Promoting => WorkerSignal::Working,
        SessionState::Closing | SessionState::Closed => WorkerSignal::Errored,
    }
}

/// Contract §5 guard (the primitive the Phase-1 review flagged as owed): a
/// subtask may transition `Working → Completed` **only when BOTH**
///
/// 1. the FSM `Ready` signal is observed ([`WorkerSignal::ReadyIdle`]), AND
/// 2. a readable `artifact` row exists (`subtask.artifact.is_some()`).
///
/// Returns `false` for every other combination — notably `ReadyIdle` with NO
/// artifact (the worker went idle WITHOUT reporting — possibly paused to ask a
/// question, byte-indistinguishable from done; Phase 3's post-`Ready` timeout
/// recovery handles it) and any non-`ReadyIdle` signal regardless of artifact.
///
/// This is a PURE predicate; the reconciler (Phase 3) owns the actual state
/// write (`set_subtask_state(.., Completed)`).
pub fn can_complete(subtask: &Subtask, signal: WorkerSignal) -> bool {
    matches!(signal, WorkerSignal::ReadyIdle) && subtask.artifact.is_some()
}

/// Build the report-contract instruction block appended to a worker's brief.
///
/// Tells the worker, in no uncertain terms, that finishing means calling the
/// runner MCP tool `orchestration_report_subtask` with the run/task identity
/// and a `CompletionReport`-shaped payload, and only THEN (optionally) printing
/// the `[TASK_COMPLETE]` sentinel. The identity (`run_id` + `task_id`) is
/// inlined so the worker reports against the right ledger row.
///
/// `report_url` is the absolute runner endpoint the tool is mounted at, so the
/// worker (which shares this runner's MCP surface) can resolve it without
/// guessing the port.
pub fn build_report_instruction(run_id: Uuid, task_id: &str, report_url: &str) -> String {
    format!(
        "\n\n---\n\
         [ORCHESTRATION REPORT CONTRACT — REQUIRED]\n\
         You are a worker subtask in an orchestrated run. When you have FINISHED \
         this subtask you MUST report a structured result before stopping:\n\n\
         1. Call the runner MCP tool `orchestration_report_subtask` with EXACTLY:\n\
            {{\n\
              \"run_id\": \"{run_id}\",\n\
              \"task_id\": \"{task_id}\",\n\
              \"completion_report\": {{\n\
                \"summaryMd\": \"<markdown summary of what you did>\",\n\
                \"deliverables\": [{{\"kind\": \"pr|commit|file|endpoint|schema-change|spec|other\", \"reference\": \"<ref>\", \"description\": \"<one line>\"}}],\n\
                \"breakingChanges\": [],\n\
                \"followUps\": []\n\
              }}\n\
            }}\n\
            (If you have no runner MCP client, POST the same JSON to `{report_url}`.)\n\
         2. Only AFTER the tool returns success, you MAY print `[TASK_COMPLETE]` \
            on its own line. The sentinel alone is NOT sufficient — the report \
            is what marks you done.\n\
         3. If you cannot complete the work, still call the tool with a summary \
            describing the blocker so the orchestrator can recover.\n\
         ---\n"
    )
}

/// Absolute URL of the `orchestration_report_subtask` HTTP endpoint on THIS
/// runner, used in the brief's fallback instruction. Reads the actually-bound
/// API port from `AppState` when available, else the bootstrap default.
fn report_endpoint_url(app_handle: &tauri::AppHandle) -> String {
    let port = app_handle
        .try_state::<Arc<crate::commands::AppState>>()
        .map(|s| s.api_port.load(std::sync::atomic::Ordering::Relaxed))
        .filter(|p| *p != 0)
        .unwrap_or(crate::mcp::types::MCP_API_PORT);
    format!("http://localhost:{port}/orchestration/report-subtask")
}

/// DISPATCH — turn one ready [`Subtask`] into a live worker AI-session tab.
///
/// Returns the freshly-minted `task_run_id` (the worker's session identity)
/// on success. The subtask is left in [`SubtaskState::Working`] with its
/// `task_run_id` persisted, so the Phase-3 reconciler can find the worker via
/// [`worker_terminal_state`] and gate completion via [`can_complete`].
///
/// Steps (contract §1):
/// 1. Allocate an isolated worktree for `subtask.repo` (when `Some`) via the
///    shared `acquire_for_terminal` path — same helper `terminal_create` uses,
///    so the worker edits an isolated checkout when `QONTINUI_AGENT_WORKTREE_MODE`
///    is on and the shared cwd otherwise.
/// 2. **Re-pick** the lowest-utilization Claude account via `pick_best_account`
///    — PER DISPATCH, not once per run, so account exhaustion across a long run
///    is mitigated.
/// 3. Spawn an interactive `ClaudeSession` (stream-json, NOT `--print`), pinned
///    to the new `task_run_id` as its CLI session id (deterministic transcript
///    for restart-resume). The spawn itself produces no `TerminalManager` PTY,
///    so step 3b durably records the worker as a `TerminalSessionRecord` tagged
///    with `page_id = run_id`, which is what lands the worker in the run's grid
///    page (the frontend's `useTerminalPages` reconcile reads it).
/// 4. Register the session in the [`SessionManager`] and mirror it into coord
///    via `AiCoordRegistrar` (`task_run_id ↔ session_id` identity binding).
/// 5. Submit `subtask.brief` + the report-contract instruction block as the
///    worker's first message.
/// 6. Persist the `task_id ↔ task_run_id` binding and flip the subtask to
///    `Working`.
///
/// On any hard failure before the session is live, the subtask is left in its
/// prior state (caller may retry) and an `Err` is returned describing the
/// failure point.
pub async fn dispatch_subtask(
    app_handle: &tauri::AppHandle,
    pg: &Arc<crate::database::pg::PgDb>,
    run_id: Uuid,
    subtask: &Subtask,
) -> Result<Uuid, String> {
    let task_run_id = Uuid::new_v4();
    info!(
        "dispatch_subtask: run={} task_id={} -> task_run_id={} (repo={:?})",
        run_id, subtask.task_id, task_run_id, subtask.repo
    );

    // Agent-registry spawn authorization (plan
    // `2026-07-28-migrate-claude-md-into-qontinui.md` Phase 4c, served clause
    // `agent-spawn-authorization`). The orchestration loop dispatches one
    // worker session per subtask — this is the workflow fan-out the clause
    // names, so it is `parallel_fanout`: bounded-needs-declaration, not a
    // standing spawn. Checked FIRST, before the worktree is acquired and the
    // account is picked, so a refusal leaves no allocated worktree behind.
    //
    // `authorize_fanout_spawn_with_budget` (not `authorize_spawn`) because this
    // class also carries a declared fan-out bound. The returned `FanoutSlot` is
    // the admission ticket: it is held for the whole dispatch and released by
    // its `Drop` on every exit path below, including the `?` returns and a
    // panic.
    //
    // NOTE the near-zero wait budget. The conductor's tick loop IS the retry
    // mechanism, and `apply_tick` is one sequential function — queueing here
    // for the module's 120s default would stall completions, gate registration
    // and the stall fingerprint for minutes at a time, once per queued
    // dispatch. `SlotUnavailable` is handled below as "not this tick", which is
    // the same degrade-to-sequential at tick granularity. The bound that
    // actually shapes this run is applied a layer up, in the conductor's
    // per-tick `effective_concurrency_cap`.
    let fanout_slot = match crate::agent_authorization::authorize_fanout_spawn_with_budget(
        None,
        FANOUT_ADMISSION_WAIT,
    )
    .await
    {
        crate::agent_authorization::FanoutAdmission::Admitted { decision, slot } => {
            if let crate::agent_authorization::SpawnDecision::SerializeToBound {
                bound,
                in_flight,
            } = &decision
            {
                // The clause's degrade-to-sequential: the bound was full, this
                // dispatch queued for a slot, and the work is proceeding. NOT a
                // failure. Logged once per serialization, never per tick.
                info!(
                    "dispatch_subtask: {} serialized behind the parallel fan-out bound \
                     (bound={bound}, in_flight={in_flight}) — proceeding sequentially",
                    subtask.task_id
                );
            }
            slot
        }
        crate::agent_authorization::FanoutAdmission::Refused(decision) => {
            // Make the refusal TERMINAL for this subtask before returning.
            //
            // The conductor treats a dispatch `Err` as a transient failure and
            // leaves the subtask `Submitted` so a later tick retries — on a 5s
            // tick that turns a standing authorization decision into an infinite
            // retry loop, one `warn!` per tick, with the orchestration run hung
            // because `TickPlan::to_fail` only covers `Working` rows. An
            // authorization refusal is a standing decision, not a transient fault:
            // retrying cannot change it. Same lesson as the looping supervisor
            // (which keeps a refusal out of its spawn-failure backoff) and
            // `spawn_run_task` (which posts a terminal lifecycle outcome).
            let mut refusal = decision
                .refusal()
                .unwrap_or_else(|| format!("spawn-authorization {}: refused", decision.label()));
            // `degrade` and `block` are different user choices and must not read
            // identically to whoever opens the failed subtask. `block` means the
            // work is blocked; `degrade` means the user chose "no spawn — do it
            // inline". The orchestrator has no inline mode (a subtask IS a
            // worker session), so the row still goes Failed, but the recorded
            // disposition is named and the operator is told what to do instead
            // rather than being left with an undifferentiated refusal.
            if matches!(
                decision,
                crate::agent_authorization::SpawnDecision::DegradeToInline { .. }
            ) {
                refusal.push_str(
                    " — the recorded disposition is `degrade` (no spawn), and the orchestration \
                     loop has no inline mode: run this subtask's work yourself, or enable the \
                     agent-registry row to let the orchestrator dispatch a worker.",
                );
            }
            warn!("dispatch_subtask: {} refused: {refusal}", subtask.task_id);
            if let Err(e) = pg
                .set_subtask_state(run_id, &subtask.task_id, SubtaskState::Failed)
                .await
            {
                warn!(
                    "dispatch_subtask: could not mark {} Failed after an authorization \
                     refusal: {e}",
                    subtask.task_id
                );
            }
            return Err(refusal);
        }
        crate::agent_authorization::FanoutAdmission::SlotUnavailable { bound, waited, .. } => {
            // TRANSIENT, deliberately unlike the refusal above: the bound is
            // occupied, not the spawn forbidden. The clause says a bound breach
            // must never become a task failure while sequential progress is
            // still possible, so the subtask stays `Submitted` and a later tick
            // retries once a slot frees.
            return Err(format!(
                "dispatch_subtask: {} not dispatched — the parallel fan-out bound ({bound}) \
                 was fully occupied ({}ms). Transient: the subtask stays queued and a later \
                 tick retries.",
                subtask.task_id,
                waited.as_millis()
            ));
        }
    };

    // 1. Worktree allocation (best-effort, mirrors terminal_create). When
    //    `repo` is None the helper is a no-op and returns the default cwd.
    let default_cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let purpose = format!(
        "orchestration worker {}: {}",
        subtask.task_id, subtask.title
    );
    let (working_dir_opt, isolated_ctx) =
        crate::agent_worktree::isolated_edit::acquire_for_terminal(
            subtask.repo.as_deref(),
            &purpose,
            Some(default_cwd.clone()),
            Some(task_run_id),
        )
        .await;
    let working_dir = working_dir_opt.unwrap_or(default_cwd);

    // 2. Re-pick the lowest-utilization account for THIS dispatch (account
    //    exhaustion mitigation — never pin once per run). No-op unless
    //    LeastUsage mode is configured; mirrors `agent_runtime`'s call site.
    crate::ai_provider::account_usage::pick_best_account();

    // Resolve managed state up-front so a missing dependency fails loud before
    // we spawn a CLI we can't track.
    let session_mgr = app_handle
        .try_state::<Arc<SessionManager>>()
        .ok_or_else(|| "dispatch_subtask: SessionManager state not available".to_string())?
        .inner()
        .clone();
    let registrar = app_handle
        .try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
        .map(|s| s.inner().clone());

    let report_url = report_endpoint_url(app_handle);
    let first_message = format!(
        "{}{}",
        subtask.brief,
        build_report_instruction(run_id, &subtask.task_id, &report_url)
    );

    // 3+4. Spawn the worker AI-session tab under page_id = run_id and register
    //      it. `ClaudeSession::spawn` is blocking (waits for the init
    //      handshake), so run it on the blocking pool like `create_ai_session`.
    let handle = app_handle.clone();
    let trid = task_run_id.to_string();
    // page_id == run_id: the tab the frontend groups this worker under is the
    // run's page. The AI-session spawn path (`ClaudeSession::spawn`) tags output
    // events by task_run_id and does NOT itself create a `TerminalManager` PTY
    // with a `page_id`; the run grouping is instead established AFTER the session
    // is live by durably recording a `TerminalSessionRecord { page_id = run_id }`
    // (step 3b below) which the frontend's page reconcile reads. The session name
    // carries the run id too for at-a-glance triage in coord + logs.
    let task_name = format!("worker:{} [run {}]", subtask.task_id, run_id);
    let wd = working_dir.clone();
    let spawn_result: Result<(), String> = spawn_blocking_tracked(move || {
        // AiSessionContext tags this worker's output events with the
        // task_run_id so the tab + transcript bind to this worker. Mirrors
        // `create_ai_session`'s `AiSessionContext::setup`.
        let session_ctx = crate::execution_context::AiSessionContext::setup(&trid, &task_name);

        // Pin the CLI session id to the task_run_id for a deterministic
        // transcript path (restart-resume parity with create_ai_session).
        let cli_session_ctx = crate::claude_session::runner::CliSessionContext {
            cli_session_id: trid.clone(),
            is_resume: false,
        };

        let session = crate::claude_session::ClaudeSession::spawn(
            &wd,
            &trid,
            &handle,
            Some(session_ctx),
            None, // finding_ctx
            None, // progress_ctx
            None, // pid_tracker
            None, // model_override
            None, // worktree (cwd already points at the worktree path)
            None, // tool_policy
            Some(&cli_session_ctx),
            None, // agent_log_emitter — orchestration path, no coord agent_logs
        )?;
        let session = Arc::new(session);

        // Park the isolated edit context on the session so its claim heartbeat
        // lives for the worker's lifetime and RELEASES on Drop (NOT leaked).
        if let Some(ctx) = isolated_ctx {
            session.set_isolated_edit_ctx(ctx);
        }

        session_mgr
            .register(&trid, session.clone())
            .map_err(|e| format!("dispatch_subtask: register session: {e}"))?;

        // 5. Submit the brief + report contract as the worker's first message.
        session
            .send_user_message(&first_message)
            .map_err(|e| format!("dispatch_subtask: send brief: {e}"))?;

        Ok(())
    })
    .await
    .map_err(|e| format!("dispatch_subtask: spawn join error: {e}"))?;

    spawn_result?;

    // 3b (Phase 5 carry-forward — VISIBLE GRID). Durably record the worker as
    // a terminal-session lifecycle row tagged with `page_id = run_id`, keyed by
    // its pinned CLI session id (== task_run_id). This is what makes the worker
    // land in the run's zone grid: the frontend's `useTerminalPages.reconcile`
    // reads `terminal_session_list_open` → `pageIdsFromSessions` and synthesizes
    // the run's page tab from any record carrying `page_id = run_id`, and the
    // grid groups workers by that page. The Phase-2 spawn path
    // (`ClaudeSession::spawn` → `SessionManager.register`) produces NO
    // `TerminalManager` PTY terminal and therefore had no `page_id`; this record
    // closes that gap WITHOUT re-architecting the in-process FSM worker (whose
    // `Result → Ready` signal `worker_terminal_state`/`can_complete` depend on).
    // Best-effort: a missing store never fails an otherwise-live worker.
    if let Some(store) = app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    {
        let trid_str = task_run_id.to_string();
        store.record_open(
            crate::session::session_lifecycle_store::TerminalSessionRecord {
                claude_session_id: trid_str.clone(),
                config_dir: None,
                working_dir: Some(working_dir.clone()),
                // page_id == run_id: the worker's grid page is the run's page.
                page_id: run_id.to_string(),
                zone_index: 0,
                title: Some(format!("worker:{}", subtask.task_id)),
                terminal_id: trid_str.clone(),
                opened_at: 0,
                last_seen_at: 0,
                state: "open".to_string(),
                closed_at: None,
                close_reason: None,
                provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
                // Authoritative: the CLI session id was pre-pinned to
                // task_run_id via `CliSessionContext { is_resume: false }` above
                // (`--session-id`).
                origin: Some(
                    crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string(),
                ),
                restore_pending_at: None,
                confirmed_at: None,
                handle: None,
                account_label: None,
                account_wrapper: None,
                session_name: None,
                name_source: None,
                tenant_id: None,
                // D1: marks this record as a WORKER rather than an
                // operator-opened session. The id is the same value the record
                // is keyed by (the CLI session id was pre-pinned to it above),
                // but stating it explicitly is what lets a reader tell the two
                // kinds apart without re-deriving the coincidence.
                task_run_id: Some(trid_str.clone()),
                bypass_permissions: None,
                restored_from_boot_at: None,
                restore_tier: None,
                finished_at: None,
                finish_reason: None,
                finish_synced: false,
            },
        );
        info!(
            "dispatch_subtask: worker durably recorded on page_id={} (task_run_id={})",
            run_id, task_run_id
        );
    } else {
        warn!(
            "dispatch_subtask: SessionLifecycleStore not available; worker {} will not surface on the run's grid page",
            task_run_id
        );
    }

    // 4 (cont). Identity binding into coord (task_run_id ↔ session_id). Mirror
    //           the `create_ai_session` registration. Best-effort — a coord
    //           hiccup never fails an otherwise-live worker.
    if let Some(registrar) = registrar {
        // task_run_id ↔ session_id identity binding (mirrors create_ai_session).
        // The run association (page_id = run_id) is encoded in the purpose so the
        // coord session plane can group workers by their run.
        registrar.register_session(
            &task_run_id.to_string(),
            &format!("orchestration worker {} (run {})", subtask.task_id, run_id),
            subtask.repo.clone(),
        );
    } else {
        warn!("dispatch_subtask: AiCoordRegistrar not available; skipping coord identity binding");
    }

    // 6. Persist the task_id ↔ task_run_id binding + flip to Working. Upsert
    //    the whole row so `task_run_id` lands atomically with the state.
    let mut updated = subtask.clone();
    updated.task_run_id = Some(task_run_id);
    updated.state = SubtaskState::Working;
    pg.upsert_subtask(&updated)
        .await
        .map_err(|e| format!("dispatch_subtask: persist task_run_id/Working: {e}"))?;

    info!(
        "dispatch_subtask: worker live — run={} task_id={} task_run_id={}",
        run_id, subtask.task_id, task_run_id
    );
    // Explicit so the release point is stated rather than inferred from scope
    // end: the worker is live and registered, so the admission is over and the
    // next queued dispatch may proceed. Every earlier exit path (the `?`
    // returns above, or a panic) releases it the same way, via `Drop`.
    drop(fanout_slot);
    Ok(task_run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration_loop::ledger::SubtaskState;
    use chrono::Utc;

    fn mk_subtask(state: SubtaskState, with_artifact: bool) -> Subtask {
        use crate::database::pg::completion_reports::CompletionReport;
        use std::collections::HashMap;
        Subtask {
            task_id: "T1".to_string(),
            run_id: Uuid::new_v4(),
            idx: 0,
            title: "title".to_string(),
            brief: "do the thing".to_string(),
            phase: "implement".to_string(),
            repo: Some("qontinui-runner".to_string()),
            depends_on: vec![],
            expected_output: "a green PR".to_string(),
            emits_subtasks: false,
            state,
            task_run_id: None,
            artifact: if with_artifact {
                Some(CompletionReport {
                    summary_md: "done".to_string(),
                    deliverables: vec![],
                    breaking_changes: vec![],
                    follow_ups: vec![],
                    artifacts: HashMap::new(),
                })
            } else {
                None
            },
            produced_by: None,
            gate_id: None,
            gate_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // --- signal_from_state mapping ------------------------------------------

    #[test]
    fn ready_maps_to_ready_idle() {
        assert_eq!(
            signal_from_state(SessionState::Ready),
            WorkerSignal::ReadyIdle
        );
    }

    #[test]
    fn busy_states_map_to_working() {
        for s in [
            SessionState::Created,
            SessionState::Initializing,
            SessionState::Processing,
            SessionState::Interrupting,
            SessionState::Promoting,
        ] {
            assert_eq!(signal_from_state(s), WorkerSignal::Working, "state {s:?}");
        }
    }

    #[test]
    fn closing_states_map_to_errored() {
        assert_eq!(
            signal_from_state(SessionState::Closing),
            WorkerSignal::Errored
        );
        assert_eq!(
            signal_from_state(SessionState::Closed),
            WorkerSignal::Errored
        );
    }

    // --- can_complete guard (contract §5) -----------------------------------

    #[test]
    fn can_complete_true_only_when_ready_and_artifact_present() {
        let st = mk_subtask(SubtaskState::Working, true);
        assert!(
            can_complete(&st, WorkerSignal::ReadyIdle),
            "ReadyIdle + artifact present must allow completion"
        );
    }

    #[test]
    fn can_complete_false_when_artifact_absent_even_if_ready() {
        let st = mk_subtask(SubtaskState::Working, false);
        assert!(
            !can_complete(&st, WorkerSignal::ReadyIdle),
            "ReadyIdle WITHOUT artifact must NOT complete (worker may have paused to ask)"
        );
    }

    #[test]
    fn can_complete_false_when_working_signal_even_with_artifact() {
        let st = mk_subtask(SubtaskState::Working, true);
        assert!(
            !can_complete(&st, WorkerSignal::Working),
            "a still-Working signal must never complete, artifact or not"
        );
    }

    #[test]
    fn can_complete_false_for_errored_and_gone() {
        let st = mk_subtask(SubtaskState::Working, true);
        assert!(!can_complete(&st, WorkerSignal::Errored));
        assert!(!can_complete(&st, WorkerSignal::Gone));
    }

    // --- build_report_instruction inlines identity --------------------------

    #[test]
    fn report_instruction_inlines_run_and_task_identity() {
        let run = Uuid::new_v4();
        let block = build_report_instruction(
            run,
            "T42",
            "http://localhost:9876/orchestration/report-subtask",
        );
        assert!(block.contains(&run.to_string()), "run_id must be inlined");
        assert!(
            block.contains("\"task_id\": \"T42\""),
            "task_id must be inlined"
        );
        assert!(
            block.contains("orchestration_report_subtask"),
            "must name the MCP tool"
        );
        assert!(
            block.contains("[TASK_COMPLETE]"),
            "must mention the confirm-only sentinel"
        );
        assert!(
            block.contains("NOT sufficient"),
            "must state the sentinel alone is insufficient"
        );
    }
}
