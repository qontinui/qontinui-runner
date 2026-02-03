//! Logging Policy Module
//!
//! Defines the validation rules for step event logging:
//! 1. No duplicate starts - each step can only have one start event
//! 2. Require start before end - complete/error requires prior start event
//! 3. No duplicate ends - each step can only have one complete/error event

use crate::step_metadata::StepMetadata;
use crate::step_types::StepType;
use crate::unified_workflow_executor::WorkflowPhase;
use std::fmt;

/// Unique identifier for a step instance.
///
/// This key uniquely identifies a step within an execution context,
/// allowing the tracker to detect duplicate events.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepKey {
    /// Which phase this step belongs to
    pub phase: WorkflowPhase,
    /// The type of step
    pub step_type: StepType,
    /// Index of this step within its phase
    pub step_index: usize,
    /// Iteration number (for verification/agentic phases)
    pub iteration: Option<u32>,
}

impl StepKey {
    /// Create a new step key from metadata.
    pub fn from_metadata(metadata: &StepMetadata) -> Self {
        Self {
            phase: metadata.phase(),
            step_type: metadata.step_type,
            step_index: metadata.step_index,
            iteration: metadata.iteration(),
        }
    }

    /// Create a step key for a setup phase step.
    pub fn setup(step_type: StepType, step_index: usize) -> Self {
        Self {
            phase: WorkflowPhase::Setup,
            step_type,
            step_index,
            iteration: None,
        }
    }

    /// Create a step key for a verification phase step.
    pub fn verification(step_type: StepType, step_index: usize, iteration: u32) -> Self {
        Self {
            phase: WorkflowPhase::Verification,
            step_type,
            step_index,
            iteration: Some(iteration),
        }
    }

    /// Create a step key for an agentic phase step.
    pub fn agentic(step_type: StepType, step_index: usize, iteration: u32) -> Self {
        Self {
            phase: WorkflowPhase::Agentic,
            step_type,
            step_index,
            iteration: Some(iteration),
        }
    }

    /// Create a step key for a completion phase step.
    pub fn completion(step_type: StepType, step_index: usize) -> Self {
        Self {
            phase: WorkflowPhase::Completion,
            step_type,
            step_index,
            iteration: None,
        }
    }
}

impl fmt::Display for StepKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(iter) = self.iteration {
            write!(
                f,
                "{}-{}-{}-iter{}",
                self.phase.as_str(),
                self.step_type.as_str(),
                self.step_index,
                iter
            )
        } else {
            write!(
                f,
                "{}-{}-{}",
                self.phase.as_str(),
                self.step_type.as_str(),
                self.step_index
            )
        }
    }
}

/// Validation errors for logging policy violations.
#[derive(Debug, Clone)]
pub enum PolicyViolation {
    /// Attempted to log a start event for a step that was already started
    DuplicateStart(StepKey),
    /// Attempted to log an end event without a prior start event
    EndWithoutStart(StepKey),
    /// Attempted to log an end event for a step that already ended
    DuplicateEnd(StepKey),
}

impl fmt::Display for PolicyViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyViolation::DuplicateStart(key) => {
                write!(f, "Duplicate start event for step: {}", key)
            }
            PolicyViolation::EndWithoutStart(key) => {
                write!(f, "End event without prior start for step: {}", key)
            }
            PolicyViolation::DuplicateEnd(key) => {
                write!(f, "Duplicate end event for step: {}", key)
            }
        }
    }
}

impl std::error::Error for PolicyViolation {}

/// Logging policy rules.
///
/// This struct defines the validation logic for step events.
/// It is stateless - actual tracking is done by ExecutionTracker.
pub struct LoggingPolicy;

impl LoggingPolicy {
    /// Validate that a start event can be logged for this step.
    ///
    /// Rules:
    /// - The step must not have been started already
    #[inline]
    pub fn validate_start(has_started: bool, key: &StepKey) -> Result<(), PolicyViolation> {
        if has_started {
            Err(PolicyViolation::DuplicateStart(key.clone()))
        } else {
            Ok(())
        }
    }

    /// Validate that an end event (complete/error) can be logged for this step.
    ///
    /// Rules:
    /// - The step must have been started first
    /// - The step must not have ended already
    #[inline]
    pub fn validate_end(
        has_started: bool,
        has_ended: bool,
        key: &StepKey,
    ) -> Result<(), PolicyViolation> {
        if !has_started {
            Err(PolicyViolation::EndWithoutStart(key.clone()))
        } else if has_ended {
            Err(PolicyViolation::DuplicateEnd(key.clone()))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_key_display() {
        let key = StepKey::setup(StepType::Prompt, 0);
        assert_eq!(key.to_string(), "setup-prompt-0");

        let key = StepKey::verification(StepType::Playwright, 2, 3);
        assert_eq!(key.to_string(), "verification-playwright-2-iter3");
    }

    #[test]
    fn test_policy_start_validation() {
        let key = StepKey::setup(StepType::Prompt, 0);

        // First start is OK
        assert!(LoggingPolicy::validate_start(false, &key).is_ok());

        // Second start is rejected
        let result = LoggingPolicy::validate_start(true, &key);
        assert!(matches!(result, Err(PolicyViolation::DuplicateStart(_))));
    }

    #[test]
    fn test_policy_end_validation() {
        let key = StepKey::setup(StepType::Prompt, 0);

        // End without start is rejected
        let result = LoggingPolicy::validate_end(false, false, &key);
        assert!(matches!(result, Err(PolicyViolation::EndWithoutStart(_))));

        // End with start is OK
        assert!(LoggingPolicy::validate_end(true, false, &key).is_ok());

        // Duplicate end is rejected
        let result = LoggingPolicy::validate_end(true, true, &key);
        assert!(matches!(result, Err(PolicyViolation::DuplicateEnd(_))));
    }
}
