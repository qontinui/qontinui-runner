//! Unified Step Result Types
//!
//! Provides unified result types for step execution across both
//! Flow Designer and Unified Workflow systems.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Result of executing a single step.
///
/// This is the core result type used by both Flow Designer and Unified Workflow
/// to track step execution outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// The step ID that was executed.
    pub step_id: String,
    /// Whether the step succeeded.
    pub success: bool,
    /// Output values from the step.
    pub outputs: HashMap<String, serde_json::Value>,
    /// Error message if the step failed.
    pub error: Option<String>,
    /// The next step to execute (determined by step logic).
    pub next_step: Option<String>,
    /// Duration of execution in milliseconds.
    pub duration_ms: u64,
    /// Additional metadata about the execution.
    pub metadata: HashMap<String, serde_json::Value>,
}

impl StepExecutionResult {
    /// Create a successful result.
    pub fn success(step_id: impl Into<String>, next_step: Option<String>) -> Self {
        Self {
            step_id: step_id.into(),
            success: true,
            outputs: HashMap::new(),
            error: None,
            next_step,
            duration_ms: 0,
            metadata: HashMap::new(),
        }
    }

    /// Create a failed result.
    pub fn failure(step_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            success: false,
            outputs: HashMap::new(),
            error: Some(error.into()),
            next_step: None,
            duration_ms: 0,
            metadata: HashMap::new(),
        }
    }

    /// Add an output value.
    pub fn with_output(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.outputs.insert(key.into(), value);
        self
    }

    /// Set duration.
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    /// Set the next step.
    pub fn with_next(mut self, next: impl Into<String>) -> Self {
        self.next_step = Some(next.into());
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Check if this result indicates a retry should be attempted.
    pub fn should_retry(&self, max_retries: u32, current_attempt: u32) -> bool {
        !self.success && current_attempt < max_retries
    }
}

/// Trait for converting various result types into StepExecutionResult.
pub trait IntoStepResult {
    /// Convert this type into a StepExecutionResult.
    fn into_step_result(self, step_id: &str) -> StepExecutionResult;
}

impl IntoStepResult for Result<serde_json::Value, String> {
    fn into_step_result(self, step_id: &str) -> StepExecutionResult {
        match self {
            Ok(value) => {
                let mut result = StepExecutionResult::success(step_id, None);
                if let Some(obj) = value.as_object() {
                    for (key, val) in obj {
                        result = result.with_output(key.clone(), val.clone());
                    }
                } else {
                    result = result.with_output("result", value);
                }
                result
            }
            Err(e) => StepExecutionResult::failure(step_id, e),
        }
    }
}

/// Result of a batch execution (multiple steps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchExecutionResult {
    /// Results from each step.
    pub step_results: Vec<StepExecutionResult>,
    /// Overall success (all steps succeeded).
    pub success: bool,
    /// Total duration in milliseconds.
    pub total_duration_ms: u64,
    /// Error summary if any steps failed.
    pub error_summary: Option<String>,
}

impl BatchExecutionResult {
    /// Create a new batch result from step results.
    pub fn from_results(results: Vec<StepExecutionResult>) -> Self {
        let success = results.iter().all(|r| r.success);
        let total_duration_ms = results.iter().map(|r| r.duration_ms).sum();
        let error_summary = if success {
            None
        } else {
            let errors: Vec<String> = results
                .iter()
                .filter(|r| !r.success)
                .filter_map(|r| r.error.clone())
                .collect();
            Some(errors.join("; "))
        };

        Self {
            step_results: results,
            success,
            total_duration_ms,
            error_summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_result_success() {
        let result = StepExecutionResult::success("step1", Some("step2".to_string()))
            .with_output("key", serde_json::json!("value"))
            .with_duration(100);

        assert!(result.success);
        assert_eq!(result.step_id, "step1");
        assert_eq!(result.next_step, Some("step2".to_string()));
        assert_eq!(result.duration_ms, 100);
        assert!(result.outputs.contains_key("key"));
    }

    #[test]
    fn test_step_result_failure() {
        let result = StepExecutionResult::failure("step1", "Something went wrong");

        assert!(!result.success);
        assert_eq!(result.error, Some("Something went wrong".to_string()));
    }

    #[test]
    fn test_should_retry() {
        let result = StepExecutionResult::failure("step1", "error");

        assert!(result.should_retry(3, 0));
        assert!(result.should_retry(3, 1));
        assert!(result.should_retry(3, 2));
        assert!(!result.should_retry(3, 3));

        let success = StepExecutionResult::success("step1", None);
        assert!(!success.should_retry(3, 0));
    }

    #[test]
    fn test_batch_result() {
        let results = vec![
            StepExecutionResult::success("step1", None).with_duration(100),
            StepExecutionResult::success("step2", None).with_duration(200),
        ];

        let batch = BatchExecutionResult::from_results(results);
        assert!(batch.success);
        assert_eq!(batch.total_duration_ms, 300);
        assert!(batch.error_summary.is_none());
    }

    #[test]
    fn test_batch_result_with_failure() {
        let results = vec![
            StepExecutionResult::success("step1", None),
            StepExecutionResult::failure("step2", "Failed"),
        ];

        let batch = BatchExecutionResult::from_results(results);
        assert!(!batch.success);
        assert!(batch.error_summary.is_some());
    }
}
