//! Execution Results Module
//!
//! Provides unified result types for executor operations, enabling consistent
//! handling of execution outcomes across different executor types.
//!
//! ## Step Execution Integration
//!
//! The `StepExecutionResult` from `step_executor` can be converted to
//! `ExecutionOutcome` using the `IntoOutcome` trait:
//!
//! ```ignore
//! use crate::executor::results::IntoOutcome;
//!
//! let step_result: StepExecutionResult = /* ... */;
//! let outcome: ExecutionOutcome = step_result.into_outcome(step_result.duration_ms);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// Execution Outcome
// =============================================================================

/// Unified result structure for all executor operations.
///
/// ExecutionOutcome provides a consistent way to represent the result of any
/// execution, regardless of the specific executor type. It includes:
///
/// - Success/failure status
/// - Summary message
/// - Arbitrary outputs as JSON values
/// - Error details if failed
/// - Timing information
/// - Retry guidance
///
/// # Example
///
/// ```ignore
/// // Creating a success outcome
/// let outcome = ExecutionOutcome::success("Completed successfully")
///     .with_output("result_count", serde_json::json!(42))
///     .with_duration(1500);
///
/// // Creating a failure outcome
/// let outcome = ExecutionOutcome::failure(
///     "Execution failed",
///     ExecutionError::new("Database connection lost"),
/// ).with_retriable(true);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    /// Whether the execution was successful.
    pub success: bool,

    /// Human-readable summary of the execution.
    pub summary: String,

    /// Arbitrary outputs from the execution.
    /// Keys are output names, values are JSON-serializable data.
    #[serde(default)]
    pub outputs: HashMap<String, serde_json::Value>,

    /// Error details if the execution failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ExecutionError>,

    /// Execution duration in milliseconds.
    pub duration_ms: u64,

    /// Whether the execution can be retried.
    /// Useful for transient failures.
    #[serde(default)]
    pub retriable: bool,
}

impl ExecutionOutcome {
    /// Create a successful outcome.
    pub fn success(summary: impl Into<String>) -> Self {
        Self {
            success: true,
            summary: summary.into(),
            outputs: HashMap::new(),
            error: None,
            duration_ms: 0,
            retriable: false,
        }
    }

    /// Create a failed outcome.
    pub fn failure(summary: impl Into<String>, error: ExecutionError) -> Self {
        Self {
            success: false,
            summary: summary.into(),
            outputs: HashMap::new(),
            error: Some(error),
            duration_ms: 0,
            retriable: false,
        }
    }

    /// Add an output value.
    #[must_use]
    pub fn with_output(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.outputs.insert(key.into(), value);
        self
    }

    /// Set the duration.
    #[must_use]
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    /// Set whether the execution is retriable.
    #[must_use]
    pub fn with_retriable(mut self, retriable: bool) -> Self {
        self.retriable = retriable;
        self
    }

    /// Set multiple outputs at once.
    #[must_use]
    pub fn with_outputs(mut self, outputs: HashMap<String, serde_json::Value>) -> Self {
        self.outputs = outputs;
        self
    }

    /// Get an output value by key.
    pub fn get_output(&self, key: &str) -> Option<&serde_json::Value> {
        self.outputs.get(key)
    }

    /// Check if there are any outputs.
    pub fn has_outputs(&self) -> bool {
        !self.outputs.is_empty()
    }
}

impl Default for ExecutionOutcome {
    fn default() -> Self {
        Self::success("No operation performed")
    }
}

// =============================================================================
// Execution Error
// =============================================================================

/// Detailed error information for failed executions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionError {
    /// The error message.
    pub message: String,

    /// Error code for categorization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    /// Additional context or details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,

    /// The step or phase where the error occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,

    /// Stack trace or error chain if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

impl ExecutionError {
    /// Create a new execution error with just a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            details: None,
            location: None,
            stack: None,
        }
    }

    /// Create an execution-type error (convenience factory).
    ///
    /// This is a common pattern for step execution failures.
    pub fn execution(message: impl Into<String>) -> Self {
        Self::new(message).with_code("EXECUTION_ERROR")
    }

    /// Create a step failure error with step details.
    pub fn step_failure(
        message: impl Into<String>,
        step_type: impl Into<String>,
        step_name: impl Into<String>,
    ) -> Self {
        Self::new(message)
            .with_code("STEP_FAILURE")
            .with_location(format!("{}:{}", step_type.into(), step_name.into()))
    }

    /// Add an error code.
    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Add details.
    #[must_use]
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Add location information.
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Add stack trace.
    #[must_use]
    pub fn with_stack(mut self, stack: impl Into<String>) -> Self {
        self.stack = Some(stack.into());
        self
    }
}

impl From<String> for ExecutionError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ExecutionError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

impl From<crate::error::AppError> for ExecutionError {
    fn from(err: crate::error::AppError) -> Self {
        let user_facing = err.to_user_facing();
        Self {
            message: err.to_string(),
            code: Some(user_facing.error_code),
            details: user_facing.details,
            location: None,
            stack: None,
        }
    }
}

// =============================================================================
// IntoOutcome Trait
// =============================================================================

/// Trait for converting executor results into ExecutionOutcome.
///
/// This allows different executor result types to be uniformly converted
/// to ExecutionOutcome for consistent handling.
///
/// # Example
///
/// ```ignore
/// impl IntoOutcome for MyResult {
///     fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
///         if self.success {
///             ExecutionOutcome::success(&self.message)
///                 .with_duration(duration_ms)
///         } else {
///             ExecutionOutcome::failure(
///                 &self.message,
///                 ExecutionError::new(&self.error.unwrap_or_default()),
///             ).with_duration(duration_ms)
///         }
///     }
/// }
/// ```
pub trait IntoOutcome {
    /// Convert to ExecutionOutcome with the given duration.
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome;
}

// Implement IntoOutcome for common result types

impl IntoOutcome for bool {
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        if self {
            ExecutionOutcome::success("Operation completed").with_duration(duration_ms)
        } else {
            ExecutionOutcome::failure(
                "Operation failed",
                ExecutionError::new("No specific error message"),
            )
            .with_duration(duration_ms)
        }
    }
}

impl<T, E> IntoOutcome for Result<T, E>
where
    T: IntoOutcome,
    E: std::fmt::Display,
{
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        match self {
            Ok(value) => value.into_outcome(duration_ms),
            Err(err) => {
                ExecutionOutcome::failure("Operation failed", ExecutionError::new(err.to_string()))
                    .with_duration(duration_ms)
            }
        }
    }
}

// =============================================================================
// Outcome Builder
// =============================================================================

/// Builder for creating ExecutionOutcome with a fluent API.
pub struct OutcomeBuilder {
    outcome: ExecutionOutcome,
}

impl OutcomeBuilder {
    /// Start building a success outcome.
    pub fn success(summary: impl Into<String>) -> Self {
        Self {
            outcome: ExecutionOutcome::success(summary),
        }
    }

    /// Start building a failure outcome.
    pub fn failure(summary: impl Into<String>, error: ExecutionError) -> Self {
        Self {
            outcome: ExecutionOutcome::failure(summary, error),
        }
    }

    /// Add an output.
    #[must_use]
    pub fn output(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.outcome.outputs.insert(key.into(), value);
        self
    }

    /// Set duration.
    #[must_use]
    pub fn duration(mut self, ms: u64) -> Self {
        self.outcome.duration_ms = ms;
        self
    }

    /// Set retriable flag.
    #[must_use]
    pub fn retriable(mut self, retriable: bool) -> Self {
        self.outcome.retriable = retriable;
        self
    }

    /// Build the outcome.
    pub fn build(self) -> ExecutionOutcome {
        self.outcome
    }
}

// =============================================================================
// Step Execution Result Conversions
// =============================================================================

/// Wrapper for `StepExecutionResult` that includes step-specific metadata
/// for unified outcome conversion.
///
/// This type provides a bridge between the step executor's result type
/// and the unified `ExecutionOutcome` type.
///
/// # Example
///
/// ```ignore
/// let step_result: StepExecutionResult = execute_step(...).await;
/// let outcome = StepExecutionOutcome::from(step_result).into_outcome();
/// ```
#[derive(Debug, Clone)]
pub struct StepExecutionOutcome {
    /// The underlying step execution result
    pub result: crate::step_executor::StepExecutionResult,
}

impl StepExecutionOutcome {
    /// Create a new step execution outcome wrapper.
    pub fn new(result: crate::step_executor::StepExecutionResult) -> Self {
        Self { result }
    }

    /// Convert to ExecutionOutcome using the step's duration.
    pub fn into_outcome(self) -> ExecutionOutcome {
        let duration_ms = self.result.duration_ms;
        self.result.into_outcome(duration_ms)
    }

    /// Convert to ExecutionOutcome with a custom duration.
    pub fn into_outcome_with_duration(self, duration_ms: u64) -> ExecutionOutcome {
        self.result.into_outcome(duration_ms)
    }

    /// Check if the step succeeded.
    pub fn success(&self) -> bool {
        self.result.success
    }

    /// Get the error message if the step failed.
    pub fn error(&self) -> Option<&str> {
        self.result.error.as_deref()
    }

    /// Get the step type.
    pub fn step_type(&self) -> &str {
        &self.result.step_type
    }

    /// Get the step name.
    pub fn step_name(&self) -> &str {
        &self.result.step_name
    }

    /// Get the step index.
    pub fn step_index(&self) -> usize {
        self.result.step_index
    }
}

impl From<crate::step_executor::StepExecutionResult> for StepExecutionOutcome {
    fn from(result: crate::step_executor::StepExecutionResult) -> Self {
        Self::new(result)
    }
}

/// Implement IntoOutcome for StepExecutionResult.
///
/// This enables seamless conversion from step execution results to
/// the unified ExecutionOutcome type.
///
/// # Example
///
/// ```ignore
/// let result: StepExecutionResult = execute_step(...).await;
/// let outcome: ExecutionOutcome = result.into_outcome(result.duration_ms);
/// ```
impl IntoOutcome for crate::step_executor::StepExecutionResult {
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        let summary = if self.success {
            format!("Step '{}' completed successfully", self.step_name)
        } else {
            format!(
                "Step '{}' failed: {}",
                self.step_name,
                self.error.as_deref().unwrap_or("Unknown error")
            )
        };

        if self.success {
            let mut outcome = ExecutionOutcome::success(&summary)
                .with_duration(duration_ms)
                .with_output("step_index", serde_json::json!(self.step_index))
                .with_output("step_type", serde_json::json!(self.step_type))
                .with_output("step_name", serde_json::json!(self.step_name));

            // Include screenshot path if available
            if let Some(ref screenshot) = self.screenshot_path {
                outcome = outcome.with_output("screenshot_path", serde_json::json!(screenshot));
            }

            // Include verification details if available
            if let Some(ref details) = self.verification_details {
                outcome = outcome.with_output(
                    "verification_details",
                    serde_json::to_value(details).unwrap_or_default(),
                );
            }

            outcome
        } else {
            let error = ExecutionError::step_failure(
                self.error.as_deref().unwrap_or("Step failed"),
                &self.step_type,
                &self.step_name,
            );

            let mut outcome = ExecutionOutcome::failure(&summary, error)
                .with_duration(duration_ms)
                .with_output("step_index", serde_json::json!(self.step_index))
                .with_output("step_type", serde_json::json!(self.step_type))
                .with_output("step_name", serde_json::json!(self.step_name));

            // Include screenshot path if available (useful for debugging failures)
            if let Some(ref screenshot) = self.screenshot_path {
                outcome = outcome.with_output("screenshot_path", serde_json::json!(screenshot));
            }

            // Include verification details for debugging
            if let Some(ref details) = self.verification_details {
                outcome = outcome.with_output(
                    "verification_details",
                    serde_json::to_value(details).unwrap_or_default(),
                );
            }

            outcome
        }
    }
}

/// Implement conversion from legacy tuple result format.
///
/// This allows gradual migration from the old `(bool, Option<String>, Option<String>)`
/// pattern to the unified ExecutionOutcome type.
///
/// The tuple format is: (success, error_message, output)
impl IntoOutcome for (bool, Option<String>, Option<String>) {
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        let (success, error, output) = self;

        if success {
            let mut outcome =
                ExecutionOutcome::success("Step completed").with_duration(duration_ms);

            if let Some(out) = output {
                outcome = outcome.with_output("result", serde_json::json!(out));
            }

            outcome
        } else {
            let error_msg = error.unwrap_or_else(|| "Step failed".to_string());
            ExecutionOutcome::failure("Step failed", ExecutionError::execution(&error_msg))
                .with_duration(duration_ms)
        }
    }
}

// =============================================================================
// Phase Result Conversions
// =============================================================================

/// Implement IntoOutcome for SetupResult
impl IntoOutcome for crate::unified_workflow_executor::SetupResult {
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        if self.success {
            ExecutionOutcome::success("Setup phase completed successfully")
                .with_duration(duration_ms)
                .with_output("step_count", serde_json::json!(self.step_results.len()))
        } else {
            let failed_steps: Vec<_> = self
                .step_results
                .iter()
                .filter(|r| !r.success)
                .map(|r| r.step_name.clone())
                .collect();

            ExecutionOutcome::failure(
                "Setup phase failed",
                ExecutionError::execution(format!("Failed steps: {}", failed_steps.join(", "))),
            )
            .with_duration(duration_ms)
            .with_output("step_count", serde_json::json!(self.step_results.len()))
            .with_output("failed_steps", serde_json::json!(failed_steps))
        }
    }
}

/// Implement IntoOutcome for VerificationResult
impl IntoOutcome for crate::unified_workflow_executor::VerificationResult {
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        let phase_result = &self.phase_result;

        if phase_result.all_passed {
            ExecutionOutcome::success("Verification phase passed")
                .with_duration(duration_ms)
                .with_output("iteration", serde_json::json!(phase_result.iteration))
                .with_output("passed_steps", serde_json::json!(phase_result.passed_steps))
                .with_output("total_steps", serde_json::json!(phase_result.total_steps))
        } else {
            let summary = if phase_result.critical_failure {
                "Verification phase failed with critical failure"
            } else {
                "Verification phase failed"
            };

            ExecutionOutcome::failure(
                summary,
                ExecutionError::execution(format!(
                    "{}/{} steps failed",
                    phase_result.failed_steps, phase_result.total_steps
                )),
            )
            .with_duration(duration_ms)
            .with_output("iteration", serde_json::json!(phase_result.iteration))
            .with_output("passed_steps", serde_json::json!(phase_result.passed_steps))
            .with_output("failed_steps", serde_json::json!(phase_result.failed_steps))
            .with_output("total_steps", serde_json::json!(phase_result.total_steps))
            .with_output(
                "critical_failure",
                serde_json::json!(phase_result.critical_failure),
            )
        }
    }
}

/// Implement IntoOutcome for CompletionResult
impl IntoOutcome for crate::unified_workflow_executor::CompletionResult {
    fn into_outcome(self, duration_ms: u64) -> ExecutionOutcome {
        if self.success {
            ExecutionOutcome::success("Completion phase finished successfully")
                .with_duration(duration_ms)
                .with_output("step_count", serde_json::json!(self.step_results.len()))
        } else {
            let failed_steps: Vec<_> = self
                .step_results
                .iter()
                .filter(|r| !r.success)
                .map(|r| r.step_name.clone())
                .collect();

            ExecutionOutcome::failure(
                "Completion phase failed",
                ExecutionError::execution(format!("Failed steps: {}", failed_steps.join(", "))),
            )
            .with_duration(duration_ms)
            .with_output("step_count", serde_json::json!(self.step_results.len()))
            .with_output("failed_steps", serde_json::json!(failed_steps))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_outcome_success() {
        let outcome = ExecutionOutcome::success("Test passed")
            .with_duration(100)
            .with_output("count", serde_json::json!(5));

        assert!(outcome.success);
        assert_eq!(outcome.summary, "Test passed");
        assert_eq!(outcome.duration_ms, 100);
        assert_eq!(outcome.get_output("count"), Some(&serde_json::json!(5)));
        assert!(outcome.error.is_none());
    }

    #[test]
    fn test_execution_outcome_failure() {
        let error = ExecutionError::new("Something went wrong")
            .with_code("ERR_001")
            .with_details("Connection refused");

        let outcome = ExecutionOutcome::failure("Test failed", error)
            .with_duration(50)
            .with_retriable(true);

        assert!(!outcome.success);
        assert!(outcome.retriable);
        assert!(outcome.error.is_some());
        let err = outcome.error.unwrap();
        assert_eq!(err.message, "Something went wrong");
        assert_eq!(err.code, Some("ERR_001".to_string()));
    }

    #[test]
    fn test_execution_error_builder() {
        let error = ExecutionError::new("Failed")
            .with_code("TEST_001")
            .with_details("Detailed info")
            .with_location("step_3")
            .with_stack("at line 42");

        assert_eq!(error.message, "Failed");
        assert_eq!(error.code, Some("TEST_001".to_string()));
        assert_eq!(error.details, Some("Detailed info".to_string()));
        assert_eq!(error.location, Some("step_3".to_string()));
        assert_eq!(error.stack, Some("at line 42".to_string()));
    }

    #[test]
    fn test_bool_into_outcome() {
        let success: ExecutionOutcome = true.into_outcome(100);
        assert!(success.success);
        assert_eq!(success.duration_ms, 100);

        let failure: ExecutionOutcome = false.into_outcome(50);
        assert!(!failure.success);
    }

    #[test]
    fn test_outcome_builder() {
        let outcome = OutcomeBuilder::success("Built successfully")
            .output("key", serde_json::json!("value"))
            .duration(200)
            .retriable(false)
            .build();

        assert!(outcome.success);
        assert_eq!(outcome.summary, "Built successfully");
        assert_eq!(outcome.duration_ms, 200);
        assert!(!outcome.retriable);
        assert_eq!(outcome.get_output("key"), Some(&serde_json::json!("value")));
    }

    #[test]
    fn test_execution_error_factories() {
        let exec_error = ExecutionError::execution("Something went wrong");
        assert_eq!(exec_error.message, "Something went wrong");
        assert_eq!(exec_error.code, Some("EXECUTION_ERROR".to_string()));

        let step_error = ExecutionError::step_failure("Step failed", "playwright", "Login Test");
        assert_eq!(step_error.message, "Step failed");
        assert_eq!(step_error.code, Some("STEP_FAILURE".to_string()));
        assert_eq!(
            step_error.location,
            Some("playwright:Login Test".to_string())
        );
    }

    #[test]
    fn test_tuple_into_outcome_success() {
        let tuple: (bool, Option<String>, Option<String>) =
            (true, None, Some("output data".to_string()));
        let outcome = tuple.into_outcome(150);

        assert!(outcome.success);
        assert_eq!(outcome.duration_ms, 150);
        assert_eq!(
            outcome.get_output("result"),
            Some(&serde_json::json!("output data"))
        );
    }

    #[test]
    fn test_tuple_into_outcome_failure() {
        let tuple: (bool, Option<String>, Option<String>) =
            (false, Some("Error occurred".to_string()), None);
        let outcome = tuple.into_outcome(200);

        assert!(!outcome.success);
        assert_eq!(outcome.duration_ms, 200);
        assert!(outcome.error.is_some());
        let err = outcome.error.unwrap();
        assert_eq!(err.message, "Error occurred");
        assert_eq!(err.code, Some("EXECUTION_ERROR".to_string()));
    }

    #[test]
    fn test_step_execution_outcome_wrapper() {
        use crate::step_executor::{StepExecutionConfig, StepExecutionResult};

        let step_result = StepExecutionResult {
            step_index: 0,
            step_type: "playwright".to_string(),
            step_name: "Login Test".to_string(),
            success: true,
            error: None,
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: 1500,
            config: StepExecutionConfig::default(),
            verification_details: None,
        };

        let wrapped = StepExecutionOutcome::from(step_result);
        assert!(wrapped.success());
        assert_eq!(wrapped.step_type(), "playwright");
        assert_eq!(wrapped.step_name(), "Login Test");
        assert_eq!(wrapped.step_index(), 0);

        let outcome = wrapped.into_outcome();
        assert!(outcome.success);
        assert_eq!(outcome.duration_ms, 1500);
        assert_eq!(
            outcome.get_output("step_type"),
            Some(&serde_json::json!("playwright"))
        );
        assert_eq!(
            outcome.get_output("step_name"),
            Some(&serde_json::json!("Login Test"))
        );
    }

    #[test]
    fn test_step_execution_result_into_outcome_failure() {
        use crate::step_executor::{StepExecutionConfig, StepExecutionResult};

        let step_result = StepExecutionResult {
            step_index: 2,
            step_type: "shell_command".to_string(),
            step_name: "Run Tests".to_string(),
            success: false,
            error: Some("Command exited with code 1".to_string()),
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms: 5000,
            config: StepExecutionConfig::default(),
            verification_details: None,
        };

        let outcome = step_result.into_outcome(5000);
        assert!(!outcome.success);
        assert_eq!(outcome.duration_ms, 5000);
        assert!(outcome.summary.contains("Run Tests"));
        assert!(outcome.summary.contains("failed"));

        let err = outcome.error.unwrap();
        assert_eq!(err.message, "Command exited with code 1");
        assert_eq!(err.code, Some("STEP_FAILURE".to_string()));
        assert_eq!(err.location, Some("shell_command:Run Tests".to_string()));
    }

    #[test]
    fn test_setup_result_into_outcome_success() {
        use crate::step_executor::{StepExecutionConfig, StepExecutionResult};
        use crate::unified_workflow_executor::SetupResult;

        let result = SetupResult {
            success: true,
            step_results: vec![StepExecutionResult {
                step_index: 0,
                step_type: "shell_command".to_string(),
                step_name: "Install deps".to_string(),
                success: true,
                error: None,
                screenshot_path: None,
                started_at: None,
                ended_at: None,
                duration_ms: 1000,
                config: StepExecutionConfig::default(),
                verification_details: None,
            }],
        };

        let outcome = result.into_outcome(1500);
        assert!(outcome.success);
        assert_eq!(outcome.duration_ms, 1500);
        assert_eq!(
            outcome.get_output("step_count"),
            Some(&serde_json::json!(1))
        );
    }

    #[test]
    fn test_completion_result_into_outcome_failure() {
        use crate::step_executor::{StepExecutionConfig, StepExecutionResult};
        use crate::unified_workflow_executor::CompletionResult;

        let result = CompletionResult {
            success: false,
            step_results: vec![StepExecutionResult {
                step_index: 0,
                step_type: "shell_command".to_string(),
                step_name: "Generate report".to_string(),
                success: false,
                error: Some("Command failed".to_string()),
                screenshot_path: None,
                started_at: None,
                ended_at: None,
                duration_ms: 500,
                config: StepExecutionConfig::default(),
                verification_details: None,
            }],
        };

        let outcome = result.into_outcome(600);
        assert!(!outcome.success);
        assert_eq!(outcome.duration_ms, 600);
        assert!(outcome.error.is_some());
    }
}
