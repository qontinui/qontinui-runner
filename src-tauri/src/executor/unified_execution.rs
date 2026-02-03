//! Unified Execution Module
//!
//! Provides high-level execution utilities that leverage the executor traits
//! for consistent step execution with unified result handling.
//!
//! This module serves as the primary integration point for:
//! - `StepExecutorAdapter` - Trait-based step execution
//! - `BatchStepExecutor` - Batch step execution with aggregation
//! - `LifecycleStepAdapter` - Lifecycle-managed execution (e.g., Playwright)
//! - `aggregate_outcomes` / `results_to_outcomes` - Result helpers
//!
//! ## Usage
//!
//! ```ignore
//! use crate::executor::unified_execution::{UnifiedStepRunner, execute_steps_with_outcomes};
//!
//! // Create a unified runner
//! let runner = UnifiedStepRunner::new(app_state, config_storage, app_handle);
//!
//! // Execute steps and get unified outcomes
//! let outcome = runner.execute_steps(&steps, "exec-123").await;
//! ```

use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::warn;

use super::results::{ExecutionOutcome, OutcomeBuilder, ExecutionError};
use super::step_adapter::{
    AdaptedStepConfig, BatchStepExecutor, LifecycleStepAdapter, StepExecutorAdapter,
    aggregate_outcomes, results_to_outcomes,
};
use super::traits::{Executor, ExecutorError, LifecycleExecutor};
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::step_executor::{ExecutionStepConfig, StepExecutionResult};

/// Unified step runner that provides high-level execution with consistent result handling.
///
/// This runner wraps the lower-level adapters and provides convenient methods
/// for common execution patterns.
pub struct UnifiedStepRunner {
    /// The underlying step executor adapter
    adapter: StepExecutorAdapter,
    /// Batch executor for multi-step operations
    batch_executor: BatchStepExecutor,
}

impl UnifiedStepRunner {
    /// Create a new unified step runner.
    pub fn new(
        app_state: Arc<AppState>,
        config_storage: Arc<TokioMutex<ConfigStorage>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        let adapter = StepExecutorAdapter::new(
            app_state.clone(),
            config_storage.clone(),
            app_handle.clone(),
        );
        let batch_executor = BatchStepExecutor::new(StepExecutorAdapter::new(
            app_state,
            config_storage,
            app_handle,
        ));

        Self {
            adapter,
            batch_executor,
        }
    }

    /// Execute a single step and return a unified outcome.
    ///
    /// This method wraps the step in an `AdaptedStepConfig` and executes it
    /// through the trait-based adapter, returning a consistent `ExecutionOutcome`.
    #[tracing::instrument(
        name = "workflow.step.unified",
        skip(self, step),
        fields(
            step_name = %step.name.as_deref().unwrap_or(&step.step_type),
            step_type = %step.step_type,
            execution_id = %execution_id
        )
    )]
    pub async fn execute_step(
        &self,
        step: &ExecutionStepConfig,
        execution_id: &str,
    ) -> ExecutionOutcome {
        let config = AdaptedStepConfig::from_execution_step(step, execution_id);
        self.adapter.execute_to_outcome(config).await
    }

    /// Execute multiple steps and return aggregated results.
    ///
    /// This method executes all steps in sequence and provides both individual
    /// outcomes and an aggregated summary.
    #[tracing::instrument(
        name = "workflow.steps.batch",
        skip(self, steps),
        fields(
            step_count = %steps.len(),
            execution_id = %execution_id
        )
    )]
    pub async fn execute_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> ExecutionOutcome {
        self.batch_executor.execute_batch(steps, execution_id).await
    }

    /// Execute steps with individual outcome tracking.
    ///
    /// Returns both the raw step results and their converted outcomes,
    /// useful when you need access to step-specific details.
    pub async fn execute_steps_detailed(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> (Vec<StepExecutionResult>, Vec<ExecutionOutcome>) {
        let pairs = self.adapter.execute_steps_to_outcomes(steps, execution_id).await;

        let results: Vec<_> = pairs.iter().map(|(r, _)| r.clone()).collect();
        let outcomes: Vec<_> = pairs.into_iter().map(|(_, o)| o).collect();

        (results, outcomes)
    }
}

/// Execute steps with lifecycle management for steps requiring setup/teardown.
///
/// This is particularly useful for Playwright tests which benefit from
/// browser reuse across multiple test steps.
///
/// # Arguments
///
/// * `app_state` - Application state
/// * `config_storage` - Configuration storage
/// * `app_handle` - Tauri app handle
/// * `steps` - Steps to execute
/// * `execution_id` - Execution identifier
/// * `step_type_filter` - Optional filter for lifecycle management (e.g., "playwright")
///
/// # Returns
///
/// An `ExecutionOutcome` summarizing the lifecycle-managed execution.
pub async fn execute_with_lifecycle(
    app_state: Arc<AppState>,
    config_storage: Arc<TokioMutex<ConfigStorage>>,
    app_handle: tauri::AppHandle,
    steps: &[ExecutionStepConfig],
    execution_id: &str,
    step_type_filter: Option<&str>,
) -> Result<ExecutionOutcome, ExecutorError> {
    let start = std::time::Instant::now();

    // Create the adapter
    let adapter = StepExecutorAdapter::new(
        app_state,
        config_storage,
        app_handle,
    );

    // Wrap with lifecycle management
    let mut lifecycle = LifecycleStepAdapter::new(adapter);

    // Apply filter if provided
    if let Some(filter) = step_type_filter {
        lifecycle = lifecycle.with_step_type_filter(filter);
    }

    // Start lifecycle
    lifecycle.start().await?;

    // Execute steps
    let mut results = Vec::new();

    for (index, step) in steps.iter().enumerate() {
        let config = AdaptedStepConfig::from_execution_step(step, execution_id)
            .with_index(index);

        match lifecycle.execute(config).await {
            Ok(result) => {
                results.push(result);
            }
            Err(e) => {
                // Log error but continue to stop lifecycle
                warn!("Step {} failed with error: {}", index, e);
                break;
            }
        }
    }

    // Stop lifecycle (cleanup)
    if let Err(e) = lifecycle.stop().await {
        warn!("Failed to stop lifecycle: {}", e);
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    // Convert results to outcomes and aggregate
    let outcomes = results_to_outcomes(results);
    let mut aggregate = aggregate_outcomes(&outcomes);
    aggregate.duration_ms = duration_ms;

    Ok(aggregate)
}

/// Convert step execution results to outcomes and aggregate them.
///
/// This is a convenience function for converting existing result collections.
///
/// # Arguments
///
/// * `results` - Vector of step execution results
///
/// # Returns
///
/// An aggregated `ExecutionOutcome` summarizing all results.
pub fn aggregate_step_results(results: Vec<StepExecutionResult>) -> ExecutionOutcome {
    let outcomes = results_to_outcomes(results);
    aggregate_outcomes(&outcomes)
}

/// Build a custom execution outcome using the builder pattern.
///
/// # Example
///
/// ```ignore
/// let outcome = build_outcome(true, "All tests passed")
///     .with_output("test_count", serde_json::json!(10))
///     .with_duration(1500);
/// ```
pub fn build_outcome(success: bool, summary: impl Into<String>) -> ExecutionOutcome {
    if success {
        OutcomeBuilder::success(summary).build()
    } else {
        OutcomeBuilder::failure(
            summary,
            ExecutionError::execution("Execution failed"),
        ).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aggregate_step_results_empty() {
        let results = vec![];
        let outcome = aggregate_step_results(results);

        assert!(outcome.success);
        assert_eq!(outcome.get_output("step_count"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn test_build_outcome_success() {
        let outcome = build_outcome(true, "Test passed");

        assert!(outcome.success);
        assert_eq!(outcome.summary, "Test passed");
    }

    #[test]
    fn test_build_outcome_failure() {
        let outcome = build_outcome(false, "Test failed");

        assert!(!outcome.success);
        assert_eq!(outcome.summary, "Test failed");
        assert!(outcome.error.is_some());
    }
}
