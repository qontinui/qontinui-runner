//! Unified Workflow State Definitions
//!
//! This module defines the explicit state machine for Unified Workflows.
//! These states replace the implicit state inference from task_runs status fields.

use serde::{Deserialize, Serialize};

use crate::workflow_state::WorkflowState;

/// State of a Unified Workflow execution.
///
/// The unified workflow follows this flow:
/// 1. Created -> SetupRunning -> SetupComplete
/// 2. VerificationRunning -> VerificationComplete (repeat with AgenticRunning as needed)
/// 3. CompletionRunning -> CompletionComplete (only if verification passed)
///
/// At any point, the workflow can transition to Failed or Stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnifiedWorkflowState {
    /// Workflow created but not yet started.
    Created,

    /// Setup phase is running.
    ///
    /// Written PER STAGE, not once per workflow: the write sits inside
    /// `run_multi_stage`'s stage loop, guarded by the stage's own
    /// `setup_*_steps`. That is why it carries the accumulator.
    SetupRunning {
        /// Stage index for multi-stage workflows. None = single-stage.
        ///
        /// REQUIRED for `any_passed` below to mean anything. The accumulator
        /// is a snapshot taken at the START of a particular stage, so a
        /// resume may only apply it when it re-enters at THAT stage. Without
        /// this field the resume falls back to
        /// `stage_index_from_checkpoints`, which answers with the newest
        /// COMPLETED checkpoint — an EARLIER stage whenever the crash landed
        /// before this stage journalled one, which is exactly the window this
        /// state covers. The run would then re-enter stage `k-1` already
        /// seeded with stage `k-1`'s own verdict, and a re-run failure there
        /// would be masked into overall success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        /// The run's `any_stage_passed` accumulator as of this write.
        ///
        /// Same durability argument as [`UnifiedWorkflowState::StageComplete::any_passed`],
        /// applied one layer earlier. The state row is overwritten on every
        /// transition, so the moment this state is persisted the
        /// `stage_complete` row that held the verdict is GONE — and without
        /// this field the verdict is gone with it, and a resume through this
        /// door restarts the accumulator at `false`.
        ///
        /// The value is the accumulator as of the START of the stage being
        /// run, which is exactly right: `any_stage_passed` is only ever set
        /// after a stage's `LoopResult` comes back, i.e. after that stage has
        /// finished persisting states, so it cannot change under a mid-stage
        /// write.
        ///
        /// `#[serde(default)]` keeps the enum deserializable from a row written
        /// before this field existed. Note the resume path does NOT go through
        /// that deserializer — `resume.rs` reads the raw `any_passed` key off
        /// `state_data` and applies its own default — so the attribute is the
        /// belt to that reader's braces, not the mechanism. Both answer
        /// `false` for an absent key, which is the verdict today's code
        /// already produces for that resume.
        #[serde(default)]
        any_passed: bool,
    },

    /// Setup phase completed successfully.
    SetupComplete,

    /// Verification phase is running.
    VerificationRunning {
        /// Current iteration (1-indexed).
        iteration: u32,
        /// Stage index for multi-stage workflows. None = single-stage (backward compat).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        /// The run's `any_stage_passed` accumulator as of this write.
        ///
        /// See [`UnifiedWorkflowState::SetupRunning::any_passed`] for why every
        /// mid-run state carries it, why the value cannot go stale within a
        /// stage, and what `#[serde(default)]` does and does not guarantee.
        #[serde(default)]
        any_passed: bool,
    },

    /// Verification phase completed for this iteration.
    VerificationComplete {
        /// Current iteration (1-indexed).
        iteration: u32,
        /// Whether all verification checks passed.
        ///
        /// This is THIS ITERATION's verdict, not the run's — `any_passed`
        /// below is the accumulator. Two different questions on one row.
        passed: bool,
        /// Stage index for multi-stage workflows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        /// The run's `any_stage_passed` accumulator as of this write.
        ///
        /// See [`UnifiedWorkflowState::SetupRunning::any_passed`] for why every
        /// mid-run state carries it, why the value cannot go stale within a
        /// stage, and what `#[serde(default)]` does and does not guarantee.
        #[serde(default)]
        any_passed: bool,
    },

    /// Agentic phase is running (AI fixing issues).
    AgenticRunning {
        /// Current iteration (1-indexed).
        iteration: u32,
        /// Stage index for multi-stage workflows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        /// The run's `any_stage_passed` accumulator as of this write.
        ///
        /// See [`UnifiedWorkflowState::SetupRunning::any_passed`] for why every
        /// mid-run state carries it, why the value cannot go stale within a
        /// stage, and what `#[serde(default)]` does and does not guarantee.
        #[serde(default)]
        any_passed: bool,
    },

    /// Agentic phase completed for this iteration.
    AgenticComplete {
        /// Current iteration (1-indexed).
        iteration: u32,
        /// Stage index for multi-stage workflows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        /// The run's `any_stage_passed` accumulator as of this write.
        ///
        /// See [`UnifiedWorkflowState::SetupRunning::any_passed`] for why every
        /// mid-run state carries it, why the value cannot go stale within a
        /// stage, and what `#[serde(default)]` does and does not guarantee.
        #[serde(default)]
        any_passed: bool,
    },

    /// A stage completed in a multi-stage workflow.
    StageComplete {
        /// Index of the completed stage (0-indexed).
        stage_index: u32,
        /// The run's `any_stage_passed` accumulator as of this stage.
        ///
        /// This is the ONLY durable record of that accumulator. There is no
        /// journal of `stage_complete` states to replay: the state row is an
        /// `INSERT ... ON CONFLICT (execution_id) DO UPDATE`
        /// (`database/pg/workflow_state.rs`), so one row per execution is
        /// overwritten on every transition, and `record_stage_transition`
        /// (`task_state.rs`) is in-memory only. `any_stage_passed` is
        /// **monotone** — set true in `run_multi_stage` when a stage's
        /// verification passes, never cleared — so the single surviving row
        /// carries exactly the value the uninterrupted run would have had, and
        /// a resume can seed the accumulator from it instead of restarting it
        /// at `false`.
        ///
        /// `#[serde(default)]` so state rows written before this field existed
        /// still deserialize. Such a row reads back `false`, which is the
        /// verdict today's code already produces for that resume — so no
        /// regression, but it is a real limitation: a pre-upgrade row cannot
        /// tell a resumed run that earlier stages passed.
        #[serde(default)]
        any_passed: bool,
    },

    /// Waiting for human approval before proceeding.
    ApprovalPending {
        /// Current iteration (1-indexed).
        iteration: u32,
        /// Stage index for multi-stage workflows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        /// Unique ID of the approval request.
        approval_id: String,
        /// Prompt shown to the human reviewer.
        prompt: String,
        /// The run's `any_stage_passed` accumulator as of this write.
        ///
        /// See [`UnifiedWorkflowState::SetupRunning::any_passed`] for why every
        /// mid-run state carries it, why the value cannot go stale within a
        /// stage, and what `#[serde(default)]` does and does not guarantee.
        #[serde(default)]
        any_passed: bool,
    },

    /// Completion phase is running.
    CompletionRunning {
        /// The run's `any_stage_passed` accumulator.
        ///
        /// Reaching this state does prove the accumulator was `true` — the
        /// write sits inside `if overall_passed` in `run_multi_stage`. But
        /// that is an invariant held by STATEMENT PLACEMENT inside a very
        /// long function, not by data: move the write out of the `if`, or add
        /// a second writer, and every resume through this door silently
        /// reports success. Carrying the value costs one field and turns the
        /// manufactured verdict into a read one for every row this binary
        /// writes.
        ///
        /// One shape still cannot be read and is answered by the invariant: a
        /// PRE-UPGRADE row, which parsed fine and genuinely has no key. That
        /// population is closed and shrinking. A NULL, unparseable or
        /// non-boolean row is NOT in it and reads `false` — see
        /// `resume.rs`'s `PersistedAnyPassed`.
        ///
        /// `#[serde(default)]` would read a pre-upgrade row back as `false`,
        /// which is why the resume arm falls back to the structural
        /// invariant (`true`) rather than to the serde default when the key
        /// is absent. See `resume_point_from_state`'s `"completion_running"`
        /// arm.
        #[serde(default)]
        any_passed: bool,
    },

    /// Completion phase finished (workflow done).
    CompletionComplete,

    /// Workflow failed with an error.
    Failed {
        /// Reason for failure.
        reason: String,
        /// Phase where failure occurred.
        phase: Option<String>,
        /// Iteration where failure occurred (if applicable).
        iteration: Option<u32>,
    },

    /// Workflow was stopped by user request.
    Stopped {
        /// Phase where it was stopped.
        phase: Option<String>,
        /// Iteration where it was stopped (if applicable).
        iteration: Option<u32>,
    },
}

impl WorkflowState for UnifiedWorkflowState {
    fn name(&self) -> &'static str {
        match self {
            UnifiedWorkflowState::Created => "created",
            UnifiedWorkflowState::SetupRunning { .. } => "setup_running",
            UnifiedWorkflowState::SetupComplete => "setup_complete",
            UnifiedWorkflowState::VerificationRunning { .. } => "verification_running",
            UnifiedWorkflowState::VerificationComplete { .. } => "verification_complete",
            UnifiedWorkflowState::AgenticRunning { .. } => "agentic_running",
            UnifiedWorkflowState::AgenticComplete { .. } => "agentic_complete",
            UnifiedWorkflowState::StageComplete { .. } => "stage_complete",
            UnifiedWorkflowState::ApprovalPending { .. } => "approval_pending",
            UnifiedWorkflowState::CompletionRunning { .. } => "completion_running",
            UnifiedWorkflowState::CompletionComplete => "completion_complete",
            UnifiedWorkflowState::Failed { .. } => "failed",
            UnifiedWorkflowState::Stopped { .. } => "stopped",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            UnifiedWorkflowState::CompletionComplete
                | UnifiedWorkflowState::Failed { .. }
                | UnifiedWorkflowState::Stopped { .. }
        )
    }

    fn is_resumable(&self) -> bool {
        // We can resume from any "running" state or when waiting for approval
        matches!(
            self,
            UnifiedWorkflowState::SetupRunning { .. }
                | UnifiedWorkflowState::VerificationRunning { .. }
                | UnifiedWorkflowState::AgenticRunning { .. }
                | UnifiedWorkflowState::ApprovalPending { .. }
                | UnifiedWorkflowState::CompletionRunning { .. }
        )
    }

    fn phase(&self) -> Option<&'static str> {
        match self {
            UnifiedWorkflowState::Created => None,
            UnifiedWorkflowState::SetupRunning { .. } | UnifiedWorkflowState::SetupComplete => {
                Some("setup")
            }
            UnifiedWorkflowState::VerificationRunning { .. }
            | UnifiedWorkflowState::VerificationComplete { .. } => Some("verification"),
            UnifiedWorkflowState::AgenticRunning { .. }
            | UnifiedWorkflowState::AgenticComplete { .. } => Some("agentic"),
            UnifiedWorkflowState::StageComplete { .. } => None,
            UnifiedWorkflowState::ApprovalPending { .. } => Some("agentic"),
            UnifiedWorkflowState::CompletionRunning { .. }
            | UnifiedWorkflowState::CompletionComplete => Some("completion"),
            // For failed/stopped states, we can't return &'static str since phase is owned String
            // Just return the known phases or None
            UnifiedWorkflowState::Failed { phase, .. } => match phase.as_deref() {
                Some("setup") => Some("setup"),
                Some("verification") => Some("verification"),
                Some("agentic") => Some("agentic"),
                Some("completion") => Some("completion"),
                _ => None,
            },
            UnifiedWorkflowState::Stopped { phase, .. } => match phase.as_deref() {
                Some("setup") => Some("setup"),
                Some("verification") => Some("verification"),
                Some("agentic") => Some("agentic"),
                Some("completion") => Some("completion"),
                _ => None,
            },
        }
    }

    fn iteration(&self) -> Option<u32> {
        match self {
            UnifiedWorkflowState::VerificationRunning { iteration, .. }
            | UnifiedWorkflowState::VerificationComplete { iteration, .. }
            | UnifiedWorkflowState::AgenticRunning { iteration, .. }
            | UnifiedWorkflowState::AgenticComplete { iteration, .. }
            | UnifiedWorkflowState::ApprovalPending { iteration, .. } => Some(*iteration),
            UnifiedWorkflowState::Failed { iteration, .. }
            | UnifiedWorkflowState::Stopped { iteration, .. } => *iteration,
            _ => None,
        }
    }
}

impl UnifiedWorkflowState {
    /// Create a new Created state.
    pub fn created() -> Self {
        UnifiedWorkflowState::Created
    }

    /// Create a SetupRunning state.
    ///
    /// `any_passed` is the caller's live `any_stage_passed` accumulator. It is
    /// a REQUIRED parameter, here and on every constructor below, rather than
    /// a defaulted or builder-set field: a call site that forgets it would
    /// persist `false` and silently lose the verdict of every stage that
    /// already passed, which is precisely the defect these fields close. A
    /// required parameter makes a missed site a compile error.
    pub fn setup_running(stage_index: Option<u32>, any_passed: bool) -> Self {
        UnifiedWorkflowState::SetupRunning {
            stage_index,
            any_passed,
        }
    }

    /// Create a SetupComplete state.
    pub fn setup_complete() -> Self {
        UnifiedWorkflowState::SetupComplete
    }

    /// Create a VerificationRunning state.
    ///
    /// `stage_index` is REQUIRED, not defaulted, for the same reason
    /// `any_passed` is: the accumulator is a per-stage snapshot, so a row that
    /// records the verdict but not the stage it was taken at cannot be applied
    /// safely on resume. Every call site has `LoopConfig::stage_index` (or the
    /// loop's own `stage_idx`) in scope.
    pub fn verification_running(
        iteration: u32,
        stage_index: Option<u32>,
        any_passed: bool,
    ) -> Self {
        UnifiedWorkflowState::VerificationRunning {
            iteration,
            stage_index,
            any_passed,
        }
    }

    /// Create a VerificationComplete state.
    ///
    /// `passed` is THIS ITERATION's verdict; `any_passed` is the run's
    /// accumulator. Two `bool`s with different meanings, in that order.    ///
    /// `stage_index` is REQUIRED, not defaulted, for the same reason
    /// `any_passed` is: the accumulator is a per-stage snapshot, so a row that
    /// records the verdict but not the stage it was taken at cannot be applied
    /// safely on resume. Every call site has `LoopConfig::stage_index` (or the
    /// loop's own `stage_idx`) in scope.
    pub fn verification_complete(
        iteration: u32,
        passed: bool,
        stage_index: Option<u32>,
        any_passed: bool,
    ) -> Self {
        UnifiedWorkflowState::VerificationComplete {
            iteration,
            passed,
            stage_index,
            any_passed,
        }
    }

    /// Create an AgenticRunning state.
    ///
    /// `stage_index` is REQUIRED, not defaulted, for the same reason
    /// `any_passed` is: the accumulator is a per-stage snapshot, so a row that
    /// records the verdict but not the stage it was taken at cannot be applied
    /// safely on resume. Every call site has `LoopConfig::stage_index` (or the
    /// loop's own `stage_idx`) in scope.
    pub fn agentic_running(iteration: u32, stage_index: Option<u32>, any_passed: bool) -> Self {
        UnifiedWorkflowState::AgenticRunning {
            iteration,
            stage_index,
            any_passed,
        }
    }

    /// Create an AgenticComplete state.
    ///
    /// `stage_index` is REQUIRED, not defaulted, for the same reason
    /// `any_passed` is: the accumulator is a per-stage snapshot, so a row that
    /// records the verdict but not the stage it was taken at cannot be applied
    /// safely on resume. Every call site has `LoopConfig::stage_index` (or the
    /// loop's own `stage_idx`) in scope.
    pub fn agentic_complete(iteration: u32, stage_index: Option<u32>, any_passed: bool) -> Self {
        UnifiedWorkflowState::AgenticComplete {
            iteration,
            stage_index,
            any_passed,
        }
    }

    /// Create a StageComplete state.
    ///
    /// `any_passed` is the caller's live `any_stage_passed` accumulator — see
    /// the field's docs for why it is persisted rather than re-derived.
    pub fn stage_complete(stage_index: u32, any_passed: bool) -> Self {
        UnifiedWorkflowState::StageComplete {
            stage_index,
            any_passed,
        }
    }

    /// Create an ApprovalPending state.
    pub fn approval_pending(
        iteration: u32,
        stage_index: Option<u32>,
        approval_id: impl Into<String>,
        prompt: impl Into<String>,
        any_passed: bool,
    ) -> Self {
        UnifiedWorkflowState::ApprovalPending {
            iteration,
            stage_index,
            approval_id: approval_id.into(),
            prompt: prompt.into(),
            any_passed,
        }
    }

    /// Create a CompletionRunning state.
    pub fn completion_running(any_passed: bool) -> Self {
        UnifiedWorkflowState::CompletionRunning { any_passed }
    }

    /// Create a CompletionComplete state.
    pub fn completion_complete() -> Self {
        UnifiedWorkflowState::CompletionComplete
    }

    /// Create a Failed state.
    pub fn failed(reason: impl Into<String>) -> Self {
        UnifiedWorkflowState::Failed {
            reason: reason.into(),
            phase: None,
            iteration: None,
        }
    }

    /// Create a Failed state with phase info.
    pub fn failed_in_phase(
        reason: impl Into<String>,
        phase: impl Into<String>,
        iteration: Option<u32>,
    ) -> Self {
        UnifiedWorkflowState::Failed {
            reason: reason.into(),
            phase: Some(phase.into()),
            iteration,
        }
    }

    /// Create a Stopped state.
    pub fn stopped() -> Self {
        UnifiedWorkflowState::Stopped {
            phase: None,
            iteration: None,
        }
    }

    /// Create a Stopped state with phase info.
    pub fn stopped_in_phase(phase: impl Into<String>, iteration: Option<u32>) -> Self {
        UnifiedWorkflowState::Stopped {
            phase: Some(phase.into()),
            iteration,
        }
    }

    /// Check if the workflow is in a "complete" state (passed verification and ran completion).
    pub fn is_success(&self) -> bool {
        matches!(self, UnifiedWorkflowState::CompletionComplete)
    }

    /// Check if verification passed in the last VerificationComplete state.
    pub fn verification_passed(&self) -> Option<bool> {
        match self {
            UnifiedWorkflowState::VerificationComplete { passed, .. } => Some(*passed),
            _ => None,
        }
    }

    /// Get the error reason if in Failed state.
    pub fn error_reason(&self) -> Option<&str> {
        match self {
            UnifiedWorkflowState::Failed { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

impl std::fmt::Display for UnifiedWorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnifiedWorkflowState::Created => write!(f, "Created"),
            UnifiedWorkflowState::SetupRunning { .. } => write!(f, "Setup Running"),
            UnifiedWorkflowState::SetupComplete => write!(f, "Setup Complete"),
            UnifiedWorkflowState::VerificationRunning {
                iteration,
                stage_index,
                ..
            } => {
                if let Some(si) = stage_index {
                    write!(
                        f,
                        "Verification Running (stage {}, iteration {})",
                        si, iteration
                    )
                } else {
                    write!(f, "Verification Running (iteration {})", iteration)
                }
            }
            UnifiedWorkflowState::VerificationComplete {
                iteration,
                passed,
                stage_index,
                ..
            } => {
                let status = if *passed { "PASSED" } else { "FAILED" };
                if let Some(si) = stage_index {
                    write!(
                        f,
                        "Verification Complete (stage {}, iteration {}, {})",
                        si, iteration, status
                    )
                } else {
                    write!(
                        f,
                        "Verification Complete (iteration {}, {})",
                        iteration, status
                    )
                }
            }
            UnifiedWorkflowState::AgenticRunning {
                iteration,
                stage_index,
                ..
            } => {
                if let Some(si) = stage_index {
                    write!(f, "Agentic Running (stage {}, iteration {})", si, iteration)
                } else {
                    write!(f, "Agentic Running (iteration {})", iteration)
                }
            }
            UnifiedWorkflowState::AgenticComplete {
                iteration,
                stage_index,
                ..
            } => {
                if let Some(si) = stage_index {
                    write!(
                        f,
                        "Agentic Complete (stage {}, iteration {})",
                        si, iteration
                    )
                } else {
                    write!(f, "Agentic Complete (iteration {})", iteration)
                }
            }
            UnifiedWorkflowState::StageComplete {
                stage_index,
                any_passed,
            } => {
                write!(
                    f,
                    "Stage {} Complete (any stage passed: {})",
                    stage_index, any_passed
                )
            }
            UnifiedWorkflowState::ApprovalPending {
                iteration,
                stage_index,
                ..
            } => {
                if let Some(si) = stage_index {
                    write!(
                        f,
                        "Approval Pending (stage {}, iteration {})",
                        si, iteration
                    )
                } else {
                    write!(f, "Approval Pending (iteration {})", iteration)
                }
            }
            UnifiedWorkflowState::CompletionRunning { .. } => write!(f, "Completion Running"),
            UnifiedWorkflowState::CompletionComplete => write!(f, "Completion Complete"),
            UnifiedWorkflowState::Failed { reason, phase, .. } => {
                if let Some(p) = phase {
                    write!(f, "Failed in {}: {}", p, reason)
                } else {
                    write!(f, "Failed: {}", reason)
                }
            }
            UnifiedWorkflowState::Stopped { phase, .. } => {
                if let Some(p) = phase {
                    write!(f, "Stopped in {}", p)
                } else {
                    write!(f, "Stopped")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_names() {
        assert_eq!(UnifiedWorkflowState::Created.name(), "created");
        assert_eq!(
            UnifiedWorkflowState::SetupRunning {
                stage_index: None,
                any_passed: false
            }
            .name(),
            "setup_running"
        );
        assert_eq!(
            UnifiedWorkflowState::verification_running(1, None, false).name(),
            "verification_running"
        );
        assert_eq!(UnifiedWorkflowState::failed("test").name(), "failed");
    }

    #[test]
    fn test_terminal_states() {
        assert!(!UnifiedWorkflowState::Created.is_terminal());
        assert!(!UnifiedWorkflowState::SetupRunning {
            stage_index: None,
            any_passed: false
        }
        .is_terminal());
        assert!(!UnifiedWorkflowState::verification_running(1, None, false).is_terminal());
        assert!(UnifiedWorkflowState::CompletionComplete.is_terminal());
        assert!(UnifiedWorkflowState::failed("error").is_terminal());
        assert!(UnifiedWorkflowState::stopped().is_terminal());
    }

    #[test]
    fn test_resumable_states() {
        assert!(!UnifiedWorkflowState::Created.is_resumable());
        assert!(UnifiedWorkflowState::SetupRunning {
            stage_index: None,
            any_passed: false
        }
        .is_resumable());
        assert!(UnifiedWorkflowState::verification_running(1, None, false).is_resumable());
        assert!(UnifiedWorkflowState::agentic_running(1, None, false).is_resumable());
        assert!(UnifiedWorkflowState::CompletionRunning { any_passed: true }.is_resumable());
        assert!(!UnifiedWorkflowState::CompletionComplete.is_resumable());
        assert!(!UnifiedWorkflowState::failed("error").is_resumable());
    }

    #[test]
    fn test_phase_and_iteration() {
        assert_eq!(UnifiedWorkflowState::Created.phase(), None);
        assert_eq!(
            UnifiedWorkflowState::SetupRunning {
                stage_index: None,
                any_passed: false
            }
            .phase(),
            Some("setup")
        );
        assert_eq!(
            UnifiedWorkflowState::verification_running(2, None, false).phase(),
            Some("verification")
        );
        assert_eq!(
            UnifiedWorkflowState::verification_running(2, None, false).iteration(),
            Some(2)
        );
        assert_eq!(
            UnifiedWorkflowState::agentic_running(3, None, false).iteration(),
            Some(3)
        );
    }

    #[test]
    fn test_serialization() {
        let state = UnifiedWorkflowState::verification_running(2, None, false);
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("verification_running"));
        assert!(json.contains("\"iteration\":2"));

        let parsed: UnifiedWorkflowState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }

    /// A `stage_complete` row written before `any_passed` existed must still
    /// deserialize. It reads back `false` — the verdict today's code already
    /// produces — so an old row degrades to the pre-existing behaviour rather
    /// than failing the resume outright.
    #[test]
    fn test_stage_complete_without_any_passed_still_deserializes() {
        let legacy = r#"{"type":"stage_complete","stage_index":1}"#;
        let parsed: UnifiedWorkflowState = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            parsed,
            UnifiedWorkflowState::StageComplete {
                stage_index: 1,
                any_passed: false,
            }
        );
    }

    #[test]
    fn test_stage_complete_round_trips_any_passed() {
        let state = UnifiedWorkflowState::stage_complete(2, true);
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"any_passed\":true"), "json was {}", json);
        let parsed: UnifiedWorkflowState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }
}
