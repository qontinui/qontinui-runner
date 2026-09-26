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
use crate::orchestration_loop::conductor::DispatchError;
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

/// How long [`dispatch_subtask`] reuses a subtask's last isolation refusal
/// before asking coord to allocate again.
///
/// Every `POST /agents/allocate` mints a coord allocation row, and the
/// conductor retries a refused row on every 5 s tick. Without this, a subtask
/// coord keeps answering `wait` for — a state that is by design unbounded —
/// would mint roughly 720 rows an hour, and a refused `shared_branch` about 60
/// before its run stalls. Coord's `retry_when` is free text, so it is reported
/// rather than parsed; a fixed interval bounds the churn instead.
const ISOLATION_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-subtask memory of the last isolation refusal, so a retry inside
/// [`ISOLATION_RETRY_BACKOFF`] returns the same answer without re-allocating.
/// Process-local on purpose: a restart simply asks coord again.
#[derive(Default)]
pub(crate) struct IsolationBackoff {
    entries: std::collections::HashMap<(Uuid, String), (std::time::Instant, DispatchError)>,
}

impl IsolationBackoff {
    /// The refusal to repeat for `(run_id, task_id)` at `now`, or `None` when
    /// there is none or its backoff has elapsed. Expired entries are dropped.
    pub(crate) fn pending(
        &mut self,
        run_id: Uuid,
        task_id: &str,
        now: std::time::Instant,
    ) -> Option<DispatchError> {
        self.entries
            .retain(|_, (at, _)| now.duration_since(*at) < ISOLATION_RETRY_BACKOFF);
        self.entries
            .get(&(run_id, task_id.to_string()))
            .map(|(_, e)| e.clone())
    }

    /// Remember a non-terminal refusal. A terminal one never retries, so it is
    /// not recorded.
    pub(crate) fn record(
        &mut self,
        run_id: Uuid,
        task_id: &str,
        now: std::time::Instant,
        error: DispatchError,
    ) {
        self.entries
            .insert((run_id, task_id.to_string()), (now, error));
    }

    /// Forget a subtask once it got its worktree.
    pub(crate) fn clear(&mut self, run_id: Uuid, task_id: &str) {
        self.entries.remove(&(run_id, task_id.to_string()));
    }
}

static ISOLATION_BACKOFF: std::sync::LazyLock<std::sync::Mutex<IsolationBackoff>> =
    std::sync::LazyLock::new(Default::default);

fn isolation_backoff() -> std::sync::MutexGuard<'static, IsolationBackoff> {
    ISOLATION_BACKOFF
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How [`dispatch_subtask`] reports a subtask that got no isolated worktree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IsolationRefusal {
    /// What the conductor reads. [`DispatchError::Transient`] keeps the row
    /// queued and out of the stall fingerprint; [`DispatchError::Failed`]
    /// leaves it `Submitted` but fingerprinted, so a permanent cause stalls the
    /// run naming it.
    pub error: DispatchError,
    /// `true` when retrying cannot change the answer, so the row is marked
    /// `Failed` before the error is returned.
    pub terminal: bool,
}

/// Classify why a worker got no isolated worktree. Pure, so every arm is
/// unit-tested without a live allocation (plan
/// `2026-09-23-conductor-e2e-phase1-defects` Phase 1):
///
/// - coord's `wait`, or a claim another agent holds → `Transient`, carrying
///   coord's reason and `retry_when`: coord said "not yet", not "no";
/// - worktree mode off → `Failed` and TERMINAL: a standing configuration;
/// - anything else (coord's `shared_branch` refused, transport blips, coord
///   5xx, `git worktree add` failures) → `Failed`, NOT terminal, since a
///   transport blip can clear on a retry and the stall window still bounds a
///   permanent cause.
///
/// Every `Failed` message leads with the `no_isolated_worktree` token the
/// continuation path uses for the same condition.
pub(crate) fn isolation_refusal(
    task_id: &str,
    repo: &str,
    err: &crate::agent_worktree::isolated_edit::WorkerIsolationError,
) -> IsolationRefusal {
    use crate::agent_worktree::isolated_edit::WorkerIsolationError;
    use crate::agent_worktree::{AllocateError, NO_ISOLATED_WORKTREE};

    match err {
        WorkerIsolationError::Allocate(AllocateError::Wait(w)) => IsolationRefusal {
            error: DispatchError::Transient(format!(
                "dispatch_subtask: {task_id} not dispatched — coord answered wait for {repo} \
                 (reason={}, blocking={}, retry_when={}). Transient: the subtask stays queued \
                 and a later tick retries.",
                w.reason.as_deref().unwrap_or("-"),
                w.blocking.as_deref().unwrap_or("-"),
                w.retry_when.as_deref().unwrap_or("-"),
            )),
            terminal: false,
        },
        WorkerIsolationError::Allocate(e @ AllocateError::ClaimConflict(_)) => IsolationRefusal {
            error: DispatchError::Transient(format!(
                "dispatch_subtask: {task_id} not dispatched — {e} for {repo}. Transient: the \
                 subtask stays queued and a later tick retries."
            )),
            terminal: false,
        },
        WorkerIsolationError::ModeOff => IsolationRefusal {
            error: DispatchError::Failed(format!(
                "{NO_ISOLATED_WORKTREE}: worktree mode is off, so subtask {task_id} ({repo}) is \
                 not dispatched into the shared checkout"
            )),
            terminal: true,
        },
        WorkerIsolationError::Allocate(e) => {
            let e = e.to_string();
            let message = if e.starts_with(NO_ISOLATED_WORKTREE) {
                format!("{e} (subtask {task_id} not dispatched)")
            } else {
                format!(
                    "{NO_ISOLATED_WORKTREE}: subtask {task_id} ({repo}) not dispatched — \
                     worktree allocate failed: {e}"
                )
            };
            IsolationRefusal {
                error: DispatchError::Failed(message),
                terminal: false,
            }
        }
    }
}

/// DISPATCH — turn one ready [`Subtask`] into a live worker AI-session tab.
///
/// Returns the freshly-minted `task_run_id` (the worker's session identity)
/// on success. The subtask is left in [`SubtaskState::Working`] with its
/// `task_run_id` persisted, so the Phase-3 reconciler can find the worker via
/// [`worker_terminal_state`] and gate completion via [`can_complete`].
///
/// Steps (contract §1):
/// 1. Allocate an isolated worktree for `subtask.repo` (when `Some`) via
///    `acquire_for_worker`, the fail-closed sibling of the terminal helper. No
///    worktree means no worker: coord's `wait` or a held claim is
///    [`DispatchError::Transient`], anything else is [`DispatchError::Failed`],
///    and worktree mode being off also marks the row `Failed` — see
///    [`isolation_refusal`].
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
/// On any hard failure the subtask is left in its prior state and an
/// [`DispatchError`] is returned describing the failure point, so the caller
/// may retry. **A failure AFTER the session is live tears that session down
/// first** — closes the CLI, unregisters it from the `SessionManager`, closes
/// its lifecycle record and its coord session — because steps 3 through 5
/// spawn, register and bind a worker BEFORE step 6 binds it to the row. Without
/// the teardown a step-6 failure returned `Err` with a fully live worker the
/// ledger has no record of, and the conductor's 5 s retry against its 300 s
/// stall window spawned roughly 60 orphaned CLI sessions (and took as many
/// worktree acquisitions) before the run stalled. Re-dispatch is safe BECAUSE
/// of the teardown, not despite it.
///
/// The error is TYPED, and the conductor reads the type rather than the text:
/// [`DispatchError::Transient`] for a fan-out bound that is momentarily full and
/// [`DispatchError::DeferredByDrain`] for a device coord has drained — in both
/// the row is queued, not stuck, so the conductor must not count it toward a
/// stall — and [`DispatchError::Failed`] for everything else.
pub async fn dispatch_subtask(
    app_handle: &tauri::AppHandle,
    pg: &Arc<crate::database::pg::PgDb>,
    run_id: Uuid,
    subtask: &Subtask,
) -> Result<Uuid, DispatchError> {
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
        crate::agent_authorization::DrainAdmission::work(
            crate::coord_drain_state::SpawnOrigin::Orchestration,
            format!("subtask:{}", subtask.task_id),
        ),
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
            return Err(refusal.into());
        }
        crate::agent_authorization::FanoutAdmission::SlotUnavailable { bound, waited, .. } => {
            // TRANSIENT, deliberately unlike the refusal above: the bound is
            // occupied, not the spawn forbidden. The clause says a bound breach
            // must never become a task failure while sequential progress is
            // still possible, so the subtask stays `Submitted` and a later tick
            // retries once a slot frees.
            return Err(DispatchError::Transient(format!(
                "dispatch_subtask: {} not dispatched — the parallel fan-out bound ({bound}) \
                 was fully occupied ({}ms). Transient: the subtask stays queued and a later \
                 tick retries.",
                subtask.task_id,
                waited.as_millis()
            )));
        }
        crate::agent_authorization::FanoutAdmission::DeferredByDrain { reason } => {
            // TRANSIENT, like the bound above: coord has drained this device (or
            // its drain state is unknown). The subtask stays `Submitted` — the
            // work is deferred, never failed — and a later tick dispatches it
            // once the drain lifts. Typed so the conductor logs it quietly and
            // `tick_exit` drops the row's fingerprint key.
            return Err(DispatchError::DeferredByDrain(format!(
                "dispatch_subtask: {} not dispatched — {reason}. Transient: the subtask stays \
                 queued and a later tick retries.",
                subtask.task_id
            )));
        }
    };

    // 1. Worktree allocation. A subtask that declares a `repo` gets an isolated
    //    worktree or is NOT dispatched: unlike an operator's terminal, a worker
    //    never falls back to the shared checkout (plan
    //    `2026-09-23-conductor-e2e-phase1-defects` Phase 1). `repo: None` still
    //    runs in the runner's process cwd — recorded in that plan as residual.
    let purpose = format!(
        "orchestration worker {}: {}",
        subtask.task_id, subtask.title
    );
    let (working_dir, isolated_ctx) = match subtask.repo.as_deref() {
        None => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());
            // Still provisioned, as `acquire_for_terminal` did for this arm:
            // the cwd `.mcp.json` is how a worker reaches the coord-mcp proxy.
            crate::agent_worktree::isolated_edit::provision_session_cwd_off_runtime(
                &cwd, false, None,
            )
            .await;
            (cwd, None)
        }
        Some(repo) => {
            // Inside the backoff after a refusal: answer as before, without
            // minting another coord allocation (see ISOLATION_RETRY_BACKOFF).
            if let Some(repeat) =
                isolation_backoff().pending(run_id, &subtask.task_id, std::time::Instant::now())
            {
                return Err(repeat);
            }
            match crate::agent_worktree::isolated_edit::acquire_for_worker(
                repo,
                &purpose,
                Some(task_run_id),
                None,
            )
            .await
            {
                Ok((wd, ctx)) => {
                    isolation_backoff().clear(run_id, &subtask.task_id);
                    (wd, Some(ctx))
                }
                Err(e) => {
                    let refusal = isolation_refusal(&subtask.task_id, repo, &e);
                    if refusal.terminal {
                        // A standing decision, like the authorization refusal
                        // above: mark the row Failed FIRST so the conductor stops
                        // re-deciding it every tick.
                        if let Err(pe) = pg
                            .set_subtask_state(run_id, &subtask.task_id, SubtaskState::Failed)
                            .await
                        {
                            warn!(
                                "dispatch_subtask: could not mark {} Failed after an \
                                 isolation refusal: {pe}",
                                subtask.task_id
                            );
                        }
                    } else {
                        isolation_backoff().record(
                            run_id,
                            &subtask.task_id,
                            std::time::Instant::now(),
                            refusal.error.clone(),
                        );
                    }
                    return Err(refusal.error);
                }
            }
        }
    };

    // 2. Re-pick the lowest-utilization account for THIS dispatch (account
    //    exhaustion mitigation — never pin once per run). No-op unless
    //    LeastUsage mode is configured; mirrors `agent_runtime`'s call site.
    crate::ai_provider::account_usage::pick_best_account();

    // Resolve managed state up-front so a missing dependency fails loud before
    // we spawn a CLI we can't track.
    let session_mgr = app_handle
        .try_state::<Arc<SessionManager>>()
        .ok_or_else(|| {
            DispatchError::Failed(
                "dispatch_subtask: SessionManager state not available".to_string(),
            )
        })?
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

        // From here the CLI is LIVE. Every failure below tears it down before
        // returning: a `?` here used to leave a running Claude process the
        // ledger has no row for, which the conductor's retry then duplicated.
        if let Err(e) = session_mgr.register(&trid, session.clone()) {
            let _ = session.close();
            return Err(format!(
                "dispatch_subtask: register session: {e} (the spawned CLI was closed)"
            ));
        }

        // 5. Submit the brief + report contract as the worker's first message.
        if let Err(e) = session.send_user_message(&first_message) {
            session_mgr.remove(&trid);
            let _ = session.close();
            return Err(format!(
                "dispatch_subtask: send brief: {e} (the spawned CLI was closed and unregistered)"
            ));
        }

        Ok(())
    })
    .await
    .map_err(|e| DispatchError::Failed(format!("dispatch_subtask: spawn join error: {e}")))?;

    spawn_result.map_err(DispatchError::Failed)?;

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
                spawn_device_default: None,
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
    if let Err(e) = pg.upsert_subtask(&updated).await {
        // The one step that makes a live worker RECONCILABLE, and the one that
        // can fail with the worker already live. Leaving it up is not a
        // recoverable state: the row stays `Submitted` with no `task_run_id`,
        // so no later tick can ever find this session — it is not `in_flight`,
        // no §5 deadline reaches it, and `worker_terminal_state` is never asked
        // about it. It would hold a worktree claim and a CLI process until the
        // runner exits, while the next tick spawns its replacement.
        let torn = teardown_unbound_worker(app_handle, task_run_id);
        return Err(DispatchError::Failed(format!(
            "dispatch_subtask: persist task_run_id/Working: {e} — the live worker was torn \
             down ({torn}), so a retry re-dispatches rather than duplicating it"
        )));
    }

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

/// Tear down a worker that is LIVE but never got bound to its subtask row —
/// the only state in which a session exists that nothing will ever reconcile.
///
/// Closes it on every plane it was registered on, in the order that leaves no
/// observer holding a stale "open": the `SessionManager` entry (so
/// `worker_terminal_state` stops resolving it), the CLI itself (which also
/// drops the isolated-edit context and releases its worktree claim), the
/// durable lifecycle record the run's grid page reads, and the coord session
/// binding. Every plane is best-effort and reported rather than propagated:
/// this runs on a path that is ALREADY failing, and the caller's error is the
/// one the operator needs to read.
///
/// Returns a short description of what it actually closed, for that error.
fn teardown_unbound_worker(app_handle: &tauri::AppHandle, task_run_id: Uuid) -> String {
    let trid = task_run_id.to_string();
    let mut closed: Vec<&str> = Vec::new();

    if let Some(mgr) = app_handle.try_state::<Arc<SessionManager>>() {
        if let Some(session) = mgr.remove(&trid) {
            closed.push("session");
            if let Err(e) = session.close() {
                warn!("teardown_unbound_worker: close {trid}: {e}");
            }
        }
    }
    if let Some(store) = app_handle
        .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>()
    {
        store.record_close(&trid, "orchestration dispatch could not bind the worker");
        closed.push("lifecycle record");
    }
    if let Some(registrar) =
        app_handle.try_state::<Arc<crate::claude_session::coord_register::AiCoordRegistrar>>()
    {
        registrar.close_session(&trid);
        closed.push("coord session");
    }

    if closed.is_empty() {
        "nothing was registered yet".to_string()
    } else {
        format!("closed: {}", closed.join(", "))
    }
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

    // --- isolation_refusal: a worker never falls back to the shared cwd ----
    //
    // Plan `2026-09-23-conductor-e2e-phase1-defects` Phase 1. `dispatch_subtask`
    // returns this refusal BEFORE the account pick and the spawn, so every arm
    // below is a dispatch that spawned no session and — unless `terminal` —
    // left the row `Submitted`.

    use crate::agent_worktree::isolated_edit::WorkerIsolationError;
    use crate::agent_worktree::{AllocateError, ClaimConflict, WaitOutcome};

    #[test]
    fn coords_wait_is_transient_and_carries_its_reason_and_retry_when() {
        let r = isolation_refusal(
            "T1",
            "qontinui-runner",
            &WorkerIsolationError::Allocate(AllocateError::Wait(WaitOutcome {
                agent_id: "a1".to_string(),
                reason: Some("upstream PR in flight".to_string()),
                blocking: Some("qontinui-runner#1".to_string()),
                retry_when: Some("after merge".to_string()),
            })),
        );
        assert!(!r.terminal, "a wait is not a standing decision");
        assert!(r.error.transient(), "{:?}", r.error);
        let text = r.error.to_string();
        assert!(text.contains("upstream PR in flight"), "{text}");
        assert!(text.contains("after merge"), "{text}");
    }

    #[test]
    fn a_held_claim_is_transient() {
        let r = isolation_refusal(
            "T1",
            "qontinui-runner",
            &WorkerIsolationError::Allocate(AllocateError::ClaimConflict(ClaimConflict {
                kind: "phase".to_string(),
                resource_key: "plan:p:phase:1".to_string(),
                current_holder: "peer".to_string(),
                intent: None,
            })),
        );
        assert!(!r.terminal);
        assert!(r.error.transient(), "{:?}", r.error);
        assert!(r.error.to_string().contains("claim already held"));
    }

    #[test]
    fn any_other_allocate_error_is_failed_but_retried() {
        let r = isolation_refusal(
            "T1",
            "qontinui-runner",
            &WorkerIsolationError::Allocate(AllocateError::Other(
                "POST https://coord/agents/allocate returned 503".to_string(),
            )),
        );
        assert!(!r.terminal, "a transport blip can clear on a retry");
        assert!(matches!(r.error, DispatchError::Failed(_)), "{:?}", r.error);
        let text = r.error.to_string();
        assert!(text.starts_with("no_isolated_worktree: "), "{text}");
        assert!(text.contains("503"), "{text}");
    }

    #[test]
    fn worktree_mode_off_is_terminal() {
        let r = isolation_refusal("T1", "qontinui-runner", &WorkerIsolationError::ModeOff);
        assert!(r.terminal, "mode off is a standing configuration");
        assert!(matches!(r.error, DispatchError::Failed(_)), "{:?}", r.error);
        let text = r.error.to_string();
        assert!(
            text.starts_with("no_isolated_worktree: worktree mode is off"),
            "{text}"
        );
    }

    #[test]
    fn a_refused_shared_branch_is_failed_and_keeps_its_token_first() {
        let refusal = crate::agent_worktree::shared_branch_policy_refusal(&[
            crate::agent_worktree::RepoRequest {
                repo: "qontinui-runner".to_string(),
                parent_sha: None,
            },
        ]);
        let r = isolation_refusal(
            "T1",
            "qontinui-runner",
            &WorkerIsolationError::Allocate(AllocateError::Other(refusal.to_string())),
        );
        assert!(!r.terminal);
        assert!(matches!(r.error, DispatchError::Failed(_)), "{:?}", r.error);
        let text = r.error.to_string();
        assert!(
            text.starts_with("no_isolated_worktree: coord chose shared_branch"),
            "{text}"
        );
        assert!(text.contains("T1"), "{text}");
    }

    // --- IsolationBackoff: a refused subtask does not re-allocate every tick --

    #[test]
    fn a_refusal_repeats_inside_the_backoff_and_expires_after_it() {
        let mut b = IsolationBackoff::default();
        let run = Uuid::new_v4();
        let t0 = std::time::Instant::now();
        assert_eq!(b.pending(run, "T1", t0), None);

        let wait = DispatchError::Transient("coord answered wait".to_string());
        b.record(run, "T1", t0, wait.clone());
        let inside = t0 + ISOLATION_RETRY_BACKOFF - std::time::Duration::from_secs(1);
        assert_eq!(b.pending(run, "T1", inside), Some(wait));
        assert_eq!(b.pending(run, "T2", inside), None, "keyed per subtask");
        assert_eq!(
            b.pending(Uuid::new_v4(), "T1", inside),
            None,
            "keyed per run"
        );

        assert_eq!(b.pending(run, "T1", t0 + ISOLATION_RETRY_BACKOFF), None);
    }

    #[test]
    fn a_granted_worktree_clears_the_backoff() {
        let mut b = IsolationBackoff::default();
        let run = Uuid::new_v4();
        let t0 = std::time::Instant::now();
        b.record(
            run,
            "T1",
            t0,
            DispatchError::Failed("no_isolated_worktree: x".into()),
        );
        b.clear(run, "T1");
        assert_eq!(b.pending(run, "T1", t0), None);
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
