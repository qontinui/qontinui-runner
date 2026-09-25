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
//!    guard [`can_complete`] holds (`ReadyIdle` AND `artifact.is_some()`), or
//!    when a `Gone` worker's report already landed (a restart between the
//!    report and the idle signal); flip `Working → Failed` on
//!    `Errored`/`Gone`-past-budget/stall-past-budget.
//! 5. **post-Ready-without-artifact recovery** — a worker that is `ReadyIdle`
//!    but has no artifact after a per-subtask timeout is re-prompted ONCE (the
//!    only place a live session is re-prompted); still no artifact after a
//!    second timeout → `Failed`.
//! 6. **harvest** — [`harvest_elaboration`] is LIVE: a Completed elaborator's
//!    `next_subtasks` are spliced into the ledger as `produced_by` children
//!    (idempotently) BEFORE the elaborator flips to `Completed`, and again as a
//!    resume safety net for one that completed in a prior process.
//! 7. **stall / exit** — exit when all subtasks are terminal AND no
//!    `emits_subtasks` row is un-harvested; stall via [`StallWatch`] over a
//!    fingerprint that EXCLUDES everything legitimately waiting — a dependency
//!    that can still be satisfied, an elaboration, a gate coord holds open, a
//!    concurrency slot, and a `Working` worker (contract §3.5) — but INCLUDES a
//!    row the runner has no coord ANSWER about (`gate_status =
//!    coord_unreachable` / `coord_error`), a row whose dependency can never be
//!    satisfied, a dispatch this tick decided and did not land, and any side
//!    effect that was attempted and failed. The watch measures each stuck ROW
//!    separately, so one healthy row cannot hide a stuck one. The `Working`
//!    worker is bounded NOT by the §5 recovery deadlines (they fire from
//!    `ReadyIdle`/`Gone` only) but by its own silence deadline,
//!    [`OrchestrationRunConfig::working_silence_secs`] — which every emitted
//!    line RESETS, so it bounds a worker that goes QUIET rather than every
//!    `Working` worker, and a wedged-but-chatty CLI has no bound at all short
//!    of `stop_orchestration_run` (see [`tick_exit`]). **Every exit
//!    the reconciler TAKES is written to `orchestration.runs`** — `complete`,
//!    `failed` (fatal / DAG cycle) or `stalled`, with `status_reason` — via
//!    [`finish_run`], so the durable row never reads `running` for a run whose
//!    reconciler has returned. The in-memory [`LoopPhase`] mirrors it for the
//!    live status strip only.
//!
//!    That guarantee covers IN-PROCESS exits only. A runner killed or crashed
//!    mid-run takes the reconciler with it and leaves its row reading `running`
//!    with no writer left, and nothing sweeps orphaned rows at boot today — so
//!    `running` means "running, or last seen running by a process that is
//!    gone". A boot-time reconcile of `running` rows with no live reconciler is
//!    the follow-up that would close it.
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
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::ai_session_executor::{self, can_complete, WorkerSignal};
use super::coord_gate::{
    classify_drift_verify, classify_gate, CoordGateClient, CoordGateError, DriftClass,
    GatePredicateSpec, GateStatus, DEFAULT_DRIFT_SUBSPACE, GATE_STATUS_COORD_BLOCKS,
    GATE_STATUS_COORD_ERROR, GATE_STATUS_COORD_UNREACHABLE,
};
use super::ledger::{Subtask, SubtaskState};
use super::loop_engine::SharedLoopState;
use super::org_chart::OrgChartSeed;
use super::types::*;
use crate::database::pg::PgDb;

// ============================================================================
// Run config (runner-local runtime knobs — never crosses the wire)
// ============================================================================

/// Runtime knobs for a conductor run. Runner-local (not a wire DTO); sensible
/// conservative defaults so a bare `/orchestrate` works without tuning.
///
/// Serialized into `orchestration.runs.config` at create so the boot sweep
/// relaunches a run at the knobs it was started with. `#[serde(default)]`
/// fills any field a stored config lacks (a knob added after the row was
/// written) from [`Default`] rather than refusing the row.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// Seconds a worker may stay `Working` with NO observed activity — no FSM
    /// edge and no new CLI output line — before the reconciler fails it.
    ///
    /// This is the only bound on a worker wedged mid-turn **that goes quiet**,
    /// and there is no bound at all on one that wedges while still emitting —
    /// see the closing paragraph. Every §5 recovery
    /// deadline above fires from `ReadyIdle` or `Gone`; a CLI that hangs inside
    /// a turn stays `Processing` forever, which
    /// [`ai_session_executor::signal_from_state`] maps to [`WorkerSignal::Working`],
    /// and nothing in `claude_session` times a `Processing` session out. Such a
    /// worker holds a concurrency slot for the life of the run, and once the cap
    /// is saturated every other ready row is excluded from the stall fingerprint
    /// as "waiting on a slot" — so the whole run goes invisible and ticks at
    /// `running` indefinitely.
    ///
    /// It is deliberately NOT a stall-fingerprint entry: a fingerprinted
    /// `Working` row false-positives on a normal long turn inside the 300 s
    /// stall window (that was the regression the `Working` exclusion closed).
    /// It is a per-worker deadline on SILENCE instead, reset by any observed
    /// output, so a busy long turn never trips it.
    ///
    /// **That reset is a limit as well as a margin, and it is the gap this
    /// field does not close.** Because any line resets the clock, a CLI that is
    /// wedged but still TALKING — a retry loop, a progress spinner, a provider
    /// streaming an error over and over — resets its own deadline forever and is
    /// bounded by nothing: not this field, not the §5 deadlines, not the stall
    /// detector (a `Working` row is excluded from the fingerprint), and not
    /// `claude_session`. The only way out is `stop_orchestration_run`. Holding
    /// the run open is the deliberate choice — a stall exit is terminal and
    /// orphans every session the run spawned — but it is a choice, not a bound,
    /// and `a_worker_that_keeps_working_is_never_written_stalled` pins the
    /// behaviour: 5000 s of emitting ticks past this deadline, no stall.
    pub working_silence_secs: i64,
    /// Seconds a SINGLE stuck row must stay continuously stuck before the run is
    /// written `stalled` ([`StallWatch`]).
    ///
    /// This is wall clock, not a tick count, and it is deliberately LONGER than
    /// every §5 recovery deadline above: those deadlines are the mechanism that
    /// bounds a live-but-quiet worker, so a stall guillotine shorter than them
    /// does not detect anything they would not have handled — it just declares
    /// the run dead first. (It used to be five identical ticks: 25 s against
    /// a 90 s report timeout, i.e. the two mechanisms contradicting each other.)
    pub stall_after_secs: i64,
    /// Seconds a row whose ONLY stuck reason is a coord block (`C:` — the
    /// runner has no coord ANSWER about it) must stay continuously stuck before
    /// it alone may write the run `stalled`.
    ///
    /// It is deliberately much longer than [`Self::stall_after_secs`], because
    /// the two measure different things. `stall_after_secs` is sized against a
    /// WORKER's report deadline — it times a row this runner owns end to end.
    /// Coord reachability is not a property of the row at all: it is a
    /// runner-wide condition (a coord deploy, an LB returning 5xx, a device-JWT
    /// expiry inside the re-mint gap) that every blocked row observes at once
    /// and that no row can influence. Timing it on a row-sized clock meant an
    /// ordinary few minutes of coord unreachability ended a run whose workers
    /// were all healthy.
    ///
    /// The number matches [`Self::working_silence_secs`], which is already this
    /// config's declared budget for "a thing we depend on has stopped
    /// answering"; a coord outage longer than a worker is allowed to be silent
    /// is an outage, not a blip. See also the in-flight guard in [`tick_exit`],
    /// which is the other half of the fix: this window bounds a coord block that
    /// is the ONLY thing left in the run, and the guard stops one ending a run
    /// with live workers in it before the window is even reached.
    pub coord_block_stall_after_secs: i64,
    /// The agent registry's declared `parallel_fanout` bound, refreshed from
    /// coord once per tick by the live loop (see
    /// [`crate::agent_authorization::current_fanout_bound`]).
    ///
    /// `None` = not resolved yet (the very first tick, or a caller — every
    /// test — that does not consult the registry), in which case only
    /// `concurrency_cap` applies. This is a runner-local cache of a coord
    /// value, deliberately re-read each tick rather than carried, so a user
    /// changing the bound in the web settings page takes effect within about a
    /// registry TTL without restarting the run.
    ///
    /// Never persisted (`#[serde(skip)]`): it is a per-tick cache of a coord
    /// value, and a relaunch re-reads it like the first tick of any run.
    #[serde(skip)]
    pub fanout_bound: Option<u32>,
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
            // > report_timeout_secs + report_reprompt_grace_secs (180), so the
            // §5 recovery path always gets to run to its own conclusion first.
            stall_after_secs: 300,
            // 30 minutes of TOTAL silence — no FSM edge, no CLI output line.
            // Sized against the longest legitimately quiet thing a worker here
            // does: a single tool call that emits nothing until it returns (a
            // full `cargo test` on this repo, serialized behind `cargo-guard`'s
            // cross-session build lock, is the worst case). It is 20x
            // `report_timeout_secs` and 6x `stall_after_secs`, so it can never
            // pre-empt the §5 recovery path or the stall detector; it exists
            // only to put an upper bound on a worker that goes silent and will
            // never report. One that wedges while still emitting resets this
            // clock on every line and is bounded by nothing — see the field doc.
            working_silence_secs: 1800,
            // 30 minutes of coord being unable to answer about a row. Same
            // budget as `working_silence_secs` and for the same reason: below
            // it, an outage is indistinguishable from a blip this fleet sees
            // routinely.
            coord_block_stall_after_secs: 1800,
            fanout_bound: None,
        }
    }
}

impl OrchestrationRunConfig {
    /// How many workers may be dispatched-and-not-terminal at once, honouring
    /// BOTH the run's own conservative cap and the agent registry's declared
    /// `parallel_fanout` bound.
    ///
    /// The registry bound is a policy ceiling (served clause
    /// `agent-spawn-authorization`: "Parallel fan-out: bound declared up front
    /// (registry default 15)"), so it can only ever LOWER the effective cap —
    /// it never raises a run above the cap the caller asked for. When the
    /// ceiling bites, the surplus ready subtasks simply stay queued and land on
    /// later ticks: that is the clause's degrade to sequential, expressed where
    /// it is observable, and it can never fail a task.
    ///
    /// Floored at 1 so a hostile or fat-fingered `0` degrades to sequential
    /// rather than wedging the run — the same clamp
    /// [`crate::agent_authorization::current_fanout_bound`] applies.
    pub fn effective_concurrency_cap(&self) -> usize {
        match self.fanout_bound {
            Some(b) => self.concurrency_cap.min(b.max(1) as usize).max(1),
            None => self.concurrency_cap,
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

/// The typed coord block on this row, if any: the `gate_status` token
/// `apply_tick` wrote because a register / poll / drift-verdict call did not
/// produce an answer — [`GATE_STATUS_COORD_UNREACHABLE`] (the runner could not
/// ask: no credential, dead transport, a 401/5xx) or [`GATE_STATUS_COORD_ERROR`]
/// (coord answered and refused the call).
///
/// It is deliberately NOT restricted to `Submitted`: a DriftVerdict verify
/// subtask is `Working` while its verdict read keeps failing, and that row needs
/// the same treatment — it is retried every tick with no bound of its own.
pub fn coord_block(subtask: &Subtask) -> Option<&str> {
    let status = subtask.gate_status.as_deref()?;
    GATE_STATUS_COORD_BLOCKS.contains(&status).then_some(status)
}

/// `true` when a subtask is **blocked on coord** — [`coord_block`] is set.
///
/// Such a row is still re-tried every tick (it stays in `to_register_gate` /
/// `to_poll_gate` / `to_verify_drift`), but unlike a gate coord is genuinely
/// holding open it is NOT "legitimately waiting": it is INCLUDED in the stall
/// fingerprint and its retry does NOT count as progress, so a run wedged on an
/// unpaired runner — or on a coord that keeps refusing the call — surfaces as
/// `stalled` instead of ticking forever.
pub fn blocked_on_coord(subtask: &Subtask) -> bool {
    coord_block(subtask).is_some()
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
/// so a subtask naming a missing dep simply never becomes ready. It is surfaced
/// as a stall rather than a crash by [`stall_fingerprint`]'s `B:` entry, which
/// is what makes that sentence true: the row is excluded from `ready_subtasks`
/// AND from every other fingerprint arm, so without `B:` it was invisible and
/// the run looped at `running` forever with no diagnosis.
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
/// children have NOT yet been spliced in: a Completed elaborator E is
/// un-harvested iff NO subtask in the run carries
/// `produced_by == Some(E.task_id)`. [`harvest_elaboration`] (live — it runs
/// before the elaborator flips to Completed, and again from `apply_tick`'s
/// `to_harvest` safety net) splices the children and so flips this predicate
/// to false. The run is not `done` while any elaborator reads un-harvested
/// (see [`compute_tick`]'s exit guard).
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

    /// Epoch seconds of the most recent OUTPUT observed from this worker, or
    /// `None` when the source cannot observe activity for it (no such session,
    /// or a session kind that runs no Claude CLI and therefore has no activity
    /// channel).
    ///
    /// This is the `Working`-silence deadline's reset signal
    /// ([`OrchestrationRunConfig::working_silence_secs`]). It is deliberately
    /// SEPARATE from [`Self::signal`]: the FSM sits at `Processing` for a whole
    /// turn, so the signal alone cannot distinguish a worker that is busy from
    /// one that is wedged — only the output stamp can. `None` makes the deadline
    /// measure from the first tick the row was observed `Working`.
    ///
    /// **`None` is only conservative when it is TRUE.** For a source that
    /// genuinely has no activity channel the fallback base (first-seen-`Working`)
    /// is at or after the worker's real last activity, so the deadline can only
    /// fire LATER than the truth. For a source that HAS activity and simply does
    /// not report it, the same base is EARLIER than the real last activity, so
    /// the deadline fires EARLY and kills a live worker. No such source exists
    /// today — `dispatch_subtask` only ever spawns a `ClaudeSession`, and
    /// [`ManagerSignalSource`] reads its output tracker — which is exactly why
    /// implementing this as `|_| None` is a silent regression rather than a
    /// visible one, and why it must never be one.
    fn last_activity(&self, task_run_id: Uuid) -> Option<i64>;
}

/// Live signal source backed by the in-process `SessionManager`.
pub struct ManagerSignalSource {
    pub session_mgr: Arc<crate::claude_session::manager::SessionManager>,
}

impl SignalSource for ManagerSignalSource {
    fn signal(&self, task_run_id: Uuid) -> WorkerSignal {
        ai_session_executor::worker_terminal_state(&self.session_mgr, task_run_id)
    }

    fn last_activity(&self, task_run_id: Uuid) -> Option<i64> {
        // `ClaudeSession::last_activity_tracker` is the shared `AtomicU64` the
        // session's stdout pump stores `now` into on EVERY line it reads
        // (`claude_session/session.rs`), so it moves whenever the CLI emits
        // anything — tool calls, assistant text, progress — and stands still
        // while the process is wedged. A worker dispatched by
        // `ai_session_executor::dispatch_subtask` is always a `ClaudeSession`,
        // so this resolves for every worker the conductor spawns; a pty
        // `WorkerSession` (no Claude CLI, no tracker) reads `None`.
        self.session_mgr.get(&task_run_id.to_string()).map(|s| {
            s.last_activity_tracker()
                .load(std::sync::atomic::Ordering::Relaxed) as i64
        })
    }
}

// ============================================================================
// Dispatcher (injected — real = spawn a worker, test = record-only)
// ============================================================================

/// Why a dispatch did not produce a live worker — the distinction stall
/// accounting turns on.
///
/// A [`Failed`](Self::Failed) dispatch is one nothing but a change in this run
/// can fix: the worktree could not be acquired, the CLI would not spawn, the
/// `Working` upsert was refused. Re-deciding it produces the same failure
/// forever, so the row is fingerprinted and the run eventually stalls with the
/// reason.
///
/// The other two variants are TRANSIENT — they resolve without anything in this
/// run changing, so [`transient`](Self::transient) reads `true` and `tick_exit`
/// DROPS the row's fingerprint key instead of accruing stall time against it:
///
/// * [`DeferredByDrain`](Self::DeferredByDrain) — coord has drained this device
///   (or its drain state is unknown). Expected for as long as the drain holds,
///   so `apply_tick` logs it at `debug!` rather than warning once per queued
///   subtask every tick.
/// * [`Transient`](Self::Transient) — today, exactly the fleet-wide
///   `parallel_fanout` bound being fully occupied. The served clause the
///   admission implements says a bound breach "must never become a task failure
///   while sequential progress is still possible", and the conductor already
///   treats ready work beyond THIS RUN's own cap as legitimate waiting. Before
///   this type existed the two were one `String` and the fleet-wide arm was
///   counted as stuck — the same condition as the run-wide one, with the
///   opposite verdict, and a busy fan-out gate wrote a healthy run `stalled`
///   after `stall_after_secs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// Coord's device drain deferred it (plan
    /// `2026-09-13-drained-runner-never-reaches-idle`). TRANSIENT and expected
    /// for as long as the drain holds: the subtask stays `Submitted` and a later
    /// tick retries, so it is logged quietly rather than as a failure.
    DeferredByDrain(String),
    /// A failure that resolves on its own — the row is queued, not stuck. The
    /// subtask stays `Submitted` and a later tick retries once the bound frees.
    Transient(String),
    /// Anything else. The subtask also stays `Submitted` and is retried, but a
    /// retry re-decides it identically, so the row keeps its fingerprint and the
    /// run eventually stalls on it.
    Failed(String),
}

impl DispatchError {
    /// `true` for the two variants that resolve without anything in this run
    /// changing ([`Self::DeferredByDrain`] and [`Self::Transient`]). Such a row
    /// is waiting, not stuck, so `tick_exit` removes its fingerprint key.
    pub fn transient(&self) -> bool {
        matches!(
            self,
            DispatchError::DeferredByDrain(_) | DispatchError::Transient(_)
        )
    }
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::DeferredByDrain(m)
            | DispatchError::Transient(m)
            | DispatchError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<String> for DispatchError {
    fn from(message: String) -> Self {
        DispatchError::Failed(message)
    }
}

/// Dispatches a ready subtask to a worker. The live impl
/// ([`AiSessionDispatcher`]) calls Phase-2
/// [`ai_session_executor::dispatch_subtask`] (spawns the AI-session, persists
/// `task_run_id` + flips to `Working`). Tests inject a fake that records the
/// dispatch and persists the transition itself, so readiness/cap/resume logic
/// is exercised WITHOUT a live spawn.
#[async_trait]
pub trait Dispatcher: Send + Sync {
    async fn dispatch(&self, run_id: Uuid, subtask: &Subtask) -> Result<Uuid, DispatchError>;

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
    async fn dispatch(&self, run_id: Uuid, subtask: &Subtask) -> Result<Uuid, DispatchError> {
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
    /// `task_id`s the runner has no coord ANSWER about ([`blocked_on_coord`]):
    /// the last register / poll / drift-verdict call either could not be made
    /// (`coord_unreachable`) or was refused by a coord that answered
    /// (`coord_error`). They ALSO appear in `to_register_gate` / `to_poll_gate`
    /// / `to_verify_drift` (the call is retried every tick), and the rows are
    /// part of `stall_fingerprint`, so a run wedged on coord stalls visibly
    /// rather than ticking forever. The retry itself moves nothing, so it
    /// produces no [`TickOutcome`] entry either.
    pub blocked_on_coord: Vec<String>,
    /// `task_id`s the reconciler failed because the worker went SILENT while
    /// `Working` — no FSM edge and no CLI output for
    /// [`OrchestrationRunConfig::working_silence_secs`]. A subset of
    /// [`Self::to_fail`], carried separately so the log and the tests can name
    /// the reason (the other `to_fail` arms are `Errored` / `Gone` / no-report).
    pub silent_workers: Vec<String>,
    /// `task_id`s with a LIVE worker this tick is not ending: `Working` with a
    /// bound `task_run_id`, not decided `to_complete`/`to_fail`, and not itself
    /// blocked on coord. This is the run's "something is genuinely happening"
    /// term, and [`tick_exit`] uses it for one job only — refusing to end a run
    /// on a coord block alone while healthy work is in flight (a stall exit
    /// RETURNS from the reconciler and orphans those workers; `finish_run`
    /// touches no session).
    ///
    /// Rows blocked on coord are excluded deliberately: a `Working` verify
    /// subtask whose verdict read keeps failing is BOTH in flight and the stuck
    /// row, and counting it would let a coord block guard itself forever.
    pub in_flight_workers: Vec<String>,
    /// `true` when the run is finished (all terminal + nothing un-harvested).
    pub done: bool,
    /// Set when a hard error (e.g. DAG cycle) should terminate the run.
    pub fatal: Option<String>,
    /// The fingerprint of "stuck-relevant" state for stall detection (excludes
    /// legitimately-blocked/elaborating rows; contract §3.5).
    pub stall_fingerprint: StallFingerprint,
}

/// What one [`apply_tick`] actually LANDED — the durable outcome, not the
/// [`TickPlan`]'s intent.
///
/// The distinction is the whole point. A plan says what the reconciler decided
/// to do; an outcome says what the ledger now holds. Stall accounting reads the
/// OUTCOME, because every "we tried and it did not work" — a dispatch that
/// threw, a `set_subtask_state` that errored, a harvest that would not parse —
/// leaves the row exactly where it was and is re-decided identically on the
/// next tick. Reading the plan's intent as evidence of life is what let a run
/// re-decide the same failing action forever while `runs.status` read
/// `running`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// `task_id`s whose worker actually spawned (`task_run_id` bound, row
    /// `Working`).
    pub dispatched: Vec<String>,
    /// `task_id`s durably written `Completed`.
    pub completed: Vec<String>,
    /// `task_id`s durably written `Failed`.
    pub failed: Vec<String>,
    /// `<op>:<task_id>` for every side effect that was ATTEMPTED and did not
    /// land: `dispatch`, `complete`, `fail`, `harvest`, `reprompt`,
    /// `gate_persist`, `drift_store`. Each becomes an `E:` entry in the stall
    /// fingerprint, because such a row is stuck in exactly the sense §3.5 means
    /// — nothing is waiting on it, the reconciler simply cannot move it — and it
    /// is the one stuck shape that CANNOT be recorded on the row itself: when
    /// the durable write is the thing failing, a typed marker written the same
    /// way fails too.
    ///
    /// The list is exhaustive over `apply_tick`'s side effects on purpose. The
    /// three that were missing each had the same shape: a failure that left the
    /// row exactly where it was, in a lifecycle state nothing else
    /// fingerprints, so the identical decision was re-made every tick forever
    /// with the run reading `running`. A `reprompt` that never lands never arms
    /// the §5 deadline that would fail the row, and pins a concurrency slot; a
    /// `gate_persist` that never lands re-registers a NEW coord gate every tick
    /// (one leaked gate row per tick) or keeps polling a gate coord already
    /// cleared; a `drift_store` that never lands re-reads the same verdict
    /// forever on a row the §5 timers are diverted around.
    pub apply_failures: Vec<String>,
    /// `task_id`s whose side effect failed TRANSIENTLY — attempted, did not
    /// land, and will land on its own once a condition outside this run clears.
    /// Today that is exactly the two dispatch outcomes
    /// [`DispatchError::transient`] reads `true` for: the fleet's declared
    /// `parallel_fanout` bound being fully occupied
    /// ([`DispatchError::Transient`]), and coord having drained this device
    /// ([`DispatchError::DeferredByDrain`]).
    ///
    /// [`tick_exit`] drops the row's stall-fingerprint KEY entirely — not just
    /// the `E:` entry, because the row also carries an `R:` from the dispatch
    /// this tick decided. A row waiting for a slot is waiting, not stuck: the
    /// served clause the admission implements says "a bound breach must never
    /// become a task failure while sequential progress is still possible", and
    /// the conductor already treats ready work beyond THIS RUN's cap as
    /// legitimate waiting. Calling the same condition stuck because the bound
    /// is fleet-wide rather than run-wide was the same condition with the
    /// opposite verdict.
    pub transient_failures: Vec<String>,
    /// `true` when the run is finished (mirrors [`TickPlan::done`], which reads
    /// committed rows).
    pub done: bool,
}

impl TickOutcome {
    /// A one-line summary for the tick log. Empty string when nothing landed.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (label, v) in [
            ("dispatched", &self.dispatched),
            ("completed", &self.completed),
            ("failed", &self.failed),
            ("did-not-land", &self.apply_failures),
            ("waiting-for-a-slot", &self.transient_failures),
        ] {
            if !v.is_empty() {
                parts.push(format!("{label}={}", v.join(",")));
            }
        }
        parts.join(" ")
    }
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
    /// First tick we observed this worker `Working` (epoch s) — the FALLBACK
    /// base for the silence deadline, used only when the signal source reports
    /// no output stamp for it.
    first_working_at: HashMap<Uuid, i64>,
}

impl ReadyIdleTimers {
    /// Drop timers for task_run_ids no longer in-flight (housekeeping).
    fn retain_in_flight(&mut self, in_flight_ids: &HashSet<Uuid>) {
        self.first_ready_no_artifact_at
            .retain(|k, _| in_flight_ids.contains(k));
        self.reprompted_at.retain(|k, _| in_flight_ids.contains(k));
        self.first_gone_at.retain(|k, _| in_flight_ids.contains(k));
        self.first_working_at
            .retain(|k, _| in_flight_ids.contains(k));
    }

    /// Observe a worker seen `Working` this tick and return how long it has been
    /// SILENT — no FSM edge (any other signal clears the entry via
    /// [`Self::clear_working_silence`]) and no output.
    ///
    /// Measured from the worker's own last-output stamp when the source has one,
    /// because that is the truth about when it last did anything; a worker that
    /// emits resets its own stretch with no bookkeeping at all. Only a source
    /// that cannot observe activity falls back to "since we first saw it
    /// `Working`" — conservative for a source that really has no activity
    /// channel, and EARLY (see [`SignalSource::last_activity`]) for one that has
    /// activity it does not report.
    fn observe_working_silence(
        &mut self,
        task_run_id: Uuid,
        last_activity: Option<i64>,
        now: i64,
    ) -> i64 {
        let first_working = *self.first_working_at.entry(task_run_id).or_insert(now);
        let base = last_activity.unwrap_or(first_working);
        (now - base).max(0)
    }

    /// Forget a worker's silence stretch — called on every non-`Working` signal,
    /// which IS the observed FSM edge.
    fn clear_working_silence(&mut self, task_run_id: Uuid) {
        self.first_working_at.remove(&task_run_id);
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

        // §5 guard — complete ONLY when ReadyIdle AND artifact present. A
        // `Gone` worker whose report already landed completes the same way:
        // the report IS the completion, and the only thing lost with the
        // process is the idle signal. This is how a restart settles a worker
        // that reported just before the runner died — the boot sweep leaves
        // such rows `working` so they come through here, where a drift-verify
        // row still has its verdict read and an elaborator still harvests
        // before it completes.
        if can_complete(st, sig) || (sig == WorkerSignal::Gone && st.artifact.is_some()) {
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
                timers.clear_working_silence(trid);
                // Still in-flight for slot accounting until the verify resolves.
                continue;
            }
            timers.clear_working_silence(trid);
            plan.to_complete.push(st.task_id.clone());
            still_inflight -= 1;
            continue;
        }

        match sig {
            WorkerSignal::ReadyIdle => {
                // Ready but NO artifact (can_complete was false ⇒ artifact
                // absent). Step-5 recovery: re-prompt once after the first
                // timeout, fail after the second.
                timers.clear_working_silence(trid);
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
                // ...and hold the only bound a mid-turn worker has, which
                // reaches it only once it goes QUIET: every emitted line resets
                // this clock, so a wedged-but-chatty CLI is bounded by nothing
                // here (see `working_silence_secs`). `Working` covers
                // `Processing`/`Initializing`/`Interrupting`/`Promoting`/
                // `Created`, none of which any other timer reaches: the §5
                // deadlines fire from `ReadyIdle` or `Gone` only, and nothing in
                // `claude_session` moves a session out of `Processing` on a
                // clock. A wedged CLI would otherwise hold its concurrency slot
                // for the life of the run — and a chatty one still does.
                let silent_for =
                    timers.observe_working_silence(trid, signals.last_activity(trid), now);
                if silent_for >= config.working_silence_secs {
                    plan.to_fail.push(st.task_id.clone());
                    plan.silent_workers.push(st.task_id.clone());
                    timers.clear_working_silence(trid);
                    still_inflight -= 1;
                }
            }
            WorkerSignal::Errored => {
                timers.clear_working_silence(trid);
                plan.to_fail.push(st.task_id.clone());
                still_inflight -= 1;
            }
            WorkerSignal::Gone => {
                // Tolerate a transient restart window, then fail.
                timers.clear_working_silence(trid);
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
    // Rows whose last gate call failed for want of coord itself. They are in
    // one of the two lists above (retried), but tagged so the retry is not
    // counted as life and the row is fingerprinted (`C:`, see
    // `stall_fingerprint`; the retry lands in no `TickOutcome` either).
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    for &i in &order {
        let st = &subtasks[i];
        // Same predicate the fingerprint uses: a terminal row carrying a stale
        // token is history, not a block, and must not be named in a stall
        // reason.
        if !is_terminal(st.state) && blocked_on_coord(st) && deps_satisfied(st, &by_id) {
            plan.blocked_on_coord.push(st.task_id.clone());
        }
    }

    // The run's live work, for `tick_exit`'s in-flight guard ONLY (see
    // `TickPlan::in_flight_workers`). Rows this tick decided to end are not
    // live, and a row blocked on coord is the stuck row itself — counting
    // either would let a run guard itself open with nothing actually working.
    let ending: HashSet<&str> = plan
        .to_complete
        .iter()
        .chain(plan.to_fail.iter())
        .map(|s| s.as_str())
        .collect();
    plan.in_flight_workers = inflight
        .iter()
        .map(|s| s.task_id.clone())
        .filter(|t| !ending.contains(t.as_str()) && !plan.blocked_on_coord.contains(t))
        .collect();

    // Dispatch (step 3) — ready subtasks in topo order, capped by remaining
    // concurrency. `still_inflight` already reflects completions/failures
    // decided above so a tick can both complete and dispatch.
    //
    // The cap is `min(concurrency_cap, registry fanout_bound)` — this is where
    // the served clause's declared parallel fan-out bound actually binds, since
    // it is derived fresh from the durable rows every tick and therefore cannot
    // leak. Ready work beyond the cap stays queued for a later tick: the
    // clause's degrade-to-sequential, never a failure.
    let slots = config
        .effective_concurrency_cap()
        .saturating_sub(still_inflight);
    // Ready work the cap cannot admit this tick is waiting on a SLOT, which is
    // legitimate waiting in exactly the sense §3.5 means. That is why only the
    // rows this tick actually DECIDES to dispatch are fingerprinted — see
    // `stall_fingerprint`.
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

    // Stall fingerprint (step 3.5) — the state that is STUCK, not the state
    // that is busy. Everything legitimately waiting (a dependency that can still
    // be satisfied, an elaboration, a gate coord holds open, a concurrency slot,
    // a worker that is working) is excluded; a row the runner has no coord
    // ANSWER about, a row whose dependency can NEVER be satisfied, and a row
    // this tick decided to dispatch are included. See `stall_fingerprint`.
    plan.stall_fingerprint = stall_fingerprint(subtasks, &order, &plan.to_dispatch);

    // Catch-all (contract §3.5, the residual arm): a tick that decided NOTHING,
    // has NOTHING in flight, and is not done cannot be waiting on anything the
    // reconciler will ever observe — whatever the reason, nothing will move
    // again. This is the term that covers the shapes the per-row arms above do
    // not enumerate (an `InputRequired` row, which no arm dispatches, completes
    // or fails, is today's example). It fires ONLY when the fingerprint is
    // otherwise empty, so it never masks a specific diagnosis.
    let decided_nothing = plan.to_dispatch.is_empty()
        && plan.to_complete.is_empty()
        && plan.to_fail.is_empty()
        && plan.to_reprompt.is_empty()
        && plan.to_harvest.is_empty()
        && plan.to_register_gate.is_empty()
        && plan.to_poll_gate.is_empty()
        && plan.to_verify_drift.is_empty();
    if decided_nothing
        && inflight.is_empty()
        && !plan.done
        && plan.stall_fingerprint.is_empty()
        && !subtasks.is_empty()
    {
        plan.stall_fingerprint.push(
            NOTHING_ACTIONABLE_KEY.to_string(),
            format!(
                "Z:nothing-actionable:{}",
                subtasks
                    .iter()
                    .filter(|s| !is_terminal(s.state))
                    .map(|s| s.task_id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        );
    }

    plan
}

/// The set of rows that are STUCK this tick, each with a human-readable reason.
///
/// Two halves on purpose. [`keys`](Self::keys) — the stuck ROWS — is what
/// [`StallWatch`] measures continuity over; [`evidence`](Self::evidence) is what
/// the operator reads. Keying on the row rather than on the reason string is
/// load-bearing: a coord behind a load balancer whose instances answer 503 (→
/// `coord_unreachable`) and 422 (→ `coord_error`) alternately produced a reason
/// string that changed every tick, which restarted the window every 5 s and let
/// a 100%-blocked run tick forever without ever stalling. The row is stuck
/// either way; only the sentence differs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StallFingerprint {
    /// Sorted, deduped `(key, entry)` pairs: the key is a `task_id` (or
    /// [`NOTHING_ACTIONABLE_KEY`]) and the entry is that row's reason sentence
    /// (`C:`/`R:`/`B:`/`W!:`/`E:`/`Z:`). One row may carry several entries.
    ///
    /// They are stored PAIRED rather than as two independent lists because a
    /// key can leave the set for a reason its entries must follow
    /// ([`Self::remove_key`]), and because the reasons decide how a key is
    /// TIMED ([`Self::coord_only_keys`]). Two parallel lists can answer
    /// neither question.
    rows: Vec<(String, String)>,
}

/// The [`StallFingerprint`] key for the "nothing is actionable and nothing is in
/// flight" catch-all, which names no single row.
pub const NOTHING_ACTIONABLE_KEY: &str = "*";

impl StallFingerprint {
    /// Record one stuck row: `key` is what continuity is measured on, `entry`
    /// is the sentence the stall reason carries.
    pub fn push(&mut self, key: String, entry: String) {
        self.rows.push((key, entry));
        self.rows.sort();
        self.rows.dedup();
    }

    /// Drop a row from the stuck set entirely — its key AND every reason it
    /// carries. This is what "not stuck after all" means: leaving the key while
    /// dropping one entry would keep [`StallWatch`]'s window running on a row
    /// nothing can name a reason for, and leaving an entry while dropping the
    /// key would put a sentence in the operator's stall reason about a row the
    /// watch is not watching. See [`TickOutcome::transient_failures`].
    pub fn remove_key(&mut self, key: &str) {
        self.rows.retain(|(k, _)| k != key);
    }

    /// `true` when nothing is stuck.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The stuck rows, sorted and deduped — what [`StallWatch`] tracks over
    /// time.
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.rows.iter().map(|(k, _)| k.clone()).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// The stuck rows whose EVERY reason is a coord block (`C:` — the runner
    /// has no coord ANSWER about the row). Those are timed on
    /// [`OrchestrationRunConfig::coord_block_stall_after_secs`] instead of the
    /// row-sized window, because coord reachability is a runner-wide condition
    /// no row can influence.
    ///
    /// "EVERY reason" is the load-bearing part. A row that is coord-blocked AND
    /// carries an `E:` for a durable write that will not land is stuck for a
    /// reason of its own, and gets the short window — the long one is for rows
    /// whose ONLY problem is that coord is not answering.
    pub fn coord_only_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for key in self.keys() {
            if self
                .rows
                .iter()
                .filter(|(k, _)| *k == key)
                .all(|(_, e)| e.starts_with("C:"))
            {
                keys.push(key);
            }
        }
        keys
    }

    /// The human-readable reasons, `|`-joined — what the stall reason carries.
    pub fn evidence(&self) -> String {
        let mut entries: Vec<&str> = self.rows.iter().map(|(_, e)| e.as_str()).collect();
        entries.sort();
        entries.dedup();
        entries.join("|")
    }
}

/// Build the stall fingerprint: the rows that are STUCK in a way the reconciler
/// itself cannot move. Everything that is legitimately waiting — on a
/// dependency that can still be satisfied, on a gate coord is genuinely holding
/// open, on a concurrency slot, or on a worker that is actively working — is
/// excluded, so a healthy run produces an empty fingerprint and can never
/// accumulate a stall (contract §3.5).
///
/// **A `Working` worker is NOT fingerprinted.** Including it meant a perfectly
/// healthy worker emitted the same `W:<task_id>:<task_run_id>` every tick with
/// nothing completing, and the detector declared the run `stalled` — a durable,
/// operator-visible verdict on a live worker, roughly 25 s after dispatch and
/// 3.6x sooner than the recovery timers it contradicted. What bounds a `Working`
/// worker is NOT a fingerprint entry and NOT the §5 deadlines (those fire from
/// `ReadyIdle`/`Gone` only): it is the per-worker silence deadline
/// [`OrchestrationRunConfig::working_silence_secs`], which is reset by observed
/// output and so never false-positives on a long turn.
///
/// That reset is the same property read from the other side, and it is a real
/// limit rather than only a safety margin: a worker that is WEDGED but still
/// emitting resets its own deadline on every line, so `working_silence_secs`
/// bounds a worker that goes QUIET, not every `Working` worker. Nothing else
/// bounds the chatty-wedged case — see the `tick_exit` doc for what that costs
/// when coord is unreachable at the same time, and why holding the run open is
/// the deliberate choice there.
///
/// The entries, and why each one is stuck rather than waiting:
///
/// - `C:<token>:<task_id>` — the runner has no coord ANSWER about this row
///   ([`coord_block`]), in ANY lifecycle state (a `Working` DriftVerdict verify
///   whose verdict read keeps failing is the non-`Submitted` case). It is
///   retried forever with no bound of its own. The token is in the sentence but
///   NOT in the key — see [`StallFingerprint`].
/// - `R:<task_id>` — a row this tick DECIDED to dispatch. If it is still
///   `Submitted` on the next tick the dispatch did not land, and the reconciler
///   will re-decide it identically forever. Only the decided rows qualify:
///   ready work beyond the cap is waiting on a slot a live worker holds.
/// - `B:<task_id>` — a row whose dependency can NEVER be satisfied: the dep
///   names no row in this run, or it names one that is terminal and not
///   `Completed`. [`deps_satisfied`] will read `false` forever, so the row is
///   excluded from `ready_subtasks` and `all_terminal` never holds — the run
///   loops at `running` with no diagnosis. A dep that is merely not `Completed`
///   YET (`Submitted`/`Working`) is ordinary waiting and is excluded.
/// - `W!:<task_id>` — a `Working` row with no `task_run_id`. Defensive only:
///   `dispatch_subtask` writes the id and the state in ONE upsert and nothing
///   else writes `Working`, so no live path produces it. It stays because the
///   row would be unreconcilable if anything ever did.
/// - `E:<op>:<task_id>` — a side effect that was attempted and did not land
///   (added by [`tick_exit`] from the [`TickOutcome`], not here).
/// - `Z:nothing-actionable:<rows>` — the catch-all, added by [`compute_tick`].
fn stall_fingerprint(
    subtasks: &[Subtask],
    order: &[usize],
    to_dispatch: &[String],
) -> StallFingerprint {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    let mut fp = StallFingerprint::default();
    for &i in order {
        let s = &subtasks[i];
        // Blocked on coord (unreachable, or coord answered and refused) — in
        // ANY state, including a `Working` DriftVerdict verify whose verdict
        // read keeps failing. Nothing else bounds these rows.
        if !is_terminal(s.state) && deps_satisfied(s, &by_id) {
            if let Some(token) = coord_block(s) {
                fp.push(s.task_id.clone(), format!("C:{token}:{}", s.task_id));
                continue;
            }
        }
        if !is_terminal(s.state) {
            if let Some(dep) = unsatisfiable_dep(s, &by_id) {
                fp.push(s.task_id.clone(), format!("B:{}:{dep}", s.task_id));
                continue;
            }
        }
        match s.state {
            SubtaskState::Working if s.task_run_id.is_none() => {
                fp.push(s.task_id.clone(), format!("W!:{}", s.task_id));
            }
            SubtaskState::Submitted if to_dispatch.contains(&s.task_id) => {
                fp.push(s.task_id.clone(), format!("R:{}", s.task_id));
            }
            // Working, Submitted-but-blocked (deps, gate or slot), terminal, or
            // reserved: excluded.
            _ => {}
        }
    }
    fp
}

/// The first dependency of `subtask` that can NEVER be satisfied — one naming no
/// row in this run, or one naming a row that is terminal and not `Completed`
/// (`Failed` / `Canceled`). [`deps_satisfied`] will read `false` for this
/// subtask forever, so it is stuck rather than waiting.
fn unsatisfiable_dep<'a>(subtask: &'a Subtask, by_id: &HashMap<&str, &Subtask>) -> Option<&'a str> {
    subtask.depends_on.iter().find_map(|dep| {
        match by_id.get(dep.as_str()) {
            // Names no row in this run.
            None => Some(dep.as_str()),
            // Terminal and not Completed — it will never become Completed.
            Some(d) if is_terminal(d.state) && d.state != SubtaskState::Completed => {
                Some(dep.as_str())
            }
            Some(_) => None,
        }
    })
}

// ============================================================================
// Harvest — splice a completed elaborator's emitted children (LIVE)
// ============================================================================

/// **Harvest a completed elaborator's emitted child org-chart.** Called from
/// `apply_tick` on every elaborator completion (before the state flip) and
/// again for any Completed-but-unharvested elaborator (resume safety net).
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
/// Returns what actually LANDED — see [`TickOutcome`], which is what stall
/// accounting reads.
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
) -> Result<TickOutcome, String> {
    let by_id: HashMap<&str, &Subtask> = subtasks.iter().map(|s| (s.task_id.as_str(), s)).collect();
    let mut outcome = TickOutcome {
        done: plan.done,
        ..Default::default()
    };

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
                    // un-spliced seeds. A harvest that keeps failing (a PRESENT
                    // but malformed `next_subtasks`) would otherwise retry
                    // forever on a `Working` row nothing fingerprints, so the
                    // failure is carried into the stall fingerprint instead.
                    warn!(
                        "apply_tick: harvest {tid} failed ({e}); leaving Working, will retry next tick"
                    );
                    outcome.apply_failures.push(format!("harvest:{tid}"));
                    continue;
                }
            }
        }

        if let Err(e) = pg
            .set_subtask_state(run_id, tid, SubtaskState::Completed)
            .await
        {
            warn!("apply_tick: complete {tid}: {e}");
            outcome.apply_failures.push(format!("complete:{tid}"));
        } else {
            info!("conductor: {tid} Working → Completed (run {run_id})");
            outcome.completed.push(tid.clone());
        }
    }

    // Failures.
    for tid in &plan.to_fail {
        if let Err(e) = pg
            .set_subtask_state(run_id, tid, SubtaskState::Failed)
            .await
        {
            warn!("apply_tick: fail {tid}: {e}");
            outcome.apply_failures.push(format!("fail:{tid}"));
        } else if plan.silent_workers.contains(tid) {
            warn!(
                "conductor: {tid} Working → Failed (run {run_id}) — no FSM edge and no worker \
                 output for {}s (working_silence_secs); the session is left alone, only the \
                 subtask and its concurrency slot are released",
                config.working_silence_secs
            );
            outcome.failed.push(tid.clone());
        } else {
            warn!("conductor: {tid} Working → Failed (run {run_id})");
            outcome.failed.push(tid.clone());
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
            Err(e) => {
                // A re-prompt that did not land is the §5 recovery path failing
                // OPEN, and it is unbounded on its own: `reprompted_at` is only
                // inserted on success, so the SECOND deadline — the one that
                // FAILS the row — is never armed, and the next tick re-decides
                // the identical re-prompt forever. Nothing else reaches the row
                // either: it is `Working` with a `task_run_id` (so the
                // fingerprint excludes it) and the worker is `ReadyIdle` (so
                // the silence deadline was cleared). It holds its concurrency
                // slot for the life of the run, which then excludes every other
                // ready row as "waiting on a slot".
                warn!("apply_tick: reprompt {tid}: {e}");
                outcome.apply_failures.push(format!("reprompt:{tid}"));
            }
        }
    }

    // Dispatches (step 3).
    for tid in &plan.to_dispatch {
        if let Some(st) = by_id.get(tid.as_str()) {
            match dispatcher.dispatch(run_id, st).await {
                Ok(trid) => {
                    info!("conductor: dispatched {tid} → worker {trid} (run {run_id})");
                    outcome.dispatched.push(tid.clone());
                }
                // Expected for as long as coord's device drain holds; every
                // tick would otherwise WARN once per queued subtask. Recorded
                // as a transient failure all the same: the row is waiting on a
                // drain that will lift, not stuck, so `tick_exit` drops its
                // fingerprint KEY and a held drain can never write a healthy
                // run `stalled`.
                Err(DispatchError::DeferredByDrain(reason)) => {
                    debug!("apply_tick: dispatch {tid} deferred by the device drain: {reason}");
                    outcome.transient_failures.push(tid.clone());
                }
                Err(e) if e.transient() => {
                    // The fleet's declared fan-out bound was full. The subtask
                    // stays `Submitted` and a later tick dispatches it once a
                    // slot frees — waiting, not stuck, exactly as ready work
                    // beyond THIS RUN's own cap is. `tick_exit` drops the row's
                    // fingerprint KEY (the `R:` this tick put there), so a busy
                    // gate can never write a healthy run `stalled`.
                    info!("conductor: {tid} queued — {e}");
                    outcome.transient_failures.push(tid.clone());
                }
                Err(e) => {
                    // The subtask stays `Submitted` (`dispatch_subtask` flips it
                    // only on success) so a later tick retries — and re-decides
                    // it identically, forever, if the cause is permanent. The
                    // row is already `R:`-fingerprinted (this tick decided to
                    // dispatch it) and the non-outcome is what stops that retry
                    // reading as life.
                    //
                    // NOT "before the worker was live", which this comment used
                    // to claim: `dispatch_subtask` spawns the CLI, acquires the
                    // worktree, registers the session and binds coord identity
                    // BEFORE it upserts `task_run_id` + `Working`, so a PG
                    // refusal at that last step fails with a fully live worker.
                    // It is `dispatch_subtask`'s own teardown — not this retry —
                    // that makes re-dispatch safe: it closes and unregisters the
                    // session it could not bind before returning `Err`. Without
                    // that, a 5s tick against a 300s stall window spawned ~60
                    // orphaned CLI sessions and took ~60 worktree acquisitions
                    // before the run stalled.
                    warn!("apply_tick: dispatch {tid}: {e}");
                    outcome.apply_failures.push(format!("dispatch:{tid}"));
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
                Err(e) => {
                    warn!("apply_tick: harvest {tid}: {e}");
                    outcome.apply_failures.push(format!("harvest:{tid}"));
                }
            }
        }
    }

    // ---- Phase 6: coord gate registration (best-effort) -----------------
    // Register a gate for each classified subtask and persist gate_id +
    // status=open. The subtask STAYS `Submitted` (no new state variant) — the
    // gate_id column annotates it blocked. On a register failure the row stays
    // ungated so the NEXT tick retries registration, and the failure is RECORDED
    // on the row as the typed block `gate_status = coord_unreachable` (no device
    // JWT / transport down / a 401) or `coord_error` (coord answered and refused
    // the call), so EVERY failure is visible per-subtask and counts toward a
    // stall. A failure that changed nothing on the row was the hole: the retry
    // read as progress and the run polled forever with nothing surfaced.
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
                    // coord registered a gate this row will never be told
                    // about: it stays ungated, so `needs_gate_registration`
                    // still holds and the NEXT tick registers ANOTHER one. One
                    // leaked coord gate row per tick, with the run reading
                    // `running` and nothing on the subtask to show for it —
                    // precisely the shape `E:` exists for, since the failing
                    // write IS the durable marker.
                    warn!("apply_tick: persist gate for {tid}: {e}");
                    outcome.apply_failures.push(format!("gate_persist:{tid}"));
                } else {
                    info!(
                        "conductor: {tid} blocked on {} gate {gate_id} (run {run_id})",
                        predicate.kind_label()
                    );
                }
            }
            Err(e) => {
                let token = coord_block_token(&e);
                warn!(
                    "apply_tick: register {} gate for {tid}: {e}; recording {token}, will retry next tick",
                    predicate.kind_label()
                );
                record_coord_block(pg, run_id, st, token, &mut outcome).await;
            }
        }
    }

    // ---- Phase 6: poll open gates (re-attach + clearance) ----------------
    // Poll each blocked-on-gate subtask and persist the new status. `cleared`
    // unblocks dispatch on the NEXT tick (the column is the durable record);
    // `failed` fails the subtask on the next tick (`gate_failed`). Every poll
    // that returns `Ok` carries a verdict coord actually gave, and persisting it
    // is what clears a typed block on recovery. A poll that produced no answer
    // — including one whose verdict token the runner does not understand, which
    // is no more an answer than a 500 — is recorded as the typed
    // block `coord_unreachable` / `coord_error` (gate_id kept, so recovery
    // resumes polling the same gate) and counts toward a stall — it is NEVER
    // written back as `open`, which would be a coord verdict coord never gave.
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
                        // coord gave a verdict the row will never carry. A
                        // `cleared` that does not persist leaves `gate_blocked`
                        // true, so the row is excluded from the fingerprint as
                        // legitimate waiting — forever, on a gate coord has
                        // already opened.
                        warn!("apply_tick: persist gate status for {tid}: {e}");
                        outcome.apply_failures.push(format!("gate_persist:{tid}"));
                    } else if matches!(status, GateStatus::Cleared) {
                        info!("conductor: gate {gate_id} CLEARED — {tid} now dispatchable (run {run_id})");
                    } else if matches!(status, GateStatus::Failed) {
                        warn!("conductor: gate {gate_id} FAILED — {tid} will be failed (run {run_id})");
                    }
                }
            }
            Err(e) => {
                let token = coord_block_token(&e);
                warn!("apply_tick: poll gate {gate_id} for {tid}: {e}; recording {token}");
                record_coord_block(pg, run_id, st, token, &mut outcome).await;
            }
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
                clear_coord_block(pg, run_id, st, &mut outcome).await;
                if let Err(e) = store_drift_verdict(pg, run_id, st, &subspace, "none").await {
                    // The verdict is read and thrown away: the row stays
                    // ReadyIdle-with-artifact and no `drift_verdict` key, so the
                    // next tick reads the SAME verdict and drops it again. A
                    // verify row is diverted before the §5 timers, so nothing
                    // else bounds it.
                    warn!("apply_tick: store no-drift verdict for {tid}: {e}");
                    outcome.apply_failures.push(format!("drift_store:{tid}"));
                    continue;
                }
                if let Err(e) = pg
                    .set_subtask_state(run_id, tid, SubtaskState::Completed)
                    .await
                {
                    warn!("apply_tick: complete verify {tid}: {e}");
                    outcome.apply_failures.push(format!("complete:{tid}"));
                } else {
                    info!("conductor: verify {tid} — DriftVerdict[{subspace}] shows NO DRIFT → Completed (run {run_id})");
                }
            }
            Ok(DriftClass::Drift(class)) => {
                clear_coord_block(pg, run_id, st, &mut outcome).await;
                if let Err(e) = store_drift_verdict(pg, run_id, st, &subspace, &class).await {
                    // Same shape as the no-drift arm: a verdict read forever,
                    // recorded never, on a row no deadline reaches.
                    warn!("apply_tick: store drift verdict for {tid}: {e}");
                    outcome.apply_failures.push(format!("drift_store:{tid}"));
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
                // A verdict read failure leaves the verify subtask
                // ReadyIdle+artifact (no verdict recorded) so a later tick
                // retries the read — never wedges, never fabricates a verdict.
                // The failure IS recorded on the row as the same typed block the
                // gate calls use: a verify has no recovery deadline of its own
                // (it is diverted before the §5 timers), so a read that keeps
                // failing would otherwise retry forever, silently, with the
                // stall reason naming only the row and no hint that the twin
                // read was the thing failing.
                let token = coord_block_token(&e);
                warn!("apply_tick: drift verdict[{subspace}] for {tid}: {e}; recording {token}");
                record_coord_block(pg, run_id, st, token, &mut outcome).await;
            }
        }
    }

    Ok(outcome)
}

/// The durable `gate_status` token a [`CoordGateError`] is recorded as:
/// [`GATE_STATUS_COORD_UNREACHABLE`] when the runner could not ask coord at all
/// (no credential, dead transport, a 401/403/5xx), [`GATE_STATUS_COORD_ERROR`]
/// when coord answered and refused the call. Both mean "no answer about this
/// row"; the token only decides which sentence the operator reads.
fn coord_block_token(err: &CoordGateError) -> &'static str {
    if err.is_unreachable() {
        GATE_STATUS_COORD_UNREACHABLE
    } else {
        GATE_STATUS_COORD_ERROR
    }
}

/// Persist the typed coord block on a subtask, keeping whatever `gate_id` the
/// row has (a registered gate resumes polling once coord is back; an
/// unregistered one is re-registered — `needs_gate_registration` still holds).
/// Idempotent: a row already carrying THIS token is not rewritten, while a row
/// carrying the OTHER one is corrected, so the operator never reads "pair this
/// runner" about a coord that is answering.
///
/// A write that FAILS here is the `E:` mechanism's own justifying case — the
/// durable marker for "no coord answer" cannot be written because the durable
/// store is what is failing — so it is recorded in the [`TickOutcome`] instead.
/// Without it the row keeps whatever `gate_status` it had (usually none), so an
/// unregistered row registers a NEW coord gate every tick and a polled one is
/// read as legitimately waiting, with nothing anywhere saying the run cannot
/// move.
async fn record_coord_block(
    pg: &Arc<PgDb>,
    run_id: Uuid,
    subtask: &Subtask,
    token: &str,
    outcome: &mut TickOutcome,
) {
    if subtask.gate_status.as_deref() == Some(token) {
        return;
    }
    if let Err(e) = pg
        .set_subtask_gate(
            run_id,
            &subtask.task_id,
            subtask.gate_id.as_deref(),
            Some(token),
        )
        .await
    {
        warn!("apply_tick: persist {token} for {}: {e}", subtask.task_id);
        outcome
            .apply_failures
            .push(format!("gate_persist:{}", subtask.task_id));
    }
}

/// Clear a typed coord block after a call that DID get an answer, on a path
/// that writes no `gate_status` of its own (the DriftVerdict read; the gate
/// paths clear it by writing the polled/registered status). A no-op on a row
/// that carries no block.
///
/// Without this a row tagged while the runner was unpaired kept the token
/// forever once it was paired: the row stayed in the stall fingerprint, and the
/// eventual stall reason pointed the operator at pairing while coord was up.
async fn clear_coord_block(
    pg: &Arc<PgDb>,
    run_id: Uuid,
    subtask: &Subtask,
    outcome: &mut TickOutcome,
) {
    let Some(stale) = coord_block(subtask) else {
        return;
    };
    if let Err(e) = pg
        .set_subtask_gate(run_id, &subtask.task_id, subtask.gate_id.as_deref(), None)
        .await
    {
        // The row keeps a coord block coord has just DISPROVED, which is worse
        // than an unrecorded one: the stall reason would point the operator at
        // coord while coord is answering.
        warn!("apply_tick: clear {stale} for {}: {e}", subtask.task_id);
        outcome
            .apply_failures
            .push(format!("gate_persist:{}", subtask.task_id));
    }
}

// ============================================================================
// Run exit — the terminal outcome written to `orchestration.runs`
// ============================================================================

/// The terminal outcome of a conductor run, and the ONLY thing that may end
/// the run loop. Every variant is written to `orchestration.runs.status` (+
/// `status_reason`) through [`finish_run`] before the loop returns, so the
/// durable row and the in-memory [`LoopPhase`] can never disagree about
/// whether a run is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunExit {
    /// All subtasks terminal and nothing un-harvested.
    Complete,
    /// A hard error: a DAG cycle, or the DESIGN bootstrap failing before a
    /// single subtask existed. `reason` is the error text.
    Failed { reason: String },
    /// The stall detector fired over an unchanged actionable fingerprint.
    /// `reason` names the pattern and any subtasks blocked on coord.
    Stalled { reason: String },
}

impl RunExit {
    /// `orchestration.runs.status` token for a completed run.
    pub const STATUS_COMPLETE: &'static str = "complete";
    /// `orchestration.runs.status` token for a failed run.
    pub const STATUS_FAILED: &'static str = "failed";
    /// `orchestration.runs.status` token for a stalled run.
    pub const STATUS_STALLED: &'static str = "stalled";

    /// A fatal tick error (today: the DAG cycle `topo_order` surfaces).
    pub fn failed(reason: impl Into<String>) -> Self {
        RunExit::Failed {
            reason: reason.into(),
        }
    }

    /// The DESIGN bootstrap failed, so the run has no subtasks to drive. The
    /// run is `failed` with the design error — never "complete on zero
    /// subtasks", which is what ticking an empty run to done would say.
    pub fn design_failed(error: impl std::fmt::Display) -> Self {
        RunExit::Failed {
            reason: format!("DESIGN bootstrap failed: {error}"),
        }
    }

    /// The stall watch fired. `blocked_on_coord` (from the tick's plan) is
    /// folded into the reason so the row names the rows that did not move; the
    /// evidence itself carries the fingerprint, whose `C:<token>:` entries say
    /// WHICH coord failure each one hit.
    pub fn stalled(evidence: impl std::fmt::Display, blocked_on_coord: &[String]) -> Self {
        let mut reason = format!("Stall detected: {evidence}");
        if !blocked_on_coord.is_empty() {
            reason.push_str(&format!(
                "; blocked on coord: {}",
                blocked_on_coord.join(", ")
            ));
        }
        RunExit::Stalled { reason }
    }

    /// The `orchestration.runs.status` token this exit writes.
    pub fn status(&self) -> &'static str {
        match self {
            RunExit::Complete => Self::STATUS_COMPLETE,
            RunExit::Failed { .. } => Self::STATUS_FAILED,
            RunExit::Stalled { .. } => Self::STATUS_STALLED,
        }
    }

    /// The `orchestration.runs.status_reason` this exit writes (`None` for a
    /// completed run).
    pub fn reason(&self) -> Option<&str> {
        match self {
            RunExit::Complete => None,
            RunExit::Failed { reason } | RunExit::Stalled { reason } => Some(reason),
        }
    }

    /// The live [`LoopPhase`] mirroring this exit for the status strip. There
    /// is no `Stalled` phase in the shared enum; a stall is an `Error` whose
    /// reason is carried in `LoopState::error` (and durably in `status_reason`).
    pub fn loop_phase(&self) -> LoopPhase {
        match self {
            RunExit::Complete => LoopPhase::Complete,
            RunExit::Failed { .. } | RunExit::Stalled { .. } => LoopPhase::Error,
        }
    }
}

/// What a fired [`StallWatch`] saw: the stuck rows, the reasons, and how long
/// the longest-stuck row has been continuously stuck. This is the whole stall
/// verdict — a run is stalled when a row the reconciler cannot move has been
/// stuck for `stall_after_secs` of wall clock, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallEvidence {
    /// The reason entries of the stuck set (see [`stall_fingerprint`]).
    pub fingerprint: String,
    /// The keys that have been stuck for the full window — the rows that fired.
    pub stuck_keys: Vec<String>,
    /// Wall-clock seconds the longest-stuck of those has been continuously
    /// stuck.
    pub unchanged_for_secs: i64,
}

impl std::fmt::Display for StallEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} stuck for {}s: {}",
            self.stuck_keys.join(","),
            self.unchanged_for_secs,
            self.fingerprint
        )
    }
}

/// Wall-clock stall accounting over the conductor's own fingerprint — the ONE
/// rule that may write a run `stalled`.
///
/// It replaces the shared [`StallDetector`](super::stall_detector::StallDetector)
/// here, which the conductor was feeding a `None` output hash: that made three
/// of its five arms count TICKS rather than unchanged state (its no-progress arm
/// fired on the 3rd record, its timeout arm measured from construction and never
/// reset, its step-limit arm counted 100 records of any kind), so the effective
/// verdict was "~20 s of anything the fingerprint mentions". A conductor tick is
/// 5 s and a worker's own recovery deadline is 90 s; a stall rule must be sized
/// against the second, not the first. The shared detector stays where it fits —
/// `loop_engine`'s workflow/pipeline loops, whose actions really are discrete
/// repeated steps.
///
/// One piece of cross-tick memory, rebuilt from empty on restart (the worst case
/// is that the window restarts, which errs toward NOT declaring a stall).
///
/// **It tracks each stuck row SEPARATELY, and there is no run-wide progress
/// veto.** Both properties exist because a single run-wide window is maskable:
/// with one window over the whole set, a run where row X is permanently stuck
/// and row Y keeps moving resets the window every time Y moves, so X never
/// accumulates a second — which is the defect this phase exists to remove,
/// re-created one level up. Per-row continuity needs no progress term at all:
/// [`stall_fingerprint`] already excludes everything that is legitimately
/// waiting, so a row that stays in it for the whole window is stuck whatever
/// the rest of the run is doing, and a row that gets unstuck simply leaves the
/// set.
#[derive(Debug, Default)]
pub struct StallWatch {
    /// Per stuck key, the epoch second it has been CONTINUOUSLY stuck since.
    /// A key absent from a tick's fingerprint is dropped, so re-appearing
    /// restarts its window.
    seen_since: HashMap<String, i64>,
}

impl StallWatch {
    /// Observe one tick. Returns evidence once ANY single key has been stuck for
    /// its own window without interruption: `coord_window_secs` for a key whose
    /// ONLY reasons are coord blocks ([`StallFingerprint::coord_only_keys`]),
    /// `window_secs` for every other key.
    ///
    /// Both windows run off the SAME continuity memory, so a row that is coord
    /// blocked for a while and then stuck for a reason of its own does not get
    /// a fresh window — only its deadline shortens.
    pub fn observe(
        &mut self,
        fingerprint: &StallFingerprint,
        now: i64,
        window_secs: i64,
        coord_window_secs: i64,
    ) -> Option<StallEvidence> {
        let keys = fingerprint.keys();
        let coord_only = fingerprint.coord_only_keys();
        // Drop anything no longer stuck — that row moved, so its window is over.
        self.seen_since.retain(|k, _| keys.contains(k));
        let mut fired: Vec<(String, i64)> = Vec::new();
        for key in &keys {
            let since = *self.seen_since.entry(key.clone()).or_insert(now);
            let stuck_for = now - since;
            let window = if coord_only.contains(key) {
                coord_window_secs
            } else {
                window_secs
            };
            if stuck_for >= window {
                fired.push((key.clone(), stuck_for));
            }
        }
        if fired.is_empty() {
            return None;
        }
        let unchanged_for_secs = fired.iter().map(|(_, s)| *s).max().unwrap_or(0);
        let mut stuck_keys: Vec<String> = fired.into_iter().map(|(k, _)| k).collect();
        stuck_keys.sort();
        Some(StallEvidence {
            fingerprint: fingerprint.evidence(),
            stuck_keys,
            unchanged_for_secs,
        })
    }

    /// The keys currently under watch, sorted (diagnostics + tests).
    pub fn watching(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.seen_since.keys().cloned().collect();
        keys.sort();
        keys
    }
}

/// Decide whether this tick ENDS the run. Pure over the plan, the tick's APPLIED
/// [`TickOutcome`] and the watch's one piece of memory, so tests drive it tick by
/// tick:
///
/// - a fatal plan (a DAG cycle) → [`RunExit::Failed`] with the error;
/// - any single stuck row held for `config.stall_after_secs` → [`RunExit::Stalled`]
///   with the evidence (and the coord-blocked rows).
///
/// It runs AFTER `apply_tick`, not before, because the outcome is what decides
/// whether the tick moved anything — see [`TickOutcome`]. The outcome also
/// CONTRIBUTES fingerprint entries: an `E:<op>:<task_id>` for every side effect
/// that was attempted and did not land. That is the one stuck shape a durable
/// per-row marker cannot record (when the row write is what is failing, so is
/// the marker), so it is carried in memory instead, and because the same failure
/// recurs every tick the entry is stable and accumulates exactly like any other.
/// It also REMOVES rows: a side effect that failed transiently
/// ([`TickOutcome::transient_failures`]) takes the whole row out of the stuck
/// set, `R:` entry included.
///
/// A tick whose only "no progress" is legitimate waiting — a working worker, an
/// open gate, a dependency that can still be satisfied, a saturated concurrency
/// cap, a fleet fan-out bound that is momentarily full — contributes NO
/// fingerprint entry at all, so a healthy run can never be written `stalled`.
///
/// One further refusal, the in-flight guard: a run is NOT ended on coord blocks
/// ALONE while it still has live workers. A stall exit is terminal and the
/// reconciler RETURNS, so it orphans every session the run spawned
/// (`finish_run` touches no session); doing that to three healthy workers
/// because a fourth row's gate poll cannot reach coord trades the whole run for
/// a diagnosis.
///
/// The guard does not wedge in any case the reconciler can observe, but the
/// bound is weaker than "every `Working` row times out". A `Working` row is
/// bounded by [`OrchestrationRunConfig::working_silence_secs`] OR by the §5
/// recovery deadlines, whichever reaches it first — and neither is guaranteed
/// to: the silence deadline measures from `SignalSource::last_activity`, which
/// every emitted CLI line resets, and the §5 deadlines fire only from
/// `ReadyIdle`/`Gone`. So a worker that is wedged but still CHATTY resets its
/// own deadline forever, and with coord unreachable at the same time the run
/// holds open instead of stalling at `coord_block_stall_after_secs`.
///
/// That is deliberately the safer of the two failure modes — a stall exit is
/// terminal and orphans the sessions, while holding open leaves them reachable
/// — it warns every tick, and `stop_orchestration_run` still ends the run. In
/// every case where a `Working` row DOES reach a deadline, the in-flight set
/// drains on its own, and because the window keeps accumulating underneath the
/// guard, the stall fires the moment it does.
pub fn tick_exit(
    plan: &TickPlan,
    outcome: &TickOutcome,
    watch: &mut StallWatch,
    now: i64,
    config: &OrchestrationRunConfig,
) -> Option<RunExit> {
    if let Some(fatal) = &plan.fatal {
        return Some(RunExit::failed(fatal.clone()));
    }
    let mut fingerprint = plan.stall_fingerprint.clone();
    for failure in &outcome.apply_failures {
        // `<op>:<task_id>` — the key is the row, so a row that fails a dispatch
        // one tick and a state write the next is continuously stuck, not two
        // half-windows.
        let task_id = failure.split_once(':').map(|(_, t)| t).unwrap_or(failure);
        fingerprint.push(task_id.to_string(), format!("E:{failure}"));
    }
    // ...and a row whose only failure was transient is not stuck at all. The
    // KEY goes, not just the `E:` entry: the row is `R:`-fingerprinted too (this
    // tick decided to dispatch it), and leaving that behind would stall the run
    // on exactly the condition the removal exists to forgive.
    for tid in &outcome.transient_failures {
        fingerprint.remove_key(tid);
    }
    let evidence = watch.observe(
        &fingerprint,
        now,
        config.stall_after_secs,
        config.coord_block_stall_after_secs,
    )?;
    if !plan.in_flight_workers.is_empty() {
        let coord_only = fingerprint.coord_only_keys();
        if evidence.stuck_keys.iter().all(|k| coord_only.contains(k)) {
            warn!(
                "conductor: holding the run OPEN despite {} — every stuck row is blocked on \
                 coord and {} worker(s) are still live ({}). A coord block alone does not end a \
                 run with work in flight; the window keeps running underneath.",
                evidence,
                plan.in_flight_workers.len(),
                plan.in_flight_workers.join(",")
            );
            return None;
        }
    }
    Some(RunExit::stalled(evidence, &plan.blocked_on_coord))
}

/// Write a run's terminal outcome — the durable row FIRST (`status` +
/// `status_reason` through [`PgDb::set_run_status_if_running`]), then the
/// in-memory loop state the status strip reads while the process lives. This is
/// the single exit path of [`run_orchestration`]; a `return` from the loop that
/// bypasses it leaves the row lying `running`, which is the defect this exists
/// to close.
///
/// It can only close it for an exit the reconciler REACHES: a killed or crashed
/// runner never runs this, and no boot-time sweep reconciles the row it left
/// behind (module docs, step 7).
///
/// The write is CONDITIONAL on the row still reading `running`, the same guard
/// `stop_orchestration_run` uses and for the mirror-image reason. The two
/// writers race one tick wide: the operator presses Stop, the row moves
/// `running → stopped`, and the reconciler — already mid-tick with its exit
/// decided — would overwrite that with `stalled`/`complete`. Whoever leaves
/// `running` first wins, and the loser says so instead of clobbering.
async fn finish_run(pg: &Arc<PgDb>, loop_state: &SharedLoopState, run_id: Uuid, exit: RunExit) {
    match &exit {
        RunExit::Complete => info!("conductor: run {run_id} complete — all subtasks terminal"),
        RunExit::Failed { reason } => error!("conductor: run {run_id} failed: {reason}"),
        RunExit::Stalled { reason } => warn!("conductor: run {run_id} stalled: {reason}"),
    }
    match pg
        .set_run_status_if_running(run_id, exit.status(), exit.reason())
        .await
    {
        Ok(true) => {}
        Ok(false) => warn!(
            "conductor: run {run_id} was stopped out from under the reconciler; keeping \
             `stopped` rather than writing status={}",
            exit.status()
        ),
        Err(e) => error!(
            "conductor: run {run_id} could not persist status={} (row still reads running): {e}",
            exit.status()
        ),
    }
    let mut st = loop_state.lock().await;
    st.error = exit.reason().map(str::to_string);
    st.phase = exit.loop_phase();
    st.running = false;
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
/// Drives the run to terminal state. Every exit but an operator stop goes
/// through [`finish_run`], which writes `orchestration.runs.status` +
/// `status_reason` (`complete` / `failed` / `stalled`) before updating the
/// [`SharedLoopState`] phase for the status surface. (A stop is written as
/// `stopped` by `loop_engine::stop_orchestration_run`, the one that sends the
/// signal.)
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
    mut config: OrchestrationRunConfig,
    mut stop_rx: watch::Receiver<bool>,
) {
    info!(
        "conductor: run {run_id} starting (cap={}, tick={}s)",
        config.concurrency_cap, config.tick_interval_secs
    );

    // Stall watch — fires only on the conductor's OWN stuck-state fingerprint,
    // and only after it has held unchanged for `stall_after_secs` of wall
    // clock. Never on legitimate waiting (a working worker, an open gate, an
    // unmet dep, a saturated cap all produce no fingerprint entry at all).
    let mut stall_watch = StallWatch::default();

    // The ONLY cross-tick memory (recovery debounce timers; rebuilt on restart).
    let mut timers = ReadyIdleTimers::default();
    let mut tick: u32 = 0;

    loop {
        if *stop_rx.borrow() {
            info!("conductor: run {run_id} stopped by signal");
            // Record the stop HERE, not only at the door that sent it:
            // `stop_all_loops` and `stop_loop_by_id` signal this loop and write
            // nothing, and a row left `running` is one the boot sweep
            // relaunches on the next start (Phase 2 of
            // `2026-09-23-conductor-e2e-phase1-defects`). Conditional, so a
            // terminal verdict already written — or `stop_orchestration_run`'s
            // own identical write — is never overwritten.
            if let Err(e) = pg
                .set_run_status_if_running(run_id, "stopped", Some("stop requested"))
                .await
            {
                warn!("conductor: run {run_id} could not record its stop: {e}");
            }
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

        // Refresh the registry's declared parallel fan-out bound for THIS tick.
        // Served clause `agent-spawn-authorization`: "Parallel fan-out: bound
        // declared up front (registry default 15)." It is re-read rather than
        // captured at run start so a user changing it mid-run takes effect, and
        // it is folded into the dispatch cap below (never carried in memory as
        // an in-flight count, which is what makes it leak-proof). Cache-backed
        // — one coord round-trip per registry TTL, not per tick.
        let refreshed_bound = crate::agent_authorization::current_fanout_bound(None).await;
        if config.fanout_bound != Some(refreshed_bound) {
            info!(
                "conductor: run {run_id} parallel fan-out bound {:?} → {refreshed_bound} \
                 (effective dispatch cap {})",
                config.fanout_bound,
                config
                    .concurrency_cap
                    .min(refreshed_bound.max(1) as usize)
                    .max(1)
            );
            config.fanout_bound = Some(refreshed_bound);
        }

        let now = Utc::now().timestamp();
        let plan = compute_tick(&subtasks, &signals, &mut timers, &config, now);

        // Surface dispatching phase when we are about to spawn workers.
        if !plan.to_dispatch.is_empty() {
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::DispatchingWorker;
        }
        if !plan.to_harvest.is_empty() {
            let mut st = loop_state.lock().await;
            st.phase = LoopPhase::Elaborating;
        }

        // Apply FIRST, then decide the exit from what actually landed. The
        // order is load-bearing: stall accounting reads the APPLIED outcome
        // (see `TickOutcome`), and a tick's intent — "we decided to dispatch
        // it", "we decided to poll that gate" — is not evidence anything moved.
        let outcome = match apply_tick(
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
            Ok(o) => o,
            Err(e) => {
                error!("conductor: run {run_id} apply_tick failed: {e}");
                TickOutcome::default()
            }
        };
        let summary = outcome.summary();
        if !summary.is_empty() {
            info!("conductor: run {run_id} tick {tick} applied: {summary}");
        }

        if outcome.done {
            finish_run(&pg, &loop_state, run_id, RunExit::Complete).await;
            return;
        }

        // Fatal (DAG cycle) or stall (§3.5, over the stuck-rows-only
        // fingerprint; see `tick_exit`) ends the run — and is WRITTEN to the
        // run row before the loop returns, so the ledger says `failed` /
        // `stalled` with the reason rather than lying `running` forever.
        if let Some(exit) = tick_exit(&plan, &outcome, &mut stall_watch, now, &config) {
            finish_run(&pg, &loop_state, run_id, exit).await;
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

    /// Scripted signal source keyed by task_run_id. Reports NO activity stamp —
    /// the conservative arm, where the `Working`-silence deadline measures from
    /// the first tick a row was seen `Working`. Use [`FakeActivity`] to script
    /// the output stamps that reset it.
    struct FakeSignals(Map<Uuid, WorkerSignal>);
    impl SignalSource for FakeSignals {
        fn signal(&self, task_run_id: Uuid) -> WorkerSignal {
            self.0
                .get(&task_run_id)
                .copied()
                .unwrap_or(WorkerSignal::Gone)
        }
        fn last_activity(&self, _task_run_id: Uuid) -> Option<i64> {
            None
        }
    }

    /// A signal source that also scripts the worker's OUTPUT stamp — the thing
    /// that distinguishes a busy mid-turn worker from a wedged one, since both
    /// sit at `WorkerSignal::Working` forever.
    struct FakeActivity {
        signal: WorkerSignal,
        activity: StdMutex<i64>,
    }
    impl FakeActivity {
        fn working(at: i64) -> Self {
            FakeActivity {
                signal: WorkerSignal::Working,
                activity: StdMutex::new(at),
            }
        }
        /// The worker emitted a line at `at`.
        fn emit(&self, at: i64) {
            *self.activity.lock().unwrap() = at;
        }
    }
    impl SignalSource for FakeActivity {
        fn signal(&self, _task_run_id: Uuid) -> WorkerSignal {
            self.signal
        }
        fn last_activity(&self, _task_run_id: Uuid) -> Option<i64> {
            Some(*self.activity.lock().unwrap())
        }
    }

    /// Records dispatch/reprompt calls; never spawns. Either side effect can be
    /// scripted to FAIL, which is what drives `apply_tick` down the arms that
    /// produce `apply_failures` / `transient_failures` — the arms every
    /// hand-built `TickOutcome` in this module skips over.
    #[derive(Default)]
    struct FakeDispatcher {
        dispatched: StdMutex<Vec<String>>,
        reprompted: StdMutex<Vec<Uuid>>,
        next_trid: StdMutex<Vec<Uuid>>,
        dispatch_err: StdMutex<Option<DispatchError>>,
        reprompt_err: StdMutex<Option<String>>,
    }
    impl FakeDispatcher {
        /// Every dispatch is ATTEMPTED and fails with `err`.
        fn dispatch_failing(err: DispatchError) -> Self {
            FakeDispatcher {
                dispatch_err: StdMutex::new(Some(err)),
                ..Default::default()
            }
        }
        /// Every re-prompt is ATTEMPTED and fails — the live shape is a CLI
        /// whose stdin write fails, which leaves the FSM at `Ready` because
        /// `send_user_message` only transitions AFTER the write.
        fn reprompt_failing(msg: &str) -> Self {
            FakeDispatcher {
                reprompt_err: StdMutex::new(Some(msg.to_string())),
                ..Default::default()
            }
        }
    }
    #[async_trait]
    impl Dispatcher for FakeDispatcher {
        async fn dispatch(&self, _run_id: Uuid, subtask: &Subtask) -> Result<Uuid, DispatchError> {
            self.dispatched
                .lock()
                .unwrap()
                .push(subtask.task_id.clone());
            if let Some(e) = self.dispatch_err.lock().unwrap().clone() {
                return Err(e);
            }
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
            if let Some(e) = self.reprompt_err.lock().unwrap().clone() {
                return Err(e);
            }
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

    /// The served clause's declared parallel fan-out bound must actually bind
    /// on dispatch — this is where the registry's `fanout_bound` stops being
    /// decorative. It can only ever LOWER the run's own cap, and the surplus
    /// ready work stays queued (degrade to sequential) rather than failing.
    #[test]
    fn the_registry_fanout_bound_caps_dispatch() {
        struct Case {
            name: &'static str,
            concurrency_cap: usize,
            fanout_bound: Option<u32>,
            want_dispatched: usize,
        }

        let cases = [
            Case {
                name: "no resolved bound → only the run's own cap applies",
                concurrency_cap: 4,
                fanout_bound: None,
                want_dispatched: 4,
            },
            Case {
                name: "a lower registry bound caps the run",
                concurrency_cap: 4,
                fanout_bound: Some(2),
                want_dispatched: 2,
            },
            Case {
                name: "a higher registry bound never RAISES the run's cap",
                concurrency_cap: 2,
                fanout_bound: Some(15),
                want_dispatched: 2,
            },
            Case {
                name: "a bound of 1 is sequential, not a stall",
                concurrency_cap: 4,
                fanout_bound: Some(1),
                want_dispatched: 1,
            },
            Case {
                name: "a bound of 0 degrades to sequential, never to zero",
                concurrency_cap: 4,
                fanout_bound: Some(0),
                want_dispatched: 1,
            },
        ];

        for c in cases {
            let rows = vec![
                mk("A", 0, &[], SubtaskState::Submitted),
                mk("B", 1, &[], SubtaskState::Submitted),
                mk("C", 2, &[], SubtaskState::Submitted),
                mk("D", 3, &[], SubtaskState::Submitted),
                mk("E", 4, &[], SubtaskState::Submitted),
            ];
            let signals = FakeSignals(Map::new());
            let mut timers = ReadyIdleTimers::default();
            let mut cf = cfg();
            cf.concurrency_cap = c.concurrency_cap;
            cf.fanout_bound = c.fanout_bound;
            let plan = compute_tick(&rows, &signals, &mut timers, &cf, 0);
            assert_eq!(
                plan.to_dispatch.len(),
                c.want_dispatched,
                "case: {}",
                c.name
            );
            // Never a failure: the surplus stays queued for a later tick.
            assert!(plan.to_fail.is_empty(), "case: {}", c.name);
            assert!(!plan.done, "case: {}", c.name);
        }
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

    /// A worker that reported and then vanished (the runner restarted between
    /// the report and the idle signal) completes on the FIRST tick, through
    /// the same path as `ReadyIdle` + artifact — never failed after grace.
    #[test]
    fn a_gone_worker_whose_report_landed_completes_at_once() {
        let trid = Uuid::new_v4();
        let mut w = mk("A", 0, &[], SubtaskState::Working);
        w.task_run_id = Some(trid);
        w.artifact = Some(report());
        let rows = vec![w];
        let signals = FakeSignals(Map::new()); // unknown ⇒ Gone
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(plan.to_complete, vec!["A".to_string()]);
        assert!(plan.to_fail.is_empty());
    }

    /// ...and a drift-verify row in that state still has its verdict read
    /// before it may complete.
    #[test]
    fn a_gone_drift_verify_with_a_report_is_still_diverted() {
        let trid = Uuid::new_v4();
        let mut v = mk("verify", 0, &[], SubtaskState::Working);
        v.expected_output = "DriftVerdict shows no drift".to_string();
        v.task_run_id = Some(trid);
        v.artifact = Some(report());
        let rows = vec![v];
        let signals = FakeSignals(Map::new()); // unknown ⇒ Gone
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &cfg(), 0);
        assert_eq!(plan.to_verify_drift, vec!["verify".to_string()]);
        assert!(plan.to_complete.is_empty());
        assert!(plan.to_fail.is_empty());
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
        let pg = PgDb::new_for_test().await;
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
    fn a_working_worker_and_a_dep_blocked_row_are_both_excluded() {
        // A Working, B Submitted-blocked-on-A: NOTHING is stuck. A live worker
        // is legitimate waiting, and B is waiting on A. An empty fingerprint is
        // what stops the run being written `stalled` while a healthy worker
        // works.
        //
        // What BOUNDS the live worker is the per-worker silence deadline
        // (`working_silence_secs`), NOT the §5 recovery deadlines: those fire
        // from `ReadyIdle`/`Gone` only, and a wedged CLI sits at `Processing`
        // (→ `WorkerSignal::Working`) forever, which no §5 timer reaches.
        //
        // And that silence deadline bounds a worker that goes QUIET, not every
        // `Working` worker: every emitted line resets it. A wedged CLI that
        // keeps TALKING is bounded by nothing here — see
        // `a_worker_that_keeps_working_is_never_written_stalled`, which drives
        // 5000 s of emitting ticks past `working_silence_secs` and asserts no
        // stall. That is deliberate (a stall exit orphans the sessions) and
        // `stop_orchestration_run` remains the way out.
        let trid = Uuid::new_v4();
        let mut a = mk("A", 0, &[], SubtaskState::Working);
        a.task_run_id = Some(trid);
        let rows = vec![a, mk("B", 1, &["A"], SubtaskState::Submitted)];
        let order = topo_order(&rows).unwrap();
        let fp = stall_fingerprint(&rows, &order, &[]);
        assert!(
            fp.is_empty(),
            "a working worker + a dep-blocked row is not stuck state: {:?}",
            fp.evidence()
        );
    }

    #[test]
    fn only_the_rows_a_tick_decided_to_dispatch_are_fingerprinted() {
        // A ready row is `R:`-fingerprinted iff THIS tick decided to dispatch
        // it: if it is still Submitted next tick the dispatch did not land, and
        // the reconciler will re-decide it identically forever. Ready work the
        // cap could not admit is waiting on a slot a live worker holds — the
        // same legitimate waiting as an unmet dependency. (The predicate used to
        // be the run-wide `dispatch_capped` flag, which mis-classified the
        // surplus rows of a partially-admitted ready set as stuck.)
        let rows = vec![mk("X", 0, &[], SubtaskState::Submitted)];
        let order = topo_order(&rows).unwrap();
        assert_eq!(
            stall_fingerprint(&rows, &order, &["X".to_string()]).evidence(),
            "R:X"
        );
        assert!(
            stall_fingerprint(&rows, &order, &[]).is_empty(),
            "ready work this tick did not decide to dispatch is not stuck state"
        );
    }

    /// A dependency that can still be satisfied is waiting; one that can NEVER
    /// be satisfied is stuck, and used to be invisible in both directions — the
    /// row is excluded from `ready_subtasks` AND (before `B:`) from the
    /// fingerprint, while `all_terminal` stayed false, so the run looped at
    /// `running` forever with no diagnosis.
    #[test]
    fn a_dependency_that_can_never_be_satisfied_is_stuck_not_waiting() {
        // A dep naming no row at all.
        let missing = vec![mk("X", 0, &["nope"], SubtaskState::Submitted)];
        let order = topo_order(&missing).unwrap();
        assert_eq!(
            stall_fingerprint(&missing, &order, &[]).evidence(),
            "B:X:nope",
            "a dep naming no row is surfaced as a stall, as topo_order's doc promises"
        );

        // A dep that FAILED: its dependent can never become ready, and the run
        // can never be done (the dependent is non-terminal forever).
        let stranded = vec![
            mk("A", 0, &[], SubtaskState::Failed),
            mk("B", 1, &["A"], SubtaskState::Submitted),
        ];
        let order = topo_order(&stranded).unwrap();
        assert_eq!(
            stall_fingerprint(&stranded, &order, &[]).evidence(),
            "B:B:A"
        );
        assert!(!all_terminal(&stranded), "the run cannot finish either");

        // Contrast: a dep that is merely not Completed YET is ordinary waiting.
        let mut live = mk("A", 0, &[], SubtaskState::Working);
        live.task_run_id = Some(Uuid::new_v4());
        let waiting = vec![live, mk("B", 1, &["A"], SubtaskState::Submitted)];
        let order = topo_order(&waiting).unwrap();
        assert!(stall_fingerprint(&waiting, &order, &[]).is_empty());
    }

    // ======================================================================
    // Phase 6 — coord gates + DriftVerdict verify
    // ======================================================================

    use super::super::coord_gate::{
        CoordGateClient, CoordGateError, DriftClass, GatePredicateSpec, GateStatus,
    };
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
        /// When set, every call fails with this typed error.
        err: GateMutex<Option<CoordGateError>>,
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
        /// Every call fails as if the runner were unpaired.
        fn set_unreachable(&self, on: bool) {
            *self.err.lock().unwrap() = on.then(|| {
                CoordGateError::Unreachable("runner has no device JWT (unpaired)".to_string())
            });
        }
        /// Every call fails with coord ANSWERING and refusing (a 404, an
        /// unmapped predicate) — the arm that used to change nothing on the row.
        fn set_failed(&self, on: bool) {
            *self.err.lock().unwrap() = on.then(|| {
                CoordGateError::Failed("coord_gate: poll gate-abc: 404: gone".to_string())
            });
        }
        fn gate_err(&self) -> Option<CoordGateError> {
            self.err.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl CoordGateClient for FakeCoordGateClient {
        async fn register_gate(
            &self,
            _run_id: Uuid,
            task_id: &str,
            _predicate: &GatePredicateSpec,
        ) -> Result<String, CoordGateError> {
            self.registered.lock().unwrap().push(task_id.to_string());
            if let Some(e) = self.gate_err() {
                return Err(e);
            }
            Ok(self.next_gate_id.clone())
        }
        async fn poll_gate(&self, gate_id: &str) -> Result<GateStatus, CoordGateError> {
            self.polled.lock().unwrap().push(gate_id.to_string());
            if let Some(e) = self.gate_err() {
                return Err(e);
            }
            Ok(self.poll_status.lock().unwrap().unwrap_or(GateStatus::Open))
        }
        async fn drift_verdict(&self, subspace: &str) -> Result<DriftClass, CoordGateError> {
            self.verdict_subspaces
                .lock()
                .unwrap()
                .push(subspace.to_string());
            if let Some(e) = self.gate_err() {
                return Err(e);
            }
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
        let fp = stall_fingerprint(&rows, &order, &[]);
        assert!(
            fp.is_empty(),
            "a gate-blocked-only set has empty actionable fingerprint (no stall): {:?}",
            fp.evidence()
        );
        // And a needs-registration row is likewise excluded.
        let rows2 = vec![mk_gated("test", 0, &[], "CI green", None, None)];
        let order2 = topo_order(&rows2).unwrap();
        assert!(
            stall_fingerprint(&rows2, &order2, &[]).is_empty(),
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
        let pg = PgDb::new_for_test().await;
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
        let pg = PgDb::new_for_test().await;
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

    // ======================================================================
    // Phase 2 (2026-09-12 consolidate onto conductor) — run-exit honesty
    // ======================================================================

    /// A DAG cycle is a fatal tick: the run exits `failed`, and the reason the
    /// row gets is the cycle error itself — not an empty `failed`.
    #[test]
    fn fatal_cycle_exit_is_written_failed_with_reason() {
        let rows = vec![
            mk("A", 0, &["C"], SubtaskState::Submitted),
            mk("B", 1, &["A"], SubtaskState::Submitted),
            mk("C", 2, &["B"], SubtaskState::Submitted),
        ];
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &cfg(), 0);
        let mut watch = StallWatch::default();

        let exit = tick_exit(&plan, &TickOutcome::default(), &mut watch, 0, &cfg())
            .expect("a cycle ends the run on tick 1");
        assert_eq!(exit.status(), "failed");
        assert_eq!(exit.status(), RunExit::STATUS_FAILED);
        assert_eq!(
            exit.reason(),
            Some("Circular dependency detected in subtask DAG"),
            "the written reason is the fatal error text"
        );
        assert_eq!(exit.loop_phase(), LoopPhase::Error);
        assert!(
            watch.watching().is_empty(),
            "a fatal exit is decided before the stall watch sees anything"
        );
    }

    /// **The regression this phase's blocker was.** A worker that is simply
    /// WORKING must never be written `stalled`, however long it works.
    ///
    /// Before the fix the fingerprint carried `W:<task_id>:<task_run_id>` for
    /// every in-flight row. A dispatched worker signalling `Working` produced
    /// that same string every tick with nothing completing, so the tick read as
    /// no-progress, the detector counted five identical records, and the run was
    /// written `stalled` with a `status_reason` — durably, operator-visibly —
    /// about 25 s after dispatch, abandoning a live worker. It also
    /// contradicted the §5 recovery deadlines in the same file, which give that
    /// worker 90 s of quiet before even a re-prompt.
    ///
    /// A worker that keeps EMITTING is never touched, however long it runs. What
    /// bounds one that goes silent is `working_silence_secs` —
    /// `a_worker_wedged_mid_turn_is_failed_on_its_silence_deadline` — not this
    /// and not the §5 deadlines, which fire from `ReadyIdle`/`Gone` only.
    #[test]
    fn a_worker_that_keeps_working_is_never_written_stalled() {
        let trid = Uuid::new_v4();
        let mut a = mk("A", 0, &[], SubtaskState::Working);
        a.task_run_id = Some(trid);
        let rows = vec![a];
        let signals = FakeActivity::working(0);
        let mut timers = ReadyIdleTimers::default();
        let mut watch = StallWatch::default();
        let c = cfg();

        // 1000 ticks = 5000 s at the 5 s tick interval: far past the old
        // 5-identical-action window, past `stall_after_secs`, and past
        // `working_silence_secs` — which this worker keeps resetting by
        // emitting, as a live CLI does.
        for tick in 1..=1000u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            signals.emit(now);
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            assert!(
                plan.stall_fingerprint.is_empty(),
                "tick {tick}: a working worker is not stuck state: {:?}",
                plan.stall_fingerprint.evidence()
            );
            assert!(plan.to_fail.is_empty(), "tick {tick}: nor is it failed");
            assert_eq!(
                tick_exit(&plan, &TickOutcome::default(), &mut watch, now, &c),
                None,
                "tick {tick} ({now}s in): a healthy worker must never end the run"
            );
        }
    }

    /// **BLOCKER 2 of round 2.** A worker whose CLI wedges mid-turn sits at
    /// `SessionState::Processing`, which `signal_from_state` maps to
    /// `WorkerSignal::Working`. Nothing else in the system bounds that — and
    /// this deadline bounds it only while it stays QUIET, since any emitted line
    /// resets the clock (`a_worker_that_keeps_working_is_never_written_stalled`
    /// is the other side of that, and is green). The §5
    /// deadlines fire from `ReadyIdle`/`Gone`, `compute_tick`'s `Working` arm
    /// CLEARS both of their timers, and no timeout in `claude_session` moves a
    /// session out of `Processing`. With the `W:` fingerprint entry removed such
    /// a worker held its concurrency slot forever, and once the cap saturated
    /// every other ready row was excluded as "waiting on a slot" — so the whole
    /// run went invisible and ticked at `running` indefinitely.
    #[test]
    fn a_worker_wedged_mid_turn_is_failed_on_its_silence_deadline() {
        let trid = Uuid::new_v4();
        let mut a = mk("A", 0, &[], SubtaskState::Working);
        a.task_run_id = Some(trid);
        let rows = vec![a];
        // Last output at t=0 and never again: the CLI is alive (the FSM still
        // says Processing) but it has stopped producing anything.
        let signals = FakeActivity::working(0);
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();

        let mut failed_at = None;
        for tick in 1..=1000u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            if !plan.to_fail.is_empty() {
                assert_eq!(plan.to_fail, vec!["A".to_string()]);
                assert_eq!(
                    plan.silent_workers,
                    vec!["A".to_string()],
                    "and the reason travels with it"
                );
                failed_at = Some(now);
                break;
            }
            assert!(
                now < c.working_silence_secs,
                "tick {tick}: not failed at {now}s, past the {}s deadline",
                c.working_silence_secs
            );
        }
        let failed_at = failed_at.expect("a permanently silent worker must be bounded");
        assert!(
            failed_at >= c.working_silence_secs,
            "never before the deadline: {failed_at}s vs {}s",
            c.working_silence_secs
        );
        assert!(
            failed_at >= c.report_timeout_secs + c.report_reprompt_grace_secs,
            "and never before the §5 recovery path could have run to its own conclusion"
        );

        // One emitted line resets the whole stretch — a long tool call that
        // finally prints is not a wedge.
        let mut timers = ReadyIdleTimers::default();
        let signals = FakeActivity::working(0);
        for tick in 1..=1000u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            if now % 600 == 0 {
                signals.emit(now);
            }
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            assert!(
                plan.to_fail.is_empty(),
                "tick {tick} ({now}s): a worker emitting every 600s is not wedged"
            );
        }
    }

    /// The one `Working` shape that IS stuck: a row with no `task_run_id`, which
    /// the tick cannot reconcile at all (no signal to read, so no §5 deadline
    /// reaches it). It stays fingerprinted.
    #[test]
    fn a_working_row_with_no_worker_id_is_still_fingerprinted() {
        let rows = vec![mk("A", 0, &[], SubtaskState::Working)];
        let order = topo_order(&rows).unwrap();
        assert_eq!(stall_fingerprint(&rows, &order, &[]).evidence(), "W!:A");
    }

    /// Ready work the concurrency cap cannot admit is waiting on a SLOT, which
    /// is legitimate waiting. Excluding `W:` rows alone would have left this
    /// false stall behind: cap=1, one worker working, three ready rows emitting
    /// an unchanged `R:b|R:c|R:d` every tick with no dispatch possible.
    #[test]
    fn ready_work_waiting_on_a_concurrency_slot_is_never_stalled() {
        let trid = Uuid::new_v4();
        let mut a = mk("a", 0, &[], SubtaskState::Working);
        a.task_run_id = Some(trid);
        let rows = vec![
            a,
            mk("b", 1, &[], SubtaskState::Submitted),
            mk("c", 2, &[], SubtaskState::Submitted),
            mk("d", 3, &[], SubtaskState::Submitted),
        ];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::Working);
        let signals = FakeSignals(sigs);
        let c = OrchestrationRunConfig {
            concurrency_cap: 1,
            ..cfg()
        };
        let mut timers = ReadyIdleTimers::default();
        let mut watch = StallWatch::default();

        for tick in 1..=200u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            assert!(plan.to_dispatch.is_empty(), "tick {tick}: cap is saturated");
            assert!(
                plan.stall_fingerprint.is_empty(),
                "tick {tick}: queued-behind-the-cap is not stuck state: {:?}",
                plan.stall_fingerprint.evidence()
            );
            assert_eq!(
                tick_exit(&plan, &TickOutcome::default(), &mut watch, now, &c),
                None,
                "tick {tick}"
            );
        }
    }

    /// The same exclusion one step further in: a ready set the cap admits only
    /// PARTIALLY. The surplus rows are waiting on a slot exactly as above, and
    /// the run is plainly healthy — it dispatches one row per tick. Under the
    /// old run-wide `dispatch_capped` flag those surplus rows WERE fingerprinted
    /// (slots > 0), and only the run-wide progress veto kept the run alive;
    /// with progress no longer a veto, mis-classifying them would stall a
    /// perfectly busy run.
    #[test]
    fn ready_work_beyond_a_partially_admitting_cap_is_not_stuck() {
        let rows = vec![
            mk("a", 0, &[], SubtaskState::Submitted),
            mk("b", 1, &[], SubtaskState::Submitted),
            mk("c", 2, &[], SubtaskState::Submitted),
        ];
        let c = OrchestrationRunConfig {
            concurrency_cap: 1,
            ..cfg()
        };
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        assert_eq!(plan.to_dispatch.len(), 1, "one slot, one dispatch decided");
        let decided = plan.to_dispatch[0].clone();
        assert_eq!(
            plan.stall_fingerprint.keys(),
            std::slice::from_ref(&decided),
            "only the decided row is fingerprinted; the other two wait on a slot"
        );
    }

    /// A genuinely stuck run — nothing dispatched, nothing completing, nothing
    /// working, and a row the runner has no coord answer about — still exits
    /// `stalled`, with the reason naming the elapsed window, the fingerprint
    /// (including WHICH coord failure) and the blocked row.
    ///
    /// The row's only reason is a coord block, so it is timed on
    /// `coord_block_stall_after_secs` rather than the row-sized window — the
    /// run still stalls, it just is not ended by an ordinary coord blip.
    #[test]
    fn a_genuinely_stuck_run_is_written_stalled_with_reason() {
        let rows = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            None,
            Some(GATE_STATUS_COORD_UNREACHABLE),
        )];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let mut watch = StallWatch::default();
        let c = cfg();

        let mut exit = None;
        let mut fired_at = None;
        for tick in 1..=500u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            assert_eq!(
                plan.stall_fingerprint.evidence(),
                "C:coord_unreachable:test"
            );
            // Retrying a coord-blocked row moves nothing, so the tick lands no
            // outcome — which is what lets the window accumulate.
            if let Some(e) = tick_exit(&plan, &TickOutcome::default(), &mut watch, now, &c) {
                exit = Some(e);
                fired_at = Some(now);
                break;
            }
        }
        let exit = exit.expect("a stuck run must eventually stall");
        let fired_at = fired_at.unwrap();
        assert!(
            fired_at >= c.coord_block_stall_after_secs,
            "never before the coord window: fired at {fired_at}s, window {}s",
            c.coord_block_stall_after_secs
        );
        assert!(
            fired_at > c.report_timeout_secs.max(c.gone_grace_secs),
            "and never before the §5 recovery deadlines it used to contradict"
        );
        assert_eq!(exit.status(), RunExit::STATUS_STALLED);
        let reason = exit.reason().expect("a stall carries its reason");
        assert!(
            reason.starts_with("Stall detected: test stuck for ")
                && reason.contains("C:coord_unreachable:test")
                && reason.ends_with("; blocked on coord: test"),
            "reason names the row, the window, the stuck state and the block: {reason:?}"
        );
        assert_eq!(exit.loop_phase(), LoopPhase::Error);
    }

    /// **BLOCKER 1 of round 2.** One healthy row must not hide a stuck one.
    ///
    /// Subtask `X`'s gate registration is refused by coord (422 →
    /// `coord_error`); subtask `Y` waits on a gate coord legitimately holds
    /// open. Every tick `to_poll_gate = [Y]` and `blocked_on_coord = [X]`, so
    /// the old run-wide `progressed()` read TRUE because of Y, the window reset,
    /// and X's entry never accumulated a second — the run polled forever at
    /// `runs.status = 'running'`, which is the exact defect this phase exists to
    /// remove, re-created whenever the run had one other answering gate.
    #[test]
    fn a_row_coord_answers_about_does_not_mask_one_it_does_not() {
        let rows = vec![
            mk_gated("X", 0, &[], "CI green", None, Some(GATE_STATUS_COORD_ERROR)),
            mk_gated("Y", 1, &[], "CI green", Some("gate-y"), Some("open")),
        ];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let mut watch = StallWatch::default();
        let c = cfg();

        let mut fired_at = None;
        for tick in 1..=500u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            // The shape the old test built and then only half-checked.
            assert_eq!(plan.to_poll_gate, vec!["Y".to_string()]);
            assert_eq!(plan.blocked_on_coord, vec!["X".to_string()]);
            assert_eq!(plan.stall_fingerprint.evidence(), "C:coord_error:X");
            assert_eq!(
                plan.stall_fingerprint.keys(),
                ["X".to_string()],
                "Y is legitimately waiting; X is stuck"
            );
            // Y's poll lands (coord answers about it) — under the old rule THIS
            // is what reset the window. It changes nothing on Y's row, so it is
            // not even an outcome.
            if let Some(e) = tick_exit(&plan, &TickOutcome::default(), &mut watch, now, &c) {
                assert!(e.reason().unwrap().contains("C:coord_error:X"));
                fired_at = Some(now);
                break;
            }
        }
        assert!(
            fired_at.is_some_and(|f| f >= c.coord_block_stall_after_secs),
            "X must stall on its own clock however healthy Y is: {fired_at:?}"
        );
    }

    /// The other half of BLOCKER 1: `to_dispatch` is a DECISION, not an
    /// outcome. A dispatch that keeps failing leaves the row `Submitted`, so the
    /// next tick re-decides it identically — forever, under a rule that read the
    /// decision as life. Now the row is `R:`-fingerprinted and the failed
    /// dispatch lands no outcome, so the two agree and the run stalls.
    #[test]
    fn a_dispatch_that_never_lands_is_not_mistaken_for_progress() {
        let rows = vec![mk("A", 0, &[], SubtaskState::Submitted)];
        let signals = FakeSignals(Map::new());
        let mut timers = ReadyIdleTimers::default();
        let mut watch = StallWatch::default();
        let c = cfg();
        // What `apply_tick` returns when `dispatcher.dispatch` errored.
        let failed_dispatch = TickOutcome {
            apply_failures: vec!["dispatch:A".to_string()],
            ..Default::default()
        };

        let mut fired_at = None;
        for tick in 1..=200u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            assert_eq!(plan.to_dispatch, vec!["A".to_string()], "re-decided");
            assert_eq!(plan.stall_fingerprint.evidence(), "R:A");
            if let Some(e) = tick_exit(&plan, &failed_dispatch, &mut watch, now, &c) {
                let reason = e.reason().unwrap();
                assert!(
                    reason.contains("R:A") && reason.contains("E:dispatch:A"),
                    "{reason}"
                );
                fired_at = Some(now);
                break;
            }
        }
        assert!(
            fired_at.is_some_and(|f| f >= c.stall_after_secs),
            "{fired_at:?}"
        );

        // A dispatch that LANDS leaves the row Working next tick, so nothing is
        // fingerprinted and nothing accumulates.
        let mut working = mk("A", 0, &[], SubtaskState::Working);
        working.task_run_id = Some(Uuid::new_v4());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&[working], &FakeActivity::working(0), &mut timers, &c, 0);
        assert!(plan.stall_fingerprint.is_empty());
    }

    /// A state write that keeps failing is the one stuck shape that cannot be
    /// recorded on the row — when the row write is what is failing, so is any
    /// marker written the same way. It is carried in the tick OUTCOME instead
    /// and folded into the fingerprint by `tick_exit`.
    #[test]
    fn a_durable_write_that_keeps_failing_is_stall_counted() {
        let trid = Uuid::new_v4();
        let mut a = mk("A", 0, &[], SubtaskState::Working);
        a.task_run_id = Some(trid);
        a.artifact = Some(report());
        let rows = vec![a];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let mut watch = StallWatch::default();
        let c = cfg();
        let write_failed = TickOutcome {
            apply_failures: vec!["complete:A".to_string()],
            ..Default::default()
        };

        let mut fired_at = None;
        for tick in 1..=200u32 {
            let now = tick as i64 * c.tick_interval_secs as i64;
            let plan = compute_tick(&rows, &signals, &mut timers, &c, now);
            assert_eq!(plan.to_complete, vec!["A".to_string()], "re-decided");
            assert!(
                plan.stall_fingerprint.is_empty(),
                "a Working row is not fingerprinted by itself — the outcome is what says it is stuck"
            );
            if let Some(e) = tick_exit(&plan, &write_failed, &mut watch, now, &c) {
                assert!(e.reason().unwrap().contains("E:complete:A"));
                fired_at = Some(now);
                break;
            }
        }
        assert!(
            fired_at.is_some_and(|f| f >= c.stall_after_secs),
            "{fired_at:?}"
        );
    }

    /// The residual arm: a tick that decides nothing, has nothing in flight and
    /// is not done. `InputRequired` is today's instance — no arm dispatches,
    /// completes or fails such a row, and `all_terminal` is false forever.
    #[test]
    fn a_run_with_nothing_actionable_and_nothing_in_flight_is_stuck() {
        let rows = vec![mk("A", 0, &[], SubtaskState::InputRequired)];
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &cfg(), 0);
        assert!(!plan.done, "an InputRequired row is not terminal");
        assert_eq!(
            plan.stall_fingerprint.keys(),
            [NOTHING_ACTIONABLE_KEY.to_string()]
        );
        assert_eq!(
            plan.stall_fingerprint.evidence(),
            "Z:nothing-actionable:A",
            "and it names the rows that will never move"
        );

        // It never fires while something specific IS diagnosed, so it can only
        // ever ADD a verdict, never replace one.
        let diagnosed = vec![mk_gated(
            "A",
            0,
            &[],
            "CI green",
            None,
            Some(GATE_STATUS_COORD_ERROR),
        )];
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&diagnosed, &FakeSignals(Map::new()), &mut timers, &cfg(), 0);
        assert_eq!(plan.stall_fingerprint.evidence(), "C:coord_error:A");
    }

    /// A [`StallFingerprint`] with one entry per key, for driving the watch.
    fn fp(entries: &[(&str, &str)]) -> StallFingerprint {
        let mut f = StallFingerprint::default();
        for (key, entry) in entries {
            f.push(key.to_string(), entry.to_string());
        }
        f
    }

    /// The watch measures how long each ROW has been continuously stuck. A row
    /// that leaves the stuck set restarts its own window on re-entry; a row that
    /// stays fires on its own clock.
    #[test]
    fn stall_watch_measures_each_row_continuously() {
        let window = 300;
        // Both windows are set to the same value here on purpose: this test is
        // about CONTINUITY, not about which class of reason gets which window
        // (that is `a_coord_block_is_timed_on_the_coord_window`).
        let coord = window;
        let mut w = StallWatch::default();
        assert_eq!(
            w.observe(&fp(&[("x", "C:x")]), 0, window, coord),
            None,
            "first"
        );
        assert_eq!(w.watching(), vec!["x".to_string()]);
        assert_eq!(
            w.observe(&fp(&[("x", "C:x")]), 299, window, coord),
            None,
            "1 s short"
        );
        // x leaves the stuck set — it moved, so its window is over.
        assert_eq!(
            w.observe(&StallFingerprint::default(), 300, window, coord),
            None
        );
        assert!(w.watching().is_empty());
        // ...and re-entering starts a fresh window rather than inheriting one.
        assert_eq!(w.observe(&fp(&[("x", "C:x")]), 301, window, coord), None);
        assert_eq!(w.observe(&fp(&[("x", "C:x")]), 599, window, coord), None);
        let ev = w
            .observe(&fp(&[("x", "C:x")]), 601, window, coord)
            .expect("300 s continuous");
        assert_eq!(ev.stuck_keys, vec!["x".to_string()]);
        assert_eq!(ev.unchanged_for_secs, 300);
        assert_eq!(ev.fingerprint, "C:x");
    }

    /// **SHOULD-FIX 4 of round 2.** An OSCILLATING reason must not defeat the
    /// window. A coord behind a load balancer whose instances answer 503 (→
    /// `coord_unreachable`) and 422 (→ `coord_error`) alternately made the
    /// fingerprint STRING change every tick; when the watch keyed on that
    /// string it restarted every 5 s and a 100%-blocked run never stalled. The
    /// key is the row; the token rides along in the evidence only.
    #[test]
    fn an_oscillating_reason_does_not_defeat_the_window() {
        let window = 300;
        // One window for both classes — the claim under test is that the KEY,
        // not the reason string, is what continuity is measured on.
        let coord = window;
        let mut w = StallWatch::default();
        let mut fired = None;
        for tick in 1..=200i64 {
            let now = tick * 5;
            // Alternate the token exactly as the two coord instances would.
            let token = if tick % 2 == 0 {
                "C:coord_unreachable:x"
            } else {
                "C:coord_error:x"
            };
            if let Some(ev) = w.observe(&fp(&[("x", token)]), now, window, coord) {
                assert_eq!(ev.stuck_keys, vec!["x".to_string()]);
                assert_eq!(ev.fingerprint, token, "the evidence carries WHICH failure");
                fired = Some(now);
                break;
            }
        }
        assert_eq!(
            fired,
            Some(305),
            "the row is stuck throughout — first seen at t=5, fires 300 s later"
        );
    }

    /// And a busy neighbour cannot hold the window open for a stuck row — the
    /// per-row rule stated directly, without the conductor around it.
    #[test]
    fn one_rows_churn_does_not_reset_another_rows_window() {
        let window = 300;
        let coord = window;
        let mut w = StallWatch::default();
        let mut fired = None;
        for tick in 1..=200i64 {
            let now = tick * 5;
            // `stuck` is present every tick; `churn-<n>` is a different row each
            // time, which under a set-equality rule changed the watched value
            // every tick and reset everything.
            let churn = format!("churn-{tick}");
            let entry = format!("R:{churn}");
            if let Some(ev) = w.observe(
                &fp(&[("stuck", "C:x:stuck"), (&churn, &entry)]),
                now,
                window,
                coord,
            ) {
                assert_eq!(ev.stuck_keys, vec!["stuck".to_string()]);
                fired = Some(now);
                break;
            }
        }
        assert_eq!(fired, Some(305), "first seen at t=5");
    }

    /// A DESIGN failure is a `failed` run carrying the design error — never a
    /// `complete` on zero subtasks.
    #[test]
    fn design_failure_exit_is_failed_with_the_error() {
        let exit = RunExit::design_failed("design AI call failed: claude --print exited 1");
        assert_eq!(exit.status(), "failed");
        assert_eq!(
            exit.reason(),
            Some("DESIGN bootstrap failed: design AI call failed: claude --print exited 1")
        );
        assert_eq!(exit.loop_phase(), LoopPhase::Error);
        // And the one exit that carries no reason is completion.
        assert_eq!(RunExit::Complete.status(), "complete");
        assert_eq!(RunExit::Complete.reason(), None);
        assert_eq!(RunExit::Complete.loop_phase(), LoopPhase::Complete);
    }

    /// A subtask the runner has no coord ANSWER about is a TYPED block in both
    /// flavours: `coord_unreachable` (could not ask) and `coord_error` (coord
    /// answered and refused). Both are still retried, neither retry is
    /// progress, both rows are IN the stall fingerprint carrying their token —
    /// unlike a gate coord is genuinely holding open, which is excluded.
    #[test]
    fn coord_blocked_rows_are_typed_and_count_toward_stall() {
        struct Case {
            name: &'static str,
            token: &'static str,
            gate_id: Option<&'static str>,
            want_in_register: bool,
            want_in_poll: bool,
        }
        let cases = [
            Case {
                name: "registration unreachable → re-registered, fingerprinted",
                token: GATE_STATUS_COORD_UNREACHABLE,
                gate_id: None,
                want_in_register: true,
                want_in_poll: false,
            },
            Case {
                name: "poll unreachable → re-polled (same gate), fingerprinted",
                token: GATE_STATUS_COORD_UNREACHABLE,
                gate_id: Some("gate-1"),
                want_in_register: false,
                want_in_poll: true,
            },
            Case {
                name: "registration refused by a reachable coord → fingerprinted",
                token: GATE_STATUS_COORD_ERROR,
                gate_id: None,
                want_in_register: true,
                want_in_poll: false,
            },
            Case {
                name: "poll refused by a reachable coord → fingerprinted",
                token: GATE_STATUS_COORD_ERROR,
                gate_id: Some("gate-1"),
                want_in_register: false,
                want_in_poll: true,
            },
        ];
        for c in &cases {
            let rows = vec![mk_gated(
                "test",
                0,
                &[],
                "CI green",
                c.gate_id,
                Some(c.token),
            )];
            assert!(blocked_on_coord(&rows[0]), "{}", c.name);
            assert_eq!(coord_block(&rows[0]), Some(c.token), "{}", c.name);
            let order = topo_order(&rows).unwrap();
            assert_eq!(
                stall_fingerprint(&rows, &order, &[]).evidence(),
                format!("C:{}:test", c.token),
                "{}: the coord-blocked row is IN the fingerprint, with its token",
                c.name
            );

            let mut timers = ReadyIdleTimers::default();
            let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &cfg(), 0);
            assert_eq!(
                plan.blocked_on_coord,
                vec!["test".to_string()],
                "{}",
                c.name
            );
            assert_eq!(
                plan.to_register_gate.contains(&"test".to_string()),
                c.want_in_register,
                "{}: retry-by-registration",
                c.name
            );
            assert_eq!(
                plan.to_poll_gate.contains(&"test".to_string()),
                c.want_in_poll,
                "{}: retry-by-poll",
                c.name
            );
            assert!(plan.to_dispatch.is_empty(), "{}: never dispatched", c.name);
        }

        // Contrast: the same row with an OPEN gate (coord answered) is excluded
        // from the fingerprint — unchanged §3.5. It is NOT excluded because the
        // poll "counts as progress"; it is excluded because coord told us to
        // wait, which is the only reason that ever justified it.
        let open = vec![mk_gated(
            "test",
            0,
            &[],
            "CI green",
            Some("gate-1"),
            Some("open"),
        )];
        let order = topo_order(&open).unwrap();
        assert!(stall_fingerprint(&open, &order, &[]).is_empty());
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&open, &FakeSignals(Map::new()), &mut timers, &cfg(), 0);
        assert!(plan.blocked_on_coord.is_empty());
        assert_eq!(plan.to_poll_gate, vec!["test".to_string()]);
    }

    /// A DriftVerdict verify whose verdict read keeps failing is `Working`, so
    /// the §5 worker deadlines never touch it (it is diverted to the verify path
    /// before them) and — once `W:` rows left the fingerprint — nothing else
    /// would either. Its typed coord block is what keeps it visible and
    /// stall-counted, in a lifecycle state the `Submitted`-only predicate could
    /// not see.
    #[test]
    fn a_verify_whose_verdict_read_keeps_failing_is_fingerprinted() {
        let trid = Uuid::new_v4();
        let mut v = mk("verify", 0, &[], SubtaskState::Working);
        v.expected_output = "DriftVerdict shows no drift".to_string();
        v.task_run_id = Some(trid);
        v.artifact = Some(report());
        // The same row WITHOUT the token, to contrast against.
        let healthy = v.clone();
        v.gate_status = Some(GATE_STATUS_COORD_ERROR.to_string());

        let order = topo_order(&[v.clone()]).unwrap();
        assert_eq!(
            stall_fingerprint(&[v.clone()], &order, &[]).evidence(),
            "C:coord_error:verify"
        );
        // Without the token the same row is a healthy in-flight worker: excluded.
        assert!(stall_fingerprint(&[healthy], &order, &[]).is_empty());

        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&[v], &FakeSignals(sigs), &mut timers, &cfg(), 0);
        assert_eq!(plan.to_verify_drift, vec!["verify".to_string()]);
        assert_eq!(plan.blocked_on_coord, vec!["verify".to_string()]);
    }

    /// What an outcome says, and what it deliberately does not. Only things that
    /// LANDED are listed; a side effect that was attempted and failed is an
    /// `apply_failures` entry, which `tick_exit` turns into a fingerprint entry
    /// rather than into evidence of life.
    /// The two designs this type merged disagree only on WHICH failures are
    /// transient, and every caller reads that through one accessor: `apply_tick`
    /// to decide `transient_failures` vs `apply_failures`, and `tick_exit` to
    /// decide whether the row keeps its stall-fingerprint key. A drain deferral
    /// is as transient as a full fan-out bound — in both the row is queued and
    /// will dispatch once a condition OUTSIDE this run clears — so counting
    /// either as stuck stalls a healthy run.
    #[test]
    fn a_drain_deferral_is_as_transient_as_a_full_fanout_bound() {
        assert!(
            DispatchError::DeferredByDrain("drained".to_string()).transient(),
            "a device drain lifts on its own; the subtask stays Submitted"
        );
        assert!(
            DispatchError::Transient("bound full".to_string()).transient(),
            "the fleet-wide fan-out bound frees on its own"
        );
        assert!(
            !DispatchError::Failed("worktree acquisition failed".to_string()).transient(),
            "everything else re-decides identically forever and must accrue stall time"
        );
        assert!(
            !DispatchError::from("a bare string is a hard failure".to_string()).transient(),
            "From<String> maps to Failed, so an untyped error is never forgiven"
        );
    }

    #[test]
    fn an_outcome_lists_what_landed_and_names_what_did_not() {
        let empty = TickOutcome::default();
        assert_eq!(empty.summary(), "", "a tick that did nothing says nothing");

        let o = TickOutcome {
            dispatched: vec!["a".to_string()],
            completed: vec!["b".to_string()],
            failed: vec![],
            apply_failures: vec!["dispatch:c".to_string()],
            transient_failures: vec!["d".to_string()],
            done: false,
        };
        assert_eq!(
            o.summary(),
            "dispatched=a completed=b did-not-land=dispatch:c waiting-for-a-slot=d",
            "a transient failure is REPORTED but is not a did-not-land"
        );

        // The failure becomes a stall-counted fingerprint entry KEYED ON THE
        // ROW, so a row that fails a dispatch one tick and a state write the
        // next is continuously stuck rather than two half-windows.
        let mut watch = StallWatch::default();
        let plan = TickPlan::default();
        let c = cfg();
        assert_eq!(tick_exit(&plan, &o, &mut watch, 0, &c), None);
        assert_eq!(
            watch.watching(),
            vec!["c".to_string()],
            "only the did-not-land row is watched — `d` is waiting for a slot"
        );
        let other = TickOutcome {
            apply_failures: vec!["complete:c".to_string()],
            ..Default::default()
        };
        let exit = tick_exit(&plan, &other, &mut watch, c.stall_after_secs, &c)
            .expect("the row was stuck across both attempts");
        assert!(exit.reason().unwrap().contains("E:complete:c"));
    }

    /// `finish_run` writes the exit to the run ROW (status + reason) before it
    /// touches the in-memory loop state; `orchestration_run_status`'s
    /// reader then finds the reason after the reconciler is gone.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture
    /// (DATABASE_URL); the `orchestration` schema (incl. `runs.status_reason`)
    /// self-heals at `PgDb::new`. Run with:
    /// `cargo test -p qontinui-runner orchestration_loop::conductor::tests::finish_run_writes_status_and_reason_to_the_run_row -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn finish_run_writes_status_and_reason_to_the_run_row() {
        let pg = PgDb::new_for_test().await;
        let cases: [(RunExit, &str, Option<&str>, LoopPhase); 3] = [
            (
                RunExit::failed("Circular dependency detected in subtask DAG"),
                "failed",
                Some("Circular dependency detected in subtask DAG"),
                LoopPhase::Error,
            ),
            (
                RunExit::stalled(
                    StallEvidence {
                        fingerprint: "C:coord_unreachable:test".to_string(),
                        stuck_keys: vec!["test".to_string()],
                        unchanged_for_secs: 300,
                    },
                    &["test".to_string()],
                ),
                "stalled",
                Some(
                    "Stall detected: test stuck for 300s: \
                     C:coord_unreachable:test; blocked on coord: test",
                ),
                LoopPhase::Error,
            ),
            (RunExit::Complete, "complete", None, LoopPhase::Complete),
        ];
        for (exit, want_status, want_reason, want_phase) in cases {
            let run_id = Uuid::new_v4();
            pg.create_run(run_id, "exit run", None, &["test".to_string()], "running")
                .await
                .expect("create_run");
            let loop_state: SharedLoopState = Arc::new(tokio::sync::Mutex::new(
                super::super::loop_engine::LoopState::new(),
            ));
            {
                let mut st = loop_state.lock().await;
                st.running = true;
                st.phase = LoopPhase::Reconciling;
            }

            finish_run(&pg, &loop_state, run_id, exit).await;

            let row = pg.get_run(run_id).await.expect("get_run").expect("row");
            assert_eq!(row.status, want_status, "durable status for {want_status}");
            assert_eq!(
                row.status_reason.as_deref(),
                want_reason,
                "durable reason for {want_status}"
            );
            let st = loop_state.lock().await;
            assert!(!st.running);
            assert_eq!(st.phase, want_phase);
            assert_eq!(st.error.as_deref(), want_reason);
            drop(st);

            let conn = pg.pool().get().await.expect("conn");
            let _ = conn
                .execute(
                    "DELETE FROM orchestration.runs WHERE run_id = $1",
                    &[&run_id],
                )
                .await;
        }
    }

    /// The mirror image of the Stop-overwrites-a-stall fix: the RECONCILER must
    /// not overwrite a `stopped` either. The operator presses Stop, the row
    /// moves `running → stopped`, and the conductor is mid-tick with its exit
    /// already decided — one tick wide, and `finish_run`'s write used to be
    /// unconditional, so `stopped` became `stalled`/`complete`.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn finish_run_does_not_overwrite_a_run_stopped_out_from_under_it() {
        let pg = PgDb::new_for_test().await;
        for exit in [
            RunExit::stalled(
                StallEvidence {
                    fingerprint: "C:coord_error:test".to_string(),
                    stuck_keys: vec!["test".to_string()],
                    unchanged_for_secs: 300,
                },
                &[],
            ),
            RunExit::Complete,
        ] {
            let run_id = Uuid::new_v4();
            pg.create_run(
                run_id,
                "stopped run",
                None,
                &["test".to_string()],
                "running",
            )
            .await
            .expect("create_run");
            // The operator's Stop lands first (this is exactly what
            // `stop_orchestration_run` writes).
            assert!(pg
                .set_run_status_if_running(run_id, "stopped", Some("stop requested"))
                .await
                .expect("stop write"));

            let loop_state: SharedLoopState = Arc::new(tokio::sync::Mutex::new(
                super::super::loop_engine::LoopState::new(),
            ));
            finish_run(&pg, &loop_state, run_id, exit).await;

            let row = pg.get_run(run_id).await.expect("get_run").expect("row");
            assert_eq!(row.status, "stopped", "the operator's verdict survives");
            assert_eq!(row.status_reason.as_deref(), Some("stop requested"));
            // The in-memory state still settles — the reconciler HAS returned.
            let st = loop_state.lock().await;
            assert!(!st.running);
            drop(st);

            let conn = pg.pool().get().await.expect("conn");
            let _ = conn
                .execute(
                    "DELETE FROM orchestration.runs WHERE run_id = $1",
                    &[&run_id],
                )
                .await;
        }
    }

    /// A DriftVerdict read that fails is recorded on the VERIFY row (which is
    /// `Working`, so no §5 deadline and no `W:` fingerprint entry bound it),
    /// and a later successful read clears the block before the verdict lands.
    ///
    /// `#[ignore]` per the `database/pg/*` convention (needs DATABASE_URL).
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn drift_verdict_failure_is_recorded_on_the_verify_row_then_cleared() {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_run(run_id, "verify run", None, &["test".to_string()], "running")
            .await
            .expect("create_run");
        let trid = Uuid::new_v4();
        let mut v = mk("verify", 0, &[], SubtaskState::Working);
        v.run_id = run_id;
        v.expected_output = "DriftVerdict shows no drift".to_string();
        v.task_run_id = Some(trid);
        v.artifact = Some(report());
        pg.upsert_subtask(&v).await.expect("upsert verify");

        let dispatcher = FakeDispatcher::default();
        let gate = FakeCoordGateClient::with_gate_id("unused");
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();
        let verify_row = |rows: &[Subtask]| {
            rows.iter()
                .find(|s| s.task_id == "verify")
                .cloned()
                .expect("verify row")
        };

        // The twin read fails (coord answered, refused) → typed block on the row.
        gate.set_failed(true);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert_eq!(plan.to_verify_drift, vec!["verify".to_string()]);
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
        let r = verify_row(&pg.list_subtasks(run_id).await.unwrap());
        assert_eq!(r.gate_status.as_deref(), Some(GATE_STATUS_COORD_ERROR));
        assert_eq!(r.state, SubtaskState::Working, "no verdict was fabricated");
        assert!(!drift_verdict_recorded(&r), "and none was recorded");

        // Which makes the stuck verify visible to the stall watch.
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 1);
        assert_eq!(plan.stall_fingerprint.evidence(), "C:coord_error:verify");

        // The twin answers → block cleared and the verdict lands.
        gate.set_failed(false);
        gate.set_drift(DriftClass::NoDrift);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 2);
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
        let r = verify_row(&pg.list_subtasks(run_id).await.unwrap());
        assert_eq!(
            r.gate_status, None,
            "the block is cleared once coord answers"
        );
        assert!(drift_verdict_recorded(&r), "the verdict is recorded");
        assert_eq!(r.state, SubtaskState::Completed);

        let conn = pg.pool().get().await.unwrap();
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    /// `apply_tick` records EVERY typed register/poll failure on the subtask —
    /// `coord_unreachable` when the runner could not ask, `coord_error` when
    /// coord answered and refused — keeping `gate_id` so a registered gate
    /// resumes polling, correcting a stale token rather than leaving it, and
    /// clearing the block once a call succeeds.
    ///
    /// `#[ignore]` per the `database/pg/*` convention (needs DATABASE_URL).
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn coord_blocks_are_recorded_on_the_subtask_and_clear_on_recovery() {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_run(
            run_id,
            "unpaired run",
            None,
            &["test".to_string()],
            "running",
        )
        .await
        .expect("create_run");
        let mut st = mk_gated("test", 0, &[], "CI green", None, None);
        st.run_id = run_id;
        pg.upsert_subtask(&st).await.expect("upsert");

        let dispatcher = FakeDispatcher::default();
        let gate = FakeCoordGateClient::with_gate_id("gate-abc");
        let mut timers = ReadyIdleTimers::default();
        let c = cfg();
        let row = |rows: &[Subtask]| rows.iter().find(|s| s.task_id == "test").cloned().unwrap();

        // Tick 1: registration fails Unreachable → typed block recorded, no gate_id.
        gate.set_unreachable(true);
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
        let r = row(&pg.list_subtasks(run_id).await.unwrap());
        assert_eq!(r.gate_id, None, "no gate was registered");
        assert_eq!(
            r.gate_status.as_deref(),
            Some(GATE_STATUS_COORD_UNREACHABLE),
            "the typed block is on the row"
        );
        assert_eq!(r.state, SubtaskState::Submitted);

        // Tick 2: still blocked → retried and fingerprinted. The retry lands
        // nothing, so the outcome stays empty and the window accumulates.
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 1);
        assert_eq!(plan.blocked_on_coord, vec!["test".to_string()]);
        assert_eq!(
            plan.stall_fingerprint.evidence(),
            "C:coord_unreachable:test"
        );

        // Tick 3: coord back → registration lands, status open, block cleared.
        gate.set_unreachable(false);
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
        let r = row(&pg.list_subtasks(run_id).await.unwrap());
        assert_eq!(r.gate_id.as_deref(), Some("gate-abc"));
        assert_eq!(r.gate_status.as_deref(), Some("open"));

        // Tick 4: poll fails Unreachable → typed block, gate_id KEPT.
        gate.set_unreachable(true);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 2);
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
        let r = row(&pg.list_subtasks(run_id).await.unwrap());
        assert_eq!(
            r.gate_id.as_deref(),
            Some("gate-abc"),
            "gate association survives"
        );
        assert_eq!(
            r.gate_status.as_deref(),
            Some(GATE_STATUS_COORD_UNREACHABLE)
        );

        // Tick 5: coord back, gate cleared → status cleared, then dispatchable.
        gate.set_unreachable(false);
        gate.set_poll(GateStatus::Cleared);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 3);
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
        let r = row(&pg.list_subtasks(run_id).await.unwrap());
        assert_eq!(r.gate_status.as_deref(), Some("cleared"));
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 4);
        assert_eq!(plan.to_dispatch, vec!["test".to_string()]);

        // ---- the second flavour: coord ANSWERS and refuses the call --------
        // This arm used to change nothing on the row: the retry read as
        // progress, the row stayed out of the fingerprint, and the run polled
        // forever reading `running`.
        let mut st2 = mk_gated(
            "failing",
            1,
            &[],
            "CI green",
            Some("gate-zzz"),
            Some("open"),
        );
        st2.run_id = run_id;
        pg.upsert_subtask(&st2).await.expect("upsert failing");
        gate.set_failed(true);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 5);
        assert!(plan.to_poll_gate.contains(&"failing".to_string()));
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
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let r = rows.iter().find(|s| s.task_id == "failing").unwrap();
        assert_eq!(
            r.gate_status.as_deref(),
            Some(GATE_STATUS_COORD_ERROR),
            "a refused poll is recorded, NOT written back as the coord verdict `open`"
        );
        assert_eq!(r.gate_id.as_deref(), Some("gate-zzz"), "gate kept");
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 6);
        assert_eq!(plan.blocked_on_coord, vec!["failing".to_string()]);
        assert!(plan
            .stall_fingerprint
            .evidence()
            .contains("C:coord_error:failing"));

        // A pairing-era `coord_unreachable` is CORRECTED, not left to point the
        // operator at pairing while coord is answering (finding 3).
        pg.set_subtask_gate(
            run_id,
            "failing",
            Some("gate-zzz"),
            Some(GATE_STATUS_COORD_UNREACHABLE),
        )
        .await
        .expect("stale token");
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 7);
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
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let r = rows.iter().find(|s| s.task_id == "failing").unwrap();
        assert_eq!(
            r.gate_status.as_deref(),
            Some(GATE_STATUS_COORD_ERROR),
            "the stale unreachable token is replaced by what is actually happening"
        );

        // And a coord that starts answering clears the block entirely.
        gate.set_failed(false);
        gate.set_poll(GateStatus::Open);
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 8);
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
        let rows = pg.list_subtasks(run_id).await.unwrap();
        let r = rows.iter().find(|s| s.task_id == "failing").unwrap();
        assert_eq!(r.gate_status.as_deref(), Some("open"), "block cleared");
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 9);
        assert!(plan.blocked_on_coord.is_empty());
        assert!(
            !plan.stall_fingerprint.evidence().contains("C:"),
            "no coord block left in the fingerprint (the other row is R:, the \
             dispatch this tick decided, which is checked against its OUTCOME \
             rather than being stuck): {:?}",
            plan.stall_fingerprint.evidence()
        );

        let conn = pg.pool().get().await.unwrap();
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    // --- apply_tick's OWN outcome, driven by a failing dispatcher ----------
    //
    // Everything above that asserts on `apply_failures` hand-builds a
    // `TickOutcome`, so deleting the `outcome.apply_failures.push(...)` lines
    // in `apply_tick` left the suite green. These drive `apply_tick` itself.

    /// Set up a one-row run in PG and return `(pg, run_id, rows)`.
    async fn one_row_run(row: Subtask) -> (Arc<PgDb>, Uuid, Vec<Subtask>) {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_run(
            run_id,
            "apply-tick outcome",
            None,
            &["implement".to_string()],
            "running",
        )
        .await
        .expect("create_run");
        let mut row = row;
        row.run_id = run_id;
        pg.upsert_subtask(&row).await.expect("upsert subtask");
        let rows = pg.list_subtasks(run_id).await.expect("list");
        (pg, run_id, rows)
    }

    async fn drop_run(pg: &Arc<PgDb>, run_id: Uuid) {
        let conn = pg.pool().get().await.expect("conn");
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    /// A dispatch that THROWS produces `apply_failures = ["dispatch:A"]` and an
    /// empty `dispatched` — the claim the whole `TickOutcome` split rests on,
    /// asserted against a real `apply_tick` rather than a hand-built outcome.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn apply_tick_records_a_thrown_dispatch_and_lands_nothing() {
        let (pg, run_id, rows) = one_row_run(mk("A", 0, &[], SubtaskState::Submitted)).await;
        let c = cfg();
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        assert_eq!(plan.to_dispatch, vec!["A".to_string()], "A is ready");

        let dispatcher = FakeDispatcher::dispatch_failing(DispatchError::Failed(
            "worktree acquisition failed".to_string(),
        ));
        let gate = FakeCoordGateClient::with_gate_id("g-1");
        let outcome = apply_tick(
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
        .expect("apply_tick");

        assert_eq!(
            outcome.apply_failures,
            vec!["dispatch:A".to_string()],
            "a dispatch that threw is an ATTEMPTED side effect that did not land"
        );
        assert!(
            outcome.dispatched.is_empty(),
            "nothing landed: {:?}",
            outcome.dispatched
        );
        assert!(outcome.transient_failures.is_empty(), "not transient");
        assert_eq!(
            *dispatcher.dispatched.lock().unwrap(),
            vec!["A".to_string()],
            "and it really was attempted"
        );
        let after = pg.list_subtasks(run_id).await.expect("list");
        assert_eq!(
            after[0].state,
            SubtaskState::Submitted,
            "the row did not move, so the next tick re-decides the same dispatch"
        );

        // The E: entry is what carries that into the stall reason.
        let mut watch = StallWatch::default();
        assert!(tick_exit(&plan, &outcome, &mut watch, 0, &c).is_none());
        match tick_exit(&plan, &outcome, &mut watch, c.stall_after_secs, &c) {
            Some(RunExit::Stalled { reason }) => assert!(
                reason.contains("E:dispatch:A"),
                "the stall reason names the failed side effect: {reason}"
            ),
            other => panic!("expected a stall, got {other:?}"),
        }
        drop_run(&pg, run_id).await;
    }

    /// A dispatch refused because the FLEET's fan-out bound is full is
    /// TRANSIENT: no `apply_failures`, and `tick_exit` drops the row's KEY — the
    /// `R:` this tick put there included — so the run never stalls on it however
    /// long the gate stays busy. The opposite verdict is what
    /// `ready_work_beyond_a_partially_admitting_cap_is_not_stuck` already gives
    /// the run-wide cap; this pins that the fleet-wide bound reads the same.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn a_full_fanout_bound_is_transient_and_never_stalls_the_run() {
        let (pg, run_id, rows) = one_row_run(mk("A", 0, &[], SubtaskState::Submitted)).await;
        let c = cfg();
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        assert_eq!(
            plan.stall_fingerprint.keys(),
            ["A".to_string()],
            "the plan alone fingerprints the decided dispatch as R:A"
        );

        let dispatcher = FakeDispatcher::dispatch_failing(DispatchError::Transient(
            "the parallel fan-out bound (15) was fully occupied".to_string(),
        ));
        let gate = FakeCoordGateClient::with_gate_id("g-1");
        let outcome = apply_tick(
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
        .expect("apply_tick");

        assert!(
            outcome.apply_failures.is_empty(),
            "a full bound is not a did-not-land: {:?}",
            outcome.apply_failures
        );
        assert_eq!(outcome.transient_failures, vec!["A".to_string()]);
        assert!(outcome.dispatched.is_empty());

        let mut watch = StallWatch::default();
        for tick in 0..200i64 {
            assert!(
                tick_exit(&plan, &outcome, &mut watch, tick * 5, &c).is_none(),
                "tick {tick}: a row waiting for a fleet slot is waiting, not stuck"
            );
        }
        assert!(
            watch.watching().is_empty(),
            "and the watch is not even tracking it: {:?}",
            watch.watching()
        );
        drop_run(&pg, run_id).await;
    }

    /// A re-prompt that will not land is recorded. It is the one side effect
    /// nothing else bounds: `reprompted_at` is inserted only on success, so the
    /// §5 deadline that FAILS the row is never armed, the row is `Working` with a
    /// `task_run_id` (excluded from the fingerprint) and `ReadyIdle` (so the
    /// silence deadline was cleared). The plan alone can never end this run.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn a_reprompt_that_never_lands_is_recorded_and_bounded() {
        let trid = Uuid::new_v4();
        let mut row = mk("A", 0, &[], SubtaskState::Working);
        row.task_run_id = Some(trid);
        row.artifact = None;
        let (pg, run_id, rows) = one_row_run(row).await;

        let c = OrchestrationRunConfig {
            // The worker has been Ready-without-artifact since the first tick.
            report_timeout_secs: 0,
            ..cfg()
        };
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert_eq!(plan.to_reprompt.len(), 1, "the §5 first deadline fired");
        assert!(
            plan.stall_fingerprint.is_empty(),
            "and the PLAN says nothing is stuck — the row is Working with a task_run_id"
        );

        let dispatcher = FakeDispatcher::reprompt_failing("stdin write failed: broken pipe");
        let gate = FakeCoordGateClient::with_gate_id("g-1");
        let outcome = apply_tick(
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
        .expect("apply_tick");

        assert_eq!(outcome.apply_failures, vec!["reprompt:A".to_string()]);
        assert!(
            timers.reprompted_at.is_empty(),
            "the second §5 deadline is NOT armed, which is why this needs its own bound"
        );
        // ...and the reconciler really does re-decide it forever.
        let plan2 = compute_tick(
            &rows,
            &signals,
            &mut timers,
            &c,
            c.report_timeout_secs + 10_000,
        );
        assert_eq!(plan2.to_reprompt.len(), 1, "re-decided, identically");
        assert!(plan2.to_fail.is_empty(), "and never failed on its own");

        // Only the OUTCOME can end this run.
        let mut plan_only = StallWatch::default();
        assert!(
            tick_exit(
                &plan,
                &TickOutcome::default(),
                &mut plan_only,
                c.stall_after_secs * 100,
                &c
            )
            .is_none(),
            "the plan alone never stalls a Working row"
        );
        let mut watch = StallWatch::default();
        assert!(tick_exit(&plan, &outcome, &mut watch, 0, &c).is_none());
        match tick_exit(&plan, &outcome, &mut watch, c.stall_after_secs, &c) {
            Some(RunExit::Stalled { reason }) => assert!(
                reason.contains("E:reprompt:A"),
                "the stall reason names the re-prompt: {reason}"
            ),
            other => panic!("expected a stall, got {other:?}"),
        }
        drop_run(&pg, run_id).await;
    }

    /// A gate verdict that coord GAVE and PG would not store is recorded. The
    /// register variant is the expensive one: the row stays ungated, so the next
    /// tick registers a NEW coord gate — one leaked gate row per tick, with the
    /// run reading `running` and nothing on the subtask to show for it.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn a_gate_verdict_that_will_not_persist_is_recorded() {
        let (pg, run_id, rows) = one_row_run(mk_gated(
            "test",
            0,
            &[],
            "CI green on this repo",
            None,
            None,
        ))
        .await;
        let c = cfg();
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        assert_eq!(plan.to_register_gate, vec!["test".to_string()]);

        // `set_subtask_gate` writes by (run_id, task_id); a run_id nothing was
        // written under is the cheapest honest "the durable write will not
        // land" — the same class as a PG refusal, and the reason the failure
        // cannot be recorded on the row itself.
        let missing_run = Uuid::new_v4();
        let dispatcher = FakeDispatcher::default();
        let gate = FakeCoordGateClient::with_gate_id("gate-abc");
        let outcome = apply_tick(
            &plan,
            &rows,
            missing_run,
            &pg,
            &dispatcher,
            &gate,
            &c,
            &mut timers,
        )
        .await
        .expect("apply_tick");

        assert_eq!(
            outcome.apply_failures,
            vec!["gate_persist:test".to_string()],
            "coord registered a gate the row will never carry"
        );
        assert_eq!(
            *gate.registered.lock().unwrap(),
            vec!["test".to_string()],
            "and the gate really was registered at coord — this is the leak"
        );
        let after = pg.list_subtasks(run_id).await.expect("list");
        assert!(
            after[0].gate_id.is_none(),
            "the row is still ungated, so the next tick registers ANOTHER gate"
        );
        drop_run(&pg, run_id).await;
    }

    // --- the coord-block window + the in-flight guard ----------------------

    /// A row whose ONLY stuck reason is a coord block is timed on
    /// `coord_block_stall_after_secs`, not the row-sized `stall_after_secs`.
    /// Coord reachability is a runner-wide condition no row can influence, and
    /// a few minutes of it is an ordinary event on this fleet.
    #[test]
    fn a_coord_block_is_timed_on_the_coord_window() {
        let c = cfg();
        let rows = vec![mk_gated(
            "X",
            0,
            &[],
            "CI green on this repo",
            Some("gate-1"),
            Some(GATE_STATUS_COORD_UNREACHABLE),
        )];
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        assert_eq!(plan.stall_fingerprint.keys(), ["X".to_string()]);
        assert_eq!(
            plan.stall_fingerprint.coord_only_keys(),
            ["X".to_string()],
            "its only reason is C:"
        );
        assert!(
            plan.in_flight_workers.is_empty(),
            "nothing is live, so the in-flight guard does not apply"
        );

        let mut watch = StallWatch::default();
        assert!(tick_exit(&plan, &TickOutcome::default(), &mut watch, 0, &c).is_none());
        assert!(
            tick_exit(
                &plan,
                &TickOutcome::default(),
                &mut watch,
                c.stall_after_secs,
                &c
            )
            .is_none(),
            "the row-sized window must NOT end a run on a coord blip"
        );
        assert!(
            tick_exit(
                &plan,
                &TickOutcome::default(),
                &mut watch,
                c.coord_block_stall_after_secs,
                &c
            )
            .is_some(),
            "but a coord outage still surfaces, on its own window"
        );
    }

    /// ...unless the SAME row is also stuck for a reason of its own, in which
    /// case the short window applies: the long one is for rows whose only
    /// problem is that coord is not answering.
    #[test]
    fn a_coord_blocked_row_that_also_fails_a_write_gets_the_short_window() {
        let c = cfg();
        let rows = vec![mk_gated(
            "X",
            0,
            &[],
            "CI green on this repo",
            Some("gate-1"),
            Some(GATE_STATUS_COORD_ERROR),
        )];
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &FakeSignals(Map::new()), &mut timers, &c, 0);
        let outcome = TickOutcome {
            apply_failures: vec!["gate_persist:X".to_string()],
            ..Default::default()
        };
        let mut watch = StallWatch::default();
        assert!(tick_exit(&plan, &outcome, &mut watch, 0, &c).is_none());
        assert!(
            tick_exit(&plan, &outcome, &mut watch, c.stall_after_secs, &c).is_some(),
            "a row that also cannot be written is stuck on its own account"
        );
    }

    /// A coord block does NOT end a run that still has live workers, however
    /// long coord stays unreachable — a stall exit RETURNS from the reconciler
    /// and orphans every session the run spawned. The window keeps accumulating
    /// underneath the guard, so the stall fires the moment the last worker is
    /// gone.
    #[test]
    fn a_coord_block_alone_does_not_end_a_run_with_live_workers() {
        let c = cfg();
        let trid = Uuid::new_v4();
        let mut worker = mk("A", 0, &[], SubtaskState::Working);
        worker.task_run_id = Some(trid);
        let blocked = mk_gated(
            "X",
            1,
            &[],
            "CI green on this repo",
            Some("gate-1"),
            Some(GATE_STATUS_COORD_UNREACHABLE),
        );
        let rows = vec![worker.clone(), blocked.clone()];
        // A busy mid-turn worker: `Working` with a moving output stamp, so the
        // silence deadline never reaches it either.
        let signals = FakeActivity::working(0);

        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert_eq!(
            plan.in_flight_workers,
            vec!["A".to_string()],
            "A is live and is not the blocked row"
        );
        assert_eq!(plan.stall_fingerprint.keys(), ["X".to_string()]);

        let mut watch = StallWatch::default();
        for now in [
            0,
            c.stall_after_secs,
            c.coord_block_stall_after_secs,
            c.coord_block_stall_after_secs * 4,
        ] {
            signals.emit(now);
            assert!(
                tick_exit(&plan, &TickOutcome::default(), &mut watch, now, &c).is_none(),
                "now={now}: a coord block must not kill a run with a healthy worker in it"
            );
        }

        // The worker finishes. Nothing is in flight, and the window that has
        // been running underneath the guard fires immediately — no extra wait.
        let rows_idle = vec![
            {
                let mut a = worker;
                a.state = SubtaskState::Completed;
                a.task_run_id = None;
                a
            },
            blocked,
        ];
        let plan_idle = compute_tick(
            &rows_idle,
            &signals,
            &mut timers,
            &c,
            c.coord_block_stall_after_secs * 4,
        );
        assert!(plan_idle.in_flight_workers.is_empty());
        assert!(
            tick_exit(
                &plan_idle,
                &TickOutcome::default(),
                &mut watch,
                c.coord_block_stall_after_secs * 4,
                &c
            )
            .is_some(),
            "and the moment the run has nothing live, the accumulated window fires"
        );
    }

    /// A `Working` row that is ITSELF blocked on coord is not counted as live
    /// work — otherwise a coord block would guard itself open forever. The live
    /// shape is a DriftVerdict verify whose verdict read keeps failing.
    #[test]
    fn a_coord_blocked_working_row_does_not_count_as_live_work() {
        let c = cfg();
        let trid = Uuid::new_v4();
        let mut verify = mk("V", 0, &[], SubtaskState::Working);
        verify.task_run_id = Some(trid);
        verify.gate_status = Some(GATE_STATUS_COORD_UNREACHABLE.to_string());
        let rows = vec![verify];
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::Working);
        let signals = FakeSignals(sigs);
        let mut timers = ReadyIdleTimers::default();
        let plan = compute_tick(&rows, &signals, &mut timers, &c, 0);
        assert!(
            plan.in_flight_workers.is_empty(),
            "the blocked row is the stuck row; it cannot vouch for itself"
        );
        let mut watch = StallWatch::default();
        assert!(tick_exit(&plan, &TickOutcome::default(), &mut watch, 0, &c).is_none());
        assert!(
            tick_exit(
                &plan,
                &TickOutcome::default(),
                &mut watch,
                c.coord_block_stall_after_secs,
                &c
            )
            .is_some(),
            "so it still stalls on the coord window"
        );
    }

    /// `run_orchestration` must call `tick_exit` AFTER `apply_tick` and feed it
    /// the OUTCOME. The scenario separates the two orders: the row is `Working`
    /// with a `task_run_id`, so the PLAN's fingerprint is EMPTY and a `tick_exit`
    /// that ran first (or ignored the outcome) would loop at `running` forever.
    /// Only the applied re-prompt failure can end this run.
    ///
    /// `#[ignore]` per the `database/pg/*` convention — needs a live PG fixture.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn run_orchestration_decides_the_exit_from_the_applied_outcome() {
        let trid = Uuid::new_v4();
        let mut row = mk("A", 0, &[], SubtaskState::Working);
        row.task_run_id = Some(trid);
        row.artifact = None;
        let (pg, run_id, _rows) = one_row_run(row).await;

        let c = OrchestrationRunConfig {
            tick_interval_secs: 1,
            report_timeout_secs: 0,
            stall_after_secs: 0,
            ..cfg()
        };
        let mut sigs = Map::new();
        sigs.insert(trid, WorkerSignal::ReadyIdle);
        let loop_state: SharedLoopState = Arc::new(tokio::sync::Mutex::new(
            super::super::loop_engine::LoopState::new(),
        ));
        let (_stop_tx, stop_rx) = watch::channel(false);

        tokio::time::timeout(
            Duration::from_secs(60),
            run_orchestration(
                loop_state.clone(),
                run_id,
                pg.clone(),
                FakeDispatcher::reprompt_failing("stdin write failed: broken pipe"),
                FakeSignals(sigs),
                FakeCoordGateClient::with_gate_id("g-1"),
                c,
                stop_rx,
            ),
        )
        .await
        .expect("the run must END — a tick_exit that ignored the outcome would loop forever");

        let run = pg.get_run(run_id).await.expect("get_run").expect("row");
        assert_eq!(run.status, "stalled");
        let reason = run.status_reason.unwrap_or_default();
        assert!(
            reason.contains("E:reprompt:A"),
            "the durable reason names the side effect that did not land: {reason}"
        );
        drop_run(&pg, run_id).await;
    }
}
