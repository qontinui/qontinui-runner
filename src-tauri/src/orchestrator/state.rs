//! Task state machine implementation.
//!
//! The orchestrator enforces state transitions and verification gates.
//! Agents cannot bypass states or determine task completion.

use serde::{Deserialize, Serialize};
use tracing::info;

use super::types::*;

/// The current state of a task in the orchestrator.
///
/// Phase flow:
///   Setup (once) -> [Verification <-> Execution]* -> Completion (once) -> AiSummary -> Complete
///
/// The Verification/Execution loop continues until all required checks pass or max iterations.
/// Deterministic summary data is accumulated throughout execution.
/// AI summary is generated at the end (unless skip_ai_summary is true).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Initial setup
    Initialize,
    /// Planning agent creating/updating verification plan
    Planning,
    /// Setup phase - runs once at the beginning
    Setup,
    /// Worker agent performing work (agentic phase)
    Execution,
    /// Running deterministic verification checks
    DeterministicVerification,
    /// Running AI verification (screenshot evaluation)
    AiVerification,
    /// Completion phase - runs once after loop exits
    Completion,
    /// AI summary generation (runs if not skipped)
    AiSummary,
    /// Task completed successfully
    Complete,
    /// Task stopped (max iterations, user stop, or error)
    Stopped,
    /// Task paused awaiting user decision (at max iterations)
    Paused,
}

impl TaskState {
    /// Check if this is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Complete | TaskState::Stopped)
    }

    /// Check if this state allows resumption.
    pub fn can_resume(&self) -> bool {
        matches!(self, TaskState::Paused | TaskState::Stopped)
    }

    /// Get human-readable state name.
    pub fn display_name(&self) -> &'static str {
        match self {
            TaskState::Initialize => "Initializing",
            TaskState::Planning => "Planning",
            TaskState::Setup => "Setup",
            TaskState::Execution => "Executing",
            TaskState::DeterministicVerification => "Verifying (Deterministic)",
            TaskState::AiVerification => "Verifying (AI)",
            TaskState::Completion => "Completion",
            TaskState::AiSummary => "AI Summary",
            TaskState::Complete => "Complete",
            TaskState::Stopped => "Stopped",
            TaskState::Paused => "Paused",
        }
    }
}

/// Reason for a state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionReason {
    /// Initial transition
    Start,
    /// Plan was created/updated
    PlanCreated,
    /// Setup phase completed
    SetupComplete,
    /// Worker emitted work_complete signal
    WorkComplete,
    /// Worker emitted need_replan signal
    NeedReplan { reason: String },
    /// Worker is continuing
    WorkerContinue,
    /// All deterministic checks passed
    DeterministicPassed,
    /// Deterministic verification failed
    DeterministicFailed { failures: Vec<String> },
    /// AI verification passed
    AiPassed,
    /// AI verification failed
    AiFailed { feedback: String },
    /// No AI criteria to verify
    NoAiCriteria,
    /// Completion phase completed
    CompletionComplete,
    /// AI summary generated (or skipped)
    AiSummaryComplete,
    /// Max iterations reached
    MaxIterations,
    /// User requested stop
    UserStop,
    /// Error occurred
    Error { message: String },
    /// User extended iterations
    Extended { additional: u32 },
}

/// Context for the task orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOrchestrator {
    /// Current state
    pub state: TaskState,

    /// Task run ID
    pub task_run_id: String,

    /// Current iteration number
    pub iteration: u32,

    /// Maximum iterations allowed
    pub max_iterations: u32,

    /// The verification plan (set after planning)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_plan: Option<VerificationPlan>,

    /// Results from last verification attempt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verification_results: Option<IterationVerificationResults>,

    /// Accumulated findings from workers
    #[serde(default)]
    pub findings: Vec<Finding>,

    /// Feedback to provide to the next worker iteration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_feedback: Option<String>,

    /// Whether AI verification should be skipped (no AI criteria)
    pub skip_ai_verification: bool,

    /// State transition history
    #[serde(default)]
    pub transition_history: Vec<StateTransition>,
}

/// Record of a state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    /// Previous state
    pub from: TaskState,
    /// New state
    pub to: TaskState,
    /// Reason for transition
    pub reason: TransitionReason,
    /// Timestamp (as string for serialization)
    pub timestamp: String,
    /// Iteration at time of transition
    pub iteration: u32,
}

impl TaskOrchestrator {
    /// Create a new orchestrator for a task.
    pub fn new(task_run_id: String, max_iterations: u32) -> Self {
        Self {
            state: TaskState::Initialize,
            task_run_id,
            iteration: 0,
            max_iterations,
            verification_plan: None,
            last_verification_results: None,
            findings: Vec::new(),
            worker_feedback: None,
            skip_ai_verification: false,
            transition_history: Vec::new(),
        }
    }

    /// Transition to a new state with reason.
    pub fn transition(&mut self, to: TaskState, reason: TransitionReason) -> Result<(), String> {
        let from = self.state;

        // Validate transition
        if !self.is_valid_transition(from, to) {
            return Err(format!(
                "Invalid state transition from {:?} to {:?}",
                from, to
            ));
        }

        // Record transition
        let transition = StateTransition {
            from,
            to,
            reason: reason.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            iteration: self.iteration,
        };

        info!(
            "Task {} state transition: {:?} -> {:?} (reason: {:?})",
            self.task_run_id, from, to, reason
        );

        self.transition_history.push(transition);
        self.state = to;

        Ok(())
    }

    /// Check if a transition is valid.
    ///
    /// Valid flow: Initialize -> Planning -> Setup -> Verification -> Execution (loop) -> Completion -> Complete
    fn is_valid_transition(&self, from: TaskState, to: TaskState) -> bool {
        match (from, to) {
            // From Initialize
            (TaskState::Initialize, TaskState::Planning) => true,
            (TaskState::Initialize, TaskState::Setup) => true, // Skip planning if no plan needed

            // From Planning
            (TaskState::Planning, TaskState::Setup) => true,
            (TaskState::Planning, TaskState::Execution) => true, // Skip setup if no setup steps

            // From Setup
            (TaskState::Setup, TaskState::DeterministicVerification) => true,
            (TaskState::Setup, TaskState::Execution) => true, // Skip verification if no verification steps
            (TaskState::Setup, TaskState::Stopped) => true,

            // From Execution
            (TaskState::Execution, TaskState::DeterministicVerification) => true,
            (TaskState::Execution, TaskState::Planning) => true, // Replan
            (TaskState::Execution, TaskState::Completion) => true, // Direct to completion (no verification)
            (TaskState::Execution, TaskState::Stopped) => true,
            (TaskState::Execution, TaskState::Paused) => true,

            // From DeterministicVerification
            (TaskState::DeterministicVerification, TaskState::AiVerification) => true,
            (TaskState::DeterministicVerification, TaskState::Execution) => true, // Failed, retry
            (TaskState::DeterministicVerification, TaskState::Completion) => true, // All passed, no AI criteria
            (TaskState::DeterministicVerification, TaskState::Paused) => true,
            (TaskState::DeterministicVerification, TaskState::Stopped) => true,

            // From AiVerification
            (TaskState::AiVerification, TaskState::Completion) => true, // All passed
            (TaskState::AiVerification, TaskState::Execution) => true,  // Failed, retry
            (TaskState::AiVerification, TaskState::Paused) => true,
            (TaskState::AiVerification, TaskState::Stopped) => true,

            // From Completion
            (TaskState::Completion, TaskState::AiSummary) => true,
            (TaskState::Completion, TaskState::Complete) => true, // When skip_ai_summary is true
            (TaskState::Completion, TaskState::Stopped) => true,

            // From AiSummary
            (TaskState::AiSummary, TaskState::Complete) => true,
            (TaskState::AiSummary, TaskState::Stopped) => true,

            // From Paused (resume)
            (TaskState::Paused, TaskState::Execution) => true,
            (TaskState::Paused, TaskState::Stopped) => true,

            // From Stopped (cannot transition, but allow to self for idempotency)
            (TaskState::Stopped, TaskState::Stopped) => true,

            // From Complete (cannot transition)
            (TaskState::Complete, TaskState::Complete) => true,

            _ => false,
        }
    }

    /// Process a worker signal and determine the next action.
    pub fn process_worker_signal(&mut self, signal: Option<WorkerSignal>) -> WorkerSignalAction {
        let signal = signal.unwrap_or(WorkerSignal::Continue);

        match signal {
            WorkerSignal::WorkComplete { reason } => {
                info!(
                    "Task {}: Worker signaled work complete: {:?}",
                    self.task_run_id, reason
                );
                WorkerSignalAction::ProceedToVerification
            }

            WorkerSignal::NeedReplan { reason } => {
                info!(
                    "Task {}: Worker requested replan: {}",
                    self.task_run_id, reason
                );
                WorkerSignalAction::Replan { reason }
            }

            WorkerSignal::Continue => {
                self.iteration += 1;
                if self.iteration >= self.max_iterations {
                    info!(
                        "Task {}: Max iterations ({}) reached",
                        self.task_run_id, self.max_iterations
                    );
                    WorkerSignalAction::MaxIterationsReached
                } else {
                    WorkerSignalAction::ContinueExecution
                }
            }

            WorkerSignal::Finding(finding) => {
                info!(
                    "Task {}: Worker recorded finding: {}",
                    self.task_run_id, finding.description
                );
                self.findings.push(finding);
                // Continue execution after recording finding
                WorkerSignalAction::ContinueExecution
            }
        }
    }

    /// Process deterministic verification results.
    pub fn process_deterministic_results(
        &mut self,
        results: Vec<VerificationResult>,
        plan: &VerificationPlan,
    ) -> DeterministicVerificationAction {
        // Check if all required criteria passed
        let required_criteria: Vec<_> = plan
            .deterministic_criteria()
            .iter()
            .filter(|c| c.required)
            .map(|c| c.id.as_str())
            .collect();

        let all_required_passed = results
            .iter()
            .filter(|r| required_criteria.contains(&r.criterion_id.as_str()))
            .all(|r| r.passed);

        let failed_criteria: Vec<_> = results
            .iter()
            .filter(|r| !r.passed && required_criteria.contains(&r.criterion_id.as_str()))
            .map(|r| r.criterion_id.clone())
            .collect();

        // Update skip flag based on plan
        self.skip_ai_verification = !plan.has_ai_criteria();

        // Store results
        let iteration_results = IterationVerificationResults {
            iteration: self.iteration,
            deterministic_results: results,
            ai_results: vec![],
            deterministic_passed: all_required_passed,
            ai_passed: self.skip_ai_verification, // True if no AI criteria
            all_passed: all_required_passed && self.skip_ai_verification,
            failure_summary: if !all_required_passed {
                Some(format!("Failed criteria: {}", failed_criteria.join(", ")))
            } else {
                None
            },
            applied_overrides: vec![],
            overridden_criteria: vec![],
        };

        self.last_verification_results = Some(iteration_results.clone());

        if all_required_passed {
            if self.skip_ai_verification {
                info!(
                    "Task {}: All deterministic checks passed, no AI criteria - proceeding to completion",
                    self.task_run_id
                );
                DeterministicVerificationAction::ProceedToCompletion
            } else {
                info!(
                    "Task {}: All deterministic checks passed, proceeding to AI verification",
                    self.task_run_id
                );
                DeterministicVerificationAction::ProceedToAiVerification
            }
        } else {
            self.iteration += 1;
            self.worker_feedback = Some(iteration_results.build_feedback());

            if self.iteration >= self.max_iterations {
                info!(
                    "Task {}: Deterministic verification failed, max iterations reached",
                    self.task_run_id
                );
                DeterministicVerificationAction::FailedMaxIterations {
                    failures: failed_criteria,
                }
            } else {
                info!(
                    "Task {}: Deterministic verification failed, returning to execution",
                    self.task_run_id
                );
                DeterministicVerificationAction::FailedReturnToExecution {
                    failures: failed_criteria,
                }
            }
        }
    }

    /// Process AI verification results.
    pub fn process_ai_results(
        &mut self,
        results: Vec<VerificationResult>,
        plan: &VerificationPlan,
    ) -> AiVerificationAction {
        // Check if all required AI criteria passed
        let required_criteria: Vec<_> = plan
            .ai_criteria()
            .iter()
            .filter(|c| c.required)
            .map(|c| c.id.as_str())
            .collect();

        let all_required_passed = results
            .iter()
            .filter(|r| required_criteria.contains(&r.criterion_id.as_str()))
            .all(|r| r.passed);

        // Update stored results
        if let Some(ref mut iter_results) = self.last_verification_results {
            iter_results.ai_results = results.clone();
            iter_results.ai_passed = all_required_passed;
            iter_results.all_passed = iter_results.deterministic_passed && all_required_passed;
            if !all_required_passed {
                iter_results.failure_summary = Some(iter_results.build_feedback());
            }
        }

        if all_required_passed {
            info!(
                "Task {}: All AI verification passed - proceeding to completion",
                self.task_run_id
            );
            AiVerificationAction::ProceedToCompletion
        } else {
            self.iteration += 1;
            if let Some(ref results) = self.last_verification_results {
                self.worker_feedback = Some(results.build_feedback());
            }

            if self.iteration >= self.max_iterations {
                info!(
                    "Task {}: AI verification failed, max iterations reached",
                    self.task_run_id
                );
                AiVerificationAction::FailedMaxIterations
            } else {
                info!(
                    "Task {}: AI verification failed, returning to execution",
                    self.task_run_id
                );
                AiVerificationAction::FailedReturnToExecution
            }
        }
    }

    /// Extend the maximum iterations.
    pub fn extend_iterations(&mut self, additional: u32, guidance: Option<String>) {
        self.max_iterations += additional;
        if let Some(guide) = guidance {
            self.worker_feedback = Some(format!(
                "## Additional Guidance\n\n{}\n\n{}",
                guide,
                self.worker_feedback.as_deref().unwrap_or("")
            ));
        }
        info!(
            "Task {}: Extended iterations by {}, new max: {}",
            self.task_run_id, additional, self.max_iterations
        );
    }

    /// Get the completion result based on current state.
    pub fn get_completion_result(&self) -> TaskCompletionResult {
        match self.state {
            TaskState::Complete => TaskCompletionResult::Success {
                iterations: self.iteration,
                findings: self.findings.clone(),
                verification_results: self
                    .last_verification_results
                    .clone()
                    .unwrap_or_else(|| IterationVerificationResults::all_passed(self.iteration)),
            },

            TaskState::Stopped => TaskCompletionResult::Stopped {
                at_iteration: self.iteration,
                findings: self.findings.clone(),
                can_resume: true,
            },

            TaskState::Paused => TaskCompletionResult::Paused {
                at_iteration: self.iteration,
                max_iterations: self.max_iterations,
                last_results: self.last_verification_results.clone(),
                findings: self.findings.clone(),
            },

            _ => TaskCompletionResult::Failed {
                reason: format!("Task in unexpected state: {:?}", self.state),
                iterations: self.iteration,
                last_results: self.last_verification_results.clone(),
                findings: self.findings.clone(),
            },
        }
    }
}

/// Action to take after processing a worker signal.
#[derive(Debug, Clone)]
pub enum WorkerSignalAction {
    /// Proceed to deterministic verification
    ProceedToVerification,
    /// Return to planning state
    Replan { reason: String },
    /// Continue execution (more work needed)
    ContinueExecution,
    /// Max iterations reached
    MaxIterationsReached,
}

/// Action to take after deterministic verification.
#[derive(Debug, Clone)]
pub enum DeterministicVerificationAction {
    /// All passed, proceed to AI verification
    ProceedToAiVerification,
    /// All passed, no AI criteria - proceed to completion phase
    ProceedToCompletion,
    /// Failed, return to execution
    FailedReturnToExecution { failures: Vec<String> },
    /// Failed, max iterations reached
    FailedMaxIterations { failures: Vec<String> },
}

/// Action to take after AI verification.
#[derive(Debug, Clone)]
pub enum AiVerificationAction {
    /// All passed - proceed to completion phase
    ProceedToCompletion,
    /// Failed, return to execution
    FailedReturnToExecution,
    /// Failed, max iterations reached
    FailedMaxIterations,
}

/// Action to take after completion phase.
#[derive(Debug, Clone)]
pub enum CompletionAction {
    /// Proceed to AI summary generation
    ProceedToAiSummary,
    /// Skip AI summary and complete directly
    SkipAiSummaryComplete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_signal_parsing() {
        // Test WORK_COMPLETE
        let output = "I've fixed the bug. [WORK_COMPLETE] The validation now works.";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(matches!(signal, Some(WorkerSignal::WorkComplete { .. })));

        // Test NEED_REPLAN
        let output = "[NEED_REPLAN] The issue is in a different service.";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(matches!(signal, Some(WorkerSignal::NeedReplan { .. })));

        // Test FINDING
        let output = "[FINDING:bug] The login function doesn't validate email format";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(matches!(signal, Some(WorkerSignal::Finding(_))));

        // Test no signal
        let output = "I'm still working on the problem...";
        let signal = WorkerSignal::parse_from_output(output);
        assert!(signal.is_none());
    }

    #[test]
    fn test_state_transitions() {
        let mut orchestrator = TaskOrchestrator::new("test-123".to_string(), 10);

        // Valid: Initialize -> Planning
        assert!(orchestrator
            .transition(TaskState::Planning, TransitionReason::Start)
            .is_ok());

        // Valid: Planning -> Setup
        assert!(orchestrator
            .transition(TaskState::Setup, TransitionReason::PlanCreated)
            .is_ok());

        // Valid: Setup -> DeterministicVerification
        assert!(orchestrator
            .transition(
                TaskState::DeterministicVerification,
                TransitionReason::SetupComplete
            )
            .is_ok());

        // Valid: DeterministicVerification -> Completion (all passed, no AI)
        assert!(orchestrator
            .transition(TaskState::Completion, TransitionReason::DeterministicPassed)
            .is_ok());

        // Valid: Completion -> AiSummary
        assert!(orchestrator
            .transition(TaskState::AiSummary, TransitionReason::CompletionComplete)
            .is_ok());

        // Valid: AiSummary -> Complete
        assert!(orchestrator
            .transition(TaskState::Complete, TransitionReason::AiSummaryComplete)
            .is_ok());
    }

    #[test]
    fn test_skip_ai_summary_transition() {
        let mut orchestrator = TaskOrchestrator::new("test-456".to_string(), 10);

        // Skip to Completion state for testing
        assert!(orchestrator
            .transition(TaskState::Planning, TransitionReason::Start)
            .is_ok());
        assert!(orchestrator
            .transition(TaskState::Setup, TransitionReason::PlanCreated)
            .is_ok());
        assert!(orchestrator
            .transition(
                TaskState::DeterministicVerification,
                TransitionReason::SetupComplete
            )
            .is_ok());
        assert!(orchestrator
            .transition(TaskState::Completion, TransitionReason::DeterministicPassed)
            .is_ok());

        // Valid: Completion -> Complete directly (when skip_ai_summary is true)
        assert!(orchestrator
            .transition(TaskState::Complete, TransitionReason::CompletionComplete)
            .is_ok());
    }

    #[test]
    fn test_invalid_transitions() {
        let mut orchestrator = TaskOrchestrator::new("test-123".to_string(), 10);

        // Valid: Initialize -> Planning
        assert!(orchestrator
            .transition(TaskState::Planning, TransitionReason::Start)
            .is_ok());

        // Valid: Planning -> Execution (skip setup)
        assert!(orchestrator
            .transition(TaskState::Execution, TransitionReason::PlanCreated)
            .is_ok());

        // Invalid: Execution -> Complete (must go through Completion)
        let result = orchestrator.transition(TaskState::Complete, TransitionReason::WorkComplete);
        assert!(result.is_err());
    }
}
