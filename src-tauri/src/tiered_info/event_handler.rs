//! Event handler for run recording.
//!
//! This module processes executor events and feeds them to the RunRecorder
//! for automatic run recording to the Tiered Information system.

#![allow(dead_code)]

use crate::database::{CheckpointDb, TriggerPoint};
use crate::safe_eprintln;
use crate::test_executor::{
    create_findings_for_failures, execute_tests_for_trigger, format_results_for_ai,
    TriggerTestsResult,
};
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
///
/// When a `task_run_id` is set, automation metrics are written to the
/// `task_run_automation` table as a child of the parent TaskRun.
/// Otherwise, they are written to the legacy `run_details` table.
pub struct RunRecordingHandler {
    /// Current active run recorder (if a workflow is running)
    active_recorder: Arc<Mutex<Option<RunRecorder>>>,
    /// Database for storing run data
    db: Arc<CheckpointDb>,
    /// Current config ID (from loaded configuration)
    config_id: Arc<Mutex<Option<String>>>,
    /// Current project ID (optional, for discovery push)
    project_id: Arc<Mutex<Option<String>>>,
    /// Current task run ID (optional, for unified task tracking)
    /// When set, automation metrics are written to task_run_automation table.
    task_run_id: Arc<Mutex<Option<String>>>,
    /// Current iteration number within the task run
    iteration_number: Arc<Mutex<u32>>,
    /// Historical durations for anomaly detection
    expected_durations: Arc<Mutex<HashMap<String, u64>>>,
    /// Tracking for current transition timing
    transition_start_times: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// Latest test results for AI context
    latest_test_results: Arc<Mutex<Option<TriggerTestsResult>>>,
    /// Current session number for task run findings
    session_number: Arc<Mutex<i32>>,
}

impl RunRecordingHandler {
    /// Create a new run recording handler.
    pub fn new(db: Arc<CheckpointDb>) -> Self {
        Self {
            active_recorder: Arc::new(Mutex::new(None)),
            db,
            config_id: Arc::new(Mutex::new(None)),
            project_id: Arc::new(Mutex::new(None)),
            task_run_id: Arc::new(Mutex::new(None)),
            iteration_number: Arc::new(Mutex::new(1)),
            expected_durations: Arc::new(Mutex::new(HashMap::new())),
            transition_start_times: Arc::new(Mutex::new(HashMap::new())),
            latest_test_results: Arc::new(Mutex::new(None)),
            session_number: Arc::new(Mutex::new(1)),
        }
    }

    /// Set the task run ID for unified task tracking.
    ///
    /// When set, automation metrics will be written to the `task_run_automation`
    /// table as a child of the specified TaskRun. This enables the unified
    /// TaskRun architecture where AI and automation are tracked together.
    ///
    /// Call this before starting a workflow to link automation to a task.
    pub async fn set_task_run(&self, task_run_id: String, iteration: u32, session_num: i32) {
        safe_eprintln!(
            "[RUN_RECORDING] set_task_run called: task_run_id={}, iteration={}, session={}",
            task_run_id,
            iteration,
            session_num
        );
        *self.task_run_id.lock().await = Some(task_run_id);
        *self.iteration_number.lock().await = iteration;
        *self.session_number.lock().await = session_num;
    }

    /// Clear the task run ID (for standalone automation runs).
    pub async fn clear_task_run(&self) {
        *self.task_run_id.lock().await = None;
        *self.iteration_number.lock().await = 1;
        *self.session_number.lock().await = 1;
        *self.latest_test_results.lock().await = None;
    }

    /// Get the current task run ID if set.
    pub async fn get_task_run_id(&self) -> Option<String> {
        self.task_run_id.lock().await.clone()
    }

    /// Set the current configuration context.
    pub async fn set_config(&self, config_id: String, project_id: Option<String>) {
        safe_eprintln!(
            "[RUN_RECORDING] set_config called: config_id={}, project_id={:?}",
            config_id,
            project_id
        );
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
        safe_eprintln!(
            "[RUN_RECORDING] on_workflow_started called, workflow_name={:?}",
            workflow_name
        );
        let config_id = match self.config_id.lock().await.clone() {
            Some(id) => {
                safe_eprintln!("[RUN_RECORDING] config_id from state: {}", id);
                id
            }
            None => {
                safe_eprintln!("[RUN_RECORDING] WARNING: no config_id set, using 'unknown'");
                warn!("Workflow started but no config_id set, using 'unknown'");
                "unknown".to_string()
            }
        };

        let project_id = self.project_id.lock().await.clone();
        let task_run_id = self.task_run_id.lock().await.clone();
        let iteration_number = *self.iteration_number.lock().await;
        let expected_durations = self.expected_durations.lock().await.clone();

        let mut recorder = RunRecorder::new(config_id.clone(), workflow_name.clone())
            .with_expected_durations(expected_durations);

        if let Some(pid) = project_id {
            recorder = recorder.with_project_id(pid);
        }

        // If we have a task_run_id, link this automation to the parent task
        if let Some(ref trid) = task_run_id {
            recorder = recorder.with_task_run(trid.clone(), iteration_number);
            safe_eprintln!(
                "[RUN_RECORDING] Linked to task_run_id={}, iteration={}",
                trid,
                iteration_number
            );
        }

        safe_eprintln!(
            "[RUN_RECORDING] Created recorder: run_id={}, config_id={}",
            recorder.run_id(),
            config_id
        );
        info!("Started run recording: {}", recorder.run_id());

        *self.active_recorder.lock().await = Some(recorder);
        safe_eprintln!("[RUN_RECORDING] Recorder stored in active_recorder");

        // Execute "before_workflow" verification tests
        let tests_result = execute_tests_for_trigger(
            &self.db,
            &config_id,
            &TriggerPoint::BeforeWorkflow,
            task_run_id.as_deref(),
        );

        if tests_result.total > 0 {
            info!(
                "Before-workflow tests: {}/{} passed",
                tests_result.passed, tests_result.total
            );

            // Create findings for failed critical tests
            if let Some(ref trid) = task_run_id {
                let session_num = *self.session_number.lock().await;
                create_findings_for_failures(
                    &self.db,
                    trid,
                    session_num,
                    &tests_result,
                    &config_id,
                );
            }

            // Store results for AI context
            *self.latest_test_results.lock().await = Some(tests_result);
        }
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
    #[tracing::instrument(
        name = "image.recognition.event",
        skip(self),
        fields(
            template_name = %template,
            confidence = %confidence,
            match_success = %success
        )
    )]
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

    /// Handle a screenshot captured event.
    ///
    /// This records screenshot actions to the automation log. Supports:
    /// - Standalone SCREENSHOT actions
    /// - Post-action screenshots (taken after other actions with a delay)
    pub async fn on_screenshot_captured(
        &self,
        screenshot_type: &str,
        file_path: &str,
        monitor: Option<String>,
        delay_seconds: Option<u32>,
        success: bool,
        associated_action: Option<String>,
        error: Option<String>,
    ) {
        if let Some(ref mut recorder) = *self.active_recorder.lock().await {
            recorder.record_screenshot(
                screenshot_type,
                file_path,
                monitor,
                delay_seconds,
                success,
                associated_action,
                error,
            );
        }
    }

    /// Handle a tree event from the Python executor.
    pub async fn on_tree_event(&self, event_type: &str, node: &serde_json::Value) {
        // Log all workflow-related events for debugging
        if event_type.starts_with("workflow") {
            safe_eprintln!(
                "[RUN_RECORDING] on_tree_event: event_type={}, node={:?}",
                event_type,
                node
            );
        }

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

                // Check if this is a screenshot action
                let action_type = node.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if action_type == "SCREENSHOT" {
                    let success = event_type == "action_completed";
                    let file_path = node.get("filename").and_then(|f| f.as_str()).unwrap_or("");
                    let monitor = node
                        .get("monitor")
                        .and_then(|m| m.as_str())
                        .map(String::from);
                    let error = if success {
                        None
                    } else {
                        node.get("error").and_then(|e| e.as_str()).map(String::from)
                    };

                    self.on_screenshot_captured(
                        "standalone",
                        file_path,
                        monitor,
                        None, // delay_seconds
                        success,
                        None, // associated_action
                        error,
                    )
                    .await;
                }
            }
            "workflow_completed" => {
                self.on_workflow_completed().await;
            }
            "workflow_failed" => {
                let error = node
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                self.on_workflow_failed(error).await;
            }
            _ => {
                debug!("Unhandled tree event type: {}", event_type);
            }
        }
    }

    /// Handle workflow completed (success).
    pub async fn on_workflow_completed(&self) {
        safe_eprintln!("[RUN_RECORDING] on_workflow_completed called");
        info!("[RUN_RECORDING] on_workflow_completed called");

        // Get config_id and task_run_id before taking the recorder
        let config_id = self.config_id.lock().await.clone();
        let task_run_id = self.task_run_id.lock().await.clone();
        let session_num = *self.session_number.lock().await;

        // Execute "after_workflow" verification tests
        if let Some(ref cid) = config_id {
            let tests_result = execute_tests_for_trigger(
                &self.db,
                cid,
                &TriggerPoint::AfterWorkflow,
                task_run_id.as_deref(),
            );

            if tests_result.total > 0 {
                info!(
                    "After-workflow tests: {}/{} passed",
                    tests_result.passed, tests_result.total
                );

                // Create findings for failed critical tests
                if let Some(ref trid) = task_run_id {
                    create_findings_for_failures(&self.db, trid, session_num, &tests_result, cid);
                }

                // Update latest test results (append after-workflow context)
                let mut results_guard = self.latest_test_results.lock().await;
                if let Some(ref mut existing) = *results_guard {
                    // Combine results
                    existing.total += tests_result.total;
                    existing.passed += tests_result.passed;
                    existing.failed += tests_result.failed;
                    existing.critical_failure =
                        existing.critical_failure || tests_result.critical_failure;
                    if !tests_result.ai_context.is_empty() {
                        existing.ai_context.push_str("\n\n");
                        existing.ai_context.push_str(&tests_result.ai_context);
                    }
                    existing.results.extend(tests_result.results);
                } else {
                    *results_guard = Some(tests_result);
                }
            }
        }

        if let Some(recorder) = self.active_recorder.lock().await.take() {
            safe_eprintln!(
                "[RUN_RECORDING] Have active recorder, run_id={}, config_id={}",
                recorder.run_id(),
                recorder.config_id()
            );
            info!(
                "[RUN_RECORDING] Have active recorder, run_id={}, config_id={}",
                recorder.run_id(),
                recorder.config_id()
            );

            safe_eprintln!("[RUN_RECORDING] Calling finish_success_with_db...");
            match recorder.finish_success_with_db(&self.db) {
                Ok(run_id) => {
                    safe_eprintln!("[RUN_RECORDING] SUCCESS: Run {} saved to database!", run_id);
                    info!(
                        "[RUN_RECORDING] Run {} completed successfully and SAVED to database",
                        run_id
                    );
                }
                Err(e) => {
                    safe_eprintln!("[RUN_RECORDING] ERROR: Failed to save run: {}", e);
                    error!("[RUN_RECORDING] Failed to save run: {}", e);
                }
            }
        } else {
            safe_eprintln!(
                "[RUN_RECORDING] WARNING: on_workflow_completed called but no active recorder!"
            );
            warn!("[RUN_RECORDING] on_workflow_completed called but no active recorder!");
        }
    }

    /// Handle workflow failed.
    pub async fn on_workflow_failed(&self, error: &str) {
        if let Some(recorder) = self.active_recorder.lock().await.take() {
            match recorder.finish_failure_with_db(&self.db, error) {
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
            match recorder.finish_timeout_with_db(&self.db) {
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
            // Still save the run but as cancelled
            match recorder.finish_cancelled_with_db(&self.db) {
                Ok(run_id) => {
                    info!("Run {} recorded as cancelled", run_id);
                }
                Err(e) => {
                    warn!("Failed to save cancelled run: {}", e);
                }
            }
        }
    }

    /// Get the latest test results formatted for AI context.
    ///
    /// Returns the combined test results from before-workflow and after-workflow tests.
    /// This can be used to provide context to AI for analysis and debugging.
    pub async fn get_test_results_ai_context(&self) -> String {
        if let Some(ref results) = *self.latest_test_results.lock().await {
            format_results_for_ai(results)
        } else {
            String::new()
        }
    }

    /// Get the latest test results.
    ///
    /// Returns a clone of the test results if any tests have been executed.
    pub async fn get_latest_test_results(&self) -> Option<TriggerTestsResult> {
        self.latest_test_results.lock().await.clone()
    }

    /// Check if any critical test has failed.
    pub async fn has_critical_test_failure(&self) -> bool {
        if let Some(ref results) = *self.latest_test_results.lock().await {
            results.critical_failure
        } else {
            false
        }
    }
}

/// Create a run recording handler from AppState.
///
/// This is a convenience function for integration with the executor.
pub fn create_run_recording_handler(db: Arc<CheckpointDb>) -> RunRecordingHandler {
    RunRecordingHandler::new(db)
}
