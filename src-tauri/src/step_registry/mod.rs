//! Step Event Registry Module
//!
//! Provides a centralized registry for step event logging, ensuring:
//! 1. All possible step events are defined exhaustively (StepEventKind enum)
//! 2. Logging policy is enforced (no duplicates, proper sequencing)
//! 3. Events are tracked per execution (ExecutionTracker)
//! 4. A single facade for all step event logging (StepEventLogger)
//!
//! This replaces scattered inline event creation with a unified, maintainable pattern.

mod definitions;
mod facade;
mod policy;
mod tracker;

pub use definitions::StepEventKind;
pub use facade::StepEventLogger;
// Re-exported for future use and testing
#[allow(unused_imports)]
pub use policy::{PolicyViolation, StepKey};
#[allow(unused_imports)]
pub use tracker::ExecutionTracker;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_metadata::StepMetadata;
    use crate::step_types::StepType;
    use crate::unified_workflow_executor::WorkflowPhase;

    #[test]
    fn test_step_event_kind_categorization() {
        assert!(StepEventKind::SetupAiStart.is_start_event());
        assert!(!StepEventKind::SetupAiComplete.is_start_event());
        assert!(StepEventKind::SetupAiComplete.is_complete_event());
        assert!(StepEventKind::SetupAiError.is_error_event());
    }

    #[test]
    fn test_step_key_from_metadata() {
        let metadata =
            StepMetadata::verification("task-123", StepType::Playwright, "Login Test", 2, 1);
        let key = StepKey::from_metadata(&metadata);

        assert_eq!(key.phase, WorkflowPhase::Verification);
        assert_eq!(key.step_type, StepType::Playwright);
        assert_eq!(key.step_index, 2);
        assert_eq!(key.iteration, Some(1));
    }

    #[test]
    fn test_policy_prevents_duplicate_starts() {
        let key = StepKey {
            phase: WorkflowPhase::Setup,
            step_type: StepType::Prompt,
            step_index: 0,
            iteration: None,
        };

        let mut tracker = ExecutionTracker::new("test-exec-1");

        // First start is allowed
        let result = tracker.can_log_start(&key);
        assert!(result.is_ok());
        tracker.record_start(&key);

        // Second start is rejected
        let result = tracker.can_log_start(&key);
        assert!(matches!(result, Err(PolicyViolation::DuplicateStart(_))));
    }

    #[test]
    fn test_policy_requires_start_before_end() {
        let key = StepKey {
            phase: WorkflowPhase::Setup,
            step_type: StepType::Prompt,
            step_index: 0,
            iteration: None,
        };

        let tracker = ExecutionTracker::new("test-exec-1");

        // End without start is rejected
        let result = tracker.can_log_end(&key);
        assert!(matches!(result, Err(PolicyViolation::EndWithoutStart(_))));
    }
}
