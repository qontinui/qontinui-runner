//! Executor Traits Module
//!
//! Provides common traits for execution result types, enabling uniform
//! access to success status, errors, and timing information across
//! different result structures.

use crate::step_executor::{ExecutionResult, StepExecutionResult};

/// Common interface for execution results.
///
/// This trait provides a uniform way to access basic execution outcome
/// information regardless of the specific result type being used.
///
/// # Implementors
///
/// - `ExecutionResult`: Overall execution result for a batch of steps
/// - `StepExecutionResult`: Result for a single step execution
pub trait ExecutionOutput {
    /// Returns true if the execution was successful.
    fn success(&self) -> bool;

    /// Returns the error message if the execution failed.
    fn error(&self) -> Option<&str>;

    /// Returns the execution duration in milliseconds, if available.
    fn duration_ms(&self) -> Option<u64>;
}

impl ExecutionOutput for ExecutionResult {
    fn success(&self) -> bool {
        self.success
    }

    fn error(&self) -> Option<&str> {
        // ExecutionResult doesn't have a single error field,
        // but we can check the first failed step
        self.steps
            .iter()
            .find(|s| !s.success)
            .and_then(|s| s.error.as_deref())
    }

    fn duration_ms(&self) -> Option<u64> {
        Some(self.total_duration_ms)
    }
}

impl ExecutionOutput for StepExecutionResult {
    fn success(&self) -> bool {
        self.success
    }

    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn duration_ms(&self) -> Option<u64> {
        Some(self.duration_ms)
    }
}

/// Extension trait for collections of execution outputs.
///
/// Provides aggregate operations over multiple execution results.
pub trait ExecutionOutputCollection {
    /// Returns true if all items in the collection succeeded.
    fn all_succeeded(&self) -> bool;

    /// Returns the count of successful items.
    fn success_count(&self) -> usize;

    /// Returns the count of failed items.
    fn failure_count(&self) -> usize;

    /// Returns the total duration of all items in milliseconds.
    fn total_duration_ms(&self) -> u64;

    /// Collects all error messages from failed items.
    fn collect_errors(&self) -> Vec<&str>;
}

impl<T: ExecutionOutput> ExecutionOutputCollection for [T] {
    fn all_succeeded(&self) -> bool {
        self.iter().all(|item| item.success())
    }

    fn success_count(&self) -> usize {
        self.iter().filter(|item| item.success()).count()
    }

    fn failure_count(&self) -> usize {
        self.iter().filter(|item| !item.success()).count()
    }

    fn total_duration_ms(&self) -> u64 {
        self.iter().filter_map(|item| item.duration_ms()).sum()
    }

    fn collect_errors(&self) -> Vec<&str> {
        self.iter()
            .filter(|item| !item.success())
            .filter_map(|item| item.error())
            .collect()
    }
}

impl<T: ExecutionOutput> ExecutionOutputCollection for Vec<T> {
    fn all_succeeded(&self) -> bool {
        self.as_slice().all_succeeded()
    }

    fn success_count(&self) -> usize {
        self.as_slice().success_count()
    }

    fn failure_count(&self) -> usize {
        self.as_slice().failure_count()
    }

    fn total_duration_ms(&self) -> u64 {
        self.as_slice().total_duration_ms()
    }

    fn collect_errors(&self) -> Vec<&str> {
        self.as_slice().collect_errors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_executor::StepExecutionConfig;

    fn make_success_result(index: usize, duration: u64) -> StepExecutionResult {
        StepExecutionResult {
            step_index: index,
            step_type: "command".to_string(),
            step_name: format!("Step {}", index),
            step_id: None,
            success: true,
            error: None,
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: duration,
            config: StepExecutionConfig::default(),
            verification_details: None,
            output_data: None,
            required: None,
            resolved_inputs: None,
            extracted_values: None,
            failure_category: None,
        }
    }

    fn make_error_result(index: usize, error: &str, duration: u64) -> StepExecutionResult {
        StepExecutionResult {
            step_index: index,
            step_type: "command".to_string(),
            step_name: format!("Step {}", index),
            step_id: None,
            success: false,
            error: Some(error.to_string()),
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: duration,
            config: StepExecutionConfig::default(),
            verification_details: None,
            output_data: None,
            required: None,
            resolved_inputs: None,
            extracted_values: None,
            failure_category: None,
        }
    }

    #[test]
    fn test_step_execution_result_success() {
        let result = make_success_result(0, 100);
        assert!(result.success());
        assert!(result.error().is_none());
        assert_eq!(result.duration_ms(), Some(100));
    }

    #[test]
    fn test_step_execution_result_error() {
        let result = make_error_result(0, "Test error", 50);
        assert!(!result.success());
        assert_eq!(result.error(), Some("Test error"));
        assert_eq!(result.duration_ms(), Some(50));
    }

    #[test]
    fn test_execution_result_success() {
        let result = ExecutionResult {
            success: true,
            total_steps: 2,
            successful_steps: 2,
            failed_steps: 0,
            total_duration_ms: 200,
            steps: vec![make_success_result(0, 100), make_success_result(1, 100)],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: Some(false),
            loop_result: None,
            task_summary: None,
        };
        assert!(result.success());
        assert!(result.error().is_none());
        assert_eq!(result.duration_ms(), Some(200));
    }

    #[test]
    fn test_execution_result_with_error() {
        let result = ExecutionResult {
            success: false,
            total_steps: 2,
            successful_steps: 1,
            failed_steps: 1,
            total_duration_ms: 150,
            steps: vec![
                make_success_result(0, 100),
                make_error_result(1, "Step failed", 50),
            ],
            captured_logs: None,
            captured_runner_logs: None,
            verification_passed: Some(false),
            loop_result: None,
            task_summary: None,
        };
        assert!(!result.success());
        assert_eq!(result.error(), Some("Step failed"));
        assert_eq!(result.duration_ms(), Some(150));
    }

    #[test]
    fn test_collection_all_succeeded() {
        let results = vec![make_success_result(0, 100), make_success_result(1, 100)];
        assert!(results.all_succeeded());
        assert_eq!(results.success_count(), 2);
        assert_eq!(results.failure_count(), 0);
    }

    #[test]
    fn test_collection_with_failures() {
        let results = vec![
            make_success_result(0, 100),
            make_error_result(1, "Error 1", 50),
            make_error_result(2, "Error 2", 50),
        ];
        assert!(!results.all_succeeded());
        assert_eq!(results.success_count(), 1);
        assert_eq!(results.failure_count(), 2);
    }

    #[test]
    fn test_collection_total_duration() {
        let results = vec![
            make_success_result(0, 100),
            make_success_result(1, 150),
            make_success_result(2, 50),
        ];
        assert_eq!(results.total_duration_ms(), 300);
    }

    #[test]
    fn test_collection_collect_errors() {
        let results = vec![
            make_success_result(0, 100),
            make_error_result(1, "Error A", 50),
            make_error_result(2, "Error B", 50),
        ];
        let errors = results.collect_errors();
        assert_eq!(errors.len(), 2);
        assert!(errors.contains(&"Error A"));
        assert!(errors.contains(&"Error B"));
    }

    #[test]
    fn test_slice_implementation() {
        let results = [make_success_result(0, 100), make_success_result(1, 100)];
        assert!(results.all_succeeded());
        assert_eq!(results.total_duration_ms(), 200);
    }
}
