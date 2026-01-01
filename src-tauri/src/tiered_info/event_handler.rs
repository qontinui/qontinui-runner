//! Event handler for run recording.
//!
//! This module processes executor events and feeds them to the RunRecorder
//! for automatic run recording to the Tiered Information system.

use crate::database::CheckpointDb;
use crate::tiered_info::{get_config_statistics, RunRecorder, TransitionRecord};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Handles run recording events from the Python executor.
///
/// This handler is designed to be used with the executor's output processor.
/// It tracks the lifecycle of workflow executions and records them to the
/// Tiered Information system.
pub struct RunRecordingHandler {
    /// Current active run recorder (if a workflow is running)
    active_recorder: Arc<Mutex<Option<RunRecorder>>>,
    /// Database for storing run data
    db: Arc<CheckpointDb>,
    /// Current config ID (from loaded configuration)
    config_id: Arc<Mutex<Option<String>>>,
    /// Current project ID (optional, for discovery push)
    project_id: Arc<Mutex<Option<String>>>,
    /// Historical durations for anomaly detection
    expected_durations: Arc<Mutex<HashMap<String, u64>>>,
    /// Tracking for current transition timing
    transition_start_times: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl RunRecordingHandler {
    /// Create a new run recording handler.
    pub fn new(db: Arc<CheckpointDb>) -> Self {
        Self {
            active_recorder: Arc::new(Mutex::new(None)),
            db,
            config_id: Arc::new(Mutex::new(None)),
            project_id: Arc::new(Mutex::new(None)),
            expected_durations: Arc::new(Mutex::new(HashMap::new())),
            transition_start_times: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Set the current configuration context.
    pub async fn set_config(&self, config_id: String, project_id: Option<String>) {
        *self.config_id.lock().await = Some(config_id.clone());
        *self.project_id.lock().await = project_id;

        // Load expected durations from statistics
        let conn = match self.db.connection() {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to get DB connection for loading stats: {}", e);
                return;
            }
        };

        if let Ok(Some(stats)) = get_config_statistics(&conn, &config_id) {
            let durations: HashMap<String, u64> = stats
                .transition_stats
                .iter()
                .map(|(k, v)| (k.clone(), v.avg_duration_ms))
                .collect();

            *self.expected_durations.lock().await = durations;
            debug!(
                "Loaded {} expected durations from statistics",
                stats.transition_stats.len()
            );
        }
    }

    /// Handle a workflow started event.
    pub async fn on_workflow_started(&self, workflow_name: Option<String>) {
        let config_id = match self.config_id.lock().await.clone() {
            Some(id) => id,
            None => {
                warn!("Workflow started but no config_id set, using 'unknown'");
                "unknown".to_string()
            }
        };

        let project_id = self.project_id.lock().await.clone();
        let expected_durations = self.expected_durations.lock().await.clone();

        let mut recorder =
            RunRecorder::new(config_id, workflow_name).with_expected_durations(expected_durations);

        if let Some(pid) = project_id {
            recorder = recorder.with_project_id(pid);
        }

        info!("Started run recording: {}", recorder.run_id());

        *self.active_recorder.lock().await = Some(recorder);
    }

    /// Handle a state entered event.
    pub async fn on_state_entered(&self, state_id: &str) {
        if let Some(ref mut recorder) = *self.active_recorder.lock().await {
            recorder.record_state_visit(state_id);
        }
    }

    /// Handle a transition started event.
    pub async fn on_transition_started(&self, from_state: &str, to_state: &str) {
        let key = format!("{}|{}", from_state, to_state);
        self.transition_start_times
            .lock()
            .await
            .insert(key, std::time::Instant::now());
    }

    /// Handle a transition completed event.
    pub async fn on_transition_completed(
        &self,
        from_state: &str,
        to_state: &str,
        action: &str,
        success: bool,
        error: Option<String>,
    ) {
        let key = format!("{}|{}", from_state, to_state);

        // Calculate duration from stored start time
        let duration_ms = {
            let mut times = self.transition_start_times.lock().await;
            if let Some(start) = times.remove(&key) {
                start.elapsed().as_millis() as u64
            } else {
                0
            }
        };

        let transition = TransitionRecord {
            from_state: from_state.to_string(),
            to_state: to_state.to_string(),
            action: action.to_string(),
            success,
            duration_ms,
            error,
        };

        if let Some(ref mut recorder) = *self.active_recorder.lock().await {
            recorder.record_transition(transition);
        }
    }

    /// Handle a template match event.
    pub async fn on_template_match(&self, template: &str, confidence: f32, success: bool) {
        if let Some(ref mut recorder) = *self.active_recorder.lock().await {
            recorder.record_template_match(template, confidence, success);
        }
    }

    /// Handle an image recognition event from Python.
    pub async fn on_image_recognition(&self, data: &serde_json::Value) {
        // Extract relevant info from the image recognition data
        if let Some(matches) = data.get("matches").and_then(|m| m.as_array()) {
            for m in matches {
                let template = m
                    .get("template")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown");
                let confidence = m.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0) as f32;
                let success = m
                    .get("found")
                    .and_then(|f| f.as_bool())
                    .unwrap_or(confidence > 0.5);

                self.on_template_match(template, confidence, success).await;
            }
        }

        // Check for no matches found (missing element)
        if let Some(pattern) = data.get("pattern").and_then(|p| p.as_str()) {
            let found = data.get("found").and_then(|f| f.as_bool()).unwrap_or(true);

            if !found {
                if let Some(ref mut recorder) = *self.active_recorder.lock().await {
                    recorder.record_missing_element(pattern, "screen capture");
                }
            }
        }
    }

    /// Handle a tree event from the Python executor.
    pub async fn on_tree_event(&self, event_type: &str, node: &serde_json::Value) {
        match event_type {
            "workflow_started" => {
                let workflow_name = node.get("name").and_then(|n| n.as_str()).map(String::from);
                self.on_workflow_started(workflow_name).await;
            }
            "state_entered" => {
                if let Some(state_id) = node.get("state_id").and_then(|s| s.as_str()) {
                    self.on_state_entered(state_id).await;
                }
            }
            "transition_started" => {
                let from = node
                    .get("from_state")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let to = node.get("to_state").and_then(|s| s.as_str()).unwrap_or("");
                self.on_transition_started(from, to).await;
            }
            "transition_completed" | "transition_failed" => {
                let from = node
                    .get("from_state")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let to = node.get("to_state").and_then(|s| s.as_str()).unwrap_or("");
                let action = node.get("action").and_then(|a| a.as_str()).unwrap_or("");
                let success = event_type == "transition_completed";
                let error = node.get("error").and_then(|e| e.as_str()).map(String::from);
                self.on_transition_completed(from, to, action, success, error)
                    .await;
            }
            "action_completed" | "action_failed" => {
                // Track action for the actions summary
                // The transition recording already handles the action detail
            }
            "workflow_completed" | "workflow_failed" => {
                // These are handled by on_workflow_completed/on_workflow_failed
            }
            _ => {
                debug!("Unhandled tree event type: {}", event_type);
            }
        }
    }

    /// Handle workflow completed (success).
    pub async fn on_workflow_completed(&self) {
        if let Some(recorder) = self.active_recorder.lock().await.take() {
            let conn = match self.db.connection() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to get DB connection for saving run: {}", e);
                    return;
                }
            };

            match recorder.finish_success(&conn) {
                Ok(run_id) => {
                    info!("Run {} completed successfully", run_id);
                }
                Err(e) => {
                    error!("Failed to save run: {}", e);
                }
            }
        }
    }

    /// Handle workflow failed.
    pub async fn on_workflow_failed(&self, error: &str) {
        if let Some(recorder) = self.active_recorder.lock().await.take() {
            let conn = match self.db.connection() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to get DB connection for saving run: {}", e);
                    return;
                }
            };

            match recorder.finish_failure(&conn, error) {
                Ok(run_id) => {
                    info!("Run {} recorded as failed: {}", run_id, error);
                }
                Err(e) => {
                    error!("Failed to save run: {}", e);
                }
            }
        }
    }

    /// Handle workflow timeout.
    pub async fn on_workflow_timeout(&self) {
        if let Some(recorder) = self.active_recorder.lock().await.take() {
            let conn = match self.db.connection() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to get DB connection for saving run: {}", e);
                    return;
                }
            };

            match recorder.finish_timeout(&conn) {
                Ok(run_id) => {
                    info!("Run {} recorded as timeout", run_id);
                }
                Err(e) => {
                    error!("Failed to save run: {}", e);
                }
            }
        }
    }

    /// Handle execution completed event from Python.
    /// This is the main entry point called when the Python executor sends execution_completed.
    pub async fn on_execution_completed(&self, data: &serde_json::Value) {
        let success = data
            .get("success")
            .and_then(|s| s.as_bool())
            .unwrap_or(true);
        let error = data
            .get("reason")
            .and_then(|r| r.as_str())
            .or_else(|| data.get("error").and_then(|e| e.as_str()));

        if success {
            self.on_workflow_completed().await;
        } else if let Some(err) = error {
            if err.to_lowercase().contains("timeout") {
                self.on_workflow_timeout().await;
            } else {
                self.on_workflow_failed(err).await;
            }
        } else {
            self.on_workflow_failed("Unknown error").await;
        }
    }

    /// Check if a run is currently being recorded.
    pub async fn is_recording(&self) -> bool {
        self.active_recorder.lock().await.is_some()
    }

    /// Get the current run ID if recording.
    pub async fn current_run_id(&self) -> Option<String> {
        self.active_recorder
            .lock()
            .await
            .as_ref()
            .map(|r| r.run_id().to_string())
    }

    /// Cancel the current recording without saving.
    pub async fn cancel_recording(&self) {
        if let Some(recorder) = self.active_recorder.lock().await.take() {
            let conn = match self.db.connection() {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        "Failed to get DB connection for saving cancelled run: {}",
                        e
                    );
                    return;
                }
            };

            // Still save the run but as cancelled
            match recorder.finish_cancelled(&conn) {
                Ok(run_id) => {
                    info!("Run {} recorded as cancelled", run_id);
                }
                Err(e) => {
                    warn!("Failed to save cancelled run: {}", e);
                }
            }
        }
    }
}

/// Create a run recording handler from AppState.
///
/// This is a convenience function for integration with the executor.
pub fn create_run_recording_handler(db: Arc<CheckpointDb>) -> RunRecordingHandler {
    RunRecordingHandler::new(db)
}
