//! Execution Helper Utilities
//!
//! This module provides shared helper functions for step execution,
//! consolidating common patterns used across different executors.
//!
//! ## Submodules
//!
//! - `prompt_builder`: Functions to consolidate prompts from execution steps
//! - `result_builder`: Functions to build step execution results
//! - `timeout_helper`: Timing and duration utilities

use crate::step_executor::{ExecutionStepConfig, StepExecutionConfig, StepExecutionResult};

/// Functions to consolidate prompts and step names from execution steps.
pub mod prompt_builder {
    use super::ExecutionStepConfig;

    /// Metadata for a sub-step within a consolidated prompt.
    #[derive(Debug, Clone)]
    pub struct SubStepMetadata {
        /// Unique identifier for this sub-step
        pub sub_step_id: String,
        /// Index in the original step list (0-based)
        pub step_index: usize,
        /// Human-readable step name
        pub step_name: String,
    }

    /// Consolidates step names from a slice of execution steps.
    ///
    /// Joins all non-empty step names with " + " separator.
    /// Returns empty string if no steps have names.
    ///
    /// # Example
    /// ```ignore
    /// let steps = vec![
    ///     ExecutionStepConfig { name: Some("Step A".into()), ..Default::default() },
    ///     ExecutionStepConfig { name: Some("Step B".into()), ..Default::default() },
    /// ];
    /// let names = consolidate_step_names(&steps);
    /// assert_eq!(names, "Step A + Step B");
    /// ```
    pub fn consolidate_step_names(steps: &[ExecutionStepConfig]) -> String {
        steps
            .iter()
            .filter_map(|s| s.name.as_ref())
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(" + ")
    }

    /// Consolidates step names with a default value if result is empty.
    ///
    /// # Arguments
    /// * `steps` - The steps to extract names from
    /// * `default_name` - Default name to use if no step names are found
    pub fn consolidate_step_names_with_default(
        steps: &[ExecutionStepConfig],
        default_name: &str,
    ) -> String {
        let names = consolidate_step_names(steps);
        if names.is_empty() {
            default_name.to_string()
        } else {
            names
        }
    }

    /// Consolidates prompts from a slice of execution steps.
    ///
    /// Joins all non-empty prompt contents with "\n\n---\n\n" separator.
    /// Returns empty string if no steps have prompts.
    ///
    /// # Example
    /// ```ignore
    /// let steps = vec![
    ///     ExecutionStepConfig { prompt_content: Some("Do X".into()), ..Default::default() },
    ///     ExecutionStepConfig { prompt_content: Some("Do Y".into()), ..Default::default() },
    /// ];
    /// let prompt = consolidate_prompts(&steps);
    /// assert_eq!(prompt, "Do X\n\n---\n\nDo Y");
    /// ```
    pub fn consolidate_prompts(steps: &[ExecutionStepConfig]) -> String {
        steps
            .iter()
            .filter_map(|s| s.prompt_content.as_ref())
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }

    /// Consolidates prompts from execution steps with structured metadata.
    ///
    /// Each sub-step gets a unique ID (using the provided prefix) and the AI
    /// is instructed to output `[STEP_COMPLETE:id]` after completing each task.
    ///
    /// Returns the consolidated prompt and metadata for each sub-step.
    ///
    /// # Arguments
    /// * `steps` - The steps to consolidate prompts from
    /// * `id_prefix` - Prefix for generating sub-step IDs (e.g., "setup", "verify")
    ///
    /// # Example
    /// ```ignore
    /// let steps = vec![
    ///     ExecutionStepConfig {
    ///         name: Some("Install deps".into()),
    ///         prompt_content: Some("Run npm install".into()),
    ///         ..Default::default()
    ///     },
    ///     ExecutionStepConfig {
    ///         name: Some("Build".into()),
    ///         prompt_content: Some("Run npm run build".into()),
    ///         ..Default::default()
    ///     },
    /// ];
    /// let (prompt, metadata) = consolidate_prompts_structured(&steps, "setup");
    /// // prompt contains instructions for AI to complete tasks and output markers
    /// // metadata contains SubStepMetadata for each step
    /// ```
    pub fn consolidate_prompts_structured(
        steps: &[ExecutionStepConfig],
        id_prefix: &str,
    ) -> (String, Vec<SubStepMetadata>) {
        let mut metadata = Vec::new();
        let mut prompt_parts = Vec::new();

        // Add instructions for the AI
        prompt_parts.push(
            "You are completing multiple tasks in sequence. After completing each task, \
             output the completion marker shown in brackets.\n\n"
                .to_string(),
        );

        for (idx, step) in steps.iter().enumerate() {
            // Use existing sub_step_id if present, otherwise generate one
            let sub_step_id = step
                .sub_step_id
                .clone()
                .unwrap_or_else(|| format!("{}-{}", id_prefix, idx));

            let step_name = step
                .name
                .clone()
                .unwrap_or_else(|| format!("Step {}", idx + 1));

            if let Some(ref content) = step.prompt_content {
                prompt_parts.push(format!(
                    "## Task {} - {}\n\n{}\n\nWhen complete, output: [STEP_COMPLETE:{}]\n",
                    idx + 1,
                    step_name,
                    content,
                    sub_step_id
                ));

                metadata.push(SubStepMetadata {
                    sub_step_id,
                    step_index: idx,
                    step_name,
                });
            }
        }

        let consolidated = prompt_parts.join("\n---\n\n");
        (consolidated, metadata)
    }

    /// Rebuilds a consolidated prompt excluding already-completed sub-steps.
    ///
    /// Used for resume functionality: when resuming a session, steps with IDs
    /// in `completed_ids` are excluded from the prompt.
    ///
    /// # Arguments
    /// * `steps` - The original steps to consolidate
    /// * `completed_ids` - Sub-step IDs that have already been completed
    /// * `id_prefix` - Prefix for generating sub-step IDs
    ///
    /// # Returns
    /// A tuple of (consolidated_prompt, remaining_metadata)
    pub fn rebuild_prompt_excluding_completed(
        steps: &[ExecutionStepConfig],
        completed_ids: &[String],
        id_prefix: &str,
    ) -> (String, Vec<SubStepMetadata>) {
        let mut metadata = Vec::new();
        let mut prompt_parts = Vec::new();

        prompt_parts.push(
            "You are continuing a task sequence. Some steps have already been completed. \
             Complete the remaining tasks and output the marker after each.\n\n"
                .to_string(),
        );

        let mut task_num = 1;
        for (idx, step) in steps.iter().enumerate() {
            let sub_step_id = step
                .sub_step_id
                .clone()
                .unwrap_or_else(|| format!("{}-{}", id_prefix, idx));

            // Skip completed steps
            if completed_ids.contains(&sub_step_id) {
                continue;
            }

            let step_name = step
                .name
                .clone()
                .unwrap_or_else(|| format!("Step {}", idx + 1));

            if let Some(ref content) = step.prompt_content {
                prompt_parts.push(format!(
                    "## Task {} - {}\n\n{}\n\nWhen complete, output: [STEP_COMPLETE:{}]\n",
                    task_num, step_name, content, sub_step_id
                ));

                metadata.push(SubStepMetadata {
                    sub_step_id,
                    step_index: idx,
                    step_name,
                });

                task_num += 1;
            }
        }

        let consolidated = prompt_parts.join("\n---\n\n");
        (consolidated, metadata)
    }
}

/// Functions to build step execution results.
pub mod result_builder {
    use super::{StepExecutionConfig, StepExecutionResult};

    /// Creates a successful step execution result.
    ///
    /// # Arguments
    /// * `step_index` - The index of the step (0-based)
    /// * `duration_ms` - Execution duration in milliseconds
    pub fn success_result(step_index: usize, duration_ms: u64) -> StepExecutionResult {
        StepExecutionResult {
            step_index,
            step_type: String::new(),
            step_name: String::new(),
            step_id: None,
            success: true,
            error: None,
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms,
            config: StepExecutionConfig::default(),
            verification_details: None,
            output_data: None,
        }
    }

    /// Creates a successful step execution result with step details.
    ///
    /// # Arguments
    /// * `step_index` - The index of the step (0-based)
    /// * `step_type` - The type of step executed
    /// * `step_name` - The name/description of the step
    /// * `duration_ms` - Execution duration in milliseconds
    pub fn success_result_with_details(
        step_index: usize,
        step_type: impl Into<String>,
        step_name: impl Into<String>,
        duration_ms: u64,
    ) -> StepExecutionResult {
        StepExecutionResult {
            step_index,
            step_type: step_type.into(),
            step_name: step_name.into(),
            step_id: None,
            success: true,
            error: None,
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms,
            config: StepExecutionConfig::default(),
            verification_details: None,
            output_data: None,
        }
    }

    /// Creates a failed step execution result.
    ///
    /// # Arguments
    /// * `step_index` - The index of the step (0-based)
    /// * `error` - The error message
    /// * `duration_ms` - Execution duration in milliseconds
    pub fn error_result(step_index: usize, error: String, duration_ms: u64) -> StepExecutionResult {
        StepExecutionResult {
            step_index,
            step_type: String::new(),
            step_name: String::new(),
            step_id: None,
            success: false,
            error: Some(error),
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms,
            config: StepExecutionConfig::default(),
            verification_details: None,
            output_data: None,
        }
    }

    /// Creates a failed step execution result with step details.
    ///
    /// # Arguments
    /// * `step_index` - The index of the step (0-based)
    /// * `step_type` - The type of step executed
    /// * `step_name` - The name/description of the step
    /// * `error` - The error message
    /// * `duration_ms` - Execution duration in milliseconds
    pub fn error_result_with_details(
        step_index: usize,
        step_type: impl Into<String>,
        step_name: impl Into<String>,
        error: String,
        duration_ms: u64,
    ) -> StepExecutionResult {
        StepExecutionResult {
            step_index,
            step_type: step_type.into(),
            step_name: step_name.into(),
            step_id: None,
            success: false,
            error: Some(error),
            screenshot_path: None,
            started_at: None,
            ended_at: None,
            duration_ms,
            config: StepExecutionConfig::default(),
            verification_details: None,
            output_data: None,
        }
    }
}

/// Timing and duration helper utilities.
pub mod timeout_helper {
    use std::time::{Duration, Instant};

    /// Records the duration since a given start time.
    ///
    /// # Arguments
    /// * `start` - The instant when the operation started
    ///
    /// # Returns
    /// Duration in milliseconds as u64
    pub fn record_duration(start: Instant) -> u64 {
        start.elapsed().as_millis() as u64
    }

    /// Creates a timeout Duration from seconds.
    ///
    /// # Arguments
    /// * `seconds` - Timeout in seconds
    ///
    /// # Returns
    /// A Duration representing the timeout
    pub fn create_timeout(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    /// Creates a timeout Duration from milliseconds.
    ///
    /// # Arguments
    /// * `millis` - Timeout in milliseconds
    ///
    /// # Returns
    /// A Duration representing the timeout
    pub fn create_timeout_millis(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// Execute a function and return its result along with the elapsed duration.
    ///
    /// This is useful for tracking execution time when building `ExecutionOutcome`:
    ///
    /// ```ignore
    /// use crate::executor::execution_helpers::timeout_helper::timed_result;
    /// use crate::executor::results::{ExecutionOutcome, IntoOutcome};
    ///
    /// let (result, duration_ms) = timed_result(|| {
    ///     // perform some operation
    ///     true
    /// });
    ///
    /// let outcome: ExecutionOutcome = result.into_outcome(duration_ms);
    /// ```
    ///
    /// # Arguments
    /// * `f` - The function to execute and time
    ///
    /// # Returns
    /// A tuple of (result, duration_ms)
    pub fn timed_result<F, T>(f: F) -> (T, u64)
    where
        F: FnOnce() -> T,
    {
        let start = Instant::now();
        let result = f();
        (result, start.elapsed().as_millis() as u64)
    }

    /// Execute an async function and return its result along with the elapsed duration.
    ///
    /// This is the async version of `timed_result` for use with async operations.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use crate::executor::execution_helpers::timeout_helper::timed_result_async;
    ///
    /// let (result, duration_ms) = timed_result_async(async {
    ///     // perform async operation
    ///     Ok::<_, String>("success")
    /// }).await;
    /// ```
    ///
    /// # Arguments
    /// * `f` - The async future to execute and time
    ///
    /// # Returns
    /// A tuple of (result, duration_ms)
    pub async fn timed_result_async<F, T>(f: F) -> (T, u64)
    where
        F: std::future::Future<Output = T>,
    {
        let start = Instant::now();
        let result = f.await;
        (result, start.elapsed().as_millis() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consolidate_step_names() {
        let steps = vec![
            ExecutionStepConfig {
                name: Some("Step A".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                name: Some("Step B".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                name: None,
                ..Default::default()
            },
        ];
        let names = prompt_builder::consolidate_step_names(&steps);
        assert_eq!(names, "Step A + Step B");
    }

    #[test]
    fn test_consolidate_step_names_empty() {
        let steps: Vec<ExecutionStepConfig> = vec![];
        let names = prompt_builder::consolidate_step_names(&steps);
        assert_eq!(names, "");
    }

    #[test]
    fn test_consolidate_step_names_with_default() {
        let steps: Vec<ExecutionStepConfig> = vec![];
        let names = prompt_builder::consolidate_step_names_with_default(&steps, "Default Task");
        assert_eq!(names, "Default Task");

        let steps_with_names = vec![ExecutionStepConfig {
            name: Some("Named Task".into()),
            ..Default::default()
        }];
        let names_with_value =
            prompt_builder::consolidate_step_names_with_default(&steps_with_names, "Default Task");
        assert_eq!(names_with_value, "Named Task");
    }

    #[test]
    fn test_consolidate_prompts() {
        let steps = vec![
            ExecutionStepConfig {
                prompt_content: Some("Do X".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                prompt_content: Some("Do Y".into()),
                ..Default::default()
            },
        ];
        let prompt = prompt_builder::consolidate_prompts(&steps);
        assert_eq!(prompt, "Do X\n\n---\n\nDo Y");
    }

    #[test]
    fn test_consolidate_prompts_empty() {
        let steps: Vec<ExecutionStepConfig> = vec![];
        let prompt = prompt_builder::consolidate_prompts(&steps);
        assert_eq!(prompt, "");
    }

    #[test]
    fn test_consolidate_prompts_structured() {
        let steps = vec![
            ExecutionStepConfig {
                name: Some("Install dependencies".into()),
                prompt_content: Some("Run npm install".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                name: Some("Build project".into()),
                prompt_content: Some("Run npm run build".into()),
                ..Default::default()
            },
        ];

        let (prompt, metadata) = prompt_builder::consolidate_prompts_structured(&steps, "setup");

        // Check that the prompt contains task instructions
        assert!(prompt.contains("completing multiple tasks in sequence"));
        assert!(prompt.contains("## Task 1 - Install dependencies"));
        assert!(prompt.contains("Run npm install"));
        assert!(prompt.contains("[STEP_COMPLETE:setup-0]"));
        assert!(prompt.contains("## Task 2 - Build project"));
        assert!(prompt.contains("Run npm run build"));
        assert!(prompt.contains("[STEP_COMPLETE:setup-1]"));

        // Check metadata
        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata[0].sub_step_id, "setup-0");
        assert_eq!(metadata[0].step_index, 0);
        assert_eq!(metadata[0].step_name, "Install dependencies");
        assert_eq!(metadata[1].sub_step_id, "setup-1");
        assert_eq!(metadata[1].step_index, 1);
        assert_eq!(metadata[1].step_name, "Build project");
    }

    #[test]
    fn test_consolidate_prompts_structured_with_existing_sub_step_id() {
        let steps = vec![
            ExecutionStepConfig {
                name: Some("First task".into()),
                prompt_content: Some("Do something".into()),
                sub_step_id: Some("custom-id-1".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                name: Some("Second task".into()),
                prompt_content: Some("Do something else".into()),
                // No sub_step_id - should be auto-generated
                ..Default::default()
            },
        ];

        let (prompt, metadata) = prompt_builder::consolidate_prompts_structured(&steps, "test");

        // First step uses custom ID
        assert!(prompt.contains("[STEP_COMPLETE:custom-id-1]"));
        assert_eq!(metadata[0].sub_step_id, "custom-id-1");

        // Second step uses auto-generated ID
        assert!(prompt.contains("[STEP_COMPLETE:test-1]"));
        assert_eq!(metadata[1].sub_step_id, "test-1");
    }

    #[test]
    fn test_rebuild_prompt_excluding_completed() {
        let steps = vec![
            ExecutionStepConfig {
                name: Some("Step A".into()),
                prompt_content: Some("Do A".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                name: Some("Step B".into()),
                prompt_content: Some("Do B".into()),
                ..Default::default()
            },
            ExecutionStepConfig {
                name: Some("Step C".into()),
                prompt_content: Some("Do C".into()),
                ..Default::default()
            },
        ];

        // Mark the first step as completed
        let completed_ids = vec!["resume-0".to_string()];
        let (prompt, metadata) =
            prompt_builder::rebuild_prompt_excluding_completed(&steps, &completed_ids, "resume");

        // Should only include steps B and C
        assert!(prompt.contains("continuing a task sequence"));
        assert!(!prompt.contains("Do A")); // Step A is excluded
        assert!(prompt.contains("Do B"));
        assert!(prompt.contains("Do C"));

        // Task numbers should be 1 and 2 (not 2 and 3)
        assert!(prompt.contains("## Task 1 - Step B"));
        assert!(prompt.contains("## Task 2 - Step C"));

        // Metadata should only include remaining steps
        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata[0].sub_step_id, "resume-1");
        assert_eq!(metadata[0].step_index, 1); // Original index preserved
        assert_eq!(metadata[1].sub_step_id, "resume-2");
        assert_eq!(metadata[1].step_index, 2);
    }

    #[test]
    fn test_success_result() {
        let result = result_builder::success_result(0, 100);
        assert!(result.success);
        assert_eq!(result.step_index, 0);
        assert_eq!(result.duration_ms, 100);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_error_result() {
        let result = result_builder::error_result(1, "Failed".to_string(), 200);
        assert!(!result.success);
        assert_eq!(result.step_index, 1);
        assert_eq!(result.duration_ms, 200);
        assert_eq!(result.error, Some("Failed".to_string()));
    }

    #[test]
    fn test_record_duration() {
        let start = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let duration = timeout_helper::record_duration(start);
        assert!(duration >= 10);
    }

    #[test]
    fn test_create_timeout() {
        let timeout = timeout_helper::create_timeout(5);
        assert_eq!(timeout, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_create_timeout_millis() {
        let timeout = timeout_helper::create_timeout_millis(500);
        assert_eq!(timeout, std::time::Duration::from_millis(500));
    }

    #[test]
    fn test_timed_result() {
        let (result, duration_ms) = timeout_helper::timed_result(|| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            42
        });

        assert_eq!(result, 42);
        assert!(
            duration_ms >= 10,
            "Duration should be at least 10ms, got {}",
            duration_ms
        );
    }

    #[test]
    fn test_timed_result_with_outcome_conversion() {
        use crate::executor::results::IntoOutcome;

        let (success, duration_ms) = timeout_helper::timed_result(|| true);
        let outcome = success.into_outcome(duration_ms);

        assert!(outcome.success);
        assert!(
            outcome.duration_ms < 10,
            "Simple operation should be very fast"
        );
    }

    #[tokio::test]
    async fn test_timed_result_async() {
        let (result, duration_ms) = timeout_helper::timed_result_async(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            "async result"
        })
        .await;

        assert_eq!(result, "async result");
        assert!(
            duration_ms >= 10,
            "Duration should be at least 10ms, got {}",
            duration_ms
        );
    }
}
