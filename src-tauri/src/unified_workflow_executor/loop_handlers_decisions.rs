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
//! Handlers already covered:
//!   - `handle_check_preconditions`     → `decide_preconditions`
//!   - `handle_evaluate_verification`   → `decide_evaluate_verification`
//!   - `handle_check_post_agentic_signals` → `decide_post_agentic_signals`
//!   - `handle_approval_gate`           → `decide_approval_mode`, `decide_approval_response`
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

use super::loop_state_machine::CompletionReason;

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
    /// All verification steps passed.
    pub all_passed: bool,
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
/// Note: the pass/critical branches short-circuit *before* fix-progress
/// bookkeeping runs in the original handler, so we don't attach a
/// `FixProgressUpdate` to them — they're terminal regardless.
pub fn decide_evaluate_verification(snap: &VerificationEvalSnapshot) -> VerificationEvalDecision {
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
