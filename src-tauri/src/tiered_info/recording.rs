//! Run Recording Service for automatic workflow execution recording.
//!
//! This module provides a `RunRecorder` that tracks workflow executions
//! and automatically records them to the Tiered Information system when
//! the workflow completes (success, failure, or timeout).

use crate::tiered_info::{
    insert_run_details, update_statistics_after_run, Anomaly, AnomalySeverity, AnomalyType,
    RunDetails, RunStatus, TemplateMatchRecord, TransitionRecord,
};
use rusqlite::Connection;
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
pub struct RunRecorder {
    /// Unique run ID
    run_id: String,
    /// Config ID (from loaded configuration)
    config_id: String,
    /// Project ID for discovery push (optional)
    project_id: Option<String>,
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
            workflow_name,
            start_time: now,
            started_at,
            states_visited: Vec::new(),
            transitions_executed: Vec::new(),
            template_matches: HashMap::new(),
            anomalies: Vec::new(),
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
        }
    }

    /// Finish recording with success status and save to database.
    ///
    /// # Arguments
    /// * `conn` - Database connection
    ///
    /// # Returns
    /// The run ID on success
    pub fn finish_success(self, conn: &Connection) -> Result<String, String> {
        info!(
            "Finishing run {} with success (duration: {}ms)",
            self.run_id,
            self.start_time.elapsed().as_millis()
        );

        let run = self.build_run_details(RunStatus::Completed, None);
        self.save_run(conn, run)
    }

    /// Finish recording with failure status and save to database.
    ///
    /// # Arguments
    /// * `conn` - Database connection
    /// * `error` - Error message describing the failure
    ///
    /// # Returns
    /// The run ID on success
    pub fn finish_failure(self, conn: &Connection, error: &str) -> Result<String, String> {
        info!(
            "Finishing run {} with failure: {} (duration: {}ms)",
            self.run_id,
            error,
            self.start_time.elapsed().as_millis()
        );

        let run = self.build_run_details(RunStatus::Failed, Some(error));
        self.save_run(conn, run)
    }

    /// Finish recording with timeout status and save to database.
    ///
    /// # Arguments
    /// * `conn` - Database connection
    ///
    /// # Returns
    /// The run ID on success
    pub fn finish_timeout(self, conn: &Connection) -> Result<String, String> {
        info!(
            "Finishing run {} with timeout (duration: {}ms)",
            self.run_id,
            self.start_time.elapsed().as_millis()
        );

        let run = self.build_run_details(RunStatus::Timeout, Some("Execution timed out"));
        self.save_run(conn, run)
    }

    /// Finish recording with cancelled status and save to database.
    ///
    /// # Arguments
    /// * `conn` - Database connection
    ///
    /// # Returns
    /// The run ID on success
    pub fn finish_cancelled(self, conn: &Connection) -> Result<String, String> {
        info!(
            "Finishing run {} as cancelled (duration: {}ms)",
            self.run_id,
            self.start_time.elapsed().as_millis()
        );

        let run = self.build_run_details(RunStatus::Cancelled, None);
        self.save_run(conn, run)
    }

    /// Save the run to the database.
    fn save_run(self, conn: &Connection, run: RunDetails) -> Result<String, String> {
        let run_id = run.id.clone();

        // Insert run details
        insert_run_details(conn, &run)?;

        // Update statistics
        if let Err(e) = update_statistics_after_run(conn, &run) {
            warn!("Failed to update statistics after run: {}", e);
            // Don't fail the whole operation if stats update fails
        }

        info!(
            "Run {} saved successfully (states: {}, transitions: {}, anomalies: {})",
            run_id,
            run.states_visited.len(),
            run.transitions_executed.len(),
            run.anomalies.len()
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

    pub fn from_state(mut self, state: String) -> Self {
        self.from_state = Some(state);
        self
    }

    pub fn to_state(mut self, state: String) -> Self {
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
    use rusqlite::Connection;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS run_details (
                id TEXT PRIMARY KEY,
                config_id TEXT NOT NULL,
                workflow_name TEXT,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                duration_ms INTEGER,
                status TEXT NOT NULL,
                success BOOLEAN,
                error_type TEXT,
                error_message TEXT,
                actions_summary TEXT,
                states_visited TEXT,
                transitions_executed TEXT,
                template_matches TEXT,
                anomalies TEXT
            );

            CREATE TABLE IF NOT EXISTS config_statistics (
                id TEXT PRIMARY KEY,
                config_id TEXT NOT NULL UNIQUE,
                config_hash TEXT,
                total_runs INTEGER DEFAULT 0,
                successful_runs INTEGER DEFAULT 0,
                failed_runs INTEGER DEFAULT 0,
                timeout_runs INTEGER DEFAULT 0,
                avg_duration_ms INTEGER,
                recent_success_rate REAL,
                recent_avg_duration_ms INTEGER,
                transition_stats TEXT,
                template_stats TEXT,
                state_stats TEXT,
                error_patterns TEXT,
                flaky_transitions TEXT,
                flaky_templates TEXT,
                first_run_at TEXT,
                last_run_at TEXT,
                last_updated_at TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_run_recorder_success() {
        let conn = create_test_db();

        let mut recorder = RunRecorder::new("config-1".into(), Some("TestWorkflow".into()));

        // Record some events
        recorder.record_state_visit("StateA");
        recorder.record_state_visit("StateB");
        recorder.record_transition(TransitionRecord {
            from_state: "StateA".into(),
            to_state: "StateB".into(),
            action: "click".into(),
            success: true,
            duration_ms: 150,
            error: None,
        });
        recorder.record_template_match("button.png", 0.95, true);

        // Finish with success
        let run_id = recorder.finish_success(&conn).unwrap();
        assert!(!run_id.is_empty());

        // Verify saved data
        let run = crate::tiered_info::get_run_details(&conn, &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(run.success, Some(true));
        assert_eq!(run.states_visited.len(), 2);
        assert_eq!(run.transitions_executed.len(), 1);
        assert_eq!(run.template_matches.len(), 1);
    }

    #[test]
    fn test_run_recorder_failure() {
        let conn = create_test_db();

        let recorder = RunRecorder::new("config-1".into(), Some("FailingWorkflow".into()));

        let run_id = recorder.finish_failure(&conn, "Element not found").unwrap();

        let run = crate::tiered_info::get_run_details(&conn, &run_id)
            .unwrap()
            .unwrap();
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.success, Some(false));
        assert!(run.error_message.as_deref().unwrap().contains("not found"));
    }

    #[test]
    fn test_anomaly_detection_timing() {
        let conn = create_test_db();

        let mut expected_durations = HashMap::new();
        expected_durations.insert("StateA|StateB".to_string(), 100u64);

        let mut recorder =
            RunRecorder::new("config-1".into(), None).with_expected_durations(expected_durations);

        // Record a slow transition (3x expected)
        recorder.record_transition(TransitionRecord {
            from_state: "StateA".into(),
            to_state: "StateB".into(),
            action: "click".into(),
            success: true,
            duration_ms: 300, // 3x the expected 100ms
            error: None,
        });

        // Should have detected a timing anomaly
        assert_eq!(recorder.anomalies.len(), 1);
        assert_eq!(
            recorder.anomalies[0].anomaly_type,
            AnomalyType::TimingAnomaly
        );
    }

    #[test]
    fn test_anomaly_detection_confidence() {
        let conn = create_test_db();

        let mut recorder = RunRecorder::new("config-1".into(), None);

        // Record a low confidence match
        recorder.record_template_match("button.png", 0.4, true);

        // Should have detected a confidence anomaly
        assert_eq!(recorder.anomalies.len(), 1);
        assert_eq!(
            recorder.anomalies[0].anomaly_type,
            AnomalyType::ConfidenceAnomaly
        );
    }

    #[test]
    fn test_transition_builder() {
        let transition = TransitionBuilder::new()
            .from_state("A".into())
            .to_state("B".into())
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
