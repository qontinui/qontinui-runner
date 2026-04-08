//! Run Recording Service for automatic workflow execution recording.
//!
//! This module provides a `RunRecorder` that tracks workflow executions
//! and automatically records them to the Tiered Information system when
//! the workflow completes (success, failure, or timeout).
//!
//! When a `task_run_id` is set on the recorder, metrics are written to the
//! `task_run_automation` table as a child of the parent TaskRun.

#![allow(dead_code)]

use crate::tiered_info::{
    Anomaly, AnomalySeverity, AnomalyType, RunDetails, RunStatus, ScreenshotRecord,
    TemplateMatchRecord, TransitionRecord,
};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

/// Thresholds for anomaly detection.
#[derive(Debug, Clone)]
pub struct AnomalyThresholds {
    /// Multiplier for timing anomaly detection (e.g., 2.0 means 2x expected duration)
    pub timing_multiplier: f64,
    /// Minimum confidence threshold for templates
    pub min_confidence: f32,
    /// Warning confidence threshold (between min and good)
    pub warning_confidence: f32,
}

impl Default for AnomalyThresholds {
    fn default() -> Self {
        Self {
            timing_multiplier: 2.0,
            min_confidence: 0.5,
            warning_confidence: 0.7,
        }
    }
}

/// Records a workflow execution and accumulates events during the run.
///
/// Usage:
/// 1. Create a new recorder when workflow starts
/// 2. Call record_* methods as events occur
/// 3. Call finish_* when workflow completes
///
/// When `task_run_id` is set, the run data is written to the `task_run_automation`
/// table as a child of the parent TaskRun. Otherwise, it's written to the legacy
/// `run_details` table.
pub struct RunRecorder {
    /// Unique run ID
    run_id: String,
    /// Config ID (from loaded configuration)
    config_id: String,
    /// Project ID for discovery push (optional)
    project_id: Option<String>,
    /// Task run ID (optional, for unified task tracking)
    /// When set, data is written to task_run_automation table.
    task_run_id: Option<String>,
    /// Iteration number within the task run
    iteration_number: u32,
    /// Workflow name being executed
    workflow_name: Option<String>,
    /// When the run started
    start_time: Instant,
    /// ISO timestamp when started
    started_at: String,
    /// States visited during the run
    states_visited: Vec<String>,
    /// Transitions executed with timing
    transitions_executed: Vec<TransitionRecord>,
    /// Template match statistics
    template_matches: HashMap<String, TemplateMatchStats>,
    /// Anomalies detected
    anomalies: Vec<Anomaly>,
    /// Screenshots captured during the run
    screenshots_captured: Vec<ScreenshotRecord>,
    /// Expected transition durations from historical data
    expected_durations: HashMap<String, u64>,
    /// Thresholds for anomaly detection
    thresholds: AnomalyThresholds,
    /// Action counts
    action_counts: ActionCounts,
}

/// Internal tracking for template match statistics.
#[derive(Debug, Default, Clone)]
struct TemplateMatchStats {
    count: u32,
    total_confidence: f32,
    failures: u32,
}

/// Action execution counts.
#[derive(Debug, Default, Clone)]
struct ActionCounts {
    total: u32,
    success: u32,
    failed: u32,
    skipped: u32,
}

impl RunRecorder {
    /// Create a new run recorder.
    ///
    /// # Arguments
    /// * `config_id` - ID of the loaded configuration
    /// * `workflow_name` - Name of the workflow being executed
    pub fn new(config_id: String, workflow_name: Option<String>) -> Self {
        let run_id = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        let started_at = chrono::Utc::now().to_rfc3339();

        info!(
            "Starting run recording: run_id={}, config_id={}, workflow={:?}",
            run_id, config_id, workflow_name
        );

        Self {
            run_id,
            config_id,
            project_id: None,
            task_run_id: None,
            iteration_number: 1,
            workflow_name,
            start_time: now,
            started_at,
            states_visited: Vec::new(),
            transitions_executed: Vec::new(),
            template_matches: HashMap::new(),
            anomalies: Vec::new(),
            screenshots_captured: Vec::new(),
            expected_durations: HashMap::new(),
            thresholds: AnomalyThresholds::default(),
            action_counts: ActionCounts::default(),
        }
    }

    /// Set the project ID for discovery push.
    pub fn with_project_id(mut self, project_id: String) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Set the task run ID for unified task tracking.
    ///
    /// When set, the run data will be written to `task_run_automation` table
    /// as a child of the specified TaskRun, instead of the legacy `run_details` table.
    pub fn with_task_run(mut self, task_run_id: String, iteration_number: u32) -> Self {
        self.task_run_id = Some(task_run_id);
        self.iteration_number = iteration_number;
        self
    }

    /// Get the task run ID if set.
    pub fn task_run_id(&self) -> Option<&str> {
        self.task_run_id.as_deref()
    }

    /// Set expected durations from historical statistics.
    pub fn with_expected_durations(mut self, durations: HashMap<String, u64>) -> Self {
        self.expected_durations = durations;
        self
    }

    /// Set custom anomaly thresholds.
    pub fn with_thresholds(mut self, thresholds: AnomalyThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    /// Get the run ID.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Get the config ID.
    pub fn config_id(&self) -> &str {
        &self.config_id
    }

    /// Get the project ID.
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// Record a state visit.
    pub fn record_state_visit(&mut self, state_id: &str) {
        debug!("Recording state visit: {}", state_id);
        // Only add if not already the last visited state (avoid duplicates for re-entry)
        if self.states_visited.last().map(|s| s.as_str()) != Some(state_id) {
            self.states_visited.push(state_id.to_string());
        }
    }

    /// Record a transition execution.
    pub fn record_transition(&mut self, transition: TransitionRecord) {
        let key = format!("{}|{}", transition.from_state, transition.to_state);
        debug!(
            "Recording transition: {} -> {} ({}ms, success={})",
            transition.from_state, transition.to_state, transition.duration_ms, transition.success
        );

        // Check for timing anomaly
        if let Some(&expected_ms) = self.expected_durations.get(&key) {
            if transition.duration_ms as f64
                > expected_ms as f64 * self.thresholds.timing_multiplier
            {
                let anomaly = Anomaly::timing_anomaly(expected_ms, transition.duration_ms, &key);
                self.anomalies.push(anomaly);
            }
        }

        // Track action counts
        self.action_counts.total += 1;
        if transition.success {
            self.action_counts.success += 1;
        } else {
            self.action_counts.failed += 1;
            // Record unexpected failure if transition was expected to succeed
            if transition.error.is_some() {
                let anomaly = Anomaly::new(
                    AnomalyType::UnexpectedFailure,
                    format!(
                        "Transition {} failed: {}",
                        key,
                        transition.error.as_deref().unwrap_or("unknown error")
                    ),
                    AnomalySeverity::High,
                );
                self.anomalies.push(anomaly);
            }
        }

        self.transitions_executed.push(transition);
    }

    /// Record a template match result.
    #[tracing::instrument(
        name = "image.recognition.record",
        skip(self),
        fields(
            template_name = %template,
            confidence = %confidence,
            match_success = %success
        )
    )]
    pub fn record_template_match(&mut self, template: &str, confidence: f32, success: bool) {
        debug!(
            "Recording template match: {} (confidence={:.2}, success={})",
            template, confidence, success
        );

        let stats = self
            .template_matches
            .entry(template.to_string())
            .or_default();
        stats.count += 1;
        stats.total_confidence += confidence;
        if !success {
            stats.failures += 1;
        }

        // Check for confidence anomaly
        if confidence < self.thresholds.min_confidence {
            let anomaly =
                Anomaly::confidence_anomaly(template, confidence, self.thresholds.min_confidence);
            self.anomalies.push(anomaly);
        } else if confidence < self.thresholds.warning_confidence {
            let anomaly = Anomaly::new(
                AnomalyType::LowConfidence,
                format!(
                    "Template '{}' matched with marginal confidence {:.2}",
                    template, confidence
                ),
                AnomalySeverity::Low,
            );
            self.anomalies.push(anomaly);
        }
    }

    /// Record a custom anomaly.
    pub fn record_anomaly(&mut self, anomaly: Anomaly) {
        debug!("Recording anomaly: {:?}", anomaly.anomaly_type);
        self.anomalies.push(anomaly);
    }

    /// Record a missing element during the run.
    pub fn record_missing_element(&mut self, element: &str, context: &str) {
        let anomaly = Anomaly::missing_element(element, context);
        self.anomalies.push(anomaly);
    }

    /// Record a state mismatch.
    pub fn record_state_mismatch(&mut self, expected: &str, actual: &str) {
        let anomaly = Anomaly::state_mismatch(expected, actual);
        self.anomalies.push(anomaly);
    }

    /// Record an action being skipped.
    pub fn record_action_skipped(&mut self, reason: &str) {
        debug!("Recording skipped action: {}", reason);
        self.action_counts.skipped += 1;
    }

    /// Record a screenshot capture.
    ///
    /// # Arguments
    /// * `screenshot_type` - "standalone" or "post_action"
    /// * `file_path` - Path to the captured screenshot file
    /// * `monitor` - Monitor identifier (if specified)
    /// * `delay_seconds` - Delay before capture (for post-action screenshots)
    /// * `success` - Whether the capture was successful
    /// * `associated_action` - Name of the associated step/action (if any)
    /// * `error` - Error message if capture failed
    pub fn record_screenshot(
        &mut self,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<String>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        debug!(
            "Recording screenshot: type={}, path={}, success={}",
            screenshot_type, file_path, success
        );

        let record = ScreenshotRecord {
            screenshot_type: screenshot_type.to_string(),
            file_path: file_path.to_string(),
            monitor,
            delay_seconds,
            success,
            associated_action,
            captured_at: chrono::Utc::now().to_rfc3339(),
            error,
        };

        // Track as an action
        self.action_counts.total += 1;
        if success {
            self.action_counts.success += 1;
        } else {
            self.action_counts.failed += 1;
        }

        self.screenshots_captured.push(record);
    }

    /// Record a standalone screenshot capture.
    pub fn record_standalone_screenshot(
        &mut self,
        file_path: &str,
        monitor: Option<String>,
        success: bool,
        error: Option<String>,
    ) {
        self.record_screenshot("standalone", file_path, monitor, None, success, None, error);
    }

    /// Record a post-action screenshot capture (delayed screenshot after another action).
    pub fn record_post_action_screenshot(
        &mut self,
        file_path: &str,
        monitor: Option<String>,
        delay_seconds: u32,
        associated_action: &str,
        success: bool,
        error: Option<String>,
    ) {
        self.record_screenshot(
            "post_action",
            file_path,
            monitor,
            Some(delay_seconds),
            success,
            Some(associated_action.to_string()),
            error,
        );
    }

    /// Build the RunDetails from accumulated data.
    fn build_run_details(&self, status: RunStatus, error: Option<&str>) -> RunDetails {
        let duration_ms = self.start_time.elapsed().as_millis() as u64;
        let ended_at = chrono::Utc::now().to_rfc3339();

        let success = match status {
            RunStatus::Completed => Some(true),
            RunStatus::Failed | RunStatus::Timeout => Some(false),
            RunStatus::Cancelled => None,
            RunStatus::Running => None,
        };

        // Convert template stats to records
        let template_records: Vec<TemplateMatchRecord> = self
            .template_matches
            .iter()
            .map(|(template, stats)| TemplateMatchRecord {
                template: template.clone(),
                count: stats.count,
                avg_confidence: if stats.count > 0 {
                    stats.total_confidence / stats.count as f32
                } else {
                    0.0
                },
                failures: stats.failures,
            })
            .collect();

        // Build actions summary
        let actions_summary = if self.action_counts.total > 0 {
            Some(crate::tiered_info::ActionsSummary {
                total: self.action_counts.total,
                success: self.action_counts.success,
                failed: self.action_counts.failed,
                skipped: self.action_counts.skipped,
            })
        } else {
            None
        };

        // Extract error type from error message
        let (error_type, error_message) = if let Some(err) = error {
            // Try to extract error type from common patterns
            let error_type = if err.contains("timeout") || err.contains("Timeout") {
                Some("Timeout".to_string())
            } else if err.contains("not found") || err.contains("NotFound") {
                Some("ElementNotFound".to_string())
            } else if err.contains("failed") || err.contains("Failed") {
                Some("ExecutionFailed".to_string())
            } else {
                Some("Error".to_string())
            };
            (error_type, Some(err.to_string()))
        } else {
            (None, None)
        };

        RunDetails {
            id: self.run_id.clone(),
            config_id: self.config_id.clone(),
            workflow_name: self.workflow_name.clone(),
            started_at: self.started_at.clone(),
            ended_at: Some(ended_at),
            duration_ms: Some(duration_ms),
            status,
            success,
            error_type,
            error_message,
            actions_summary,
            states_visited: self.states_visited.clone(),
            transitions_executed: self.transitions_executed.clone(),
            template_matches: template_records,
            anomalies: self.anomalies.clone(),
            screenshots: self.screenshots_captured.clone(),
        }
    }

    /// Finish recording with success status and save to database.
    pub async fn finish_success_with_db(self) -> Result<String, String> {
        let run = self.build_run_details(RunStatus::Completed, None);
        self.persist_run(run).await
    }

    /// Finish recording with failure status and save to database.
    pub async fn finish_failure_with_db(self, error: &str) -> Result<String, String> {
        let run = self.build_run_details(RunStatus::Failed, Some(error));
        self.persist_run(run).await
    }

    /// Finish recording with timeout status and save to database.
    pub async fn finish_timeout_with_db(self) -> Result<String, String> {
        let run = self.build_run_details(RunStatus::Timeout, Some("Workflow execution timed out"));
        self.persist_run(run).await
    }

    /// Finish recording with cancelled status and save to database.
    pub async fn finish_cancelled_with_db(self) -> Result<String, String> {
        let run = self.build_run_details(RunStatus::Cancelled, None);
        self.persist_run(run).await
    }

    /// Persist a built RunDetails. If a task_run_id is set, writes a full
    /// row to `task_run_automation` via PG; otherwise logs stats only.
    async fn persist_run(self, run: RunDetails) -> Result<String, String> {
        let task_run_id = self.task_run_id.clone();
        let iteration_number = self.iteration_number;
        let run_id = run.id.clone();

        let Some(task_run_id) = task_run_id else {
            warn!(
                "Run {} does not have a task_run_id - logging stats only (no task_run_automation record)",
                run_id
            );
            info!(
                "Run {} stats: states={}, transitions={}, anomalies={}",
                run_id,
                run.states_visited.len(),
                run.transitions_executed.len(),
                run.anomalies.len()
            );
            return Ok(run_id);
        };

        let Some(pg) = crate::database::pg::PgDb::try_global() else {
            warn!(
                "Run {}: PG not initialized, cannot persist task_run_automation",
                run_id
            );
            return Err("PG not initialized".to_string());
        };

        // Map RunStatus to schema status string (matches existing
        // create/complete/fail_task_run_automation conventions).
        let automation_status = match run.status {
            RunStatus::Completed => "success",
            RunStatus::Failed => "failed",
            RunStatus::Timeout => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Running => "running",
        };

        // Override error_type for timeouts so it's distinguishable.
        let error_type = match run.status {
            RunStatus::Timeout => Some("Timeout".to_string()),
            _ => run.error_type.clone(),
        };

        let ended_at = run.ended_at.clone().unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let duration_ms = run.duration_ms.unwrap_or(0).min(i32::MAX as u64) as i32;

        // Serialize JSON-blob columns. Schema uses TEXT for these.
        let actions_summary = run
            .actions_summary
            .as_ref()
            .and_then(|a| serde_json::to_string(a).ok());
        let states_visited = serde_json::to_string(&run.states_visited).ok();
        let transitions_executed = serde_json::to_string(&run.transitions_executed).ok();
        let template_matches = serde_json::to_string(&run.template_matches).ok();
        let anomalies = serde_json::to_string(&run.anomalies).ok();

        if !run.screenshots.is_empty() {
            debug!(
                "Run {}: dropping {} screenshot records (no schema column)",
                run_id,
                run.screenshots.len()
            );
        }

        pg.save_task_run_automation(
            &run_id,
            &task_run_id,
            run.workflow_name.as_deref(),
            &run.started_at,
            &ended_at,
            duration_ms,
            automation_status,
            run.success,
            error_type.as_deref(),
            run.error_message.as_deref(),
            actions_summary.as_deref(),
            states_visited.as_deref(),
            transitions_executed.as_deref(),
            template_matches.as_deref(),
            anomalies.as_deref(),
            iteration_number,
        )
        .await?;

        info!(
            "Persisted run {} to task_run_automation (task_run_id={}, status={})",
            run_id, task_run_id, automation_status
        );

        Ok(run_id)
    }

}

/// Builder pattern for creating transitions from Python events.
#[derive(Debug, Default)]
pub struct TransitionBuilder {
    from_state: Option<String>,
    to_state: Option<String>,
    action: Option<String>,
    success: bool,
    duration_ms: u64,
    error: Option<String>,
}

impl TransitionBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn from_state(mut self, state: String) -> Self {
        self.from_state = Some(state);
        self
    }

    pub fn with_to_state(mut self, state: String) -> Self {
        self.to_state = Some(state);
        self
    }

    pub fn action(mut self, action: String) -> Self {
        self.action = Some(action);
        self
    }

    pub fn success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    pub fn duration_ms(mut self, duration: u64) -> Self {
        self.duration_ms = duration;
        self
    }

    pub fn error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    pub fn build(self) -> Option<TransitionRecord> {
        Some(TransitionRecord {
            from_state: self.from_state?,
            to_state: self.to_state?,
            action: self.action.unwrap_or_else(|| "unknown".to_string()),
            success: self.success,
            duration_ms: self.duration_ms,
            error: self.error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transition_builder() {
        let transition = TransitionBuilder::new()
            .from_state("A".into())
            .with_to_state("B".into())
            .action("click".into())
            .success(true)
            .duration_ms(100)
            .build()
            .unwrap();

        assert_eq!(transition.from_state, "A");
        assert_eq!(transition.to_state, "B");
        assert!(transition.success);
    }
}
