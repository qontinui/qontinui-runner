//! Step Adapter Module
//!
//! Provides trait-based adapters for step execution, bridging the gap between
//! the `Executor` trait and the concrete step implementations in `StepExecutor`.
//!
//! ## Design Philosophy
//!
//! The `StepExecutor` contains a large switch statement over step types. This module
//! provides a trait-based abstraction layer that:
//!
//! 1. Uses `Executor` trait for uniform step execution interface
//! 2. Uses `LifecycleExecutor` for steps with setup/teardown (e.g., Playwright)
//! 3. Uses `IntoOutcome` for consistent result conversion
//!
//! ## Usage Patterns
//!
//! ### 1. Single Step Execution with Unified Outcome
//!
//! ```ignore
//! use crate::executor::{StepExecutorAdapter, AdaptedStepConfig, IntoOutcome};
//!
//! let adapter = StepExecutorAdapter::new(app_state, config_storage, app_handle);
//! let config = AdaptedStepConfig::from_execution_step(&step, "exec-123");
//!
//! // Execute and get unified outcome
//! let outcome = adapter.execute_to_outcome(config).await;
//! println!("Result: {} (took {}ms)", outcome.summary, outcome.duration_ms);
//! ```
//!
//! ### 2. Batch Execution with Aggregated Results
//!
//! ```ignore
//! use crate::executor::{StepExecutorAdapter, BatchStepExecutor};
//!
//! let adapter = StepExecutorAdapter::new(app_state, config_storage, app_handle);
//! let batch = BatchStepExecutor::new(adapter);
//!
//! // Execute all steps and get a summary outcome
//! let outcome = batch.execute_batch(&steps, "exec-123").await;
//! if outcome.success {
//!     println!("All {} steps completed", outcome.get_output("step_count"));
//! }
//! ```
//!
//! ### 3. Lifecycle-Managed Execution (for Playwright, etc.)
//!
//! ```ignore
//! use crate::executor::{StepExecutorAdapter, LifecycleStepAdapter, LifecycleExecutor};
//!
//! let adapter = StepExecutorAdapter::new(app_state, config_storage, app_handle);
//! let mut lifecycle = LifecycleStepAdapter::new(adapter)
//!     .with_step_type_filter("playwright");
//!
//! // Start lifecycle (e.g., launch browser)
//! lifecycle.start().await?;
//!
//! // Execute multiple Playwright steps in the same browser session
//! for step in playwright_steps {
//!     let config = AdaptedStepConfig::from_execution_step(&step, exec_id);
//!     let result = lifecycle.execute(config).await?;
//! }
//!
//! // Clean up (e.g., close browser)
//! lifecycle.stop().await?;
//! ```
//!
//! ### 4. Using FromContext for Dependency Injection
//!
//! ```ignore
//! use crate::executor::{ExecutorContext, StepExecutorAdapter, FromContext};
//!
//! let context = ExecutorContext::minimal(app_state, app_handle)
//!     .with_config_storage(storage)
//!     .with_execution_context("exec-123", "My Workflow");
//!
//! let adapter = StepExecutorAdapter::from_context(context)?;
//! ```
//!
//! ### 5. Converting Results to Outcomes
//!
//! ```ignore
//! use crate::executor::{results_to_outcomes, aggregate_outcomes, IntoOutcome};
//!
//! // Convert existing step results
//! let outcomes = results_to_outcomes(step_results);
//!
//! // Aggregate into a single summary
//! let summary = aggregate_outcomes(&outcomes);
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, info};

use super::context::ExecutorContext;
use super::results::{ExecutionError, ExecutionOutcome, IntoOutcome};
use super::traits::{Executor, ExecutorError, FromContext, LifecycleExecutor};
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::step_executor::{ExecutionStepConfig, StepExecutionResult, StepExecutor};

// =============================================================================
// Adapted Step Config
// =============================================================================

/// Configuration for executing a step through the adapter.
///
/// This wraps an `ExecutionStepConfig` with additional metadata needed for
/// trait-based execution.
#[derive(Debug, Clone)]
pub struct AdaptedStepConfig {
    /// The underlying step configuration
    pub step: ExecutionStepConfig,
    /// Execution ID for logging and tracking
    pub execution_id: String,
    /// Step index in the sequence (0-based)
    pub step_index: usize,
}

impl AdaptedStepConfig {
    /// Create from an execution step config.
    pub fn from_execution_step(step: &ExecutionStepConfig, execution_id: &str) -> Self {
        Self {
            step: step.clone(),
            execution_id: execution_id.to_string(),
            step_index: 0,
        }
    }

    /// Create with a specific step index.
    pub fn with_index(mut self, index: usize) -> Self {
        self.step_index = index;
        self
    }
}

// =============================================================================
// Step Executor Adapter
// =============================================================================

/// Adapter that implements `Executor` trait for step execution.
///
/// This wraps a `StepExecutor` and provides a trait-based interface for
/// executing individual steps with consistent result conversion.
pub struct StepExecutorAdapter {
    /// The underlying step executor
    executor: StepExecutor,
}

impl StepExecutorAdapter {
    /// Create a new adapter with app state and config storage.
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            executor: StepExecutor::with_app_handle(app_state, config_storage, app_handle),
        }
    }

    /// Create from an existing StepExecutor.
    pub fn from_executor(executor: StepExecutor) -> Self {
        Self { executor }
    }

    /// Execute a step and convert the result to ExecutionOutcome.
    ///
    /// This is a convenience method that handles the IntoOutcome conversion.
    pub async fn execute_to_outcome(&self, config: AdaptedStepConfig) -> ExecutionOutcome {
        let result = self.execute_internal(&config).await;
        let duration_ms = result.duration_ms;

        // Use IntoOutcome to convert the result
        result.into_outcome(duration_ms)
    }

    /// Execute multiple steps and collect outcomes.
    ///
    /// Returns a vector of (step_result, outcome) pairs.
    pub async fn execute_steps_to_outcomes(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> Vec<(StepExecutionResult, ExecutionOutcome)> {
        let mut results = Vec::new();

        for (index, step) in steps.iter().enumerate() {
            let config =
                AdaptedStepConfig::from_execution_step(step, execution_id).with_index(index);

            let step_result = self.execute_internal(&config).await;
            let outcome = step_result.clone().into_outcome(step_result.duration_ms);

            results.push((step_result, outcome));
        }

        results
    }

    /// Internal execution that returns the step result directly.
    #[tracing::instrument(
        name = "workflow.step.execute",
        skip(self, config),
        fields(
            step_name = %config.step.name.as_deref().unwrap_or(&config.step.step_type),
            step_type = %config.step.step_type,
            step_index = %config.step_index,
            execution_id = %config.execution_id
        )
    )]
    async fn execute_internal(&self, config: &AdaptedStepConfig) -> StepExecutionResult {
        let start_time = std::time::Instant::now();
        let started_at = chrono::Utc::now().to_rfc3339();

        let step = &config.step;
        let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());

        debug!(
            "Executing step via adapter: {} (type: {})",
            step_name, step.step_type
        );

        // Execute the step using the underlying executor
        let result = self
            .executor
            .execute_steps(std::slice::from_ref(step), &config.execution_id)
            .await;

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let ended_at = chrono::Utc::now().to_rfc3339();

        // Extract the first result or create a failure result
        if let Some(step_result) = result.steps.into_iter().next() {
            StepExecutionResult {
                started_at: Some(started_at),
                ended_at: Some(ended_at),
                ..step_result
            }
        } else {
            StepExecutionResult {
                step_index: config.step_index,
                step_type: step.step_type.clone(),
                step_name,
                step_id: step.id.clone(),
                success: false,
                error: Some("No result returned from step execution".to_string()),
                screenshot_path: None,
                started_at: Some(started_at),
                ended_at: Some(ended_at),
                duration_ms,
                config: Default::default(),
                verification_details: None,
                output_data: None,
                required: None,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
            }
        }
    }
}

#[async_trait]
impl Executor for StepExecutorAdapter {
    type Config = AdaptedStepConfig;
    type Output = StepExecutionResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        let result = self.execute_internal(&config).await;

        if result.success {
            Ok(result)
        } else {
            // Still return Ok with the result - the error is in the result itself
            // This allows callers to inspect the failure details
            Ok(result)
        }
    }

    fn name(&self) -> &'static str {
        "step-executor-adapter"
    }
}

impl FromContext for StepExecutorAdapter {
    fn from_context(context: ExecutorContext) -> Result<Self, ExecutorError> {
        let config_storage = context
            .config_storage()
            .ok_or_else(|| ExecutorError::missing("config_storage"))?
            .clone();

        Ok(Self::new(
            context.app_state.clone(),
            config_storage,
            context.app_handle.clone(),
        ))
    }
}

// =============================================================================
// Lifecycle-Aware Step Adapter
// =============================================================================

/// A step adapter with lifecycle management for steps that need setup/teardown.
///
/// This is useful for steps like Playwright tests that need browser initialization
/// before execution and cleanup after.
///
/// ## Example
///
/// ```ignore
/// let mut adapter = LifecycleStepAdapter::new(inner_adapter);
///
/// // Start the lifecycle (e.g., initialize browser)
/// adapter.start().await?;
///
/// // Execute multiple steps within the same lifecycle
/// for step in steps {
///     let outcome = adapter.execute(step).await?;
/// }
///
/// // Clean up
/// adapter.stop().await?;
/// ```
pub struct LifecycleStepAdapter {
    /// Inner adapter for actual execution
    inner: StepExecutorAdapter,
    /// Whether the lifecycle is currently active
    running: bool,
    /// Step type this lifecycle handles (e.g., "playwright")
    step_type_filter: Option<String>,
}

impl LifecycleStepAdapter {
    /// Create a new lifecycle adapter wrapping an inner adapter.
    pub fn new(inner: StepExecutorAdapter) -> Self {
        Self {
            inner,
            running: false,
            step_type_filter: None,
        }
    }

    /// Filter to only handle specific step types.
    ///
    /// When set, the lifecycle methods only apply to steps matching this type.
    pub fn with_step_type_filter(mut self, step_type: impl Into<String>) -> Self {
        self.step_type_filter = Some(step_type.into());
        self
    }

    /// Check if this lifecycle adapter handles the given step type.
    fn handles_step_type(&self, step_type: &str) -> bool {
        match &self.step_type_filter {
            Some(filter) => filter == step_type,
            None => true, // Handle all types if no filter
        }
    }
}

#[async_trait]
impl Executor for LifecycleStepAdapter {
    type Config = AdaptedStepConfig;
    type Output = StepExecutionResult;

    async fn execute(&self, config: Self::Config) -> Result<Self::Output, ExecutorError> {
        // Check if lifecycle is started for filtered step types
        if self.handles_step_type(&config.step.step_type) && !self.running {
            return Err(ExecutorError::invalid_state(format!(
                "Lifecycle not started for step type '{}'",
                config.step.step_type
            )));
        }

        self.inner.execute(config).await
    }

    fn name(&self) -> &'static str {
        "lifecycle-step-adapter"
    }
}

#[async_trait]
impl LifecycleExecutor for LifecycleStepAdapter {
    async fn start(&mut self) -> Result<(), ExecutorError> {
        if self.running {
            return Err(ExecutorError::invalid_state("Lifecycle already started"));
        }

        info!(
            "Starting lifecycle for step adapter{}",
            self.step_type_filter
                .as_ref()
                .map(|t| format!(" (filter: {})", t))
                .unwrap_or_default()
        );

        // Lifecycle initialization would go here
        // For example, starting a browser for Playwright tests
        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ExecutorError> {
        if !self.running {
            return Err(ExecutorError::invalid_state("Lifecycle not running"));
        }

        info!(
            "Stopping lifecycle for step adapter{}",
            self.step_type_filter
                .as_ref()
                .map(|t| format!(" (filter: {})", t))
                .unwrap_or_default()
        );

        // Lifecycle cleanup would go here
        // For example, closing browser
        self.running = false;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running
    }
}

// =============================================================================
// Batch Executor
// =============================================================================

/// Executes multiple steps and produces a batch result.
///
/// This provides a higher-level abstraction for executing step sequences
/// with proper lifecycle management and result aggregation.
pub struct BatchStepExecutor {
    adapter: StepExecutorAdapter,
}

impl BatchStepExecutor {
    /// Create a new batch executor.
    pub fn new(adapter: StepExecutorAdapter) -> Self {
        Self { adapter }
    }

    /// Execute all steps and produce a batch outcome.
    ///
    /// Returns an `ExecutionOutcome` that summarizes the batch execution.
    pub async fn execute_batch(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> ExecutionOutcome {
        let start_time = std::time::Instant::now();

        if steps.is_empty() {
            return ExecutionOutcome::success("No steps to execute")
                .with_duration(0)
                .with_output("step_count", serde_json::json!(0));
        }

        let results = self
            .adapter
            .execute_steps_to_outcomes(steps, execution_id)
            .await;

        let duration_ms = start_time.elapsed().as_millis() as u64;
        let total_steps = results.len();
        let successful_steps = results.iter().filter(|(r, _)| r.success).count();
        let failed_steps = total_steps - successful_steps;

        // Collect step summaries
        let step_summaries: Vec<serde_json::Value> = results
            .iter()
            .map(|(result, _)| {
                serde_json::json!({
                    "index": result.step_index,
                    "type": result.step_type,
                    "name": result.step_name,
                    "success": result.success,
                    "duration_ms": result.duration_ms,
                    "error": result.error,
                })
            })
            .collect();

        if failed_steps == 0 {
            ExecutionOutcome::success(format!("All {} steps completed successfully", total_steps))
                .with_duration(duration_ms)
                .with_output("step_count", serde_json::json!(total_steps))
                .with_output("successful_steps", serde_json::json!(successful_steps))
                .with_output("step_summaries", serde_json::json!(step_summaries))
        } else {
            let first_error = results
                .iter()
                .find(|(r, _)| !r.success)
                .map(|(r, _)| {
                    r.error
                        .clone()
                        .unwrap_or_else(|| "Unknown error".to_string())
                })
                .unwrap_or_else(|| "Unknown error".to_string());

            ExecutionOutcome::failure(
                format!("{} of {} steps failed", failed_steps, total_steps),
                ExecutionError::execution(&first_error),
            )
            .with_duration(duration_ms)
            .with_output("step_count", serde_json::json!(total_steps))
            .with_output("successful_steps", serde_json::json!(successful_steps))
            .with_output("failed_steps", serde_json::json!(failed_steps))
            .with_output("step_summaries", serde_json::json!(step_summaries))
        }
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Convert a vector of step results to execution outcomes.
///
/// This is useful when you have existing step results and want to convert
/// them to the unified outcome format.
pub fn results_to_outcomes(results: Vec<StepExecutionResult>) -> Vec<ExecutionOutcome> {
    results
        .into_iter()
        .map(|r| {
            let duration_ms = r.duration_ms;
            r.into_outcome(duration_ms)
        })
        .collect()
}

/// Create a batch outcome from step results.
///
/// Aggregates multiple step results into a single summary outcome.
pub fn aggregate_outcomes(outcomes: &[ExecutionOutcome]) -> ExecutionOutcome {
    if outcomes.is_empty() {
        return ExecutionOutcome::success("No steps executed")
            .with_output("step_count", serde_json::json!(0));
    }

    let total = outcomes.len();
    let successful = outcomes.iter().filter(|o| o.success).count();
    let failed = total - successful;
    let total_duration: u64 = outcomes.iter().map(|o| o.duration_ms).sum();

    if failed == 0 {
        ExecutionOutcome::success(format!("All {} steps completed", total))
            .with_duration(total_duration)
            .with_output("step_count", serde_json::json!(total))
            .with_output("successful_steps", serde_json::json!(successful))
    } else {
        let first_error = outcomes
            .iter()
            .find(|o| !o.success)
            .and_then(|o| o.error.as_ref())
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "Unknown error".to_string());

        ExecutionOutcome::failure(
            format!("{}/{} steps failed", failed, total),
            ExecutionError::execution(&first_error),
        )
        .with_duration(total_duration)
        .with_output("step_count", serde_json::json!(total))
        .with_output("successful_steps", serde_json::json!(successful))
        .with_output("failed_steps", serde_json::json!(failed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapted_step_config_creation() {
        let step = ExecutionStepConfig {
            step_type: "shell_command".to_string(),
            name: Some("Test Step".to_string()),
            ..Default::default()
        };

        let config = AdaptedStepConfig::from_execution_step(&step, "exec-123").with_index(5);

        assert_eq!(config.execution_id, "exec-123");
        assert_eq!(config.step_index, 5);
        assert_eq!(config.step.step_type, "shell_command");
    }

    #[test]
    fn test_results_to_outcomes() {
        let results = vec![
            StepExecutionResult {
                step_index: 0,
                step_type: "shell_command".to_string(),
                step_name: "Test".to_string(),
                step_id: None,
                success: true,
                error: None,
                screenshot_path: None,
                started_at: None,
                ended_at: None,
                duration_ms: 100,
                config: Default::default(),
                verification_details: None,
                output_data: None,
                required: None,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
            },
            StepExecutionResult {
                step_index: 1,
                step_type: "shell_command".to_string(),
                step_name: "Test 2".to_string(),
                step_id: None,
                success: false,
                error: Some("Failed".to_string()),
                screenshot_path: None,
                started_at: None,
                ended_at: None,
                duration_ms: 50,
                config: Default::default(),
                verification_details: None,
                output_data: None,
                required: None,
                resolved_inputs: None,
                extracted_values: None,
                failure_category: None,
            },
        ];

        let outcomes = results_to_outcomes(results);
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].success);
        assert!(!outcomes[1].success);
    }

    #[test]
    fn test_aggregate_outcomes_all_success() {
        let outcomes = vec![
            ExecutionOutcome::success("Step 1 done").with_duration(100),
            ExecutionOutcome::success("Step 2 done").with_duration(200),
        ];

        let aggregate = aggregate_outcomes(&outcomes);
        assert!(aggregate.success);
        assert_eq!(aggregate.duration_ms, 300);
        assert_eq!(
            aggregate.get_output("step_count"),
            Some(&serde_json::json!(2))
        );
    }

    #[test]
    fn test_aggregate_outcomes_with_failure() {
        let outcomes = vec![
            ExecutionOutcome::success("Step 1 done").with_duration(100),
            ExecutionOutcome::failure("Step 2 failed", ExecutionError::execution("Some error"))
                .with_duration(50),
        ];

        let aggregate = aggregate_outcomes(&outcomes);
        assert!(!aggregate.success);
        assert_eq!(
            aggregate.get_output("failed_steps"),
            Some(&serde_json::json!(1))
        );
    }

    #[test]
    fn test_aggregate_empty_outcomes() {
        let outcomes: Vec<ExecutionOutcome> = vec![];
        let aggregate = aggregate_outcomes(&outcomes);
        assert!(aggregate.success);
        assert_eq!(
            aggregate.get_output("step_count"),
            Some(&serde_json::json!(0))
        );
    }
}
