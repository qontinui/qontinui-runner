//! Iteration Bundle Module
//!
//! Provides a complete, self-contained data bundle for each AI session iteration.
//! The bundle contains all relevant data the AI needs to debug issues without
//! having to search for logs or understand the runner architecture.
//!
//! Key design principles:
//! - Everything in one place - AI doesn't search, it reads what's given
//! - Iteration-scoped - Only data from current iteration, not historical
//! - Source-labeled - Every piece of data says where it came from
//! - Relevance-filtered - Only include logs relevant to the workflow's step types
//! - Screenshots linked to steps - Clear which screenshot shows state after which step

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

use crate::step_executor::{ExecutionResult, ExecutionStepConfig, LogSourceConfig};

// ============================================================================
// Core Types
// ============================================================================

/// Complete data bundle for one AI session iteration.
/// Contains everything the AI needs without requiring any file searches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationBundle {
    /// Iteration number (1-indexed)
    pub iteration: u32,
    /// Task run ID this iteration belongs to
    pub task_run_id: String,
    /// When this iteration started
    pub timestamp_start: String,
    /// When this iteration ended (pre-execution complete)
    pub timestamp_end: String,

    /// Pre-execution step results with screenshots
    pub pre_execution: PreExecutionData,

    /// Logs captured during this iteration (filtered by relevance)
    pub logs: IterationLogs,

    /// Findings from previous iterations
    pub previous_findings: Vec<PreviousFinding>,

    /// Summary of previous AI output (last N chars)
    pub previous_output_summary: Option<String>,
}

/// Pre-execution automation results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreExecutionData {
    /// Individual step results with linked screenshots
    pub steps: Vec<StepResultWithScreenshot>,
    /// Overall success/failure
    pub success: bool,
    /// Total steps
    pub total_steps: usize,
    /// Successful steps count
    pub successful_steps: usize,
    /// Failed steps count
    pub failed_steps: usize,
    /// Total duration in milliseconds
    pub total_duration_ms: u64,
}

/// A step result with its associated screenshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResultWithScreenshot {
    /// Step index (0-based)
    pub step_index: usize,
    /// Step type (workflow, state, action, screenshot, playwright)
    pub step_type: String,
    /// Step name for display
    pub step_name: String,
    /// Whether the step succeeded
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Screenshot taken AFTER this step completed
    pub screenshot_after: Option<ScreenshotRef>,
    /// Step configuration parameters (for AI visibility)
    pub config: StepConfigSummary,
}

/// Summary of step configuration parameters (for AI visibility)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepConfigSummary {
    /// For action steps: "click", "double_click", "right_click"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_type: Option<String>,
    /// Target image ID for action steps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_image_id: Option<String>,
    /// Target image name for display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_image_name: Option<String>,
    /// Monitor index (0 = primary)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_index: Option<i32>,
    /// Delay before screenshot in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_delay: Option<u32>,
    /// Timeout for this step in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    /// Playwright script ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playwright_script_id: Option<String>,
    /// Initial state IDs for workflow steps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_state_ids: Option<Vec<String>>,
}

/// Reference to a screenshot with context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRef {
    /// Path to the screenshot file
    pub path: String,
    /// Human-readable description
    pub description: String,
}

/// Logs captured during this iteration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IterationLogs {
    /// User-defined application logs (always included if configured)
    /// Key: source name, Value: captured log data
    pub application: HashMap<String, LogCapture>,

    /// Runner GUI automation logs (only if workflow has GUI steps)
    pub gui_automation: Option<GuiAutomationLogs>,

    /// Playwright test results (only if workflow has playwright steps)
    pub playwright: Option<PlaywrightLogs>,

    /// Input validation data (only if "Capture Input for Validation" is enabled)
    pub input_validation: Option<InputValidationLogs>,
}

/// Captured log content with extracted errors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogCapture {
    /// Source name (e.g., "Backend", "Frontend")
    pub source_name: String,
    /// Path to the log file
    pub source_path: String,
    /// Raw log content captured during iteration
    pub content: String,
    /// Extracted error messages (for highlighting)
    pub errors: Vec<String>,
    /// Extracted warning messages
    pub warnings: Vec<String>,
    /// Number of lines captured
    pub line_count: usize,
}

/// GUI automation logs (actions + image recognition)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiAutomationLogs {
    /// Action/workflow execution events
    pub actions: Vec<ActionEvent>,
    /// Image recognition/template matching events
    pub image_recognition: Vec<ImageRecognitionEvent>,
}

/// An action execution event from runner-actions.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEvent {
    pub timestamp: String,
    pub event_type: String,
    pub step_name: Option<String>,
    pub success: Option<bool>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    /// Raw JSON for additional details
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// An image recognition event from runner-image-recognition.jsonl
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecognitionEvent {
    pub timestamp: String,
    pub template_name: String,
    pub matched: bool,
    pub confidence: f64,
    pub threshold: f64,
    pub location: Option<ImageLocation>,
    pub screenshot_path: Option<String>,
}

/// Location of a matched/expected image
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageLocation {
    pub x: i32,
    pub y: i32,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// Playwright test results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightLogs {
    /// Overall pass/fail
    pub passed: bool,
    /// Individual spec results
    pub specs: Vec<PlaywrightSpec>,
    /// Console output from tests
    pub console_output: Option<String>,
    /// Screenshot paths from failures
    pub failure_screenshots: Vec<String>,
}

/// A single Playwright test spec result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightSpec {
    pub name: String,
    pub passed: bool,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// Input validation data - compares reported vs actual click positions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputValidationLogs {
    /// Whether input capture was enabled during this iteration
    pub enabled: bool,
    /// Session ID for the input capture
    pub session_id: String,
    /// Path to the raw input events JSONL file
    pub events_file: Option<String>,
    /// Click events with position comparison
    pub click_comparisons: Vec<ClickComparison>,
    /// Summary of discrepancies
    pub summary: InputValidationSummary,
}

/// Comparison of a reported click position vs actual captured position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickComparison {
    /// Timestamp of the click
    pub timestamp: String,
    /// Step name that triggered this click
    pub step_name: Option<String>,
    /// Template/image name being clicked
    pub target_name: Option<String>,
    /// Where the automation reported it would click
    pub reported_position: ClickPosition,
    /// Where the actual mouse event occurred (from input capture)
    pub actual_position: Option<ClickPosition>,
    /// Distance between reported and actual in pixels
    pub discrepancy_pixels: Option<f64>,
    /// Whether the discrepancy is significant (> threshold)
    pub is_significant: bool,
}

/// A click position with coordinates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickPosition {
    pub x: i32,
    pub y: i32,
    /// Monitor index if known
    pub monitor: Option<i32>,
}

/// Summary of input validation results
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputValidationSummary {
    /// Total clicks that occurred
    pub total_clicks: usize,
    /// Clicks with captured actual positions
    pub clicks_captured: usize,
    /// Clicks with significant discrepancies
    pub significant_discrepancies: usize,
    /// Maximum discrepancy in pixels
    pub max_discrepancy_pixels: Option<f64>,
    /// Average discrepancy in pixels
    pub avg_discrepancy_pixels: Option<f64>,
}

/// A finding from a previous iteration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviousFinding {
    pub category: String,
    pub severity: String,
    pub title: String,
    pub status: String,
    pub iteration_detected: u32,
    pub resolution: Option<String>,
}

// ============================================================================
// Log Relevance Filtering
// ============================================================================

/// Which log sources are relevant for this workflow
#[derive(Debug, Clone, Default)]
pub struct RelevantLogSources {
    /// Include runner GUI automation logs (actions + image recognition)
    pub gui_automation: bool,
    /// Include Playwright test logs
    pub playwright: bool,
    /// User-defined application logs (always true if configured)
    pub application: bool,
}

impl RelevantLogSources {
    /// Determine which logs are relevant based on the step types in the workflow
    pub fn from_steps(steps: &[ExecutionStepConfig]) -> Self {
        let has_gui_automation = steps
            .iter()
            .any(|s| matches!(s.step_type.as_str(), "workflow" | "state" | "action"));
        let has_playwright = steps.iter().any(|s| s.step_type == "playwright");

        Self {
            gui_automation: has_gui_automation,
            playwright: has_playwright,
            application: true, // Always include user-defined sources
        }
    }

    /// Log what's relevant for debugging
    pub fn log_relevance(&self) {
        info!(
            "Log relevance: GUI automation={}, Playwright={}, Application={}",
            self.gui_automation, self.playwright, self.application
        );
    }
}

// ============================================================================
// Screenshot Naming
// ============================================================================

/// Generate a screenshot filename that clearly indicates it was taken after a step
pub fn screenshot_filename(iteration: u32, step_index: usize, step_name: &str) -> String {
    // Sanitize step name for filename
    let safe_name: String = step_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe_name = safe_name.trim_matches('-');

    format!(
        "iter-{}-step-{}-{}-after.png",
        iteration,
        step_index + 1,
        safe_name
    )
}

/// Get the directory for iteration screenshots
pub fn iteration_screenshots_dir(task_run_id: &str) -> PathBuf {
    get_dev_logs_dir()
        .join("iterations")
        .join(task_run_id)
        .join("screenshots")
}

/// Get the .dev-logs directory
fn get_dev_logs_dir() -> PathBuf {
    // Use the same path as file_logger.rs
    PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs")
}

// ============================================================================
// Bundle Creation
// ============================================================================

impl IterationBundle {
    /// Create a new iteration bundle from execution results and captured data
    pub fn new(
        iteration: u32,
        task_run_id: String,
        timestamp_start: String,
        timestamp_end: String,
        execution_result: &ExecutionResult,
        _relevant_logs: &RelevantLogSources,
    ) -> Self {
        // Convert step results to include screenshot references
        let steps: Vec<StepResultWithScreenshot> = execution_result
            .steps
            .iter()
            .map(|step| {
                let screenshot_after = step.screenshot_path.as_ref().map(|path| ScreenshotRef {
                    path: path.clone(),
                    description: format!("State after completing '{}'", step.step_name),
                });

                StepResultWithScreenshot {
                    step_index: step.step_index,
                    step_type: step.step_type.clone(),
                    step_name: step.step_name.clone(),
                    success: step.success,
                    error: step.error.clone(),
                    duration_ms: step.duration_ms,
                    screenshot_after,
                    config: StepConfigSummary {
                        action_type: step.config.action_type.clone(),
                        target_image_id: step.config.target_image_id.clone(),
                        target_image_name: step.config.target_image_name.clone(),
                        monitor_index: step.config.monitor_index,
                        screenshot_delay: step.config.screenshot_delay,
                        timeout_seconds: step.config.timeout_seconds,
                        playwright_script_id: step.config.playwright_script_id.clone(),
                        initial_state_ids: step.config.initial_state_ids.clone(),
                    },
                }
            })
            .collect();

        let pre_execution = PreExecutionData {
            steps,
            success: execution_result.success,
            total_steps: execution_result.total_steps,
            successful_steps: execution_result.successful_steps,
            failed_steps: execution_result.failed_steps,
            total_duration_ms: execution_result.total_duration_ms,
        };

        // Initialize logs (will be populated by capture methods)
        let logs = IterationLogs::default();

        Self {
            iteration,
            task_run_id,
            timestamp_start,
            timestamp_end,
            pre_execution,
            logs,
            previous_findings: Vec::new(),
            previous_output_summary: None,
        }
    }

    /// Add application logs from captured sources
    pub fn add_application_logs(
        &mut self,
        captured: &HashMap<String, String>,
        sources: &[LogSourceConfig],
    ) {
        for source in sources {
            if let Some(content) = captured.get(&source.name) {
                if !content.trim().is_empty() {
                    let (errors, warnings) = extract_errors_and_warnings(content);
                    let line_count = content.lines().count();

                    self.logs.application.insert(
                        source.name.clone(),
                        LogCapture {
                            source_name: source.name.clone(),
                            source_path: source.path.clone(),
                            content: content.clone(),
                            errors,
                            warnings,
                            line_count,
                        },
                    );
                }
            }
        }
    }

    /// Add GUI automation logs (if relevant)
    pub fn add_gui_automation_logs(
        &mut self,
        actions: Vec<ActionEvent>,
        image_recognition: Vec<ImageRecognitionEvent>,
    ) {
        self.logs.gui_automation = Some(GuiAutomationLogs {
            actions,
            image_recognition,
        });
    }

    /// Add Playwright logs (if relevant)
    pub fn add_playwright_logs(&mut self, playwright: PlaywrightLogs) {
        self.logs.playwright = Some(playwright);
    }

    /// Add input validation logs (if "Capture Input for Validation" was enabled)
    pub fn add_input_validation_logs(&mut self, input_validation: InputValidationLogs) {
        self.logs.input_validation = Some(input_validation);
    }

    /// Add previous findings for context
    pub fn add_previous_findings(&mut self, findings: Vec<PreviousFinding>) {
        self.previous_findings = findings;
    }

    /// Add previous output summary
    pub fn add_previous_output(&mut self, output: String, max_chars: usize) {
        if output.len() > max_chars {
            self.previous_output_summary = Some(output[output.len() - max_chars..].to_string());
        } else {
            self.previous_output_summary = Some(output);
        }
    }
}

/// A captured input event from the Python input capture service
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapturedInputEvent {
    timestamp: f64,
    event_type: String,
    x: Option<i32>,
    y: Option<i32>,
    #[serde(default)]
    button: Option<String>,
}

/// Read captured input events from the .dev-logs/input_events/ directory
fn read_captured_input_events() -> Vec<CapturedInputEvent> {
    let dev_logs_dir = std::path::Path::new(".dev-logs/input_events");
    if !dev_logs_dir.exists() {
        return Vec::new();
    }

    let mut all_events = Vec::new();

    // Find the most recent events file
    if let Ok(entries) = std::fs::read_dir(dev_logs_dir) {
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
            .collect();

        // Sort by modification time (most recent first)
        files.sort_by(|a, b| {
            b.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .cmp(
                    &a.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                )
        });

        // Read the most recent file
        if let Some(entry) = files.first() {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                for line in content.lines() {
                    if let Ok(event) = serde_json::from_str::<CapturedInputEvent>(line) {
                        // Only include mouse_click events
                        if event.event_type == "mouse_click" {
                            all_events.push(event);
                        }
                    }
                }
            }
        }
    }

    all_events
}

/// Match captured clicks to reported clicks by timestamp proximity
fn match_captured_to_reported(
    reported_timestamp: &str,
    captured_events: &[CapturedInputEvent],
    tolerance_seconds: f64,
) -> Option<(CapturedInputEvent, f64)> {
    // Parse reported timestamp (ISO 8601 format)
    let reported_time = chrono::DateTime::parse_from_rfc3339(reported_timestamp)
        .or_else(|_| chrono::DateTime::parse_from_str(reported_timestamp, "%Y-%m-%dT%H:%M:%S%.fZ"))
        .map(|dt| dt.timestamp() as f64 + dt.timestamp_subsec_millis() as f64 / 1000.0)
        .ok()?;

    // Find the closest captured event within tolerance
    let mut best_match: Option<(CapturedInputEvent, f64)> = None;
    for event in captured_events {
        let time_diff = (event.timestamp - reported_time).abs();
        if time_diff <= tolerance_seconds {
            if best_match.is_none() || time_diff < best_match.as_ref().unwrap().1 {
                best_match = Some((event.clone(), time_diff));
            }
        }
    }

    best_match
}

/// Calculate pixel discrepancy between two positions
fn calculate_discrepancy(reported: &ClickPosition, actual: &ClickPosition) -> f64 {
    let dx = (actual.x - reported.x) as f64;
    let dy = (actual.y - reported.y) as f64;
    (dx * dx + dy * dy).sqrt()
}

/// Collect input validation data from action events
///
/// This extracts click positions from action events, reads captured input events
/// from the Python input capture service, matches them by timestamp, and computes
/// discrepancies between reported and actual click positions.
pub fn collect_input_validation_from_actions(
    action_events: &[ActionEvent],
    session_id: &str,
) -> InputValidationLogs {
    // Read captured input events from the most recent file
    let captured_events = read_captured_input_events();
    let events_file = if !captured_events.is_empty() {
        Some(".dev-logs/input_events/".to_string())
    } else {
        None
    };

    let mut click_comparisons = Vec::new();
    let mut clicks_captured = 0;
    let mut significant_discrepancies = 0;
    let mut max_discrepancy: Option<f64> = None;
    let mut total_discrepancy = 0.0;

    // Threshold for significant discrepancy (5 pixels)
    const SIGNIFICANT_THRESHOLD: f64 = 5.0;
    // Tolerance for timestamp matching (2 seconds)
    const TIMESTAMP_TOLERANCE: f64 = 2.0;

    for event in action_events {
        // Only process click actions (check event_type field)
        let event_type = event.event_type.to_lowercase();
        if !event_type.contains("click") && !event_type.contains("action") {
            continue;
        }

        // Also check extra fields for action_type
        let action_type = event
            .extra
            .get("action_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !action_type.is_empty() && !action_type.to_lowercase().contains("click") {
            continue;
        }

        // Extract reported position from extra fields
        let x = event
            .extra
            .get("x")
            .and_then(|v| v.as_i64())
            .map(|x| x as i32);
        let y = event
            .extra
            .get("y")
            .and_then(|v| v.as_i64())
            .map(|y| y as i32);
        let monitor = event
            .extra
            .get("monitor")
            .and_then(|v| v.as_i64())
            .map(|m| m as i32);
        let target_name = event
            .extra
            .get("target_name")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let (Some(x_val), Some(y_val)) = (x, y) {
            let reported_position = ClickPosition {
                x: x_val,
                y: y_val,
                monitor,
            };

            // Try to match with captured events
            let (actual_position, discrepancy_pixels, is_significant) = if let Some((
                captured,
                _time_diff,
            )) =
                match_captured_to_reported(&event.timestamp, &captured_events, TIMESTAMP_TOLERANCE)
            {
                if let (Some(cx), Some(cy)) = (captured.x, captured.y) {
                    let actual = ClickPosition {
                        x: cx,
                        y: cy,
                        monitor: None, // Captured events don't track monitor
                    };
                    let discrepancy = calculate_discrepancy(&reported_position, &actual);
                    let is_sig = discrepancy > SIGNIFICANT_THRESHOLD;

                    clicks_captured += 1;
                    total_discrepancy += discrepancy;
                    if is_sig {
                        significant_discrepancies += 1;
                    }
                    if max_discrepancy.is_none() || discrepancy > max_discrepancy.unwrap() {
                        max_discrepancy = Some(discrepancy);
                    }

                    (Some(actual), Some(discrepancy), is_sig)
                } else {
                    (None, None, false)
                }
            } else {
                (None, None, false)
            };

            click_comparisons.push(ClickComparison {
                timestamp: event.timestamp.clone(),
                step_name: event.step_name.clone(),
                target_name,
                reported_position,
                actual_position,
                discrepancy_pixels,
                is_significant,
            });
        }
    }

    let total_clicks = click_comparisons.len();
    let avg_discrepancy = if clicks_captured > 0 {
        Some(total_discrepancy / clicks_captured as f64)
    } else {
        None
    };

    InputValidationLogs {
        enabled: true,
        session_id: session_id.to_string(),
        events_file,
        click_comparisons,
        summary: InputValidationSummary {
            total_clicks,
            clicks_captured,
            significant_discrepancies,
            max_discrepancy_pixels: max_discrepancy,
            avg_discrepancy_pixels: avg_discrepancy,
        },
    }
}

// ============================================================================
// Log Parsing Helpers
// ============================================================================

/// Extract error and warning messages from log content
fn extract_errors_and_warnings(content: &str) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for line in content.lines() {
        let lower = line.to_lowercase();

        // Check for errors
        if lower.contains("error")
            || lower.contains("exception")
            || lower.contains("traceback")
            || lower.contains("failed")
            || lower.contains("panic")
        {
            errors.push(line.to_string());
        }
        // Check for warnings
        else if lower.contains("warning") || lower.contains("warn") {
            warnings.push(line.to_string());
        }
    }

    // Limit to avoid overwhelming
    errors.truncate(20);
    warnings.truncate(10);

    (errors, warnings)
}

/// Parse runner-actions.jsonl content into ActionEvent list
pub fn parse_action_events(content: &str) -> Vec<ActionEvent> {
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<ActionEvent>(line).ok())
        .collect()
}

/// Parse runner-image-recognition.jsonl content into ImageRecognitionEvent list
pub fn parse_image_recognition_events(content: &str) -> Vec<ImageRecognitionEvent> {
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<ImageRecognitionEvent>(line).ok())
        .collect()
}

// ============================================================================
// Markdown Rendering
// ============================================================================

impl IterationBundle {
    /// Render the bundle as markdown for inclusion in AI prompt
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        // Header
        md.push_str(&format!("## Iteration {} Data Bundle\n\n", self.iteration));
        md.push_str(&format!("**Task Run:** {}\n", self.task_run_id));
        md.push_str(&format!(
            "**Time:** {} to {}\n\n",
            self.timestamp_start, self.timestamp_end
        ));

        // Pre-execution results
        md.push_str("### Pre-Execution Results\n\n");
        self.render_pre_execution(&mut md);

        // Application logs (if any)
        if !self.logs.application.is_empty() {
            md.push_str("---\n\n### Application Logs (This Iteration Only)\n\n");
            self.render_application_logs(&mut md);
        }

        // GUI automation logs (if relevant and present)
        if let Some(ref gui) = self.logs.gui_automation {
            md.push_str("---\n\n### Runner GUI Automation Logs\n\n");
            self.render_gui_automation_logs(&mut md, gui);
        }

        // Playwright logs (if relevant and present)
        if let Some(ref playwright) = self.logs.playwright {
            md.push_str("---\n\n### Playwright Test Results\n\n");
            self.render_playwright_logs(&mut md, playwright);
        }

        // Input validation logs (if enabled)
        if let Some(ref input_validation) = self.logs.input_validation {
            md.push_str("---\n\n### Input Validation (Click Position Comparison)\n\n");
            self.render_input_validation_logs(&mut md, input_validation);
        }

        // Previous findings
        if !self.previous_findings.is_empty() {
            md.push_str("---\n\n### Previous Iteration Findings\n\n");
            self.render_previous_findings(&mut md);
        }

        // Previous output
        if let Some(ref output) = self.previous_output_summary {
            md.push_str("---\n\n### Previous AI Output (Summary)\n\n");
            md.push_str("```\n");
            md.push_str(output);
            md.push_str("\n```\n\n");
        }

        md
    }

    fn render_pre_execution(&self, md: &mut String) {
        // Summary table
        md.push_str("| # | Step | Type | Result | Duration |\n");
        md.push_str("|---|------|------|--------|----------|\n");

        for step in &self.pre_execution.steps {
            let result = if step.success {
                "✅ Success".to_string()
            } else {
                "❌ Failed".to_string()
            };

            md.push_str(&format!(
                "| {} | {} | {} | {} | {}ms |\n",
                step.step_index + 1,
                step.step_name,
                step.step_type,
                result,
                step.duration_ms
            ));
        }

        md.push('\n');

        // Step details (config, errors, screenshots)
        for step in &self.pre_execution.steps {
            // Step configuration details (non-empty fields only)
            let config_parts = self.format_step_config(&step.config, &step.step_type);
            if !config_parts.is_empty() {
                md.push_str(&format!(
                    "**Step {} Config:** {}\n\n",
                    step.step_index + 1,
                    config_parts.join(", ")
                ));
            }

            if let Some(ref error) = step.error {
                md.push_str(&format!(
                    "**Step {} Error:** {}\n\n",
                    step.step_index + 1,
                    error
                ));
            }

            if let Some(ref screenshot) = step.screenshot_after {
                md.push_str(&format!(
                    "📷 **Step {} Screenshot:** `{}` - {}\n\n",
                    step.step_index + 1,
                    screenshot.path,
                    screenshot.description
                ));
            }
        }

        // Overall summary
        md.push_str(&format!(
            "**Summary:** {} of {} steps succeeded ({}ms total)\n\n",
            self.pre_execution.successful_steps,
            self.pre_execution.total_steps,
            self.pre_execution.total_duration_ms
        ));
    }

    /// Format step configuration as a list of key=value pairs
    fn format_step_config(&self, config: &StepConfigSummary, step_type: &str) -> Vec<String> {
        let mut parts = Vec::new();

        // Only include relevant fields based on step type
        match step_type {
            "action" => {
                if let Some(ref action_type) = config.action_type {
                    parts.push(format!("action={}", action_type));
                }
                if let Some(ref target) = config.target_image_name {
                    parts.push(format!("target=\"{}\"", target));
                } else if let Some(ref target_id) = config.target_image_id {
                    parts.push(format!("target_id={}", target_id));
                }
            }
            "workflow" | "state" => {
                if let Some(ref initial_states) = config.initial_state_ids {
                    if !initial_states.is_empty() {
                        parts.push(format!("initial_states=[{}]", initial_states.join(", ")));
                    }
                }
            }
            "playwright" => {
                if let Some(ref script_id) = config.playwright_script_id {
                    parts.push(format!("script_id={}", script_id));
                }
            }
            _ => {}
        }

        // Common fields for all step types
        if let Some(monitor) = config.monitor_index {
            parts.push(format!("monitor={}", monitor));
        }
        if let Some(delay) = config.screenshot_delay {
            parts.push(format!("screenshot_delay={}s", delay));
        }
        if let Some(timeout) = config.timeout_seconds {
            parts.push(format!("timeout={}s", timeout));
        }

        parts
    }

    fn render_application_logs(&self, md: &mut String) {
        for (name, log) in &self.logs.application {
            md.push_str(&format!("#### {}\n\n", name));
            md.push_str(&format!("**Source:** `{}`\n\n", log.source_path));

            // Highlight errors first
            if !log.errors.is_empty() {
                md.push_str("**Errors detected:**\n```\n");
                for error in &log.errors {
                    md.push_str(error);
                    md.push('\n');
                }
                md.push_str("```\n\n");
            }

            // Then warnings
            if !log.warnings.is_empty() {
                md.push_str("**Warnings:**\n```\n");
                for warning in &log.warnings {
                    md.push_str(warning);
                    md.push('\n');
                }
                md.push_str("```\n\n");
            }

            // Full log (limited)
            let lines: Vec<&str> = log.content.lines().collect();
            let display_lines = if lines.len() > 50 {
                md.push_str(&format!(
                    "**Log ({} lines, showing last 50):**\n",
                    log.line_count
                ));
                &lines[lines.len() - 50..]
            } else {
                md.push_str(&format!("**Log ({} lines):**\n", log.line_count));
                &lines[..]
            };

            md.push_str("```\n");
            for line in display_lines {
                md.push_str(line);
                md.push('\n');
            }
            md.push_str("```\n\n");
        }
    }

    fn render_gui_automation_logs(&self, md: &mut String, gui: &GuiAutomationLogs) {
        // Image recognition summary
        if !gui.image_recognition.is_empty() {
            md.push_str("#### Image Recognition\n\n");
            md.push_str("| Template | Matched | Confidence | Threshold |\n");
            md.push_str("|----------|---------|------------|----------|\n");

            for event in &gui.image_recognition {
                let matched = if event.matched { "✅ Yes" } else { "❌ No" };
                md.push_str(&format!(
                    "| {} | {} | {:.1}% | {:.1}% |\n",
                    event.template_name,
                    matched,
                    event.confidence * 100.0,
                    event.threshold * 100.0
                ));
            }
            md.push('\n');

            // Show failed matches with more detail
            for event in gui.image_recognition.iter().filter(|e| !e.matched) {
                md.push_str(&format!(
                    "**Failed match `{}`:** confidence {:.1}% < threshold {:.1}%",
                    event.template_name,
                    event.confidence * 100.0,
                    event.threshold * 100.0
                ));
                if let Some(ref path) = event.screenshot_path {
                    md.push_str(&format!(" (see `{}`)", path));
                }
                md.push_str("\n\n");
            }
        }

        // Action events timeline
        if !gui.actions.is_empty() {
            md.push_str("#### Action Timeline\n\n");
            md.push_str("```json\n");
            for event in &gui.actions {
                if let Ok(json) = serde_json::to_string(event) {
                    md.push_str(&json);
                    md.push('\n');
                }
            }
            md.push_str("```\n\n");
        }
    }

    fn render_playwright_logs(&self, md: &mut String, playwright: &PlaywrightLogs) {
        let status = if playwright.passed {
            "✅ PASSED"
        } else {
            "❌ FAILED"
        };
        md.push_str(&format!("**Overall:** {}\n\n", status));

        if !playwright.specs.is_empty() {
            md.push_str("| Test | Result | Duration |\n");
            md.push_str("|------|--------|----------|\n");

            for spec in &playwright.specs {
                let result = if spec.passed { "✅ Pass" } else { "❌ Fail" };
                md.push_str(&format!(
                    "| {} | {} | {}ms |\n",
                    spec.name, result, spec.duration_ms
                ));
            }
            md.push('\n');

            // Show errors for failed specs
            for spec in playwright.specs.iter().filter(|s| !s.passed) {
                if let Some(ref error) = spec.error {
                    md.push_str(&format!(
                        "**`{}` Error:**\n```\n{}\n```\n\n",
                        spec.name, error
                    ));
                }
            }
        }

        // Failure screenshots
        if !playwright.failure_screenshots.is_empty() {
            md.push_str("**Failure Screenshots:**\n");
            for path in &playwright.failure_screenshots {
                md.push_str(&format!("- `{}`\n", path));
            }
            md.push('\n');
        }

        // Console output
        if let Some(ref console) = playwright.console_output {
            if !console.is_empty() {
                md.push_str("**Console Output:**\n```\n");
                md.push_str(console);
                md.push_str("\n```\n\n");
            }
        }
    }

    fn render_previous_findings(&self, md: &mut String) {
        for finding in &self.previous_findings {
            let status_icon = match finding.status.as_str() {
                "resolved" => "✅",
                "in_progress" => "🔄",
                _ => "📋",
            };

            md.push_str(&format!(
                "- {} [{}:{}] **{}** (Iteration {})",
                status_icon,
                finding.category,
                finding.severity,
                finding.title,
                finding.iteration_detected
            ));

            if let Some(ref resolution) = finding.resolution {
                md.push_str(&format!(" - {}", resolution));
            }

            md.push('\n');
        }
        md.push('\n');
    }

    fn render_input_validation_logs(&self, md: &mut String, validation: &InputValidationLogs) {
        // Summary
        md.push_str(&format!(
            "**Capture Session:** `{}`\n\n",
            validation.session_id
        ));

        let summary = &validation.summary;
        if summary.significant_discrepancies > 0 {
            md.push_str(&format!(
                "⚠️ **{} significant discrepancies detected** out of {} clicks\n\n",
                summary.significant_discrepancies, summary.total_clicks
            ));
        } else if summary.total_clicks > 0 {
            md.push_str(&format!(
                "✅ **No significant discrepancies** ({} clicks captured)\n\n",
                summary.clicks_captured
            ));
        }

        if let Some(max) = summary.max_discrepancy_pixels {
            md.push_str(&format!("- Max discrepancy: {:.1}px\n", max));
        }
        if let Some(avg) = summary.avg_discrepancy_pixels {
            md.push_str(&format!("- Avg discrepancy: {:.1}px\n", avg));
        }
        md.push('\n');

        // Click comparison table (only show if there are comparisons)
        if !validation.click_comparisons.is_empty() {
            md.push_str("#### Click Position Comparisons\n\n");
            md.push_str("| Step | Target | Reported (x,y) | Actual (x,y) | Δ px | Status |\n");
            md.push_str("|------|--------|----------------|--------------|------|--------|\n");

            for click in &validation.click_comparisons {
                let step = click.step_name.as_deref().unwrap_or("-");
                let target = click.target_name.as_deref().unwrap_or("-");
                let reported = format!(
                    "({}, {})",
                    click.reported_position.x, click.reported_position.y
                );
                let actual = click
                    .actual_position
                    .as_ref()
                    .map(|p| format!("({}, {})", p.x, p.y))
                    .unwrap_or_else(|| "N/A".to_string());
                let delta = click
                    .discrepancy_pixels
                    .map(|d| format!("{:.1}", d))
                    .unwrap_or_else(|| "-".to_string());
                let status = if click.is_significant {
                    "⚠️ OFFSET"
                } else if click.actual_position.is_some() {
                    "✅ OK"
                } else {
                    "❓ N/A"
                };

                md.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} |\n",
                    step, target, reported, actual, delta, status
                ));
            }
            md.push('\n');

            // Highlight significant discrepancies
            for click in validation
                .click_comparisons
                .iter()
                .filter(|c| c.is_significant)
            {
                if let Some(ref actual) = click.actual_position {
                    md.push_str(&format!(
                        "⚠️ **{}**: Reported ({}, {}) but clicked ({}, {}) - off by {:.1}px\n",
                        click.target_name.as_deref().unwrap_or("Click"),
                        click.reported_position.x,
                        click.reported_position.y,
                        actual.x,
                        actual.y,
                        click.discrepancy_pixels.unwrap_or(0.0)
                    ));
                }
            }
            md.push('\n');
        }

        // Link to raw events file
        if let Some(ref file) = validation.events_file {
            md.push_str(&format!("**Raw events file:** `{}`\n\n", file));
        }
    }
}

// ============================================================================
// Storage
// ============================================================================

impl IterationBundle {
    /// Save the bundle to a JSON file
    pub fn save_to_file(&self, base_dir: &PathBuf) -> Result<PathBuf, String> {
        let dir = base_dir.join("iterations").join(&self.task_run_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create iteration directory: {}", e))?;

        let path = dir.join(format!("iteration-{}.json", self.iteration));
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize bundle: {}", e))?;

        std::fs::write(&path, json).map_err(|e| format!("Failed to write bundle file: {}", e))?;

        info!("Saved iteration bundle to {:?}", path);
        Ok(path)
    }

    /// Load a bundle from a JSON file
    pub fn load_from_file(path: &PathBuf) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read bundle file: {}", e))?;

        serde_json::from_str(&content).map_err(|e| format!("Failed to parse bundle: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screenshot_filename() {
        assert_eq!(
            screenshot_filename(3, 0, "web-extraction"),
            "iter-3-step-1-web-extraction-after.png"
        );
        assert_eq!(
            screenshot_filename(1, 2, "Click: start button"),
            "iter-1-step-3-Click--start-button-after.png"
        );
    }

    #[test]
    fn test_relevant_log_sources() {
        // No steps = no GUI automation
        let empty: Vec<ExecutionStepConfig> = vec![];
        let relevant = RelevantLogSources::from_steps(&empty);
        assert!(!relevant.gui_automation);
        assert!(!relevant.playwright);
        assert!(relevant.application);
    }

    #[test]
    fn test_extract_errors() {
        let log = "INFO: Starting...\nERROR: Something failed\nWARN: Watch out\nDEBUG: Details";
        let (errors, warnings) = extract_errors_and_warnings(log);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("ERROR"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("WARN"));
    }
}
