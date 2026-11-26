//! Pure decision functions extracted from `loop_handlers.rs`.
//!
//! Each handler in `loop_handlers.rs` mixes three concerns:
//!   1. Read state (pg queries, env lookups, channel reads, timeouts).
//!   2. Decide what happens next (branching on state).
//!   3. Execute side effects (SDK call, PG write, event emission).
//!
//! This module owns (2): it exposes plain synchronous functions that take an
//! owned snapshot of the data needed to branch and return a `*Decision` enum.
//! The async handler is then:
//!
//!   let snap = build_*_snapshot(ctx, ...).await;
//!   let decision = decisions::decide_*(&snap);
//!   match decision { ... execute side effects ... }
//!
//! The point is that decisions become unit-testable with plain fixtures — no
//! Postgres, no SDK, no tokio runtime.
//!
//! WIRED: production branches on these decisions, so a change here changes runs.
//!   - `handle_evaluate_verification` (`evaluate_verification.rs`)
//!                                      → `decide_evaluate_verification`,
//!                                        `required_step_count`
//!   - `handle_check_post_agentic_signals` → `decide_post_agentic_signals`
//!   - `run_loop_state_machine`'s post-loop compensation → `compensation_applies`
//!   - `LoopController::run_multi_agent_fix` → `targeted_group_fixed`
//!
//! NOT WIRED: tests are the only callers. Each of these handlers still has
//! its own inline branching, so changing one of these functions changes no
//! run until its handler calls it.
//!   - `handle_check_preconditions`     ↛ `decide_preconditions`
//!   - `handle_approval_gate`           ↛ `decide_approval_mode`, `decide_approval_response`
//!
//! `dead_code` is allowed crate-wide, so nothing reports an unwired decision:
//! read the call sites, not the compiler, before trusting this list.
//!
//! Handlers deliberately *not* extracted:
//!   - `handle_check_environment`: the branches are side-effect-only (log env
//!     warnings) and always return `LoopState::RunVerification`. No decision
//!     worth naming.
//!   - `handle_run_verification`, `handle_build_failure_context`,
//!     `handle_run_agentic`, `handle_post_agentic`, `handle_advance_iteration`:
//!     their terminal state transitions are unconditional or trivially
//!     dependent on success/failure booleans that callers already inspect
//!     directly. Extraction would add indirection without buying testability.
//!   - `handle_fix_escalation`: the decision is literally
//!     `if blocking_approval { wait-for-human } else { exhaust }`, interleaved
//!     with multi-second tokio::select loops. Splitting it would require
//!     threading the approval response out of async; not a win.

use crate::step_executor::{StepExecutionResult, VerificationPhaseResult};

use super::loop_state_machine::CompletionReason;
use super::types::{LoopResult, RollbackPolicy};

// =============================================================================
// 1. CheckPreconditions
// =============================================================================

/// Snapshot of state read by `handle_check_preconditions` before it decides
/// whether to continue the loop or terminate with a specific reason.
///
/// Built after `ctx.iteration` has been incremented for the candidate iteration.
#[derive(Debug, Clone)]
pub struct PreconditionsSnapshot {
    /// The iteration number this handler is entering (post-increment).
    pub iteration: u32,
    /// The configured iteration cap (floor of 1 already enforced upstream).
    pub max_iterations: u32,
    /// Whether the task has been stopped externally (read before and after the
    /// pause wait). If `true` at *either* observation the loop exits.
    pub stopped_before_pause: bool,
    pub stopped_after_pause: bool,
    /// Global session-budget ceiling across all stages; `None` if unlimited.
    pub max_sessions: Option<i64>,
    /// Current sessions count from the DB; `None` if the task row couldn't be
    /// read (treated as "don't enforce the budget this iteration").
    pub sessions_count: Option<i64>,
    /// Whether the escalation policy's wall-clock limit has been exceeded.
    pub escalation_time_exceeded: bool,
    /// Seconds elapsed since loop start; only consulted when
    /// `escalation_time_exceeded` is true (used to populate `elapsed_ms`).
    pub elapsed_secs: u64,
    /// Whether a phase-timeout budget has been exceeded for the agentic phase.
    pub phase_timeout_exceeded: bool,
    /// Elapsed ms for the phase-timeout branch; only consulted when
    /// `phase_timeout_exceeded` is true.
    pub phase_elapsed_ms: u64,
}

/// Outcome of the preconditions check.
#[derive(Debug, Clone)]
pub enum PreconditionDecision {
    /// Continue — proceed to the next state (`CheckEnvironment`).
    Continue,
    /// Bail out; attach the exact reason the Complete state should carry.
    Complete {
        reason: CompletionReason,
        /// True when the iteration counter must be decremented because this
        /// iteration never really ran. Preserves the current handler behavior.
        rollback_iteration: bool,
    },
}

/// Pure preconditions decision. Behavior mirrors `handle_check_preconditions`
/// exactly, including the iteration-rollback side effect flag.
pub fn decide_preconditions(snap: &PreconditionsSnapshot) -> PreconditionDecision {
    if snap.stopped_before_pause {
        return PreconditionDecision::Complete {
            reason: CompletionReason::Stopped,
            rollback_iteration: true,
        };
    }
    if snap.stopped_after_pause {
        return PreconditionDecision::Complete {
            reason: CompletionReason::Stopped,
            rollback_iteration: true,
        };
    }
    if let (Some(max), Some(current)) = (snap.max_sessions, snap.sessions_count) {
        if current >= max {
            return PreconditionDecision::Complete {
                reason: CompletionReason::MaxSessionsReached,
                rollback_iteration: true,
            };
        }
    }
    if snap.escalation_time_exceeded {
        return PreconditionDecision::Complete {
            reason: CompletionReason::PhaseTimeout {
                phase: "escalation_time_limit".to_string(),
                elapsed_ms: snap.elapsed_secs * 1000,
            },
            rollback_iteration: true,
        };
    }
    if snap.phase_timeout_exceeded {
        return PreconditionDecision::Complete {
            reason: CompletionReason::PhaseTimeout {
                phase: "loop".to_string(),
                elapsed_ms: snap.phase_elapsed_ms,
            },
            rollback_iteration: true,
        };
    }
    if snap.iteration > snap.max_iterations {
        return PreconditionDecision::Complete {
            reason: CompletionReason::MaxIterationsReached,
            rollback_iteration: true,
        };
    }
    PreconditionDecision::Continue
}

// =============================================================================
// 2. EvaluateVerification
// =============================================================================

/// Snapshot of the fields `handle_evaluate_verification` reads before it picks
/// the next state. This does *not* include the verification_result itself —
/// the handler needs to move that value into whichever LoopState variant it
/// returns, so the decision speaks only about the branch taken.
#[derive(Debug, Clone)]
pub struct VerificationEvalSnapshot {
    /// The producer's `all_passed`: every REQUIRED step passed. Never read
    /// alone: `required_steps` is tested first. The shared fold
    /// (`step_executor/verification_execution.rs`) reports `false` over a set
    /// with no required step, so that order is defense in depth against a
    /// producer that reports `true` there, as every producer did before plan
    /// 2026-11-06-a-product-workflow-verification-with-nothing-to-check-never-reports-passed.
    pub all_passed: bool,
    /// How many step results are required, counted by `required_step_count`.
    /// Zero means nothing was checked: an empty step list, every step
    /// filtered out, or only `required: false` steps. Nothing checked is never
    /// a pass.
    pub required_steps: usize,
    /// One of the verification steps flagged a critical (infra) failure.
    pub critical_failure: bool,
    /// Current iteration's passed step count (used for fix-progress tracking).
    pub passed_steps: usize,
    /// Highest passed-step count seen in any prior iteration (the "best so far").
    pub best_passed_checks: usize,
    /// Current fix-attempt counter (pre-increment).
    pub fix_attempts: u32,
    /// Configured fix-attempt ceiling. `0` disables the fix-escalation path.
    pub max_fix_attempts: u32,
}

/// Side effect the caller must apply to `ctx` after we decide. Extracted so
/// tests don't need to mutate a real `LoopContext`.
#[derive(Debug, Clone, PartialEq)]
pub enum FixProgressUpdate {
    /// Progress detected — reset `fix_attempts` to 0, bump `best_passed_checks`
    /// to the current `passed_steps`, and record `last_progress_iteration`.
    Reset { new_best: usize },
    /// No progress — increment `fix_attempts` by one.
    Increment,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerificationEvalDecision {
    /// No required step produced a result, so nothing was checked. Record an
    /// iteration result and Complete with `NoVerificationSteps`. That is not a
    /// pass. It is not a failure an agentic fix could change either, so no fix
    /// iteration runs.
    NoVerificationSteps,
    /// Verification passed — record an iteration result and Complete.
    VerificationPassed,
    /// Critical failure — record an iteration result and Complete with error.
    CriticalFailure,
    /// Non-critical failure, fix-attempts exhausted — go to FixEscalation.
    FixEscalation { progress: FixProgressUpdate },
    /// Non-critical failure, keep trying — go to BuildFailureContext.
    ContinueToFix { progress: FixProgressUpdate },
}

/// Pure evaluation of the verification result. Returns which LoopState the
/// handler should produce and what fix-progress bookkeeping to apply first.
///
/// Note: the nothing-checked, pass and critical branches short-circuit
/// *before* fix-progress bookkeeping, so we don't attach a
/// `FixProgressUpdate` to them — they're terminal regardless.
pub fn decide_evaluate_verification(snap: &VerificationEvalSnapshot) -> VerificationEvalDecision {
    // Before `all_passed`: a producer that reports `all_passed` over a set
    // with no required step (every producer did, before this plan) would
    // otherwise turn "nothing was checked" into a pass.
    if snap.required_steps == 0 {
        return VerificationEvalDecision::NoVerificationSteps;
    }
    if snap.all_passed {
        return VerificationEvalDecision::VerificationPassed;
    }
    if snap.critical_failure {
        return VerificationEvalDecision::CriticalFailure;
    }

    let progress = if snap.passed_steps > snap.best_passed_checks {
        FixProgressUpdate::Reset {
            new_best: snap.passed_steps,
        }
    } else {
        FixProgressUpdate::Increment
    };

    // Compute the fix_attempts value that will be observed *after* the
    // progress update above. We don't actually mutate a counter — we just
    // predict what it will be, the same way the handler does.
    let effective_fix_attempts = match progress {
        FixProgressUpdate::Reset { .. } => 0,
        FixProgressUpdate::Increment => snap.fix_attempts.saturating_add(1),
    };

    if snap.max_fix_attempts > 0 && effective_fix_attempts >= snap.max_fix_attempts {
        return VerificationEvalDecision::FixEscalation { progress };
    }

    VerificationEvalDecision::ContinueToFix { progress }
}

/// How many of `step_results` are REQUIRED: `required` is not `Some(false)`.
///
/// This is the shared fold's own default (`required.unwrap_or(true)` in
/// `step_executor/verification_execution.rs`), so a step that does not say
/// counts as required. It is the one count behind both
/// `VerificationEvalSnapshot::required_steps` and `targeted_group_fixed`, so
/// "nothing was checked" has a single definition.
pub fn required_step_count(step_results: &[StepExecutionResult]) -> usize {
    step_results
        .iter()
        .filter(|r| r.required != Some(false))
        .count()
}

/// Whether a multi-agent fix group's targeted verification shows the group
/// FIXED: at least one required step ran, and every required step passed.
///
/// A targeted run whose step indices selected nothing, or selected only
/// `required: false` steps, checked nothing. That is no evidence the fix
/// worked, so the group is not credited.
pub fn targeted_group_fixed(result: &VerificationPhaseResult) -> bool {
    required_step_count(&result.step_results) > 0 && result.all_passed
}

/// Whether `run_loop_state_machine`'s post-loop compensation applies: first the
/// rollback under the configured `RollbackPolicy`, then the worktree finalizer.
///
/// Both undo work, so both need a REFUTED run: a policy is configured, the
/// loop did not pass, and nobody stopped it. A `no_verification_steps` exit
/// refuted nothing because nothing was checked. So a `last_good` or `clean`
/// policy never reverts or removes work on such an exit.
pub fn compensation_applies(result: &LoopResult, policy: &RollbackPolicy) -> bool {
    !matches!(policy, RollbackPolicy::None)
        && !result.verification_passed
        && !result.was_stopped
        && !result.no_verification_steps
}

// =============================================================================
// 3. CheckPostAgenticSignals
// =============================================================================

/// Snapshot of the AI-output signals `handle_check_post_agentic_signals` needs.
#[derive(Debug, Clone)]
pub struct PostAgenticSignalsSnapshot {
    /// Whether the parsed agentic output's `unfixable` flag is set.
    pub parsed_unfixable: Option<bool>,
    /// Whether the raw agentic output contains an unfixable marker.
    pub raw_output_has_unfixable_marker: bool,
    /// Reason text from the parsed output's `unfixable_reason`, if any.
    pub unfixable_reason: Option<String>,
    /// Stop flag observed *before* the pause wait.
    pub stopped_before_pause: bool,
    /// Stop flag observed *after* the pause wait.
    pub stopped_after_pause: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PostAgenticSignalsDecision {
    /// AI declared unfixable — complete with UnfixableErrors. The caller is
    /// responsible for logging the reason text.
    Unfixable { reason: String },
    /// Task was stopped (before or after the pause wait) — complete Stopped.
    Stopped,
    /// Proceed to the approval gate.
    ProceedToApproval,
}

pub fn decide_post_agentic_signals(
    snap: &PostAgenticSignalsSnapshot,
) -> PostAgenticSignalsDecision {
    // Mirror the handler: prefer the structured `parsed.unfixable`, fall back
    // to the raw-marker check.
    let is_unfixable = match snap.parsed_unfixable {
        Some(b) => b,
        None => snap.raw_output_has_unfixable_marker,
    };
    if is_unfixable {
        let reason = snap
            .unfixable_reason
            .clone()
            .unwrap_or_else(|| "(no reason provided)".to_string());
        return PostAgenticSignalsDecision::Unfixable { reason };
    }
    if snap.stopped_before_pause {
        return PostAgenticSignalsDecision::Stopped;
    }
    if snap.stopped_after_pause {
        return PostAgenticSignalsDecision::Stopped;
    }
    PostAgenticSignalsDecision::ProceedToApproval
}

// =============================================================================
// 4. ApprovalGate (mode selection + response interpretation)
// =============================================================================

/// Inputs the approval gate reads *before* issuing any IO to decide which
/// code path (deferred, blocking, or none) to take this iteration.
#[derive(Debug, Clone)]
pub struct ApprovalModeSnapshot {
    /// Workflow-level `approval_gate` flag.
    pub workflow_approval_gate_enabled: bool,
    /// True when the AI output contains `[APPROVAL_GATE]` marker.
    pub output_contains_marker: bool,
    /// Whether the agentic outcome was Success.
    pub agentic_success: bool,
    /// True only when the user opted into synchronous approval.
    pub blocking_approval: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalMode {
    /// No approval required — skip straight to AdvanceIteration.
    None,
    /// Deferred path: record a deferred question (if confidence/risk gate
    /// says so, but that sub-decision happens at question-emission time) and
    /// continue without blocking.
    Deferred,
    /// Blocking path: register an approval request and wait for a human.
    Blocking,
}

pub fn decide_approval_mode(snap: &ApprovalModeSnapshot) -> ApprovalMode {
    let needs_approval = snap.workflow_approval_gate_enabled || snap.output_contains_marker;
    if !needs_approval {
        return ApprovalMode::None;
    }
    if !snap.agentic_success {
        return ApprovalMode::None;
    }
    if snap.blocking_approval {
        ApprovalMode::Blocking
    } else {
        ApprovalMode::Deferred
    }
}

/// Outcome of the (blocking-only) human response. Used after the tokio::select!
/// in the handler returns a response.
#[derive(Debug, Clone)]
pub struct ApprovalResponseSnapshot {
    /// `action` field from the ApprovalResponse (already-lowercased expected).
    pub action: String,
    /// `approved` field from the ApprovalResponse.
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalResponseDecision {
    /// User aborted — complete the loop with ApprovalAborted.
    Abort,
    /// User approved — proceed to AdvanceIteration.
    Approved,
    /// User rejected (or receiver dropped w/ approved=false) — advance the
    /// iteration anyway so the AI can retry on the next pass.
    Rejected,
}

pub fn decide_approval_response(snap: &ApprovalResponseSnapshot) -> ApprovalResponseDecision {
    if snap.action == "abort" {
        return ApprovalResponseDecision::Abort;
    }
    if snap.approved {
        ApprovalResponseDecision::Approved
    } else {
        ApprovalResponseDecision::Rejected
    }
}

// =============================================================================
// Tests — these are the regression gates for the refactor.
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    // ---- Preconditions ----

    fn precond_snap() -> PreconditionsSnapshot {
        PreconditionsSnapshot {
            iteration: 1,
            max_iterations: 5,
            stopped_before_pause: false,
            stopped_after_pause: false,
            max_sessions: None,
            sessions_count: None,
            escalation_time_exceeded: false,
            elapsed_secs: 0,
            phase_timeout_exceeded: false,
            phase_elapsed_ms: 0,
        }
    }

    #[test]
    fn preconditions_happy_path_continues() {
        let snap = precond_snap();
        assert!(matches!(
            decide_preconditions(&snap),
            PreconditionDecision::Continue
        ));
    }

    #[test]
    fn preconditions_stopped_before_pause_aborts_with_rollback() {
        let mut snap = precond_snap();
        snap.stopped_before_pause = true;
        let d = decide_preconditions(&snap);
        match d {
            PreconditionDecision::Complete {
                reason,
                rollback_iteration,
            } => {
                assert!(matches!(reason, CompletionReason::Stopped));
                assert!(rollback_iteration);
            }
            other => panic!("expected Complete(Stopped), got {:?}", other),
        }
    }

    #[test]
    fn preconditions_max_iterations_boundary_exceeds_on_one_over() {
        // iteration == max_iterations is allowed; iteration == max_iterations + 1 is not.
        let mut snap = precond_snap();
        snap.iteration = snap.max_iterations;
        assert!(matches!(
            decide_preconditions(&snap),
            PreconditionDecision::Continue
        ));

        snap.iteration = snap.max_iterations + 1;
        match decide_preconditions(&snap) {
            PreconditionDecision::Complete { reason, .. } => {
                assert!(matches!(reason, CompletionReason::MaxIterationsReached));
            }
            _ => panic!("expected MaxIterationsReached"),
        }
    }

    #[test]
    fn preconditions_session_budget_enforced_only_with_both_values() {
        let mut snap = precond_snap();
        // max set, count missing → don't enforce
        snap.max_sessions = Some(3);
        snap.sessions_count = None;
        assert!(matches!(
            decide_preconditions(&snap),
            PreconditionDecision::Continue
        ));

        // Both set, count under → continue.
        snap.sessions_count = Some(2);
        assert!(matches!(
            decide_preconditions(&snap),
            PreconditionDecision::Continue
        ));

        // Both set, count >= max → exhaust.
        snap.sessions_count = Some(3);
        match decide_preconditions(&snap) {
            PreconditionDecision::Complete { reason, .. } => {
                assert!(matches!(reason, CompletionReason::MaxSessionsReached));
            }
            _ => panic!("expected MaxSessionsReached"),
        }
    }

    #[test]
    fn preconditions_escalation_time_carries_elapsed_seconds_as_ms() {
        let mut snap = precond_snap();
        snap.escalation_time_exceeded = true;
        snap.elapsed_secs = 42;
        match decide_preconditions(&snap) {
            PreconditionDecision::Complete { reason, .. } => match reason {
                CompletionReason::PhaseTimeout { phase, elapsed_ms } => {
                    assert_eq!(phase, "escalation_time_limit");
                    assert_eq!(elapsed_ms, 42_000);
                }
                other => panic!("expected PhaseTimeout, got {:?}", other),
            },
            _ => panic!("expected Complete"),
        }
    }

    // ---- EvaluateVerification ----

    fn eval_snap() -> VerificationEvalSnapshot {
        VerificationEvalSnapshot {
            all_passed: false,
            required_steps: 3,
            critical_failure: false,
            passed_steps: 3,
            best_passed_checks: 3,
            fix_attempts: 0,
            max_fix_attempts: 3,
        }
    }

    #[test]
    fn eval_all_passed_wins_over_everything() {
        let mut snap = eval_snap();
        snap.all_passed = true;
        // even with critical_failure also set (shouldn't happen in practice but
        // proves ordering):
        snap.critical_failure = true;
        assert_eq!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::VerificationPassed
        );
    }

    #[test]
    fn eval_critical_failure_when_not_passed() {
        let mut snap = eval_snap();
        snap.critical_failure = true;
        assert_eq!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::CriticalFailure
        );
    }

    #[test]
    fn eval_progress_resets_fix_counter() {
        let mut snap = eval_snap();
        snap.passed_steps = 5;
        snap.best_passed_checks = 3;
        snap.fix_attempts = 2;
        match decide_evaluate_verification(&snap) {
            VerificationEvalDecision::ContinueToFix { progress } => {
                assert_eq!(progress, FixProgressUpdate::Reset { new_best: 5 });
            }
            other => panic!("expected ContinueToFix with reset, got {:?}", other),
        }
    }

    #[test]
    fn eval_escalates_when_fix_attempts_would_reach_max() {
        let mut snap = eval_snap();
        snap.passed_steps = 3;
        snap.best_passed_checks = 3; // no progress
        snap.fix_attempts = 2; // would become 3 after increment
        snap.max_fix_attempts = 3;
        match decide_evaluate_verification(&snap) {
            VerificationEvalDecision::FixEscalation { progress } => {
                assert_eq!(progress, FixProgressUpdate::Increment);
            }
            other => panic!("expected FixEscalation, got {:?}", other),
        }
    }

    #[test]
    fn eval_max_fix_attempts_zero_disables_escalation() {
        let mut snap = eval_snap();
        snap.fix_attempts = 999;
        snap.max_fix_attempts = 0;
        assert!(matches!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::ContinueToFix { .. }
        ));
    }

    /// The predecessor plan's vacuous-green trap, pinned. An empty set, fed the
    /// `all_passed: true` the old producers reported for it, still ends
    /// `NoVerificationSteps`: a producer that says "all passed" over nothing
    /// cannot manufacture a pass.
    #[test]
    fn eval_empty_set_fed_all_passed_ends_no_verification_steps() {
        let snap = VerificationEvalSnapshot {
            all_passed: true,
            required_steps: 0,
            critical_failure: false,
            passed_steps: 0,
            best_passed_checks: 0,
            fix_attempts: 0,
            max_fix_attempts: 3,
        };
        assert_eq!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::NoVerificationSteps
        );
    }

    /// Two informational steps that both passed: the shared fold says
    /// `all_passed`, but no required step ran, so nothing was checked.
    #[test]
    fn eval_all_informational_set_fed_all_passed_ends_no_verification_steps() {
        let snap = VerificationEvalSnapshot {
            all_passed: true,
            required_steps: 0,
            critical_failure: false,
            passed_steps: 2,
            best_passed_checks: 0,
            fix_attempts: 0,
            max_fix_attempts: 3,
        };
        assert_eq!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::NoVerificationSteps
        );
    }

    /// The smallest verification that checked something: one required step,
    /// passed. That is a pass, so the new branch takes nothing from it.
    #[test]
    fn eval_one_required_step_passing_is_verification_passed() {
        let snap = VerificationEvalSnapshot {
            all_passed: true,
            required_steps: 1,
            critical_failure: false,
            passed_steps: 1,
            best_passed_checks: 0,
            fix_attempts: 0,
            max_fix_attempts: 3,
        };
        assert_eq!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::VerificationPassed
        );
    }

    /// Nothing checked is decided first, before `all_passed` and before a
    /// critical flag: with no required step there is no failure to attribute.
    #[test]
    fn eval_nothing_checked_is_decided_before_critical_failure() {
        let snap = VerificationEvalSnapshot {
            all_passed: false,
            required_steps: 0,
            critical_failure: true,
            passed_steps: 0,
            best_passed_checks: 0,
            fix_attempts: 0,
            max_fix_attempts: 3,
        };
        assert_eq!(
            decide_evaluate_verification(&snap),
            VerificationEvalDecision::NoVerificationSteps
        );
    }

    // ---- PostAgenticSignals ----

    fn signals_snap() -> PostAgenticSignalsSnapshot {
        PostAgenticSignalsSnapshot {
            parsed_unfixable: None,
            raw_output_has_unfixable_marker: false,
            unfixable_reason: None,
            stopped_before_pause: false,
            stopped_after_pause: false,
        }
    }

    #[test]
    fn signals_parsed_unfixable_wins_over_raw_marker() {
        let mut snap = signals_snap();
        // parsed says false, raw contains marker — parsed wins.
        snap.parsed_unfixable = Some(false);
        snap.raw_output_has_unfixable_marker = true;
        assert_eq!(
            decide_post_agentic_signals(&snap),
            PostAgenticSignalsDecision::ProceedToApproval
        );
    }

    #[test]
    fn signals_raw_marker_used_when_no_parsed() {
        let mut snap = signals_snap();
        snap.parsed_unfixable = None;
        snap.raw_output_has_unfixable_marker = true;
        match decide_post_agentic_signals(&snap) {
            PostAgenticSignalsDecision::Unfixable { reason } => {
                assert_eq!(reason, "(no reason provided)");
            }
            other => panic!("expected Unfixable, got {:?}", other),
        }
    }

    #[test]
    fn signals_stopped_any_time_aborts() {
        let mut snap = signals_snap();
        snap.stopped_before_pause = true;
        assert_eq!(
            decide_post_agentic_signals(&snap),
            PostAgenticSignalsDecision::Stopped
        );

        let mut snap = signals_snap();
        snap.stopped_after_pause = true;
        assert_eq!(
            decide_post_agentic_signals(&snap),
            PostAgenticSignalsDecision::Stopped
        );
    }

    #[test]
    fn signals_happy_path_proceeds_to_approval() {
        let snap = signals_snap();
        assert_eq!(
            decide_post_agentic_signals(&snap),
            PostAgenticSignalsDecision::ProceedToApproval
        );
    }

    // ---- Approval mode + response ----

    #[test]
    fn approval_mode_not_needed_when_flags_off() {
        let snap = ApprovalModeSnapshot {
            workflow_approval_gate_enabled: false,
            output_contains_marker: false,
            agentic_success: true,
            blocking_approval: true,
        };
        assert_eq!(decide_approval_mode(&snap), ApprovalMode::None);
    }

    #[test]
    fn approval_mode_none_when_agent_failed() {
        let snap = ApprovalModeSnapshot {
            workflow_approval_gate_enabled: true,
            output_contains_marker: false,
            agentic_success: false,
            blocking_approval: true,
        };
        assert_eq!(decide_approval_mode(&snap), ApprovalMode::None);
    }

    #[test]
    fn approval_mode_marker_triggers_blocking_when_opted_in() {
        let snap = ApprovalModeSnapshot {
            workflow_approval_gate_enabled: false,
            output_contains_marker: true,
            agentic_success: true,
            blocking_approval: true,
        };
        assert_eq!(decide_approval_mode(&snap), ApprovalMode::Blocking);
    }

    #[test]
    fn approval_mode_defaults_to_deferred() {
        let snap = ApprovalModeSnapshot {
            workflow_approval_gate_enabled: true,
            output_contains_marker: false,
            agentic_success: true,
            blocking_approval: false,
        };
        assert_eq!(decide_approval_mode(&snap), ApprovalMode::Deferred);
    }

    #[test]
    fn approval_response_abort_wins() {
        let d = decide_approval_response(&ApprovalResponseSnapshot {
            action: "abort".to_string(),
            approved: true, // even with approved=true, abort wins
        });
        assert_eq!(d, ApprovalResponseDecision::Abort);
    }

    #[test]
    fn approval_response_approved_vs_rejected() {
        let d = decide_approval_response(&ApprovalResponseSnapshot {
            action: "approve".to_string(),
            approved: true,
        });
        assert_eq!(d, ApprovalResponseDecision::Approved);

        let d = decide_approval_response(&ApprovalResponseSnapshot {
            action: "reject".to_string(),
            approved: false,
        });
        assert_eq!(d, ApprovalResponseDecision::Rejected);
    }
}

// The pure helpers behind the nothing-checked verdict: the required-step
// count, the targeted-group credit and the compensation gate.
#[cfg(test)]
mod nothing_checked_decision_tests {
    use super::*;

    fn step(required: Option<bool>, success: bool) -> StepExecutionResult {
        StepExecutionResult {
            step_index: 0,
            step_type: "command".to_string(),
            step_name: "check".to_string(),
            step_id: None,
            success,
            error: None,
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: 0,
            config: Default::default(),
            verification_details: None,
            output_data: None,
            required,
            resolved_inputs: None,
            extracted_values: None,
            failure_category: None,
            interrupted: None,
        }
    }

    fn phase_result(
        step_results: Vec<StepExecutionResult>,
        all_passed: bool,
    ) -> VerificationPhaseResult {
        VerificationPhaseResult {
            iteration: 1,
            all_passed,
            total_steps: step_results.len(),
            passed_steps: step_results.iter().filter(|s| s.success).count(),
            failed_steps: step_results.iter().filter(|s| !s.success).count(),
            skipped_steps: 0,
            total_duration_ms: 0,
            step_results,
            critical_failure: false,
            console_errors: None,
            app_health: None,
            browser_events: None,
            network_failures: None,
        }
    }

    /// `required` absent counts as required (the shared fold's default);
    /// `Some(false)` does not.
    #[test]
    fn required_step_count_counts_zero_one_and_many() {
        assert_eq!(required_step_count(&[]), 0);
        assert_eq!(required_step_count(&[step(Some(false), true)]), 0);
        assert_eq!(
            required_step_count(&[step(Some(false), true), step(None, true)]),
            1
        );
        assert_eq!(
            required_step_count(&[
                step(Some(true), false),
                step(None, true),
                step(Some(false), false),
            ]),
            2
        );
    }

    /// A fix group whose targeted run selected zero steps is not credited as
    /// fixed. This is the result the old empty arm returned:
    /// `all_passed: true` over nothing.
    #[test]
    fn a_zero_step_targeted_group_is_not_fixed() {
        assert!(!targeted_group_fixed(&phase_result(Vec::new(), true)));
    }

    #[test]
    fn an_informational_only_targeted_group_is_not_fixed() {
        assert!(!targeted_group_fixed(&phase_result(
            vec![step(Some(false), true)],
            true
        )));
    }

    #[test]
    fn a_targeted_group_whose_required_steps_pass_is_fixed() {
        assert!(targeted_group_fixed(&phase_result(
            vec![step(None, true)],
            true
        )));
        assert!(targeted_group_fixed(&phase_result(
            vec![
                step(Some(true), true),
                step(None, true),
                step(Some(false), false),
            ],
            true
        )));
    }

    #[test]
    fn a_targeted_group_with_a_failing_required_step_is_not_fixed() {
        assert!(!targeted_group_fixed(&phase_result(
            vec![step(None, false)],
            false
        )));
    }

    fn loop_exit(
        verification_passed: bool,
        was_stopped: bool,
        no_verification_steps: bool,
    ) -> LoopResult {
        LoopResult {
            iterations_run: 1,
            verification_passed,
            max_iterations_reached: false,
            critical_failure: false,
            was_stopped,
            unfixable_errors: false,
            iteration_results: Vec::new(),
            total_tokens: None,
            total_cost_usd: None,
            fix_attempts_exhausted: false,
            no_verification_steps,
            files_modified: Vec::new(),
            fix_attempts: 0,
            ci_auto_resumes: 0,
            node_rollups: Vec::new(),
        }
    }

    /// A `no_verification_steps` exit under `rollback_policy: clean` (or
    /// `last_good`) leaves the worktree and its commits alone. Neither the
    /// rollback nor the worktree finalizer runs, because nothing refuted the
    /// work.
    #[test]
    fn a_no_verification_steps_exit_under_a_rollback_policy_compensates_nothing() {
        let nothing_checked = loop_exit(false, false, true);
        assert!(!compensation_applies(
            &nothing_checked,
            &RollbackPolicy::Clean
        ));
        assert!(!compensation_applies(
            &nothing_checked,
            &RollbackPolicy::LastGood
        ));
    }

    /// The control: a refuted run does compensate under either policy, and
    /// does not when no policy is configured.
    #[test]
    fn a_failed_exit_under_a_rollback_policy_compensates() {
        let failed = loop_exit(false, false, false);
        assert!(compensation_applies(&failed, &RollbackPolicy::Clean));
        assert!(compensation_applies(&failed, &RollbackPolicy::LastGood));
        assert!(!compensation_applies(&failed, &RollbackPolicy::None));
    }

    #[test]
    fn a_passed_or_stopped_exit_compensates_nothing() {
        assert!(!compensation_applies(
            &loop_exit(true, false, false),
            &RollbackPolicy::Clean
        ));
        assert!(!compensation_applies(
            &loop_exit(false, true, false),
            &RollbackPolicy::Clean
        ));
    }
}
