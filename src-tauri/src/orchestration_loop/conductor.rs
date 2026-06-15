//! Approach-D Conductor/Engine — Phase 3 stateless reconciler.
//!
//! The **conductor** is the extended loop engine that drives a run's subtask
//! DAG to completion. It is a Rust BACKGROUND TASK, not a session (refinement
//! R1): it holds NO task state in memory between ticks — every tick re-derives
//! readiness, in-flight set, and exit condition from the durable ledger
//! (`orchestration.subtasks`) via [`PgDb::list_subtasks`]. A kill+restart of
//! the runner therefore resumes mid-run with no lost or duplicated dispatch
//! (contract §6 RESUME / statelessness).
//!
//! ## Tick (contract §3)
//!
//! 1. **observe** — `list_subtasks(run_id)` loads ALL persisted rows.
//! 2. **determine_execution_order** — Kahn topo-sort over the persisted DAG
//!    (reuses [`crate::orchestration_loop::subtask_executor`]'s algorithm,
//!    adapted to the ledger [`Subtask`] shape) so dispatch respects deps and a
//!    cycle is surfaced rather than silently stalling.
//! 3. **dispatch** — every `Submitted` subtask whose `depends_on` are ALL
//!    `Completed`, up to a concurrency cap (run config), is handed to
//!    [`crate::orchestration_loop::ai_session_executor::dispatch_subtask`].
//! 4. **reconcile** — poll each in-flight worker's FSM signal (Phase-2
//!    `worker_terminal_state`); flip `Working → Completed` ONLY when the §5
//!    guard [`can_complete`] holds (`ReadyIdle` AND `artifact.is_some()`); flip
//!    `Working → Failed` on `Errored`/`Gone`-past-budget/stall-past-budget.
//! 5. **post-Ready-without-artifact recovery** — a worker that is `ReadyIdle`
//!    but has no artifact after a per-subtask timeout is re-prompted ONCE (the
//!    only place a live session is re-prompted); still no artifact after a
//!    second timeout → `Failed`.
//! 6. **harvest hook** — [`harvest_elaboration`] is the Phase-4 extension
//!    point; in Phase 3 it is a no-op (documented below).
//! 7. **stall / exit** — exit when all subtasks are terminal AND no
//!    `emits_subtasks` row is un-harvested; stall via [`StallDetector`] over a
//!    fingerprint that EXCLUDES legitimately-blocked/elaborating rows
//!    (contract §3.5).
//!
//! ## Testability
//!
//! The tick's DECISIONS are computed by pure functions over `&[Subtask]` plus
//! an injected [`SignalSource`] — see [`compute_tick`]. The live background
//! task ([`run_orchestration`]) wires the real DB, the real
//! [`AiSessionDispatcher`], and the real [`worker_terminal_state`] signal; the
//! unit tests inject a fake dispatcher + a scripted signal and assert on the
//! [`TickPlan`] WITHOUT spawning any worker. This is the factoring the spec's
//! §5 ("factor the tick logic so readiness/guard/resume decisions are
//! unit-testable WITHOUT live spawns") requires.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::watch;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::ai_session_executor::{self, can_complete, WorkerSignal};
use super::coord_gate::{
    classify_drift_verify, classify_gate, CoordGateClient, DriftClass, GatePredicateSpec,
    GateStatus, DEFAULT_DRIFT_SUBSPACE,
};
use super::ledger::{Subtask, SubtaskState};
use super::loop_engine::SharedLoopState;
use super::org_chart::OrgChartSeed;
use super::stall_detector::StallDetector;
use super::types::*;
use crate::database::pg::PgDb;

// ============================================================================
// Run config (runner-local runtime knobs — never crosses the wire)
// ============================================================================

/// Runtime knobs for a conductor run. Runner-local (not a wire DTO); sensible
/// conservative defaults so a bare `/orchestrate` works without tuning.
#[derive(Debug, Clone)]
pub struct OrchestrationRunConfig {
    /// Seconds between reconciler ticks.
    pub tick_interval_secs: u64,
    /// Maximum number of workers dispatched-and-not-terminal at once.
    pub concurrency_cap: usize,
    /// Seconds a `ReadyIdle`-without-artifact worker is tolerated before the
    /// reconciler re-prompts it once (the §step-5 recovery first deadline).
    pub report_timeout_secs: i64,
    /// Seconds after the re-prompt before a still-artifact-less worker is
    /// failed (the §step-5 recovery second deadline).
    pub report_reprompt_grace_secs: i64,
    /// Seconds a `Gone` worker (vanished from the SessionManager) is tolerated
    /// before being failed — covers a transient restart window.
    pub gone_grace_secs: i64,
}

impl Default for OrchestrationRunConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 5,
            // Conservative default per spec §3.3 ("default conservative, e.g.
            // 3 in-flight").
            concurrency_cap: 3,
            report_timeout_secs: 90,
            report_reprompt_grace_secs: 90,
            gone_grace_secs: 60,
        }
    }
}

// ============================================================================
// Pure DAG / readiness derivation over the durable rows
// ============================================================================

/// `true` when the subtask is in a terminal lifecycle state (no further
/// reconciler action). `InputRequired` is NOT terminal (reserved A2A state).
pub fn is_terminal(state: SubtaskState) -> bool {
    matches!(
        state,
        SubtaskState::Completed | SubtaskState::Failed | SubtaskState::Canceled
    )
}

/// `true` when every subtask is terminal.
pub fn all_terminal(subtasks: &[Subtask]) -> bool {
    subtasks.iter().all(|s| is_terminal(s.state))
}

// ============================================================================
// Phase 6 — gate-blocked predicates (read the durable gate_id/gate_status cols)
// ============================================================================

/// `true` when this subtask is **blocked-on-gate**: it is `Submitted`, carries a
/// registered `gate_id`, and the gate has NOT cleared (`gate_status` is anything
/// other than `cleared`/`failed`). Such a row is NOT dispatchable this tick — the
/// gate's external condition (CI green / PR merged / deploy healthy) is not yet
/// met. This is the contract's "a `Submitted` variant annotation": no new
/// `SubtaskState`, the block lives in the gate columns.
///
/// A `failed` gate is NOT "blocked" here — the reconciler fails the subtask
/// instead (see [`compute_tick`]); a `cleared` gate is also not blocked — the
/// subtask becomes dispatchable.
pub fn gate_blocked(subtask: &Subtask) -> bool {
    subtask.state == SubtaskState::Submitted
        && subtask.gate_id.is_some()
        && !matches!(
            subtask.gate_status.as_deref(),
            Some("cleared") | Some("failed")
        )
}

/// `true` when a `Submitted`+deps-satisfied subtask carries a registered gate
/// whose status says the gate FAILED — the subtask must be failed (its external
/// pre-condition can never be met).
pub fn gate_failed(subtask: &Subtask) -> bool {
    subtask.state == SubtaskState::Submitted
        && subtask.gate_id.is_some()
        && subtask.gate_status.as_deref() == Some("failed")
}

/// `true` when a `Submitted` subtask still needs a gate registered: its
/// `expected_output` classifies to an observable external condition but it has no
/// `gate_id` yet. The reconciler registers the gate (holding dispatch) this tick.
pub fn needs_gate_registration(subtask: &Subtask) -> bool {
    subtask.state == SubtaskState::Submitted
        && subtask.gate_id.is_none()
        && classify_gate(&subtask.expected_output, subtask.repo.as_deref()).is_some()
}

/// `true` when a subtask is a DriftVerdict **verify** subtask (its
/// `expected_output` matches the drift-verify pattern). The reconciler reads the
/// Digital-Twin verdict at this subtask's completion rather than completing it
/// blindly.
pub fn is_drift_verify(subtask: &Subtask) -> bool {
    classify_drift_verify(&subtask.expected_output)
}

/// The `CompletionReport.artifacts` key under which a verify subtask's read
/// `DriftVerdict` is stored (`artifact.artifacts["drift_verdict"]`, contract §4).
pub const DRIFT_VERDICT_ARTIFACT_KEY: &str = "drift_verdict";

/// `true` when a verify subtask already has its DriftVerdict recorded at
/// `artifact.artifacts["drift_verdict"]` — so the reconciler does not re-read it
/// (idempotent / resume: a recorded no-drift verdict lets the normal completion
/// path take over; a recorded drift verdict means the remediation was already
/// spliced).
pub fn drift_verdict_recorded(subtask: &Subtask) -> bool {
    subtask
        .artifact
        .as_ref()
        .map(|a| a.artifacts.contains_key(DRIFT_VERDICT_ARTIFACT_KEY))
        .unwrap_or(false)
}

/// Topological order (by `task_id`) over the persisted DAG using the same
/// Kahn algorithm as [`crate::orchestration_loop::subtask_executor`], adapted
/// to the ledger [`Subtask`] shape. Returns `Err` on a dependency cycle.
///
/// Unknown `depends_on` ids (a dep that names no row in this run) are ignored
/// for ordering — they are treated as "not satisfiable" by [`deps_satisfied`]
/// so a subtask naming a missing dep simply never becomes ready (surfaced as a
/// stall rather than a crash).
pub fn topo_order(subtasks: &[Subtask]) -> Result<Vec<usize>, String> {
    let n = subtasks.len();
    let id_to_index: HashMap<&str, usize> = subtasks
        .iter()
        .enumerate()
        .map(|(i, s)| (s.task_id.as_str(), i))
        .collect();

    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, subtask) in subtasks.iter().enumerate() {
        for dep_id in &subtask.depends_on {
            if let Some(&dep_idx) = id_to_index.get(dep_id.as_str()) {
                dependents[dep_idx].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    while let Some(idx) = queue.pop() {
        order.push(idx);
        for &dependent in &dependents[idx] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push(dependent);
            }
        }
    }

    if order.len() != n {
        return Err("Circular dependency detected in subtask DAG".to_string());
    }
    Ok(order)
}

/// `true` when ALL of `subtask`'s declared dependencies resolve to a
/// `Completed` row in this run. A dep naming no row, or a dep in any
/// non-`Completed` state, makes this `false` (the subtask is not yet ready).
pub fn deps_satisfied(subtask: &Subtask, by_id: &HashMap<&str, &Subtask>) -> bool {
    subtask.depends_on.iter().all(|dep| {
        by_id
            .get(dep.as_str())
            .map(|d| d.state == SubtaskState::Completed)
            .unwrap_or(false)
    })
}

/// Subtasks that may be dispatched THIS tick (state `Submitted`, all deps
/// `Completed`), in topo order. Does NOT apply the concurrency cap — that is
/// applied in [`compute_tick`] using the live in-flight count.
///
/// Phase 6: a subtask that is **blocked-on-gate** ([`gate_blocked`]) or still
/// **needs a gate registered** ([`needs_gate_registration`]) is NOT dispatchable
/// — its observable external pre-condition is unmet (or its gate isn't yet
/// registered/polled). A subtask whose gate has CLEARED falls through here as
/// dispatchable (it's `Submitted`, deps satisfied, and no longer gate-blocked).
pub fn ready_subtasks<'a>(subtasks: &'a [Subtask], order: &[usize]) -> Vec<&'a Subtask> {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    order
        .iter()
        .map(|&i| &subtasks[i])
        .filter(|s| {
            s.state == SubtaskState::Submitted
                && deps_satisfied(s, &by_id)
                && !gate_blocked(s)
                && !gate_failed(s)
                && !needs_gate_registration(s)
        })
        .collect()
}

/// Subtasks (deps satisfied) that need a gate registered this tick — the
/// reconciler registers a coord gate for each, then holds it blocked. In topo
/// order so a deterministic, dependency-respecting registration order.
pub fn subtasks_needing_gate<'a>(subtasks: &'a [Subtask], order: &[usize]) -> Vec<&'a Subtask> {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    order
        .iter()
        .map(|&i| &subtasks[i])
        .filter(|s| deps_satisfied(s, &by_id) && needs_gate_registration(s))
        .collect()
}

/// Subtasks (deps satisfied) that are blocked on an OPEN gate — the reconciler
/// re-polls each this tick (resume / re-attach path). In topo order.
pub fn gate_blocked_subtasks<'a>(subtasks: &'a [Subtask], order: &[usize]) -> Vec<&'a Subtask> {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    order
        .iter()
        .map(|&i| &subtasks[i])
        .filter(|s| deps_satisfied(s, &by_id) && gate_blocked(s))
        .collect()
}

/// Subtasks (deps satisfied) whose gate FAILED — the reconciler fails each this
/// tick. (`gate_blocked` excludes failed gates, so they get their own collector.)
/// In topo order.
pub fn gate_failed_subtasks<'a>(subtasks: &'a [Subtask], order: &[usize]) -> Vec<&'a Subtask> {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    order
        .iter()
        .map(|&i| &subtasks[i])
        .filter(|s| deps_satisfied(s, &by_id) && gate_failed(s))
        .collect()
}

/// In-flight subtasks: `Working` with a bound `task_run_id`.
pub fn in_flight(subtasks: &[Subtask]) -> Vec<&Subtask> {
    subtasks
        .iter()
        .filter(|s| s.state == SubtaskState::Working && s.task_run_id.is_some())
        .collect()
}

/// Elaborator rows (`emits_subtasks == true`) that are `Completed` but whose
/// children have NOT yet been spliced in. **Phase-3 definition** of
/// "un-harvested" (documented for Phase 4 to wire cleanly): a Completed
/// elaborator E is un-harvested iff NO subtask in the run carries
/// `produced_by == Some(E.task_id)`. Phase 4's [`harvest_elaboration`] splices
/// children and so flips this predicate to false; until then a Completed
/// elaborator with children declared in its artifact would read as
/// un-harvested. In Phase 3 the harvest hook is a no-op, so to avoid wedging
/// the exit on a design-only DAG we additionally treat an elaborator as
/// "harvested" once it is terminal AND the hook has run for it (see
/// [`compute_tick`] / [`ExitState`]). The pure predicate here is the raw
/// "children present?" check Phase 4 owns.
pub fn unharvested_elaborators(subtasks: &[Subtask]) -> Vec<&Subtask> {
    let has_children: HashSet<&str> = subtasks
        .iter()
        .filter_map(|s| s.produced_by.as_deref())
        .collect();
    subtasks
        .iter()
        .filter(|s| {
            s.emits_subtasks
                && s.state == SubtaskState::Completed
                && !has_children.contains(s.task_id.as_str())
        })
        .collect()
}

// ============================================================================
// Signal source (injected — real = SessionManager FSM, test = scripted)
// ============================================================================

/// Reads a worker's current [`WorkerSignal`] by `task_run_id`. The live impl
/// reads the `SessionManager` FSM via the Phase-2
/// [`ai_session_executor::worker_terminal_state`]; tests inject a scripted map.
pub trait SignalSource {
    fn signal(&self, task_run_id: Uuid) -> WorkerSignal;
}

/// Live signal source backed by the in-process `SessionManager`.
pub struct ManagerSignalSource {
    pub session_mgr: Arc<crate::claude_session::manager::SessionManager>,
}

impl SignalSource for ManagerSignalSource {
    fn signal(&self, task_run_id: Uuid) -> WorkerSignal {
        ai_session_executor::worker_terminal_state(&self.session_mgr, task_run_id)
    }
}

// ============================================================================
// Dispatcher (injected — real = spawn a worker, test = record-only)
// ============================================================================

/// Dispatches a ready subtask to a worker. The live impl
/// ([`AiSessionDispatcher`]) calls Phase-2
/// [`ai_session_executor::dispatch_subtask`] (spawns the AI-session, persists
/// `task_run_id` + flips to `Working`). Tests inject a fake that records the
/// dispatch and persists the transition itself, so readiness/cap/resume logic
/// is exercised WITHOUT a live spawn.
#[async_trait]
pub trait Dispatcher: Send + Sync {
    async fn dispatch(&self, run_id: Uuid, subtask: &Subtask) -> Result<Uuid, String>;

    /// Re-prompt an in-flight worker (step-5 recovery). The live impl POSTs to
    /// the runner's `submit-prompt` path; tests record the call.
    async fn reprompt(&self, task_run_id: Uuid, message: &str) -> Result<(), String>;
}

/// Live dispatcher: spawns workers via Phase-2 dispatch and re-prompts via the
/// in-process terminal/session submit path.
pub struct AiSessionDispatcher {
    pub app_handle: tauri::AppHandle,
    pub pg: Arc<PgDb>,
}

#[async_trait]
impl Dispatcher for AiSessionDispatcher {
    async fn dispatch(&self, run_id: Uuid, subtask: &Subtask) -> Result<Uuid, String> {
        ai_session_executor::dispatch_subtask(&self.app_handle, &self.pg, run_id, subtask).await
    }

    async fn reprompt(&self, task_run_id: Uuid, message: &str) -> Result<(), String> {
        // Workers are interactive ClaudeSessions registered in the
        // SessionManager keyed by task_run_id; re-prompt by submitting a user
        // message on that session (the same funnel `submit_prompt_handler`
        // uses for terminals — here directly on the session). Best-effort.
        use tauri::Manager;
        let mgr = self
            .app_handle
            .try_state::<Arc<crate::claude_session::manager::SessionManager>>()
            .ok_or_else(|| "reprompt: SessionManager not available".to_string())?
            .inner()
            .clone();
        let session = mgr
            .get(&task_run_id.to_string())
            .ok_or_else(|| format!("reprompt: no session for {task_run_id}"))?;
        session
            .send_user_message(message)
            .map(|_| ())
            .map_err(|e| format!("reprompt: send_user_message: {e}"))
    }
}

// ============================================================================
// Tick plan (the pure DECISIONS — tests assert on this)
// ============================================================================

/// One reconciler tick's decisions, derived purely from the durable rows +
/// injected signals. Applying it ([`apply_tick`]) is what produces side
/// effects; computing it ([`compute_tick`]) is pure and unit-testable.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickPlan {
    /// `task_id`s to dispatch this tick (already capped by concurrency).
    pub to_dispatch: Vec<String>,
    /// `task_id`s to flip `Working → Completed` (the §5 guard held).
    pub to_complete: Vec<String>,
    /// `task_id`s to flip `Working → Failed` (errored / gone-past-budget /
    /// no-artifact-past-recovery).
    pub to_fail: Vec<String>,
    /// `(task_id, task_run_id)` of workers to re-prompt once (step-5 recovery).
    pub to_reprompt: Vec<(String, Uuid)>,
    /// Completed elaborators whose harvest hook should fire this tick.
    pub to_harvest: Vec<String>,
    /// Phase 6: `task_id`s whose `expected_output` classifies to an observable
    /// external condition and which have NO gate yet — `apply_tick` registers a
    /// coord gate for each (and persists `gate_id` + `gate_status=open`),
    /// holding the subtask blocked.
    pub to_register_gate: Vec<String>,
    /// Phase 6: `task_id`s with a registered, not-yet-cleared gate — `apply_tick`
    /// re-polls each and persists the new `gate_status` (cleared ⇒ unblocks next
    /// tick; failed ⇒ fails next tick). This is also the restart re-attach path.
    pub to_poll_gate: Vec<String>,
    /// Phase 6: `task_id`s of completed DriftVerdict **verify** subtasks whose
    /// verdict `apply_tick` must read — no-drift ⇒ complete; drift ⇒ splice a
    /// remediation subtask + store the verdict (the verify stays not-Completed
    /// until remediation + re-verify resolve).
    pub to_verify_drift: Vec<String>,
    /// `true` when the run is finished (all terminal + nothing un-harvested).
    pub done: bool,
    /// Set when a hard error (e.g. DAG cycle) should terminate the run.
    pub fatal: Option<String>,
    /// The fingerprint of "stuck-relevant" state for stall detection (excludes
    /// legitimately-blocked/elaborating rows; contract §3.5).
    pub stall_fingerprint: String,
}

/// Per-`task_run_id` timing the live loop tracks across ticks for the §5
/// recovery deadlines. This is the ONLY cross-tick memory the conductor keeps,
/// and it is purely an OPTIMIZATION / debounce for recovery timers — it is NOT
/// task state: it is rebuilt-from-empty on restart and the worst case of
/// losing it is that a recovery timer restarts (the ledger remains the single
/// source of truth for what has been dispatched/completed). Statelessness of
/// the DAG drive itself is preserved.
#[derive(Debug, Clone, Default)]
pub struct ReadyIdleTimers {
    /// First tick we observed this worker `ReadyIdle`-without-artifact (epoch s).
    first_ready_no_artifact_at: HashMap<Uuid, i64>,
    /// Tick we re-prompted this worker (epoch s); `Some` ⇒ already reprompted.
    reprompted_at: HashMap<Uuid, i64>,
    /// First tick we observed this worker `Gone` (epoch s).
    first_gone_at: HashMap<Uuid, i64>,
}

impl ReadyIdleTimers {
    /// Drop timers for task_run_ids no longer in-flight (housekeeping).
    fn retain_in_flight(&mut self, in_flight_ids: &HashSet<Uuid>) {
        self.first_ready_no_artifact_at
            .retain(|k, _| in_flight_ids.contains(k));
        self.reprompted_at.retain(|k, _| in_flight_ids.contains(k));
        self.first_gone_at.retain(|k, _| in_flight_ids.contains(k));
    }
}

/// Compute one tick's [`TickPlan`] purely from the persisted rows, an injected
/// signal source, the recovery timers, and config. No DB writes, no spawns.
///
/// `now` is passed in (epoch seconds) so tests are deterministic.
pub fn compute_tick<S: SignalSource>(
    subtasks: &[Subtask],
    signals: &S,
    timers: &mut ReadyIdleTimers,
    config: &OrchestrationRunConfig,
    now: i64,
) -> TickPlan {
    let mut plan = TickPlan::default();

    // observe + order (step 1+2). A cycle is fatal — surface it.
    let order = match topo_order(subtasks) {
        Ok(o) => o,
        Err(e) => {
            plan.fatal = Some(e);
            return plan;
        }
    };

    // Reconcile in-flight workers FIRST (step 4+5) so completions free up
    // concurrency slots used by the dispatch decision below.
    let inflight = in_flight(subtasks);
    let inflight_ids: HashSet<Uuid> = inflight.iter().filter_map(|s| s.task_run_id).collect();
    timers.retain_in_flight(&inflight_ids);

    let mut still_inflight = inflight.len();
    for st in &inflight {
        let trid = match st.task_run_id {
            Some(id) => id,
            None => continue,
        };
        let sig = signals.signal(trid);

        // §5 guard — complete ONLY when ReadyIdle AND artifact present.
        if can_complete(st, sig) {
            // Phase 6: a DriftVerdict VERIFY subtask does not complete blindly —
            // its job is to surface the Digital-Twin verdict. Route it to the
            // verify path (`apply_tick` reads the verdict and either completes it
            // [no drift] or splices a remediation subtask [drift]). Once the
            // verdict is stored in its artifact AND the verdict is no-drift, a
            // later tick's `can_complete` still holds and it completes normally —
            // so we only divert a verify subtask whose verdict has NOT yet been
            // recorded (no `drift_verdict` artifact key).
            if is_drift_verify(st) && !drift_verdict_recorded(st) {
                plan.to_verify_drift.push(st.task_id.clone());
                // Still in-flight for slot accounting until the verify resolves.
                continue;
            }
            plan.to_complete.push(st.task_id.clone());
            still_inflight -= 1;
            continue;
        }

        match sig {
            WorkerSignal::ReadyIdle => {
                // Ready but NO artifact (can_complete was false ⇒ artifact
                // absent). Step-5 recovery: re-prompt once after the first
                // timeout, fail after the second.
                let first = *timers.first_ready_no_artifact_at.entry(trid).or_insert(now);
                match timers.reprompted_at.get(&trid).copied() {
                    None => {
                        if now - first >= config.report_timeout_secs {
                            plan.to_reprompt.push((st.task_id.clone(), trid));
                        }
                    }
                    Some(reprompted) => {
                        if now - reprompted >= config.report_reprompt_grace_secs {
                            plan.to_fail.push(st.task_id.clone());
                            still_inflight -= 1;
                        }
                    }
                }
            }
            WorkerSignal::Working => {
                // Active mid-turn — clear any stale ready/gone timers.
                timers.first_ready_no_artifact_at.remove(&trid);
                timers.first_gone_at.remove(&trid);
            }
            WorkerSignal::Errored => {
                plan.to_fail.push(st.task_id.clone());
                still_inflight -= 1;
            }
            WorkerSignal::Gone => {
                // Tolerate a transient restart window, then fail.
                let first = *timers.first_gone_at.entry(trid).or_insert(now);
                if now - first >= config.gone_grace_secs {
                    plan.to_fail.push(st.task_id.clone());
                    still_inflight -= 1;
                }
            }
        }
    }

    // Phase 6 — coord gate decisions (BEFORE dispatch, so a gate-bound subtask
    // never enters `ready_subtasks` until its gate clears):
    //   - a deps-satisfied Submitted row whose `expected_output` classifies to an
    //     observable external condition but has NO gate yet → register a gate
    //     (held blocked; `apply_tick` persists gate_id + status=open).
    //   - a deps-satisfied Submitted row with a registered gate whose status is
    //     `failed` → fail the subtask (its pre-condition can never be met).
    //   - a deps-satisfied Submitted row blocked on an OPEN gate → re-poll it
    //     (this is also the restart re-attach path: gate_id is durable, so a
    //     fresh process re-reads it here and resumes polling, never re-registers).
    // `ready_subtasks` already EXCLUDES gate-blocked + needs-registration rows,
    // so the dispatch step below cannot dispatch a gate-bound subtask until its
    // gate clears.
    for st in subtasks_needing_gate(subtasks, &order) {
        plan.to_register_gate.push(st.task_id.clone());
    }
    for st in gate_failed_subtasks(subtasks, &order) {
        plan.to_fail.push(st.task_id.clone());
    }
    for st in gate_blocked_subtasks(subtasks, &order) {
        plan.to_poll_gate.push(st.task_id.clone());
    }

    // Dispatch (step 3) — ready subtasks in topo order, capped by remaining
    // concurrency. `still_inflight` already reflects completions/failures
    // decided above so a tick can both complete and dispatch.
    let slots = config.concurrency_cap.saturating_sub(still_inflight);
    if slots > 0 {
        for st in ready_subtasks(subtasks, &order).into_iter().take(slots) {
            plan.to_dispatch.push(st.task_id.clone());
        }
    }

    // Harvest hook (step 6). Mark Completed-but-unharvested elaborators so the
    // live loop calls `harvest_elaboration` for them (resume / safety-net
    // path; in steady state the completion path harvests BEFORE flipping).
    let unharvested = unharvested_elaborators(subtasks);
    for e in &unharvested {
        plan.to_harvest.push(e.task_id.clone());
    }

    // Exit (step 7 — contract §3.5 termination guard). The run is done iff:
    //   (a) every subtask is terminal, AND
    //   (b) NO `emits_subtasks` row is still un-harvested (a Completed
    //       elaborator whose seeds have not yet been spliced).
    // (b) is load-bearing: the plan can still GROW while an elaborator's seeds
    // are un-harvested, so declaring done then would lose the generated work.
    // A non-terminal elaborator is already caught by (a). Once its seeds are
    // spliced they appear as `produced_by` children — themselves subtasks that
    // must be terminal for (a) — so the predicate naturally tightens as the
    // DAG grows. Note we use the PERSISTED rows here: this tick may QUEUE a
    // harvest, but `done` only reads committed state, so the run reads done on
    // a LATER tick once the splice has landed and all children are terminal.
    //
    // Phase 6: a gate-blocked subtask is `Submitted` (non-terminal) and a verify
    // subtask awaiting its DriftVerdict is `Working` (non-terminal), so BOTH are
    // already caught by (a) — the run is NEVER `done` while a gate is open or a
    // verify is pending. No extra term needed; the gate/verify block lives in
    // non-terminal lifecycle states by construction.
    plan.done = all_terminal(subtasks) && unharvested.is_empty();

    // Stall fingerprint (step 3.5) — EXCLUDE legitimately-blocked rows so
    // "waiting on a dependency / elaboration" never reads as a stall. Include
    // only: in-flight worker identities + states of rows that are actually
    // actionable (Submitted-with-deps-satisfied or Working). A Submitted row
    // whose deps are unmet is legitimately blocked → excluded.
    plan.stall_fingerprint = stall_fingerprint(subtasks, &order);

    plan
}

/// Build the stall fingerprint: a stable string over the ACTIONABLE state
/// only. Legitimately-blocked subtasks (Submitted with unmet deps) and
/// terminal subtasks are excluded so a run that is correctly waiting never
/// trips the stall detector (contract §3.5).
fn stall_fingerprint(subtasks: &[Subtask], order: &[usize]) -> String {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    let mut parts: Vec<String> = Vec::new();
    for &i in order {
        let s = &subtasks[i];
        match s.state {
            // In-flight: include identity so a worker making progress (which
            // would not change the fingerprint by itself, but completions do)
            // is represented; the stall detector fires only on REPEATED
            // identical fingerprints across the no-progress window.
            SubtaskState::Working => {
                parts.push(format!(
                    "W:{}:{}",
                    s.task_id,
                    s.task_run_id.map(|i| i.to_string()).unwrap_or_default()
                ));
            }
            // Ready-to-dispatch: actionable. Phase 6: a subtask blocked on an
            // OPEN gate, or one awaiting gate registration, is NOT actionable —
            // it is legitimately waiting on an external condition (the same shape
            // as elaboration-waiting), so it is EXCLUDED here. This is what makes
            // a gate-wait-only tick produce an unchanged/empty actionable
            // fingerprint, so pure gate-waiting can never accumulate a stall
            // (contract §3.5 stall-exclusion, extended to gates).
            SubtaskState::Submitted
                if deps_satisfied(s, &by_id)
                    && !gate_blocked(s)
                    && !gate_failed(s)
                    && !needs_gate_registration(s) =>
            {
                parts.push(format!("R:{}", s.task_id));
            }
            // Submitted-but-blocked (deps OR gate), terminal, or reserved: excluded.
            _ => {}
        }
    }
    parts.sort();
    parts.join("|")
}

// ============================================================================
// Harvest hook — Phase 4 extension point (Phase 3: no-op)
// ============================================================================

/// **Phase-4 — harvest a completed elaborator's emitted child org-chart.**
///
/// Given a Completed elaborator subtask (`emits_subtasks == true`) and the full
/// current row set, this:
/// 1. reads the elaborator's `artifact.artifacts["orchestration"].next_subtasks`
///    (gracefully treating absent/empty/null as "harvest 0 — valid"),
/// 2. materializes each seed into a child [`Subtask`] with
///    `produced_by = Some(elaborator.task_id)`, the parent appended to its
///    `depends_on`, and `idx` after the current max, and
/// 3. idempotently inserts them via the SHARED [`splice_org_chart`] path
///    (INSERT-IF-NOT-EXISTS, so re-reading the same artifact across ticks is a
///    no-op).
///
/// Returns the number of NEW rows actually inserted. Once the splice lands,
/// [`unharvested_elaborators`]'s predicate flips to false for this elaborator
/// (its children now carry `produced_by == elaborator.task_id`).
///
/// An elaborator with NO readable artifact yet cannot be harvested — returns
/// `Ok(0)` so the completion path below leaves it `Working` for a later tick.
/// A PRESENT-but-malformed `next_subtasks` returns `Err` (contract §8 Q1).
pub async fn harvest_elaboration(
    pg: &Arc<PgDb>,
    run_id: Uuid,
    elaborator: &Subtask,
) -> Result<u64, String> {
    let Some(artifact) = elaborator.artifact.as_ref() else {
        // No artifact to harvest from yet. Not an error — the completion guard
        // (`can_complete`) gates completion on the artifact being present, so a
        // harvest call without an artifact simply yields zero children.
        return Ok(0);
    };

    let seeds = super::org_chart::parse_next_subtasks(artifact)?;
    if seeds.is_empty() {
        info!(
            "harvest_elaboration: run={} elaborator={} carried no next_subtasks (0 harvested)",
            run_id, elaborator.task_id
        );
        return Ok(0);
    }

    let inserted =
        super::org_chart::splice_org_chart(pg, run_id, &seeds, Some(elaborator.task_id.as_str()))
            .await?;
    info!(
        "harvest_elaboration: run={} elaborator={} spliced {} new child subtask(s)",
        run_id, elaborator.task_id, inserted
    );
    Ok(inserted)
}

// ============================================================================
// Apply a tick (side effects) + the live background loop
// ============================================================================

/// Apply a computed [`TickPlan`] against the DB + dispatcher (side effects).
/// Returns `Ok(true)` when the run is done.
#[allow(clippy::too_many_arguments)]
async fn apply_tick<D: Dispatcher, G: CoordGateClient>(
    plan: &TickPlan,
    subtasks: &[Subtask],
    run_id: Uuid,
    pg: &Arc<PgDb>,
    dispatcher: &D,
    gate_client: &G,
    config: &OrchestrationRunConfig,
    timers: &mut ReadyIdleTimers,
) -> Result<bool, String> {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();

    // Completions (§5 guard already held in compute_tick).
    //
    // CRASH-SAFE ELABORATOR ORDERING (contract §3.5): an `emits_subtasks`
    // subtask flips `Working → Completed` ONLY AFTER its seeds are durably
    // spliced. We harvest FIRST and set Completed ONLY on harvest success; on
    // harvest failure the row is left `Working` so a later tick retries —
    // never marked Completed with un-spliced seeds. Because the splice is
    // insert-if-not-exists, a crash between the splice and the state flip is
    // safe: the next tick re-reads the same artifact, the splice is a no-op,
    // and the flip is retried. Non-elaborating subtasks complete directly.
    for tid in &plan.to_complete {
        let is_elaborator = by_id
            .get(tid.as_str())
            .map(|s| s.emits_subtasks)
            .unwrap_or(false);

        if is_elaborator {
            let Some(st) = by_id.get(tid.as_str()) else {
                continue;
            };
            match harvest_elaboration(pg, run_id, st).await {
                Ok(n) => {
                    info!(
                        "conductor: harvested {n} seed(s) from elaborator {tid} before completing (run {run_id})"
                    );
                }
                Err(e) => {
                    // Leave it Working — retry next tick. NEVER complete with
                    // un-spliced seeds.
                    warn!(
                        "apply_tick: harvest {tid} failed ({e}); leaving Working, will retry next tick"
                    );
                    continue;
                }
            }
        }

        if let Err(e) = pg
            .set_subtask_state(run_id, tid, SubtaskState::Completed)
            .await
        {
            warn!("apply_tick: complete {tid}: {e}");
        } else {
            info!("conductor: {tid} Working → Completed (run {run_id})");
        }
    }

    // Failures.
    for tid in &plan.to_fail {
        if let Err(e) = pg
            .set_subtask_state(run_id, tid, SubtaskState::Failed)
            .await
        {
            warn!("apply_tick: fail {tid}: {e}");
        } else {
            warn!("conductor: {tid} Working → Failed (run {run_id})");
        }
    }

    // Re-prompts (step-5 recovery; record the timer so we only do it once).
    let reprompt_msg = "You appear to have stopped without reporting. \
        Emit your completion report NOW via the orchestration_report_subtask \
        tool (run_id + task_id + a CompletionReport), then print [TASK_COMPLETE].";
    for (tid, trid) in &plan.to_reprompt {
        match dispatcher.reprompt(*trid, reprompt_msg).await {
            Ok(()) => {
                info!("conductor: re-prompted {tid} ({trid}) for missing report");
                timers.reprompted_at.insert(*trid, Utc::now().timestamp());
            }
            Err(e) => warn!("apply_tick: reprompt {tid}: {e}"),
        }
    }

    // Dispatches (step 3).
    for tid in &plan.to_dispatch {
        if let Some(st) = by_id.get(tid.as_str()) {
            match dispatcher.dispatch(run_id, st).await {
                Ok(trid) => info!("conductor: dispatched {tid} → worker {trid} (run {run_id})"),
                Err(e) => {
                    // Dispatch failed before the worker was live; the subtask
                    // stays Submitted (dispatch_subtask only flips on success)
                    // so a later tick retries. Log loudly.
                    warn!("apply_tick: dispatch {tid}: {e}");
                }
            }
        }
    }

    // Harvest already-Completed-but-unharvested elaborators (resume / safety
    // net). The completion path above splices BEFORE flipping an elaborator to
    // Completed, so in steady state this list is empty. It still fires for an
    // elaborator that became Completed in a PRIOR process (a crash after the
    // flip but before — under the old ordering — the splice, or a row migrated
    // in already-Completed): the stateless reconciler re-derives it as
    // `unharvested` and harvests it here. Idempotent (insert-if-not-exists).
    for tid in &plan.to_harvest {
        if let Some(st) = by_id.get(tid.as_str()) {
            match harvest_elaboration(pg, run_id, st).await {
                Ok(n) if n > 0 => {
                    info!("conductor: late-harvested {n} seed(s) from completed elaborator {tid} (run {run_id})")
                }
                Ok(_) => {}
                Err(e) => warn!("apply_tick: harvest {tid}: {e}"),
            }
        }
    }

    // ---- Phase 6: coord gate registration (best-effort) -----------------
    // Register a gate for each classified subtask and persist gate_id +
    // status=open. The subtask STAYS `Submitted` (no new state variant) — the
    // gate_id column annotates it blocked. On a register failure we leave the
    // row ungated so the NEXT tick retries registration (never wedges; coord may
    // be transiently unreachable).
    for tid in &plan.to_register_gate {
        let Some(st) = by_id.get(tid.as_str()) else {
            continue;
        };
        let Some(predicate) = classify_gate(&st.expected_output, st.repo.as_deref()) else {
            continue;
        };
        match gate_client.register_gate(run_id, tid, &predicate).await {
            Ok(gate_id) => {
                if let Err(e) = pg
                    .set_subtask_gate(run_id, tid, Some(&gate_id), Some("open"))
                    .await
                {
                    warn!("apply_tick: persist gate for {tid}: {e}");
                } else {
                    info!(
                        "conductor: {tid} blocked on {} gate {gate_id} (run {run_id})",
                        predicate.kind_label()
                    );
                }
            }
            Err(e) => warn!(
                "apply_tick: register {} gate for {tid} failed ({e}); will retry next tick",
                predicate.kind_label()
            ),
        }
    }

    // ---- Phase 6: poll open gates (re-attach + clearance) ----------------
    // Poll each blocked-on-gate subtask and persist the new status. `cleared`
    // unblocks dispatch on the NEXT tick (the column is the durable record);
    // `failed` fails the subtask on the next tick (`gate_failed`). A poll that
    // can't reach coord returns `Unknown` → persisted as `open` → keep waiting.
    for tid in &plan.to_poll_gate {
        let Some(st) = by_id.get(tid.as_str()) else {
            continue;
        };
        let Some(gate_id) = st.gate_id.as_deref() else {
            continue;
        };
        match gate_client.poll_gate(gate_id).await {
            Ok(status) => {
                let col = status.as_column();
                // Only write when the status changed, to avoid needless updates.
                if st.gate_status.as_deref() != Some(col) {
                    if let Err(e) = pg
                        .set_subtask_gate(run_id, tid, Some(gate_id), Some(col))
                        .await
                    {
                        warn!("apply_tick: persist gate status for {tid}: {e}");
                    } else if matches!(status, GateStatus::Cleared) {
                        info!("conductor: gate {gate_id} CLEARED — {tid} now dispatchable (run {run_id})");
                    } else if matches!(status, GateStatus::Failed) {
                        warn!("conductor: gate {gate_id} FAILED — {tid} will be failed (run {run_id})");
                    }
                }
            }
            Err(e) => warn!("apply_tick: poll gate {gate_id} for {tid}: {e} (keeping open)"),
        }
    }

    // ---- Phase 6: DriftVerdict verify (read → complete | remediate) ------
    // A verify subtask is ReadyIdle+artifact (its worker run finished) but its
    // verdict is not yet recorded. Read the Digital-Twin DriftVerdict, store it
    // at artifact.artifacts["drift_verdict"], then:
    //   - NO DRIFT → complete the verify subtask.
    //   - DRIFT    → splice a remediation subtask depending on the verify (and a
    //     re-verify that depends on the remediation) and store the verdict; the
    //     verify subtask is left non-Completed so the run continues into
    //     remediation. (Chosen over a bare Fail: remediation is more powerful —
    //     it gives the run a path to converge rather than dead-ending.)
    for tid in &plan.to_verify_drift {
        let Some(st) = by_id.get(tid.as_str()) else {
            continue;
        };
        let subspace = drift_subspace_for(st);
        match gate_client.drift_verdict(&subspace).await {
            Ok(DriftClass::NoDrift) => {
                if let Err(e) = store_drift_verdict(pg, run_id, st, &subspace, "none").await {
                    warn!("apply_tick: store no-drift verdict for {tid}: {e}");
                    continue;
                }
                if let Err(e) = pg
                    .set_subtask_state(run_id, tid, SubtaskState::Completed)
                    .await
                {
                    warn!("apply_tick: complete verify {tid}: {e}");
                } else {
                    info!("conductor: verify {tid} — DriftVerdict[{subspace}] shows NO DRIFT → Completed (run {run_id})");
                }
            }
            Ok(DriftClass::Drift(class)) => {
                if let Err(e) = store_drift_verdict(pg, run_id, st, &subspace, &class).await {
                    warn!("apply_tick: store drift verdict for {tid}: {e}");
                    continue;
                }
                match splice_drift_remediation(pg, run_id, st, &subspace, &class).await {
                    Ok(n) => info!(
                        "conductor: verify {tid} — DriftVerdict[{subspace}] DRIFT={class} → spliced {n} remediation row(s) (run {run_id})"
                    ),
                    Err(e) => warn!("apply_tick: splice remediation for {tid}: {e}"),
                }
            }
            Err(e) => {
                // Best-effort: a verdict read failure leaves the verify subtask
                // ReadyIdle+artifact (not recorded) so a later tick retries the
                // read. Never wedges, never fabricates a verdict.
                warn!("apply_tick: drift verdict[{subspace}] for {tid}: {e} (will retry)");
            }
        }
    }

    let _ = config;
    Ok(plan.done)
}

/// The twin sub-space a verify subtask reads. Parsed from a `subspace=<id>` /
/// `twin=<id>` token in the `expected_output` when present, else the default
/// fleet-wide [`DEFAULT_DRIFT_SUBSPACE`] (`health`).
fn drift_subspace_for(subtask: &Subtask) -> String {
    for key in ["subspace", "twin", "sub_space"] {
        for tok in subtask.expected_output.split_whitespace() {
            let kv = tok.split_once('=').or_else(|| tok.split_once(':'));
            if let Some((k, v)) = kv {
                if k.eq_ignore_ascii_case(key) && !v.is_empty() {
                    return v
                        .trim_matches(|c: char| c == '"' || c == '\'' || c == ',')
                        .to_string();
                }
            }
        }
    }
    DEFAULT_DRIFT_SUBSPACE.to_string()
}

/// Store a read DriftVerdict on a verify subtask's artifact under
/// `artifact.artifacts["drift_verdict"]` (contract §4), preserving any existing
/// artifact body. Persists via `write_subtask_artifact` (the same JSONB column).
async fn store_drift_verdict(
    pg: &Arc<PgDb>,
    run_id: Uuid,
    subtask: &Subtask,
    subspace: &str,
    drift_class: &str,
) -> Result<(), String> {
    let mut report = subtask
        .artifact
        .clone()
        .unwrap_or_else(default_verify_report);
    let verdict = serde_json::json!({
        "subspace": subspace,
        "drift_class": drift_class,
        "no_drift": drift_class.eq_ignore_ascii_case("none"),
    });
    report
        .artifacts
        .insert(DRIFT_VERDICT_ARTIFACT_KEY.to_string(), verdict);
    pg.write_subtask_artifact(run_id, &subtask.task_id, &report)
        .await
}

/// A minimal `CompletionReport` for a verify subtask that completed its worker
/// run but somehow lost its artifact (defensive — the §5 guard usually means
/// `artifact` is `Some`, but a verdict store must never panic on `None`).
fn default_verify_report() -> crate::database::pg::completion_reports::CompletionReport {
    crate::database::pg::completion_reports::CompletionReport {
        summary_md: "Digital-Twin DriftVerdict verify".to_string(),
        deliverables: vec![],
        breaking_changes: vec![],
        follow_ups: vec![],
        artifacts: std::collections::HashMap::new(),
    }
}

/// On a DRIFT verdict, splice (idempotently) a remediation subtask + a re-verify
/// subtask via the SHARED `splice_org_chart` path with `produced_by = verify
/// task_id` (so the splice is the same monotonic-growth operation Phase 4 uses).
///
/// - `remediate-<verify>` depends on the verify subtask: it does the corrective
///   work for the observed drift class.
/// - `reverify-<verify>` depends on the remediation: it re-reads the DriftVerdict
///   for the same sub-space (its `expected_output` is a fresh drift-verify
///   string), closing the loop. Idempotent: a re-tick re-splices 0 (the
///   `(run_id, task_id)` rows already exist).
async fn splice_drift_remediation(
    pg: &Arc<PgDb>,
    run_id: Uuid,
    verify: &Subtask,
    subspace: &str,
    drift_class: &str,
) -> Result<u64, String> {
    let remediate_id = format!("remediate-{}", verify.task_id);
    let reverify_id = format!("reverify-{}", verify.task_id);
    let seeds = vec![
        OrgChartSeed {
            id: remediate_id.clone(),
            title: format!("Remediate {subspace} drift ({drift_class})"),
            brief: format!(
                "The Digital-Twin DriftVerdict for sub-space `{subspace}` reported drift class \
                 `{drift_class}` after verify `{}`. Investigate and remediate the declared-vs-actual \
                 drift so the next DriftVerdict reads `none` (no drift).",
                verify.task_id
            ),
            phase: verify.phase.clone(),
            // `splice_org_chart` appends the producing parent (the verify) as a
            // dependency automatically (produced_by wiring), so the remediation
            // depends on the verify subtask.
            depends_on: vec![],
            expected_output: format!(
                "the {subspace} drift is remediated (the corrective change is applied)"
            ),
            repo: verify.repo.clone(),
            emits_subtasks: false,
        },
        OrgChartSeed {
            id: reverify_id,
            title: format!("Re-verify {subspace} DriftVerdict"),
            brief: format!(
                "Re-read the Digital-Twin DriftVerdict for sub-space `{subspace}` after the \
                 remediation and confirm it now shows no drift."
            ),
            phase: verify.phase.clone(),
            // Depends on the remediation (a cross-edge the seed declares). The
            // verify-parent dep is added by `splice_org_chart` on top.
            depends_on: vec![remediate_id],
            expected_output: format!(
                "DriftVerdict shows no drift subspace={subspace}"
            ),
            repo: verify.repo.clone(),
            emits_subtasks: false,
        },
    ];
    super::org_chart::splice_org_chart(pg, run_id, &seeds, Some(verify.task_id.as_str())).await
}

/// The live conductor background task. Stateless over the ledger: each tick
/// reloads ALL rows and re-derives everything (statelessness / resume).
///
/// Drives the run to terminal state, updating the [`SharedLoopState`] phase for
/// the status surface and respecting the stop signal.
///
/// ## Phase 6 restart re-attach (gate durability)
///
/// The conductor holds NO gate state in memory. A subtask's gate association is
/// the durable `gate_id` + `gate_status` columns; on EVERY tick the observe step
/// (`list_subtasks`) re-reads them, and `compute_tick`'s [`gate_blocked_subtasks`]
/// re-derives every `Submitted` row with a non-null `gate_id` whose status is not
/// `cleared`/`failed` as blocked-on-gate and re-polls it (`to_poll_gate`). So a
/// kill+restart mid-wait resumes polling the SAME coord gate without
/// re-registering it (the gate row is the single source of truth) — exactly the
/// statelessness invariant Phase 3 established, extended to gates.
#[allow(clippy::too_many_arguments)]
pub async fn run_orchestration<D: Dispatcher, S: SignalSource, G: CoordGateClient>(
    loop_state: SharedLoopState,
    run_id: Uuid,
    pg: Arc<PgDb>,
    dispatcher: D,
    signals: S,
    gate_client: G,
    config: OrchestrationRunConfig,
    mut stop_rx: watch::Receiver<bool>,
) {
    info!(
        "conductor: run {run_id} starting (cap={}, tick={}s)",
        config.concurrency_cap, config.tick_interval_secs
    );

    // Stall detector — fires only on the conductor's OWN no-progress
    // fingerprint (actionable state), never on legitimate waiting.
    let stall_cfg = StallDetectorConfig::default();
    let mut stall_detector = StallDetector::new(stall_cfg);

    // The ONLY cross-tick memory (recovery debounce timers; rebuilt on restart).
    let mut timers = ReadyIdleTimers::default();
    let mut tick: u32 = 0;

    loop {
        if *stop_rx.borrow() {
            info!("conductor: run {run_id} stopped by signal");
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::Stopped;
            st.running = false;
            return;
        }
        tick += 1;

        // observe — reload ALL rows (no in-memory carry-over).
        {
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::Reconciling;
            st.current_iteration = tick;
        }
        let subtasks = match pg.list_subtasks(run_id).await {
            Ok(rows) => rows,
            Err(e) => {
                error!("conductor: run {run_id} list_subtasks failed: {e}");
                sleep_or_stop(&config, &mut stop_rx).await;
                continue;
            }
        };

        let now = Utc::now().timestamp();
        let plan = compute_tick(&subtasks, &signals, &mut timers, &config, now);

        if let Some(fatal) = &plan.fatal {
            error!("conductor: run {run_id} fatal: {fatal}");
            let mut st = loop_state.lock().await;
            st.error = Some(fatal.clone());
            st.phase = LoopPhase::Error;
            st.running = false;
            return;
        }

        // Stall check over the ACTIONABLE-only fingerprint (§3.5). A tick whose
        // only "no progress" is waiting on blocked/elaborating rows produces an
        // EMPTY/unchanged actionable fingerprint AND no dispatch/complete — but
        // we only record an action when there IS actionable state, so pure
        // waiting cannot accumulate a stall.
        // Phase 6: gate registration / polling / verify reads are PROGRESS — a
        // tick that registers a gate, observes a gate transition, or reads a
        // verdict is doing real work, so it must NOT count toward a stall.
        let progressed = !plan.to_complete.is_empty()
            || !plan.to_dispatch.is_empty()
            || !plan.to_fail.is_empty()
            || !plan.to_register_gate.is_empty()
            || !plan.to_poll_gate.is_empty()
            || !plan.to_verify_drift.is_empty();
        if !plan.stall_fingerprint.is_empty() {
            stall_detector.record_action(
                plan.stall_fingerprint.clone(),
                "conductor".to_string(),
                tick,
                None,
            );
            if !progressed {
                if let Some(pattern) = stall_detector.check() {
                    warn!("conductor: run {run_id} stall detected: {pattern}");
                    let mut st = loop_state.lock().await;
                    st.error = Some(format!("Stall detected: {pattern}"));
                    st.phase = LoopPhase::Error;
                    st.running = false;
                    return;
                }
            }
        }

        // Surface dispatching phase when we are about to spawn workers.
        if !plan.to_dispatch.is_empty() {
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::DispatchingWorker;
        }
        if !plan.to_harvest.is_empty() {
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::Elaborating;
        }

        let done = match apply_tick(
            &plan,
            &subtasks,
            run_id,
            &pg,
            &dispatcher,
            &gate_client,
            &config,
            &mut timers,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                error!("conductor: run {run_id} apply_tick failed: {e}");
                false
            }
        };

        if done {
            info!("conductor: run {run_id} complete — all subtasks terminal");
            let _ = pg
                .set_run_status(run_id, "complete")
                .await
                .map_err(|e| warn!("conductor: set_run_status complete: {e}"));
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::Complete;
            st.running = false;
            return;
        }

        sleep_or_stop(&config, &mut stop_rx).await;
    }
}

/// Sleep the tick interval, returning early if the stop signal fires.
async fn sleep_or_stop(config: &OrchestrationRunConfig, stop_rx: &mut watch::Receiver<bool>) {
    let dur = Duration::from_secs(config.tick_interval_secs);
    tokio::select! {
        _ = tokio::time::sleep(dur) => {}
        _ = stop_rx.changed() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pg::completion_reports::CompletionReport;
    use std::collections::HashMap as Map;
    use std::sync::Mutex as StdMutex;

    fn report() -> CompletionReport {
        CompletionReport {
            summary_md: "done".to_string(),
            deliverables: vec![],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts: Map::new(),
        }
    }

    fn mk(task_id: &str, idx: i32, depends_on: &[&str], state: SubtaskState) -> Subtask {
        Subtask {
            task_id: task_id.to_string(),
            run_id: Uuid::nil(),
            idx,
            title: format!("t-{task_id}"),
            brief: "b".to_string(),
            phase: "implement".to_string(),
            repo: None,
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            expected_output: "x".to_string(),
            emits_subtasks: false,
            state,
            task_run_id: None,
            artifact: None,
            produced_by: None,
            gate_id: None,
            gate_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Scripted signal source keyed by task_run_id.
    struct FakeSignals(Map<Uuid, WorkerSignal>);
    impl SignalSource for FakeSignals {
        fn signal(&self, task_run_id: Uuid) -> WorkerSignal {
            self.0
                .get(&task_run_id)
                .copied()
                .unwrap_or(WorkerSignal::Gone)
        }
    }

    /// Records dispatch/reprompt calls; never spawns.
    #[derive(Default)]
    struct FakeDispatcher {
        dispatched: StdMutex<Vec<String>>,
        reprompted: StdMutex<Vec<Uuid>>,
        next_trid: StdMutex<Vec<Uuid>>,
    }
    #[async_trait]
    impl Dispatcher for FakeDispatcher {
        async fn dispatch(&self, _run_id: Uuid, subtask: &Subtask) -> Result<Uuid, String> {
            self.dispatched
                .lock()
                .unwrap()
                .push(subtask.task_id.clone());
            let trid = self
                .next_trid
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(Uuid::new_v4);
            Ok(trid)
        }
        async fn reprompt(&self, task_run_id: Uuid, _message: &str) -> Result<(), String> {
            self.reprompted.lock().unwrap().push(task_run_id);
            Ok(())
        }
    }

    fn cfg() -> OrchestrationRunConfig {
        OrchestrationRunConfig::default()
    }

    // --- topo / readiness -------------------------------------------------

    #[test]
    fn linear_dag_only_root_ready_initially() {
        // A → B → C, all Submitted. Only A is ready.
        let rows = vec![
            mk("A", 0, &[], SubtaskState::Submitted),
            mk("B", 1, &["A"], SubtaskState::Submitted),
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];
        let order = topo_order(&rows).unwrap();
        let ready: Vec<_> = ready_subtasks(&rows, &order)
            .iter()
            .map(|s| s.task_id.clone())
            .collect();
        assert_eq!(ready, vec!["A".to_string()], "only the root is ready");
    }

    #[test]
    fn b_ready_only_after_a_completed() {
        let rows = vec![
            mk("A", 0, &[], SubtaskState::Completed),
            mk("B", 1, &["A"], SubtaskState::Submitted),
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];
        let order = topo_order(&rows).unwrap();
        let ready: Vec<_> = ready_subtasks(&rows, &order)
            .iter()
            .map(|s| s.task_id.clone())
            .collect();
        assert_eq!(
            ready,
            vec!["B".to_string()],
            "B ready once A Completed; C still blocked"
        );
    }

    #[test]
    fn cycle_is_fatal() {
        let rows = vec![
            mk("A", 0, &["C"], SubtaskState::Submitted),
            mk("B", 1, &["A"], SubtaskState::Submitted),
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(plan.fatal.is_some(), "a dependency cycle must be fatal");
    }

    // --- tick: dispatch + concurrency cap ---------------------------------

    #[test]
    fn first_tick_dispatches_only_root_in_linear_dag() {
        let rows = vec![
            mk("A", 0, &[], SubtaskState::Submitted),
            mk("B", 1, &["A"], SubtaskState::Submitted),
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(plan.to_dispatch, vec!["A".to_string()]);
        assert!(!plan.done);
    }

    #[test]
    fn concurrency_cap_limits_parallel_dispatch() {
        // 5 independent roots, cap = 2 → only 2 dispatched this tick.
        let rows = vec![
            mk("A", 0, &[], SubtaskState::Submitted),
            mk("B", 1, &[], SubtaskState::Submitted),
            mk("C", 2, &[], SubtaskState::Submitted),
            mk("D", 3, &[], SubtaskState::Submitted),
            mk("E", 4, &[], SubtaskState::Submitted),
        ];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let mut c = cfg();
        c.concurrency_cap = 2;
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert_eq!(plan.to_dispatch.len(), 2, "cap limits dispatch to 2");
    }

    #[test]
    fn cap_accounts_for_in_flight_workers() {
        // 1 Working (in-flight), 3 ready roots, cap 3 → only 2 slots free.
        let trid = Uuid::new_v4();
        let mut working = mk("W", 0, &[], SubtaskState::Working);
        working.task_run_id = Some(trid);
        let rows = vec![
            working,
            mk("A", 1, &[], SubtaskState::Submitted),
            mk("B", 2, &[], SubtaskState::Submitted),
            mk("C", 3, &[], SubtaskState::Submitted),
        ];
        // The Working worker is still busy.
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::Working);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_dispatch.len(),
            2,
            "3-cap minus 1 in-flight = 2 slots"
        );
        assert!(plan.to_complete.is_empty());
    }

    // --- tick: §5 completion guard ----------------------------------------

    #[test]
    fn ready_with_artifact_completes() {
        let trid = Uuid::new_v4();
        let mut w = mk("A", 0, &[], SubtaskState::Working);
        w.task_run_id = Some(trid);
        w.artifact = Some(report());
        let rows = vec![w];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(plan.to_complete, vec!["A".to_string()]);
        // `done` is re-derived from PERSISTED state — A is still `Working` in
        // the rows this tick (completion is APPLIED to the DB after compute),
        // so the run reads done only on the NEXT tick once A is Completed.
        // Statelessness: the exit predicate never reads queued-but-unwritten
        // transitions. Simulate the write + re-derive:
        assert!(
            !plan.done,
            "done is re-derived next tick after the DB write"
        );
        let mut applied = rows.clone();
        applied[0].state = SubtaskState::Completed;
        let plan2 = compute_tick(&applied, &signals, &mut timers, &cfg(), 1);
        assert!(
            plan2.done,
            "after A is persisted Completed, the run is done"
        );
    }

    #[test]
    fn ready_without_artifact_does_not_complete_and_eventually_reprompts_then_fails() {
        let trid = Uuid::new_v4();
        let mut w = mk("A", 0, &[], SubtaskState::Working);
        w.task_run_id = Some(trid);
        // NO artifact.
        let rows = vec![w];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();

        // t=0: ready-no-artifact observed, not yet past timeout → nothing.
        let p0 = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert!(p0.to_complete.is_empty());
        assert!(p0.to_reprompt.is_empty());
        assert!(p0.to_fail.is_empty());

        // t=timeout: re-prompt once.
        let p1 = compute_tick(&rows, &signals, &mut timers, &c, c.report_timeout_secs);
        assert_eq!(p1.to_reprompt.len(), 1, "re-prompt after first timeout");
        // simulate the live loop recording the reprompt timer:
        timers.reprompted_at.insert(trid, c.report_timeout_secs);

        // t=timeout+grace: still no artifact → Failed.
        let p2 = compute_tick(
            &rows,
            &signals,
            &mut timers,
            &c,
            c.report_timeout_secs + c.report_reprompt_grace_secs,
        );
        assert_eq!(
            p2.to_fail,
            vec!["A".to_string()],
            "fail after reprompt grace"
        );
    }

    #[test]
    fn errored_signal_fails_subtask() {
        let trid = Uuid::new_v4();
        let mut w = mk("A", 0, &[], SubtaskState::Working);
        w.task_run_id = Some(trid);
        let rows = vec![w];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::Errored);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(plan.to_fail, vec!["A".to_string()]);
    }

    #[test]
    fn gone_worker_fails_only_after_grace() {
        let trid = Uuid::new_v4();
        let mut w = mk("A", 0, &[], SubtaskState::Working);
        w.task_run_id = Some(trid);
        let rows = vec![w];
        let signals = FakeSignals(Map::new()); // unknown ⇒ Gone
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();
        let p0 = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert!(p0.to_fail.is_empty(), "gone tolerated within grace");
        let p1 = compute_tick(&rows, &signals, &mut timers, &c, c.gone_grace_secs);
        assert_eq!(p1.to_fail, vec!["A".to_string()], "gone-past-grace fails");
    }

    // --- KILL + RESTART RESUME (statelessness proof) ----------------------

    #[tokio::test]
    async fn resume_does_not_redispatch_working_nor_rerun_completed() {
        // Mid-run snapshot AS PERSISTED IN THE LEDGER:
        //   A = Completed (done — must NOT be re-run)
        //   B = Working with a bound task_run_id (in-flight — must NOT be re-dispatched)
        //   C = Submitted depends_on B (blocked — must NOT dispatch)
        let b_trid = Uuid::new_v4();
        let mut b = mk("B", 1, &["A"], SubtaskState::Working);
        b.task_run_id = Some(b_trid);
        let rows = vec![
            mk("A", 0, &[], SubtaskState::Completed),
            b,
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];

        // Fresh process: timers EMPTY (all in-memory state dropped). B is still
        // working per its FSM.
        let mut sigs = Map::new();
        sigs.insert(b_trid, WorkerSignal::Working);
        let signals = FakeSignals(sigs);
        let dispatcher = FakeDispatcher::default();
        let mut timers = ReadyIdleTimers::default();

        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        // No re-dispatch of B (already Working) and no re-run of A (Completed);
        // C is blocked on B.
        assert!(
            plan.to_dispatch.is_empty(),
            "nothing dispatched on resume — B in-flight, C blocked, A done"
        );
        assert!(plan.to_complete.is_empty());
        assert!(plan.to_fail.is_empty());
        assert!(!plan.done, "B still in-flight ⇒ not done");

        // Now B reports + goes Ready. Next tick (still a fresh-state derivation)
        // completes B and dispatches C — no duplication.
        let b_trid2 = b_trid;
        let mut b2 = mk("B", 1, &["A"], SubtaskState::Working);
        b2.task_run_id = Some(b_trid2);
        b2.artifact = Some(report());
        let rows2 = vec![
            mk("A", 0, &[], SubtaskState::Completed),
            b2,
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];
        let mut sigs2 = Map::new();
        sigs2.insert(b_trid2, WorkerSignal::ReadyIdle);
        let signals2 = FakeSignals(sigs2);
        let plan2 = compute_tick(&rows2, &signals2, &mut timers, &cfg(), 1);
        assert_eq!(plan2.to_complete, vec!["B".to_string()]);
        // C is NOT dispatched this same tick: its dep B is still PERSISTED
        // `Working` (B's completion is written to the DB only after compute).
        // This is the stateless invariant — dispatch reads committed state, so
        // C waits for the tick AFTER B's completion lands. No duplication.
        assert!(
            plan2.to_dispatch.is_empty(),
            "C waits until B's Completed is persisted (next tick), never double-dispatched"
        );

        // Simulate the applied write: B Completed. THEN C becomes ready.
        let mut rows3 = rows2.clone();
        rows3[1].state = SubtaskState::Completed;
        let signals3 = FakeSignals(Map::new());
        let plan3 = compute_tick(&rows3, &signals3, &mut timers, &cfg(), 2);
        assert_eq!(
            plan3.to_dispatch,
            vec!["C".to_string()],
            "C dispatched once B persisted Completed"
        );
        assert!(
            plan3.to_complete.is_empty(),
            "A and B already Completed — not re-completed"
        );

        // The fake dispatcher was never invoked by compute_tick (pure).
        assert!(dispatcher.dispatched.lock().unwrap().is_empty());
    }

    // --- Phase 4: progressive elaboration exit guard (contract §3.5) -------

    /// Helper: build an `emits_subtasks` elaborator row in the given state,
    /// optionally with an artifact.
    fn mk_elaborator(task_id: &str, idx: i32, state: SubtaskState, with_artifact: bool) -> Subtask {
        let mut s = mk(task_id, idx, &[], state);
        s.emits_subtasks = true;
        if with_artifact {
            s.artifact = Some(report());
        }
        s
    }

    #[test]
    fn run_not_done_while_elaborator_non_terminal() {
        // A single Working elaborator → not terminal → not done.
        let trid = Uuid::new_v4();
        let mut e = mk_elaborator("E", 0, SubtaskState::Working, false);
        e.task_run_id = Some(trid);
        let rows = vec![e];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::Working);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(
            !plan.done,
            "a non-terminal elaborator must keep the run not-done"
        );
    }

    #[test]
    fn run_not_done_while_completed_elaborator_unharvested() {
        // A Completed elaborator whose children are NOT yet spliced (no row
        // carries produced_by == "E"). all_terminal() is true, but the §3.5
        // guard must keep the run not-done until the seeds are harvested.
        let rows = vec![mk_elaborator("E", 0, SubtaskState::Completed, true)];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(
            !plan.done,
            "a Completed-but-unharvested elaborator must keep the run not-done (plan can still grow)"
        );
        assert_eq!(
            plan.to_harvest,
            vec!["E".to_string()],
            "the unharvested elaborator is queued for harvest"
        );
    }

    #[test]
    fn run_done_once_elaborator_harvested_and_children_terminal() {
        // E Completed AND a child row carries produced_by == "E" AND that child
        // is terminal → all_terminal && nothing unharvested → done.
        let mut child = mk("child", 1, &["E"], SubtaskState::Completed);
        child.produced_by = Some("E".to_string());
        let rows = vec![mk_elaborator("E", 0, SubtaskState::Completed, true), child];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(
            plan.to_harvest.is_empty(),
            "E now has a produced_by child ⇒ harvested"
        );
        assert!(
            plan.done,
            "harvested elaborator + all children terminal ⇒ run done"
        );
    }

    #[test]
    fn run_not_done_while_harvested_child_still_in_flight() {
        // E Completed + harvested, but the spliced child is still Submitted →
        // not all terminal → not done (the grown DAG must finish).
        let mut child = mk("child", 1, &["E"], SubtaskState::Submitted);
        child.produced_by = Some("E".to_string());
        let rows = vec![mk_elaborator("E", 0, SubtaskState::Completed, true), child];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(
            !plan.done,
            "a still-Submitted harvested child keeps the run not-done"
        );
        // The child becomes ready to dispatch once E is Completed (its dep).
        assert_eq!(plan.to_dispatch, vec!["child".to_string()]);
    }

    #[test]
    fn harvest_parses_two_seeds_with_parent_dep_and_idx() {
        // Verify the harvest READ path (artifact → seeds → rows) without a DB:
        // parse_next_subtasks + seeds_to_subtasks compose to exactly 2 child
        // rows, each produced_by = parent, depends_on contains the parent, and
        // idx after the parent's idx.
        use crate::orchestration_loop::org_chart::{parse_next_subtasks, seeds_to_subtasks};
        let run_id = Uuid::new_v4();
        let mut e = mk_elaborator("spec", 0, SubtaskState::Completed, false);
        let mut artifact = report();
        artifact.artifacts.insert(
            "orchestration".to_string(),
            serde_json::json!({
                "next_subtasks": [
                    {"id": "build-a", "title": "A", "brief": "b", "phase": "implement",
                     "depends_on": [], "expected_output": "x", "repo": "qontinui-mobile"},
                    {"id": "build-b", "title": "B", "brief": "b", "phase": "implement",
                     "depends_on": ["build-a"], "expected_output": "x"}
                ]
            }),
        );
        e.artifact = Some(artifact);

        let seeds = parse_next_subtasks(e.artifact.as_ref().unwrap()).unwrap();
        assert_eq!(seeds.len(), 2, "exactly 2 build seeds harvested");

        // start_idx after the parent (idx 0) → 1.
        let rows = seeds_to_subtasks(run_id, &seeds, Some(&e.task_id), e.idx + 1);
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(
                r.produced_by.as_deref(),
                Some("spec"),
                "produced_by = parent"
            );
            assert!(
                r.depends_on.contains(&"spec".to_string()),
                "parent dep wired in"
            );
        }
        assert_eq!(rows[0].idx, 1, "idx after the parent");
        assert_eq!(rows[1].idx, 2);
        assert!(
            rows[1].depends_on.contains(&"build-a".to_string()),
            "cross-edge preserved"
        );
    }

    #[test]
    fn all_terminal_run_is_done() {
        let rows = vec![
            mk("A", 0, &[], SubtaskState::Completed),
            mk("B", 1, &["A"], SubtaskState::Failed),
        ];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(plan.done, "Completed+Failed are both terminal ⇒ done");
        assert!(plan.to_dispatch.is_empty());
    }

    // --- stall fingerprint excludes legitimately-blocked rows (§3.5) ------

    // --- Phase 4: DB-gated end-to-end harvest (count + idempotency) --------

    /// Harvest a Completed elaborator whose artifact carries 2 build seeds:
    /// the FIRST harvest splices exactly 2 rows (each `produced_by = parent`,
    /// `depends_on` containing the parent, `idx` after the parent); the SECOND
    /// harvest of the SAME artifact inserts 0 (insert-if-not-exists ⇒
    /// idempotent across the stateless reconciler's re-reads).
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture
    /// (DATABASE_URL) with the `orchestration` schema (self-heals at
    /// `PgDb::new`). Run with:
    /// `cargo test -p qontinui-runner orchestration_loop::conductor::tests::harvest_splices_then_is_idempotent -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn harvest_splices_then_is_idempotent() {
        let pg = Arc::new(PgDb::new_blocking_for_test());
        let run_id = Uuid::new_v4();
        pg.create_run(
            run_id,
            "elaborate",
            None,
            &["plan".to_string(), "implement".to_string()],
            "running",
        )
        .await
        .expect("create_run");

        // Persist a Completed elaborator "spec" (idx 0) carrying 2 build seeds.
        let mut elaborator = mk_elaborator("spec", 0, SubtaskState::Completed, false);
        elaborator.run_id = run_id;
        let mut artifact = report();
        artifact.artifacts.insert(
            "orchestration".to_string(),
            serde_json::json!({
                "next_subtasks": [
                    {"id": "build-a", "title": "A", "brief": "b", "phase": "implement",
                     "depends_on": [], "expected_output": "x", "repo": "qontinui-mobile"},
                    {"id": "build-b", "title": "B", "brief": "b", "phase": "implement",
                     "depends_on": ["build-a"], "expected_output": "x"}
                ]
            }),
        );
        elaborator.artifact = Some(artifact);
        pg.upsert_subtask(&elaborator)
            .await
            .expect("upsert elaborator");

        // First harvest: exactly 2 NEW rows.
        let n1 = harvest_elaboration(&pg, run_id, &elaborator)
            .await
            .expect("first harvest");
        assert_eq!(n1, 2, "first harvest splices exactly 2 seeds");

        let rows = pg.list_subtasks(run_id).await.expect("list");
        let a = rows
            .iter()
            .find(|s| s.task_id == "build-a")
            .expect("build-a present");
        let b = rows
            .iter()
            .find(|s| s.task_id == "build-b")
            .expect("build-b present");
        assert_eq!(a.produced_by.as_deref(), Some("spec"));
        assert_eq!(b.produced_by.as_deref(), Some("spec"));
        assert!(
            a.depends_on.contains(&"spec".to_string()),
            "parent dep on build-a"
        );
        assert!(
            b.depends_on.contains(&"spec".to_string()),
            "parent dep on build-b"
        );
        assert!(
            b.depends_on.contains(&"build-a".to_string()),
            "cross-edge preserved"
        );
        assert!(a.idx > elaborator.idx, "child idx after parent");
        assert!(b.idx > a.idx, "children sequential");
        assert_eq!(a.state, SubtaskState::Submitted);

        // Second harvest of the SAME artifact: 0 new rows (idempotent).
        let n2 = harvest_elaboration(&pg, run_id, &elaborator)
            .await
            .expect("second harvest");
        assert_eq!(n2, 0, "re-reading the same artifact splices 0 (idempotent)");
        let rows2 = pg.list_subtasks(run_id).await.expect("list2");
        assert_eq!(
            rows2.len(),
            3,
            "still elaborator + 2 children — no duplicates"
        );

        // Cleanup (FK CASCADE removes subtasks with the run).
        let conn = pg.pool().get().await.expect("conn");
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    #[test]
    fn blocked_rows_excluded_from_stall_fingerprint() {
        // A Working, B Submitted-blocked-on-A. Fingerprint must mention only A
        // (the actionable in-flight row), NOT the blocked B.
        let trid = Uuid::new_v4();
        let mut a = mk("A", 0, &[], SubtaskState::Working);
        a.task_run_id = Some(trid);
        let rows = vec![a, mk("B", 1, &["A"], SubtaskState::Submitted)];
        let order = topo_order(&rows).unwrap();
        let fp = stall_fingerprint(&rows, &order);
        assert!(fp.contains("W:A:"), "in-flight A present");
        assert!(
            !fp.contains("B"),
            "blocked B excluded from stall fingerprint"
        );
    }

    #[test]
    fn pure_waiting_fingerprint_stable_but_only_blocked_is_empty() {
        // Only a blocked row (no in-flight, deps unmet) ⇒ empty fingerprint ⇒
        // the live loop never records a stall action for pure waiting.
        let rows = [
            mk("A", 0, &[], SubtaskState::Working), // in-flight but...
        ];
        // Actually make A in-flight with id so it's actionable:
        let trid = Uuid::new_v4();
        let mut a = rows[0].clone();
        a.task_run_id = Some(trid);
        let order = topo_order(&[a.clone()]).unwrap();
        let fp = stall_fingerprint(&[a], &order);
        assert!(!fp.is_empty());

        // A truly pure-waiting set: one Submitted row whose dep is missing.
        let blocked = vec![mk("X", 0, &["missing"], SubtaskState::Submitted)];
        let order2 = topo_order(&blocked).unwrap();
        let fp2 = stall_fingerprint(&blocked, &order2);
        assert!(
            fp2.is_empty(),
            "a blocked-only set has empty actionable fingerprint"
        );
    }

    // ======================================================================
    // Phase 6 — coord gates + DriftVerdict verify
    // ======================================================================

    use super::super::coord_gate::{CoordGateClient, DriftClass, GatePredicateSpec, GateStatus};
    use std::sync::Mutex as GateMutex;

    /// A subtask with an `expected_output` that drives the gate classifier, and
    /// optional pre-set gate columns (simulating a prior tick's persistence).
    fn mk_gated(
        task_id: &str,
        idx: i32,
        deps: &[&str],
        expected_output: &str,
        gate_id: Option<&str>,
        gate_status: Option<&str>,
    ) -> Subtask {
        let mut s = mk(task_id, idx, deps, SubtaskState::Submitted);
        s.expected_output = expected_output.to_string();
        s.repo = Some("qontinui-web".to_string());
        s.gate_id = gate_id.map(|g| g.to_string());
        s.gate_status = gate_status.map(|g| g.to_string());
        s
    }

    /// Scripted coord client: returns a fixed gate_id on register, a scripted
    /// status on poll, and a scripted drift verdict. Records calls for assertions.
    #[derive(Default)]
    struct FakeCoordGateClient {
        registered: GateMutex<Vec<String>>,
        polled: GateMutex<Vec<String>>,
        verdict_subspaces: GateMutex<Vec<String>>,
        next_gate_id: String,
        poll_status: GateMutex<Option<GateStatus>>,
        drift: GateMutex<Option<DriftClass>>,
    }

    impl FakeCoordGateClient {
        fn with_gate_id(id: &str) -> Self {
            FakeCoordGateClient {
                next_gate_id: id.to_string(),
                ..Default::default()
            }
        }
        fn set_poll(&self, s: GateStatus) {
            *self.poll_status.lock().unwrap() = Some(s);
        }
        fn set_drift(&self, d: DriftClass) {
            *self.drift.lock().unwrap() = Some(d);
        }
    }

    #[async_trait]
    impl CoordGateClient for FakeCoordGateClient {
        async fn register_gate(
            &self,
            _run_id: Uuid,
            task_id: &str,
            _predicate: &GatePredicateSpec,
        ) -> Result<String, String> {
            self.registered.lock().unwrap().push(task_id.to_string());
            Ok(self.next_gate_id.clone())
        }
        async fn poll_gate(&self, gate_id: &str) -> Result<GateStatus, String> {
            self.polled.lock().unwrap().push(gate_id.to_string());
            Ok(self.poll_status.lock().unwrap().unwrap_or(GateStatus::Open))
        }
        async fn drift_verdict(&self, subspace: &str) -> Result<DriftClass, String> {
            self.verdict_subspaces
                .lock()
                .unwrap()
                .push(subspace.to_string());
            Ok(self
                .drift
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(DriftClass::NoDrift))
        }
    }

    // --- classifier-driven compute_tick decisions (pure, no DB) -----------

    #[test]
    fn ci_green_subtask_needs_gate_then_is_not_dispatched() {
        // A test-phase subtask whose expected_output is "CI green" and which has
        // NO gate yet: compute_tick must queue it for gate REGISTRATION and must
        // NOT dispatch it.
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green on this repo",
            None,
            None,
        )];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_register_gate,
            vec!["test".to_string()],
            "a CI-green subtask with no gate must register one"
        );
        assert!(
            plan.to_dispatch.is_empty(),
            "a subtask awaiting gate registration must NOT dispatch"
        );
        assert!(
            !plan.done,
            "run not done while a gate-bound subtask is unresolved"
        );
    }

    #[test]
    fn blocked_on_open_gate_polls_and_does_not_dispatch() {
        // Gate registered (gate_id set), status open → re-poll, no dispatch.
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            Some("gate-123"),
            Some("open"),
        )];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_poll_gate,
            vec!["test".to_string()],
            "open gate is re-polled"
        );
        assert!(
            plan.to_dispatch.is_empty(),
            "blocked-on-open-gate is not dispatchable"
        );
        assert!(
            plan.to_register_gate.is_empty(),
            "already registered — no re-register"
        );
        assert!(!plan.done);
    }

    #[test]
    fn cleared_gate_unblocks_dispatch() {
        // Gate cleared → the subtask is dispatchable; no further poll/register.
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            Some("gate-123"),
            Some("cleared"),
        )];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_dispatch,
            vec!["test".to_string()],
            "a cleared gate makes the subtask dispatchable"
        );
        assert!(
            plan.to_poll_gate.is_empty(),
            "cleared gate is no longer polled"
        );
        assert!(plan.to_register_gate.is_empty());
    }

    #[test]
    fn failed_gate_fails_subtask() {
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            Some("gate-123"),
            Some("failed"),
        )];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_fail,
            vec!["test".to_string()],
            "a failed gate fails the subtask"
        );
        assert!(plan.to_dispatch.is_empty());
        assert!(
            plan.to_poll_gate.is_empty(),
            "a failed gate is not re-polled"
        );
    }

    #[test]
    fn restart_reattaches_to_persisted_gate_without_reregister() {
        // RESTART RE-ATTACH: a subtask persisted with gate_id set + status open is
        // recognized as gate-blocked on a fresh load (fresh empty timers) — NOT
        // re-registered (gate_id present), NOT dispatched (status not cleared).
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            Some("gate-persisted"),
            Some("open"),
        )];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default(); // fresh process
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert!(
            plan.to_register_gate.is_empty(),
            "a persisted gate_id must NOT be re-registered on restart"
        );
        assert_eq!(
            plan.to_poll_gate,
            vec!["test".to_string()],
            "restart re-attaches by re-polling the durable gate"
        );
        assert!(
            plan.to_dispatch.is_empty(),
            "still blocked — not dispatched on restart"
        );
    }

    #[test]
    fn gate_wait_only_tick_does_not_trip_stall_fingerprint() {
        // A subtask blocked on an open gate must be EXCLUDED from the actionable
        // stall fingerprint (it's legitimately waiting on coord). With only a
        // gate-blocked row present, the fingerprint is empty → no stall accrual.
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            Some("gate-123"),
            Some("open"),
        )];
        let order = topo_order(&rows).unwrap();
        let fp = stall_fingerprint(&rows, &order);
        assert!(
            fp.is_empty(),
            "a gate-blocked-only set has empty actionable fingerprint (no stall): {fp:?}"
        );
        // And a needs-registration row is likewise excluded.
        let rows2 = vec![mk_gated("test", 0, &[], "CI green", None, None)];
        let order2 = topo_order(&rows2).unwrap();
        assert!(
            stall_fingerprint(&rows2, &order2).is_empty(),
            "a needs-gate-registration row is excluded from the stall fingerprint"
        );
    }

    #[test]
    fn drift_verify_subtask_is_diverted_not_completed() {
        // A verify subtask that is ReadyIdle+artifact but whose verdict is NOT yet
        // recorded must route to `to_verify_drift`, NOT `to_complete`.
        let trid = Uuid::new_v4();
        let mut v = mk("verify", 0, &[], SubtaskState::Working);
        v.expected_output = "DriftVerdict shows no drift".to_string();
        v.task_run_id = Some(trid);
        v.artifact = Some(report()); // artifact present, but no drift_verdict key
        let rows = vec![v];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_verify_drift,
            vec!["verify".to_string()],
            "an unrecorded-verdict verify subtask diverts to the verify path"
        );
        assert!(
            plan.to_complete.is_empty(),
            "a verify subtask must not complete before its verdict is read"
        );
        assert!(!plan.done, "run not done while a verify is pending");
    }

    #[test]
    fn drift_verify_with_recorded_verdict_completes_normally() {
        // Once the no-drift verdict is recorded in the artifact, the verify
        // subtask completes normally (not re-diverted).
        let trid = Uuid::new_v4();
        let mut v = mk("verify", 0, &[], SubtaskState::Working);
        v.expected_output = "DriftVerdict shows no drift".to_string();
        v.task_run_id = Some(trid);
        let mut rep = report();
        rep.artifacts.insert(
            DRIFT_VERDICT_ARTIFACT_KEY.to_string(),
            serde_json::json!({"drift_class": "none", "no_drift": true}),
        );
        v.artifact = Some(rep);
        let rows = vec![v];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(
            plan.to_complete,
            vec!["verify".to_string()],
            "a verify with a recorded verdict completes normally"
        );
        assert!(plan.to_verify_drift.is_empty(), "not re-diverted");
    }

    // --- apply_tick PG-fixture end-to-end (gate persistence + remediation) ---

    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture
    /// (DATABASE_URL) with the `orchestration` schema (self-heals at `PgDb::new`,
    /// incl. the Phase-6 gate columns). Run with:
    /// `cargo test -p qontinui-runner orchestration_loop::conductor::tests::gate_register_persists_and_clears_then_dispatches -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn gate_register_persists_and_clears_then_dispatches() {
        let pg = Arc::new(PgDb::new_blocking_for_test());
        let run_id = Uuid::new_v4();
        pg.create_run(run_id, "gate run", None, &["test".to_string()], "running")
            .await
            .expect("create_run");

        // Persist a CI-green test subtask with NO gate yet.
        let mut st = mk_gated("test", 0, &[], "CI green", None, None);
        st.run_id = run_id;
        pg.upsert_subtask(&st).await.expect("upsert");

        let dispatcher = FakeDispatcher::default();
        let gate = FakeCoordGateClient::with_gate_id("gate-abc");
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();

        // Tick 1: register the gate. apply_tick persists gate_id + status=open.
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        assert_eq!(plan.to_register_gate, vec!["test".to_string()]);
        apply_tick(
            &plan,
            &rows,
            run_id,
            &pg,
            &dispatcher,
            &gate,
            &c,
            &mut timers,
        )
        .await
        .unwrap();
        let after = pg.list_subtasks(run_id).await.unwrap();
        let row = after.iter().find(|s| s.task_id == "test").unwrap();
        assert_eq!(
            row.gate_id.as_deref(),
            Some("gate-abc"),
            "gate_id persisted"
        );
        assert_eq!(
            row.gate_status.as_deref(),
            Some("open"),
            "status persisted open"
        );
        assert!(
            dispatcher.dispatched.lock().unwrap().is_empty(),
            "not dispatched while open"
        );

        // Tick 2: gate still open → re-poll, no dispatch.
        gate.set_poll(GateStatus::Open);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 1);
        assert_eq!(plan.to_poll_gate, vec!["test".to_string()]);
        apply_tick(
            &plan,
            &rows,
            run_id,
            &pg,
            &dispatcher,
            &gate,
            &c,
            &mut timers,
        )
        .await
        .unwrap();
        assert!(
            dispatcher.dispatched.lock().unwrap().is_empty(),
            "still not dispatched"
        );

        // Tick 3: gate clears → apply_tick persists status=cleared.
        gate.set_poll(GateStatus::Cleared);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 2);
        apply_tick(
            &plan,
            &rows,
            run_id,
            &pg,
            &dispatcher,
            &gate,
            &c,
            &mut timers,
        )
        .await
        .unwrap();
        let after = pg.list_subtasks(run_id).await.unwrap();
        let row = after.iter().find(|s| s.task_id == "test").unwrap();
        assert_eq!(
            row.gate_status.as_deref(),
            Some("cleared"),
            "status now cleared"
        );

        // Tick 4: cleared → dispatched.
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 3);
        assert_eq!(
            plan.to_dispatch,
            vec!["test".to_string()],
            "cleared → dispatch"
        );
        apply_tick(
            &plan,
            &rows,
            run_id,
            &pg,
            &dispatcher,
            &gate,
            &c,
            &mut timers,
        )
        .await
        .unwrap();
        assert_eq!(
            dispatcher.dispatched.lock().unwrap().as_slice(),
            &["test".to_string()]
        );

        // Cleanup.
        let conn = pg.pool().get().await.unwrap();
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    /// DriftVerdict: no-drift → verify Completed + artifact["drift_verdict"] set;
    /// drift → a remediation subtask spliced (depends on verify) + verdict stored.
    /// `#[ignore]` — needs PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn drift_verify_no_drift_completes_drift_splices_remediation() {
        let pg = Arc::new(PgDb::new_blocking_for_test());
        let dispatcher = FakeDispatcher::default();
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();

        // ---- Case A: NO DRIFT → verify Completed + verdict stored. ----
        let run_a = Uuid::new_v4();
        pg.create_run(run_a, "verify run", None, &["test".to_string()], "running")
            .await
            .unwrap();
        let trid = Uuid::new_v4();
        let mut v = mk("verify", 0, &[], SubtaskState::Working);
        v.run_id = run_a;
        v.expected_output = "DriftVerdict shows no drift subspace=health".to_string();
        v.task_run_id = Some(trid);
        v.artifact = Some(report());
        pg.upsert_subtask(&v).await.unwrap();

        let gate = FakeCoordGateClient::default();
        gate.set_drift(DriftClass::NoDrift);
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);

        let rows = pg.list_subtasks(run_a).await.unwrap();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert_eq!(plan.to_verify_drift, vec!["verify".to_string()]);
        apply_tick(
            &plan,
            &rows,
            run_a,
            &pg,
            &dispatcher,
            &gate,
            &c,
            &mut timers,
        )
        .await
        .unwrap();
        let after = pg.list_subtasks(run_a).await.unwrap();
        let vrow = after.iter().find(|s| s.task_id == "verify").unwrap();
        assert_eq!(vrow.state, SubtaskState::Completed, "no drift → Completed");
        let stored = vrow
            .artifact
            .as_ref()
            .and_then(|a| a.artifacts.get(DRIFT_VERDICT_ARTIFACT_KEY))
            .expect("drift_verdict stored on artifact");
        assert_eq!(
            stored.get("drift_class").and_then(|v| v.as_str()),
            Some("none")
        );
        assert_eq!(
            gate.verdict_subspaces.lock().unwrap().as_slice(),
            &["health".to_string()]
        );

        // ---- Case B: DRIFT → remediation spliced + verdict stored. ----
        let run_b = Uuid::new_v4();
        pg.create_run(
            run_b,
            "verify run b",
            None,
            &["test".to_string()],
            "running",
        )
        .await
        .unwrap();
        let trid_b = Uuid::new_v4();
        let mut vb = mk("verify", 0, &[], SubtaskState::Working);
        vb.run_id = run_b;
        vb.expected_output = "DriftVerdict shows no drift subspace=schema".to_string();
        vb.task_run_id = Some(trid_b);
        vb.artifact = Some(report());
        pg.upsert_subtask(&vb).await.unwrap();

        let gate_b = FakeCoordGateClient::default();
        gate_b.set_drift(DriftClass::Drift("active_negation".to_string()));
        let mut sigs_b = Map::new();
        sigs_b.insert(trid_b, WorkerSignal::ReadyIdle);
        let signals_b = FakeSignals(sigs_b);

        let rows = pg.list_subtasks(run_b).await.unwrap();
        let plan = compute_tick(&rows, &signals_b, &mut timers, &c, 0);
        apply_tick(
            &plan,
            &rows,
            run_b,
            &pg,
            &dispatcher,
            &gate_b,
            &c,
            &mut timers,
        )
        .await
        .unwrap();
        let after = pg.list_subtasks(run_b).await.unwrap();
        // verify subtask is NOT Completed (drift → remediate path).
        let vrow = after.iter().find(|s| s.task_id == "verify").unwrap();
        assert_ne!(
            vrow.state,
            SubtaskState::Completed,
            "drift → verify not Completed"
        );
        let stored = vrow
            .artifact
            .as_ref()
            .and_then(|a| a.artifacts.get(DRIFT_VERDICT_ARTIFACT_KEY))
            .expect("drift_verdict stored");
        assert_eq!(
            stored.get("drift_class").and_then(|v| v.as_str()),
            Some("active_negation")
        );
        // A remediation subtask depending on the verify was spliced.
        let rem = after
            .iter()
            .find(|s| s.task_id == "remediate-verify")
            .expect("remediation subtask spliced");
        assert_eq!(
            rem.produced_by.as_deref(),
            Some("verify"),
            "produced_by = verify"
        );
        assert!(
            rem.depends_on.contains(&"verify".to_string()),
            "remediation depends on the verify subtask"
        );
        let rev = after
            .iter()
            .find(|s| s.task_id == "reverify-verify")
            .expect("re-verify subtask spliced");
        assert!(
            rev.depends_on.contains(&"remediate-verify".to_string()),
            "re-verify depends on the remediation"
        );

        // Cleanup.
        let conn = pg.pool().get().await.unwrap();
        for r in [run_a, run_b] {
            let _ = conn
                .execute("DELETE FROM orchestration.runs WHERE run_id = $1", &[&r])
                .await;
        }
    }
}
