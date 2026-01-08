//! Step Executor Module
//!
//! Provides unified execution of automation steps (workflows, actions, states,
//! screenshots, Playwright tests). This is the core execution layer used by:
//! - Run page (single workflow execution)
//! - AI Builder (multi-step execution before AI session)
//! - MCP API (direct step execution)
//!
//! The design principle: multi-step execution is the foundation, and running
//! a single workflow is just a special case (one step of type "workflow").

use crate::action_service::UnifiedActionService;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::executor::file_logger::FileLogger;
use crate::iteration_bundle::{
    parse_action_events, parse_image_recognition_events, ActionEvent, ImageRecognitionEvent,
    RelevantLogSources,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

/// Configuration for a single execution step
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutionStepConfig {
    /// Step type: "workflow", "state", "action", "screenshot", "playwright", "prompt"
    #[serde(rename = "type")]
    pub step_type: String,

    /// Step name (workflow name, state name, or description)
    #[serde(default)]
    pub name: Option<String>,

    /// For action steps: "click", "double_click", "right_click"
    #[serde(rename = "actionType")]
    pub action_type: Option<String>,

    /// Target image ID for action steps
    #[serde(rename = "targetImageId")]
    pub target_image_id: Option<String>,

    /// Target image name for display
    #[serde(rename = "targetImageName")]
    pub target_image_name: Option<String>,

    /// Monitor index (0 = primary)
    #[serde(rename = "monitorIndex", default)]
    pub monitor_index: Option<i32>,

    /// Whether to take screenshot after this step
    #[serde(rename = "takeScreenshot", default)]
    pub take_screenshot: bool,

    /// Delay before screenshot in seconds
    #[serde(rename = "screenshotDelay", default)]
    pub screenshot_delay: u32,

    /// Monitor for screenshot ("all" or index)
    #[serde(rename = "screenshotMonitor", default)]
    pub screenshot_monitor: Option<serde_json::Value>,

    /// Playwright script ID
    #[serde(rename = "playwrightScriptId")]
    pub playwright_script_id: Option<String>,

    /// Prompt content (for prompt steps - not executed, passed to AI)
    #[serde(rename = "promptContent")]
    pub prompt_content: Option<String>,

    /// Timeout for this step in seconds (default: 300 for workflows, 30 for actions)
    #[serde(rename = "timeoutSeconds", default)]
    pub timeout_seconds: Option<u64>,

    /// Initial state IDs for workflow steps (overrides default initial states)
    #[serde(rename = "initialStateIds", default)]
    pub initial_state_ids: Option<Vec<String>>,
}

impl ExecutionStepConfig {
    /// Create a workflow step (convenience constructor)
    pub fn workflow(name: &str) -> Self {
        Self {
            step_type: "workflow".to_string(),
            name: Some(name.to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: false,
            screenshot_delay: 0,
            screenshot_monitor: None,
            playwright_script_id: None,
            prompt_content: None,
            timeout_seconds: Some(300),
            initial_state_ids: None,
        }
    }

    /// Create a workflow step with screenshot
    pub fn workflow_with_screenshot(name: &str, delay: u32) -> Self {
        Self {
            step_type: "workflow".to_string(),
            name: Some(name.to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: None,
            take_screenshot: true,
            screenshot_delay: delay,
            screenshot_monitor: None,
            playwright_script_id: None,
            prompt_content: None,
            timeout_seconds: Some(300),
            initial_state_ids: None,
        }
    }

    /// Create a screenshot step
    pub fn screenshot(monitor: Option<i32>, delay: u32) -> Self {
        Self {
            step_type: "screenshot".to_string(),
            name: Some("Capture Screenshot".to_string()),
            action_type: None,
            target_image_id: None,
            target_image_name: None,
            monitor_index: monitor,
            take_screenshot: true,
            screenshot_delay: delay,
            screenshot_monitor: monitor.map(|m| serde_json::Value::Number(m.into())),
            playwright_script_id: None,
            prompt_content: None,
            timeout_seconds: Some(30),
            initial_state_ids: None,
        }
    }
}

/// Result of executing a single step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    /// Step index (0-based)
    pub step_index: usize,
    /// Step type that was executed
    pub step_type: String,
    /// Step name for display
    pub step_name: String,
    /// Whether the step succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Path to screenshot if captured
    pub screenshot_path: Option<String>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Step configuration (for AI visibility)
    pub config: StepExecutionConfig,
}

/// Step configuration captured for AI visibility
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepExecutionConfig {
    /// For action steps: "click", "double_click", "right_click"
    pub action_type: Option<String>,
    /// Target image ID for action steps
    pub target_image_id: Option<String>,
    /// Target image name for display
    pub target_image_name: Option<String>,
    /// Monitor index (0 = primary)
    pub monitor_index: Option<i32>,
    /// Delay before screenshot in seconds
    pub screenshot_delay: Option<u32>,
    /// Timeout for this step in seconds
    pub timeout_seconds: Option<u64>,
    /// Playwright script ID
    pub playwright_script_id: Option<String>,
    /// Initial state IDs for workflow steps
    pub initial_state_ids: Option<Vec<String>>,
}

/// Result of executing all steps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether all steps completed successfully
    pub success: bool,
    /// Total number of steps
    pub total_steps: usize,
    /// Number of successful steps
    pub successful_steps: usize,
    /// Number of failed steps
    pub failed_steps: usize,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Individual step results
    pub steps: Vec<StepExecutionResult>,
    /// Logs captured during execution (from .dev-logs/)
    #[serde(default)]
    pub captured_logs: Option<CapturedLogs>,
    /// Runner logs captured during execution (GUI automation events)
    #[serde(default)]
    pub captured_runner_logs: Option<CapturedRunnerLogs>,
}

/// A log source configuration (passed from frontend)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceConfig {
    /// Unique identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Absolute path to the log file
    pub path: String,
    /// Whether this source is enabled
    pub enabled: bool,
}

/// Logs captured from application log files during automation
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedLogs {
    /// Log entries per source (keyed by source name)
    pub sources: HashMap<String, String>,
}

/// Runner logs captured during automation (GUI automation + Playwright)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedRunnerLogs {
    /// Action/workflow execution events (from runner-actions.jsonl)
    pub actions: Vec<ActionEvent>,
    /// Image recognition events (from runner-image-recognition.jsonl)
    pub image_recognition: Vec<ImageRecognitionEvent>,
}

impl ExecutionResult {
    /// Generate a markdown summary of the execution results
    pub fn to_markdown_summary(&self) -> String {
        if self.steps.is_empty() {
            return String::new();
        }

        let mut summary = String::new();
        summary.push_str("\n## Pre-Execution Results\n\n");
        summary.push_str("The following steps were executed deterministically by the runner:\n\n");

        for result in &self.steps {
            summary.push_str(&format!(
                "{}. **{}** ({}): {} in {}ms\n",
                result.step_index + 1,
                result.step_name,
                result.step_type,
                if result.success {
                    "Success".to_string()
                } else {
                    format!(
                        "Failed - {}",
                        result.error.as_deref().unwrap_or("unknown error")
                    )
                },
                result.duration_ms
            ));

            if let Some(ref path) = result.screenshot_path {
                summary.push_str(&format!("   Screenshot: `{}`\n", path));
            }
        }

        summary.push_str(&format!(
            "\n**Summary:** {} of {} steps completed successfully.\n",
            self.successful_steps, self.total_steps
        ));

        if self.failed_steps > 0 {
            summary.push_str("\n**Note:** Some steps failed. Please analyze the errors above.\n");
        }

        // Include captured logs if any
        if let Some(ref logs) = self.captured_logs {
            if !logs.sources.is_empty() {
                summary.push_str("\n## Application Logs (Captured During Automation)\n\n");

                for (name, content) in &logs.sources {
                    if !content.trim().is_empty() {
                        summary.push_str(&format!("### {} Logs\n\n```\n", name));
                        // Limit to last 100 lines to avoid overwhelming the AI
                        let lines: Vec<&str> = content.lines().collect();
                        let start = if lines.len() > 100 {
                            lines.len() - 100
                        } else {
                            0
                        };
                        for line in &lines[start..] {
                            summary.push_str(line);
                            summary.push('\n');
                        }
                        summary.push_str("```\n\n");
                    }
                }
            }
        }

        summary
    }
}

/// Step Executor - executes automation steps using UnifiedActionService
pub struct StepExecutor {
    action_service: UnifiedActionService,
    app_state: Arc<AppState>,
}

impl StepExecutor {
    /// Create a new StepExecutor
    pub fn new(app_state: Arc<AppState>, config_storage: Arc<TokioMutex<ConfigStorage>>) -> Self {
        Self {
            action_service: UnifiedActionService::new(app_state.clone(), config_storage),
            app_state,
        }
    }

    /// Record a screenshot capture event to the RunRecordingHandler.
    ///
    /// This ensures screenshots captured directly by the step executor
    /// (not through Python) are still recorded in the automation logs.
    async fn record_screenshot_event(
        &self,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<i32>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        let monitor_str = monitor.map(|m| m.to_string());
        self.app_state
            .run_recording_handler
            .on_screenshot_captured(
                screenshot_type,
                file_path,
                monitor_str,
                delay_seconds,
                success,
                associated_action,
                error,
            )
            .await;
    }

    /// Execute a list of steps and return results
    ///
    /// This is the core execution function used by all consumers.
    /// Steps are executed in order, and execution continues even if a step fails
    /// (so the caller can see all results and decide how to proceed).
    pub async fn execute_steps(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
    ) -> ExecutionResult {
        self.execute_steps_with_log_sources(steps, execution_id, &[])
            .await
    }

    /// Execute steps with log source configuration for log capture
    pub async fn execute_steps_with_log_sources(
        &self,
        steps: &[ExecutionStepConfig],
        execution_id: &str,
        log_sources: &[LogSourceConfig],
    ) -> ExecutionResult {
        let mut results = Vec::new();
        let total_start = std::time::Instant::now();

        if steps.is_empty() {
            return ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: results,
                captured_logs: None,
                captured_runner_logs: None,
            };
        }

        // Determine which logs are relevant based on step types
        let relevant_logs = RelevantLogSources::from_steps(steps);
        relevant_logs.log_relevance();

        // Record log file positions before execution (only for enabled sources)
        let log_positions = Self::capture_log_positions(log_sources);

        // Record runner log positions (only if GUI automation is relevant)
        let runner_log_positions = if relevant_logs.gui_automation {
            Self::capture_runner_log_positions()
        } else {
            HashMap::new()
        };

        info!(
            "Executing {} steps for execution {}",
            steps.len(),
            execution_id
        );

        for (index, step) in steps.iter().enumerate() {
            let step_name = step.name.clone().unwrap_or_else(|| step.step_type.clone());
            let start_time = std::time::Instant::now();

            info!(
                "Executing step {}/{}: {} ({})",
                index + 1,
                steps.len(),
                step_name,
                step.step_type
            );

            let (success, error, screenshot_path) = self.execute_single_step(step).await;

            // Take post-step screenshot if requested (and step succeeded)
            let final_screenshot =
                if step.take_screenshot && success && step.step_type != "screenshot" {
                    self.capture_post_step_screenshot(step)
                        .await
                        .or(screenshot_path)
                } else {
                    screenshot_path
                };

            let duration_ms = start_time.elapsed().as_millis() as u64;

            if success {
                info!(
                    "Step {}/{} completed successfully in {}ms",
                    index + 1,
                    steps.len(),
                    duration_ms
                );
            } else {
                warn!("Step {}/{} failed: {:?}", index + 1, steps.len(), error);
            }

            results.push(StepExecutionResult {
                step_index: index,
                step_type: step.step_type.clone(),
                step_name,
                success,
                error,
                screenshot_path: final_screenshot,
                duration_ms,
                config: StepExecutionConfig {
                    action_type: step.action_type.clone(),
                    target_image_id: step.target_image_id.clone(),
                    target_image_name: step.target_image_name.clone(),
                    monitor_index: step.monitor_index,
                    screenshot_delay: if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    timeout_seconds: step.timeout_seconds,
                    playwright_script_id: step.playwright_script_id.clone(),
                    initial_state_ids: step.initial_state_ids.clone(),
                },
            });
        }

        let successful_steps = results.iter().filter(|r| r.success).count();
        let failed_steps = results.len() - successful_steps;

        info!(
            "Completed {} steps: {} succeeded, {} failed",
            results.len(),
            successful_steps,
            failed_steps
        );

        // Capture logs that were written during execution
        let captured_logs = Self::capture_logs_since(log_sources, log_positions);

        // Capture runner logs (only if GUI automation was relevant)
        let captured_runner_logs = if relevant_logs.gui_automation {
            Self::capture_runner_logs_since(runner_log_positions)
        } else {
            None
        };

        ExecutionResult {
            success: failed_steps == 0,
            total_steps: results.len(),
            successful_steps,
            failed_steps,
            total_duration_ms: total_start.elapsed().as_millis() as u64,
            steps: results,
            captured_logs,
            captured_runner_logs,
        }
    }

    /// Get current file positions for configured log sources
    fn capture_log_positions(
        log_sources: &[LogSourceConfig],
    ) -> std::collections::HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let path = std::path::Path::new(&source.path);
            if let Ok(mut file) = std::fs::File::open(path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(source.id.clone(), pos);
                }
            }
        }

        positions
    }

    /// Read log content that was written since the given positions
    fn capture_logs_since(
        log_sources: &[LogSourceConfig],
        positions: std::collections::HashMap<String, u64>,
    ) -> Option<CapturedLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let mut sources = std::collections::HashMap::new();

        for source in log_sources {
            if !source.enabled {
                continue;
            }

            let start_pos = positions.get(&source.id).copied().unwrap_or(0);
            let path = std::path::Path::new(&source.path);

            if let Ok(mut file) = std::fs::File::open(path) {
                if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                    let mut content = String::new();
                    if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                        sources.insert(source.name.clone(), content);
                    }
                }
            }
        }

        if sources.is_empty() {
            None
        } else {
            Some(CapturedLogs { sources })
        }
    }

    /// Get the .dev-logs directory path
    fn get_dev_logs_dir() -> PathBuf {
        PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs")
    }

    /// Get current file positions for runner log files (actions + image recognition)
    fn capture_runner_log_positions() -> HashMap<String, u64> {
        use std::io::{Seek, SeekFrom};

        let mut positions = HashMap::new();
        let dev_logs = Self::get_dev_logs_dir();

        // Track positions for runner-actions.jsonl and runner-image-recognition.jsonl
        for filename in &["runner-actions.jsonl", "runner-image-recognition.jsonl"] {
            let path = dev_logs.join(filename);
            if let Ok(mut file) = std::fs::File::open(&path) {
                if let Ok(pos) = file.seek(SeekFrom::End(0)) {
                    positions.insert(filename.to_string(), pos);
                    info!(
                        "Captured runner log position for {}: {} bytes",
                        filename, pos
                    );
                }
            }
        }

        positions
    }

    /// Read runner logs that were written since the given positions
    fn capture_runner_logs_since(positions: HashMap<String, u64>) -> Option<CapturedRunnerLogs> {
        use std::io::{Read, Seek, SeekFrom};

        let dev_logs = Self::get_dev_logs_dir();
        let mut actions = Vec::new();
        let mut image_recognition = Vec::new();

        // Read runner-actions.jsonl
        let actions_path = dev_logs.join("runner-actions.jsonl");
        let start_pos = positions.get("runner-actions.jsonl").copied().unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&actions_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    actions = parse_action_events(&content);
                    info!("Captured {} action events from runner log", actions.len());
                }
            }
        }

        // Read runner-image-recognition.jsonl
        let ir_path = dev_logs.join("runner-image-recognition.jsonl");
        let start_pos = positions
            .get("runner-image-recognition.jsonl")
            .copied()
            .unwrap_or(0);
        if let Ok(mut file) = std::fs::File::open(&ir_path) {
            if file.seek(SeekFrom::Start(start_pos)).is_ok() {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                    image_recognition = parse_image_recognition_events(&content);
                    info!(
                        "Captured {} image recognition events from runner log",
                        image_recognition.len()
                    );
                }
            }
        }

        if actions.is_empty() && image_recognition.is_empty() {
            None
        } else {
            Some(CapturedRunnerLogs {
                actions,
                image_recognition,
            })
        }
    }

    /// Execute a single step and return (success, error, screenshot_path)
    async fn execute_single_step(
        &self,
        step: &ExecutionStepConfig,
    ) -> (bool, Option<String>, Option<String>) {
        let timeout = step
            .timeout_seconds
            .unwrap_or(match step.step_type.as_str() {
                "workflow" => 300,
                "state" => 300,
                _ => 30,
            });

        match step.step_type.as_str() {
            "workflow" => {
                if let Some(ref workflow_name) = step.name {
                    match self
                        .action_service
                        .run_workflow(
                            workflow_name,
                            None,
                            step.monitor_index,
                            timeout,
                            step.initial_state_ids.as_deref(),
                        )
                        .await
                    {
                        Ok(result) => (result.success, result.error, None),
                        Err(e) => (false, Some(format!("Workflow error: {}", e)), None),
                    }
                } else {
                    (false, Some("No workflow name specified".to_string()), None)
                }
            }
            "state" => {
                if let Some(ref state_name) = step.name {
                    match self
                        .action_service
                        .go_to_state(state_name, None, step.monitor_index, timeout)
                        .await
                    {
                        Ok(result) => {
                            if result.success {
                                info!(
                                    "GO_TO_STATE '{}': Success. Check Python logs for details \
                                    (transition may have been skipped if state was already active)",
                                    state_name
                                );
                            }
                            (result.success, result.error, None)
                        }
                        Err(e) => (false, Some(format!("State navigation error: {}", e)), None),
                    }
                } else {
                    (false, Some("No state name specified".to_string()), None)
                }
            }
            "action" => {
                if let (Some(ref action_type), Some(ref image_id)) =
                    (&step.action_type, &step.target_image_id)
                {
                    match self
                        .action_service
                        .execute_action(action_type, image_id, None, step.monitor_index)
                        .await
                    {
                        Ok(result) => (
                            result.success,
                            result.message.filter(|_| !result.success),
                            None,
                        ),
                        Err(e) => (false, Some(format!("Action error: {}", e)), None),
                    }
                } else {
                    (
                        false,
                        Some("No action type or image ID specified".to_string()),
                        None,
                    )
                }
            }
            "screenshot" => {
                let monitor = match &step.screenshot_monitor {
                    Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v as i32),
                    Some(serde_json::Value::String(s)) if s == "all" => None,
                    _ => step.monitor_index,
                };
                let delay = if step.screenshot_delay > 0 {
                    Some(step.screenshot_delay as f64)
                } else {
                    None
                };

                // Get sequence number for tree events
                use std::sync::atomic::{AtomicU32, Ordering};
                static SCREENSHOT_SEQUENCE: AtomicU32 = AtomicU32::new(1);
                let sequence = SCREENSHOT_SEQUENCE.fetch_add(1, Ordering::SeqCst);
                let timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

                // Build action node for tree events
                let action_node = serde_json::json!({
                    "action_type": "SCREENSHOT",
                    "action_id": format!("screenshot-{}", sequence),
                    "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
                    "delay_seconds": delay.unwrap_or(0.0),
                });

                // Emit action_started tree event
                FileLogger::log_tree_event(
                    "action_started",
                    &action_node,
                    &[],
                    timestamp,
                    sequence,
                );

                let result = self.action_service.capture_screenshot(monitor, delay).await;
                let end_timestamp = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;

                match result {
                    // Use absolute_path instead of screenshot_path to avoid relative path resolution issues
                    Ok(res) => {
                        // Record screenshot event for automation logs
                        let file_path = res.absolute_path.clone().unwrap_or_default();

                        // Emit action_completed tree event
                        let completed_node = serde_json::json!({
                            "action_type": "SCREENSHOT",
                            "action_id": format!("screenshot-{}", sequence),
                            "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
                            "delay_seconds": delay.unwrap_or(0.0),
                            "success": res.success,
                            "filename": &file_path,
                        });
                        FileLogger::log_tree_event(
                            if res.success {
                                "action_completed"
                            } else {
                                "action_failed"
                            },
                            &completed_node,
                            &[],
                            end_timestamp,
                            sequence,
                        );

                        self.record_screenshot_event(
                            "standalone",
                            &file_path,
                            monitor,
                            if step.screenshot_delay > 0 {
                                Some(step.screenshot_delay)
                            } else {
                                None
                            },
                            res.success,
                            None,
                            res.error.clone(),
                        )
                        .await;
                        (res.success, res.error, res.absolute_path)
                    }
                    Err(e) => {
                        let error_msg = format!("Screenshot error: {}", e);

                        // Emit action_failed tree event
                        let failed_node = serde_json::json!({
                            "action_type": "SCREENSHOT",
                            "action_id": format!("screenshot-{}", sequence),
                            "monitor": monitor.map(|m| m.to_string()).unwrap_or_else(|| "all".to_string()),
                            "delay_seconds": delay.unwrap_or(0.0),
                            "success": false,
                            "error": &error_msg,
                        });
                        FileLogger::log_tree_event(
                            "action_failed",
                            &failed_node,
                            &[],
                            end_timestamp,
                            sequence,
                        );

                        // Record failed screenshot event
                        self.record_screenshot_event(
                            "standalone",
                            "",
                            monitor,
                            if step.screenshot_delay > 0 {
                                Some(step.screenshot_delay)
                            } else {
                                None
                            },
                            false,
                            None,
                            Some(error_msg.clone()),
                        )
                        .await;
                        (false, Some(error_msg), None)
                    }
                }
            }
            "playwright" => {
                if let Some(ref script_id) = step.playwright_script_id {
                    self.run_playwright_script(script_id).await
                } else {
                    (
                        false,
                        Some("No Playwright script ID specified".to_string()),
                        None,
                    )
                }
            }
            "prompt" => {
                // Prompt steps are text for the AI, not executed here
                (true, None, None)
            }
            _ => {
                warn!("Unknown step type: {}", step.step_type);
                (
                    false,
                    Some(format!("Unknown step type: {}", step.step_type)),
                    None,
                )
            }
        }
    }

    /// Capture a post-step screenshot
    async fn capture_post_step_screenshot(&self, step: &ExecutionStepConfig) -> Option<String> {
        // Apply configured screenshot delay (no default delay)
        if step.screenshot_delay > 0 {
            info!(
                "Waiting {}s before screenshot capture",
                step.screenshot_delay
            );
            tokio::time::sleep(std::time::Duration::from_secs(step.screenshot_delay as u64)).await;
        }

        let monitor = match &step.screenshot_monitor {
            Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v as i32),
            Some(serde_json::Value::String(s)) if s == "all" => None,
            _ => step.monitor_index,
        };

        // Build associated action description
        let associated_action = match step.step_type.as_str() {
            "workflow" => step.name.clone().map(|n| format!("workflow:{}", n)),
            "action" => step.action_type.clone().map(|t| format!("action:{}", t)),
            "state" => step.name.clone().map(|n| format!("state:{}", n)),
            _ => Some(format!("step:{}", step.step_type)),
        };

        match self.action_service.capture_screenshot(monitor, None).await {
            Ok(result) => {
                let file_path = result.absolute_path.clone().unwrap_or_default();
                // Record post-action screenshot event
                self.record_screenshot_event(
                    "post_action",
                    &file_path,
                    monitor,
                    if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    result.success,
                    associated_action,
                    result.error,
                )
                .await;
                result.absolute_path // Use absolute path for step screenshots
            }
            Err(e) => {
                warn!("Failed to capture post-step screenshot: {}", e);
                // Record failed post-action screenshot event
                self.record_screenshot_event(
                    "post_action",
                    "",
                    monitor,
                    if step.screenshot_delay > 0 {
                        Some(step.screenshot_delay)
                    } else {
                        None
                    },
                    false,
                    associated_action,
                    Some(format!("Screenshot error: {}", e)),
                )
                .await;
                None
            }
        }
    }

    /// Run a Playwright test script via HTTP API
    async fn run_playwright_script(
        &self,
        script_id: &str,
    ) -> (bool, Option<String>, Option<String>) {
        let client = reqwest::Client::new();
        let url = format!("http://localhost:9876/playwright/scripts/{}/run", script_id);

        match client
            .post(&url)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
        {
            Ok(response) => {
                if let Ok(json) = response.json::<serde_json::Value>().await {
                    let success = json
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let error = if !success {
                        json.get("error")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    };
                    (success, error, None)
                } else {
                    (
                        false,
                        Some("Failed to parse Playwright response".to_string()),
                        None,
                    )
                }
            }
            Err(e) => (
                false,
                Some(format!("Playwright request error: {}", e)),
                None,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_step_creation() {
        let step = ExecutionStepConfig::workflow("TestWorkflow");
        assert_eq!(step.step_type, "workflow");
        assert_eq!(step.name, Some("TestWorkflow".to_string()));
        assert_eq!(step.take_screenshot, false);
    }

    #[test]
    fn test_workflow_with_screenshot_creation() {
        let step = ExecutionStepConfig::workflow_with_screenshot("TestWorkflow", 2);
        assert_eq!(step.step_type, "workflow");
        assert_eq!(step.name, Some("TestWorkflow".to_string()));
        assert_eq!(step.take_screenshot, true);
        assert_eq!(step.screenshot_delay, 2);
    }

    #[test]
    fn test_execution_result_empty_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 0,
            successful_steps: 0,
            failed_steps: 0,
            total_duration_ms: 0,
            steps: vec![],
            captured_logs: None,
            captured_runner_logs: None,
        };
        assert_eq!(result.to_markdown_summary(), "");
    }

    #[test]
    fn test_execution_result_summary() {
        let result = ExecutionResult {
            success: true,
            total_steps: 2,
            successful_steps: 2,
            failed_steps: 0,
            total_duration_ms: 1500,
            steps: vec![
                StepExecutionResult {
                    step_index: 0,
                    step_type: "workflow".to_string(),
                    step_name: "Login".to_string(),
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot1.png".to_string()),
                    duration_ms: 1000,
                    config: StepExecutionConfig::default(),
                },
                StepExecutionResult {
                    step_index: 1,
                    step_type: "screenshot".to_string(),
                    step_name: "Capture".to_string(),
                    success: true,
                    error: None,
                    screenshot_path: Some("screenshot2.png".to_string()),
                    duration_ms: 500,
                    config: StepExecutionConfig::default(),
                },
            ],
            captured_logs: None,
            captured_runner_logs: None,
        };
        let summary = result.to_markdown_summary();
        assert!(summary.contains("Pre-Execution Results"));
        assert!(summary.contains("Login"));
        assert!(summary.contains("2 of 2 steps completed successfully"));
    }
}
