//! Exploration task execution
//!
//! This module implements the main exploration task that orchestrates
//! state machine exploration.

use chrono::Utc;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::exploration::{ExplorationStrategy, StateExplorer, StateMachineGraph};
use super::report::ExplorationReport;
use super::types::{
    ElementCheck, ExplorationConfig, ExplorationResult, ExplorationStatus, StateExplorationResult,
    TransitionExplorationResult,
};

use crate::commands::AppState;

/// Events emitted during exploration for real-time updates
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExplorationEvent {
    /// Exploration task started
    Started {
        run_id: String,
        config_path: String,
        strategy: String,
        total_states: u32,
    },
    /// Starting to explore a state
    StateStarted {
        state_id: String,
        state_name: String,
        sequence: u32,
        total: u32,
    },
    /// State exploration completed
    StateCompleted {
        state_id: String,
        state_name: String,
        status: String,
        duration_ms: u64,
        discrepancy_count: u32,
    },
    /// Starting a transition
    TransitionStarted {
        transition_id: String,
        from_state: String,
        to_state: String,
    },
    /// Transition completed
    TransitionCompleted {
        transition_id: String,
        status: String,
        duration_ms: u64,
    },
    /// Screenshot captured
    ScreenshotCaptured { path: String, state_id: String },
    /// Exploration task completed
    Completed {
        run_id: String,
        status: String,
        summary: String,
        report_path: Option<String>,
    },
    /// Error occurred
    Error {
        message: String,
        state_id: Option<String>,
    },
}

/// Main exploration task that executes the exploration run
pub struct ExplorationTask {
    config: ExplorationConfig,
    app_state: Arc<AppState>,
    result: ExplorationResult,
    event_sender: Option<mpsc::Sender<ExplorationEvent>>,
    start_time: Instant,
}

impl ExplorationTask {
    /// Create a new exploration task
    pub fn new(config: ExplorationConfig, app_state: Arc<AppState>) -> Self {
        let run_id = uuid::Uuid::new_v4().to_string();
        let result =
            ExplorationResult::new(run_id, config.config_path.clone(), config.strategy.clone());

        Self {
            config,
            app_state,
            result,
            event_sender: None,
            start_time: Instant::now(),
        }
    }

    /// Set event sender for real-time updates
    pub fn with_event_sender(mut self, sender: mpsc::Sender<ExplorationEvent>) -> Self {
        self.event_sender = Some(sender);
        self
    }

    /// Execute the exploration task
    pub async fn execute(mut self) -> Result<ExplorationResult, String> {
        info!(
            "Starting exploration task {} for {}",
            self.result.run_id, self.config.config_path
        );

        self.start_time = Instant::now();

        // Load and validate the config
        let config_value = self.load_config().await?;

        // Build the state machine graph
        let graph = StateMachineGraph::from_config(&config_value);

        self.result.total_states = graph.states.len() as u32;
        self.result.total_transitions = graph.transitions.len() as u32;

        // Get config name
        let config_name = config_value
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("Unknown")
            .to_string();

        // Send started event
        self.send_event(ExplorationEvent::Started {
            run_id: self.result.run_id.clone(),
            config_path: self.config.config_path.clone(),
            strategy: self.config.strategy.clone(),
            total_states: self.result.total_states,
        })
        .await;

        // Create the explorer based on strategy
        let strategy = ExplorationStrategy::from_str(&self.config.strategy);
        let mut explorer = StateExplorer::new(graph.clone(), strategy);

        // Configure explorer
        if self.config.max_states > 0 {
            explorer = explorer.with_max_states(self.config.max_states);
        }

        if !self.config.target_state_ids.is_empty() || !self.config.target_transition_ids.is_empty()
        {
            explorer = explorer.with_targets(
                self.config.target_state_ids.clone(),
                self.config.target_transition_ids.clone(),
            );
        }

        if let Some(seed) = self.config.random_seed {
            explorer = explorer.with_seed(seed);
        }

        // Generate the exploration path
        let path = explorer.generate_path();
        info!(
            "Generated exploration path with {} states and {} transitions",
            path.states.len(),
            path.transitions.len()
        );

        // Execute the exploration
        let mut visited: HashSet<String> = HashSet::new();
        let total_states = path.states.len() as u32;

        for (index, state_visit) in path.states.iter().enumerate() {
            // Check timeout
            if self.config.max_duration_seconds > 0
                && self.start_time.elapsed().as_secs() > self.config.max_duration_seconds
            {
                warn!("Exploration timeout reached");
                break;
            }

            // Skip already visited states (can happen with some strategies)
            if visited.contains(&state_visit.state_id) {
                continue;
            }
            visited.insert(state_visit.state_id.clone());

            // Get state info from graph
            let state_info = graph.states.get(&state_visit.state_id);

            // Send state started event
            self.send_event(ExplorationEvent::StateStarted {
                state_id: state_visit.state_id.clone(),
                state_name: state_visit.state_name.clone(),
                sequence: index as u32 + 1,
                total: total_states,
            })
            .await;

            // Explore the state
            let exploration = self
                .explore_state(&state_visit.state_id, &state_visit.state_name, state_info)
                .await;

            // Check if we should stop on failure
            if self.config.stop_on_first_failure
                && matches!(exploration.status, ExplorationStatus::Failed)
            {
                self.result.add_state_exploration(exploration.clone());

                self.send_event(ExplorationEvent::StateCompleted {
                    state_id: exploration.state_id.clone(),
                    state_name: exploration.state_name.clone(),
                    status: format!("{:?}", exploration.status),
                    duration_ms: exploration.duration_ms,
                    discrepancy_count: exploration
                        .expected_elements_found
                        .iter()
                        .filter(|e| !e.found)
                        .count() as u32
                        + exploration
                            .unexpected_elements_found
                            .iter()
                            .filter(|e| e.found)
                            .count() as u32,
                })
                .await;

                warn!(
                    "Stopping exploration due to failure at state {}",
                    exploration.state_id
                );
                break;
            }

            let discrepancy_count = exploration
                .expected_elements_found
                .iter()
                .filter(|e| !e.found)
                .count() as u32
                + exploration
                    .unexpected_elements_found
                    .iter()
                    .filter(|e| e.found)
                    .count() as u32;

            self.send_event(ExplorationEvent::StateCompleted {
                state_id: exploration.state_id.clone(),
                state_name: exploration.state_name.clone(),
                status: format!("{:?}", exploration.status),
                duration_ms: exploration.duration_ms,
                discrepancy_count,
            })
            .await;

            self.result.add_state_exploration(exploration);

            // Delay between states
            if self.config.state_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(self.config.state_delay_ms)).await;
            }
        }

        // Explore transitions if configured
        for transition_step in &path.transitions {
            if !transition_step.verify {
                continue;
            }

            self.send_event(ExplorationEvent::TransitionStarted {
                transition_id: transition_step.transition_id.clone(),
                from_state: transition_step.from_state_id.clone(),
                to_state: transition_step.to_state_id.clone(),
            })
            .await;

            let transition_info = graph.transitions.get(&transition_step.transition_id);
            let exploration = self
                .explore_transition(
                    &transition_step.transition_id,
                    &transition_step.from_state_id,
                    &transition_step.to_state_id,
                    transition_info.and_then(|t| t.expected_duration_ms),
                )
                .await;

            self.send_event(ExplorationEvent::TransitionCompleted {
                transition_id: exploration.transition_id.clone(),
                status: format!("{:?}", exploration.status),
                duration_ms: exploration.duration_ms,
            })
            .await;

            self.result.add_transition_exploration(exploration);
        }

        // Complete the result
        self.result.total_duration_ms = self.start_time.elapsed().as_millis() as u64;
        self.result.complete();

        // Generate and save report
        let report = ExplorationReport::from_result(&self.result, config_name);

        let output_dir = self.config.output_directory.clone().unwrap_or_else(|| {
            crate::paths::get_state_explorer_dir()
                .to_string_lossy()
                .to_string()
        });

        match report.save_json(&PathBuf::from(&output_dir)) {
            Ok(path) => {
                self.result.report_path = Some(path.to_string_lossy().to_string());
                info!("Exploration report saved to {:?}", path);

                // Also save markdown version
                if let Err(e) = report.save_markdown(&PathBuf::from(&output_dir)) {
                    warn!("Failed to save markdown report: {}", e);
                }
            }
            Err(e) => {
                error!("Failed to save exploration report: {}", e);
            }
        }

        // Send completion event
        self.send_event(ExplorationEvent::Completed {
            run_id: self.result.run_id.clone(),
            status: format!("{:?}", self.result.overall_status),
            summary: self.result.summary.clone(),
            report_path: self.result.report_path.clone(),
        })
        .await;

        info!(
            "Exploration task {} completed: {}",
            self.result.run_id, self.result.summary
        );

        Ok(self.result)
    }

    /// Load the configuration file
    async fn load_config(&self) -> Result<Value, String> {
        let config_path = self.config.config_path.clone();

        // Load config via Python bridge - use a separate scope to ensure lock is dropped before await
        {
            let mut bridge_lock = self.app_state.python_bridge.lock().unwrap();
            if let Some(ref mut bridge) = *bridge_lock {
                if !bridge.is_running() {
                    return Err("Python executor not running".to_string());
                }

                // Send load command
                bridge
                    .load_configuration(&config_path)
                    .map_err(|e| format!("Failed to load config: {}", e))?;
            } else {
                return Err("Python executor not initialized".to_string());
            }
            // bridge_lock is dropped here at end of scope
        }

        // Wait a bit for config to load (lock is now released)
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Get the loaded config from app state
        let config_lock = self.app_state.current_config.lock().unwrap();
        if let Some(ref config) = *config_lock {
            // Convert to Value for graph building
            serde_json::to_value(config).map_err(|e| format!("Failed to serialize config: {}", e))
        } else {
            Err("Config not loaded".to_string())
        }
    }

    /// Explore a single state using Python bridge for actual state detection
    async fn explore_state(
        &self,
        state_id: &str,
        state_name: &str,
        state_info: Option<&super::exploration::StateInfo>,
    ) -> StateExplorationResult {
        let started_at = Utc::now().to_rfc3339();
        let start = Instant::now();

        debug!("Exploring state: {} ({})", state_name, state_id);

        // Capture screenshot if configured
        let screenshot_path = if self.config.capture_screenshots {
            self.capture_screenshot(state_id).await
        } else {
            None
        };

        // Initialize result containers
        let mut expected_checks: Vec<ElementCheck> = Vec::new();
        let mut unexpected_checks: Vec<ElementCheck> = Vec::new();
        let mut detection_confidence: f64 = 0.0;
        let mut error_message: Option<String> = None;

        // Use Python bridge for state detection
        let detection_result = self.detect_state_via_python(state_id).await;

        match detection_result {
            Ok(result) => {
                // Extract detection confidence from result
                if let Some(details) = result.get("detection_details").and_then(|d| d.as_array()) {
                    for detail in details {
                        if detail.get("state_id").and_then(|s| s.as_str()) == Some(state_id) {
                            detection_confidence = detail
                                .get("confidence")
                                .and_then(|c| c.as_f64())
                                .unwrap_or(0.0);
                            break;
                        }
                    }
                }

                // Check if state was detected
                let detected_states = result
                    .get("detected_states")
                    .and_then(|d| d.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();

                if !detected_states.contains(&state_id) {
                    debug!("State {} not detected by Python bridge", state_id);
                }
            }
            Err(e) => {
                error!("Failed to detect state via Python bridge: {}", e);
                error_message = Some(e);
            }
        }

        // Verify expected and unexpected elements if state_info is available
        if let Some(info) = state_info {
            // Verify expected elements via Python bridge
            if !info.expected_elements.is_empty() {
                let element_result = self
                    .verify_elements_via_python(&info.expected_elements, &[])
                    .await;

                match element_result {
                    Ok(result) => {
                        if let Some(expected_results) =
                            result.get("expected_results").and_then(|r| r.as_array())
                        {
                            for elem_result in expected_results {
                                let element_id = elem_result
                                    .get("element_id")
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let found = elem_result
                                    .get("found")
                                    .and_then(|f| f.as_bool())
                                    .unwrap_or(false);
                                let confidence = elem_result
                                    .get("confidence")
                                    .and_then(|c| c.as_f64())
                                    .unwrap_or(0.0);
                                let location = elem_result.get("location").and_then(|l| {
                                    if l.is_null() {
                                        None
                                    } else {
                                        let x =
                                            l.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                        let y =
                                            l.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                        let width =
                                            l.get("width").and_then(|v| v.as_i64()).unwrap_or(0)
                                                as i32;
                                        let height =
                                            l.get("height").and_then(|v| v.as_i64()).unwrap_or(0)
                                                as i32;
                                        Some((x, y, width, height))
                                    }
                                });

                                expected_checks.push(ElementCheck {
                                    element: element_id,
                                    found,
                                    confidence,
                                    location,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to verify expected elements: {}", e);
                        // Fall back to creating placeholder checks for expected elements
                        for element in &info.expected_elements {
                            expected_checks.push(ElementCheck {
                                element: element.clone(),
                                found: false,
                                confidence: 0.0,
                                location: None,
                            });
                        }
                    }
                }
            }

            // Verify unexpected elements if any
            if !info.unexpected_elements.is_empty() {
                let element_result = self
                    .verify_elements_via_python(&[], &info.unexpected_elements)
                    .await;

                match element_result {
                    Ok(result) => {
                        if let Some(unexpected_results) =
                            result.get("unexpected_results").and_then(|r| r.as_array())
                        {
                            for elem_result in unexpected_results {
                                let element_id = elem_result
                                    .get("element_id")
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let found = elem_result
                                    .get("found")
                                    .and_then(|f| f.as_bool())
                                    .unwrap_or(false);
                                let confidence = elem_result
                                    .get("confidence")
                                    .and_then(|c| c.as_f64())
                                    .unwrap_or(0.0);
                                let location = elem_result.get("location").and_then(|l| {
                                    if l.is_null() {
                                        None
                                    } else {
                                        let x =
                                            l.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                        let y =
                                            l.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                                        let width =
                                            l.get("width").and_then(|v| v.as_i64()).unwrap_or(0)
                                                as i32;
                                        let height =
                                            l.get("height").and_then(|v| v.as_i64()).unwrap_or(0)
                                                as i32;
                                        Some((x, y, width, height))
                                    }
                                });

                                unexpected_checks.push(ElementCheck {
                                    element: element_id,
                                    found,
                                    confidence,
                                    location,
                                });
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to verify unexpected elements: {}", e);
                    }
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Determine status based on actual detection results
        let all_expected_found = expected_checks.is_empty() || expected_checks.iter().all(|c| c.found);
        let no_unexpected = unexpected_checks.iter().all(|c| !c.found);

        let status = if error_message.is_some() {
            ExplorationStatus::Failed
        } else if detection_confidence >= 0.5 && all_expected_found && no_unexpected {
            ExplorationStatus::Passed
        } else {
            ExplorationStatus::Failed
        };

        // Send screenshot event if captured
        if let Some(ref path) = screenshot_path {
            self.send_event(ExplorationEvent::ScreenshotCaptured {
                path: path.clone(),
                state_id: state_id.to_string(),
            })
            .await;
        }

        StateExplorationResult {
            state_id: state_id.to_string(),
            state_name: state_name.to_string(),
            status,
            screenshot_path,
            started_at,
            completed_at: Some(Utc::now().to_rfc3339()),
            duration_ms,
            expected_elements_found: expected_checks,
            unexpected_elements_found: unexpected_checks,
            detection_confidence,
            error: error_message,
            ai_notes: None,
        }
    }

    /// Detect state via Python bridge
    async fn detect_state_via_python(&self, state_id: &str) -> Result<Value, String> {
        let params = serde_json::json!({
            "state_ids": [state_id],
            "capture_screenshot": self.config.capture_screenshots,
        });

        // Use a separate scope to ensure lock is dropped before await
        let result = {
            let mut bridge_lock = self.app_state.python_bridge.lock().unwrap();
            if let Some(ref mut bridge) = *bridge_lock {
                if !bridge.is_running() {
                    return Err("Python executor not running".to_string());
                }

                // Send command and wait for response
                bridge.send_command_and_wait(
                    "detect_current_states",
                    Some(params),
                    Duration::from_secs(30),
                )
            } else {
                return Err("Python executor not initialized".to_string());
            }
        };

        match result {
            Ok(response) => {
                if response.success {
                    Ok(response.data.unwrap_or(serde_json::json!({})))
                } else {
                    Err(response
                        .error
                        .unwrap_or_else(|| "Unknown error".to_string()))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Verify elements via Python bridge
    async fn verify_elements_via_python(
        &self,
        expected_elements: &[String],
        unexpected_elements: &[String],
    ) -> Result<Value, String> {
        let params = serde_json::json!({
            "expected_elements": expected_elements,
            "unexpected_elements": unexpected_elements,
        });

        // Use a separate scope to ensure lock is dropped before await
        let result = {
            let mut bridge_lock = self.app_state.python_bridge.lock().unwrap();
            if let Some(ref mut bridge) = *bridge_lock {
                if !bridge.is_running() {
                    return Err("Python executor not running".to_string());
                }

                // Send command and wait for response
                bridge.send_command_and_wait(
                    "verify_elements",
                    Some(params),
                    Duration::from_secs(30),
                )
            } else {
                return Err("Python executor not initialized".to_string());
            }
        };

        match result {
            Ok(response) => {
                if response.success {
                    Ok(response.data.unwrap_or(serde_json::json!({})))
                } else {
                    Err(response
                        .error
                        .unwrap_or_else(|| "Unknown error".to_string()))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Explore a transition between states
    async fn explore_transition(
        &self,
        transition_id: &str,
        from_state_id: &str,
        to_state_id: &str,
        expected_duration_ms: Option<u64>,
    ) -> TransitionExplorationResult {
        let started_at = Utc::now().to_rfc3339();
        let start = Instant::now();

        debug!(
            "Exploring transition: {} ({} -> {})",
            transition_id, from_state_id, to_state_id
        );

        // Capture before screenshot if configured
        let screenshot_before = if self.config.capture_transition_screenshots {
            self.capture_screenshot(from_state_id).await
        } else {
            None
        };

        // Execute the transition via Python bridge
        let transition_result = self.execute_transition(transition_id).await;

        // Capture after screenshot if configured
        let screenshot_after = if self.config.capture_transition_screenshots {
            self.capture_screenshot(to_state_id).await
        } else {
            None
        };

        let duration_ms = start.elapsed().as_millis() as u64;
        let duration_exceeded = expected_duration_ms
            .map(|expected| duration_ms > expected)
            .unwrap_or(false);

        let (status, error) = match transition_result {
            Ok(_) => (ExplorationStatus::Passed, None),
            Err(e) => (ExplorationStatus::Failed, Some(e)),
        };

        TransitionExplorationResult {
            transition_id: transition_id.to_string(),
            from_state_id: from_state_id.to_string(),
            to_state_id: to_state_id.to_string(),
            status,
            screenshot_before,
            screenshot_after,
            started_at,
            completed_at: Some(Utc::now().to_rfc3339()),
            duration_ms,
            expected_duration_ms,
            duration_exceeded,
            error,
            notes: None,
        }
    }

    /// Capture a screenshot via Python bridge
    async fn capture_screenshot(&self, context: &str) -> Option<String> {
        let output_dir = self.config.output_directory.clone().unwrap_or_else(|| {
            crate::paths::get_state_explorer_screenshots_dir()
                .to_string_lossy()
                .to_string()
        });

        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            error!("Failed to create screenshot directory: {}", e);
            return None;
        }

        let params = serde_json::json!({
            "context": format!("{}-{}", self.result.run_id, context.replace(['/', '\\', ' '], "_")),
            "output_directory": output_dir,
            "save_to_file": true,
        });

        // Use Python bridge to capture actual screenshot
        let result = {
            let mut bridge_lock = self.app_state.python_bridge.lock().unwrap();
            if let Some(ref mut bridge) = *bridge_lock {
                if !bridge.is_running() {
                    warn!("Python executor not running, cannot capture screenshot");
                    return None;
                }

                bridge.send_command_and_wait(
                    "verification_capture_screenshot",
                    Some(params),
                    Duration::from_secs(10),
                )
            } else {
                warn!("Python executor not initialized, cannot capture screenshot");
                return None;
            }
        };

        match result {
            Ok(response) => {
                if response.success {
                    // Extract file_path from response data
                    if let Some(data) = response.data {
                        if let Some(file_path) = data.get("file_path").and_then(|p| p.as_str()) {
                            info!("Screenshot captured: {}", file_path);
                            return Some(file_path.to_string());
                        }
                    }
                    warn!("Screenshot captured but no file path returned");
                    None
                } else {
                    warn!("Failed to capture screenshot: {:?}", response.error);
                    None
                }
            }
            Err(e) => {
                error!("Error capturing screenshot: {}", e);
                None
            }
        }
    }

    /// Execute a transition via Python bridge
    async fn execute_transition(&self, transition_id: &str) -> Result<(), String> {
        let mut bridge_lock = self.app_state.python_bridge.lock().unwrap();
        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let params = serde_json::json!({
                "transition_id": transition_id
            });

            bridge
                .send_command("execute_transition", Some(params))
                .map_err(|e| format!("Failed to execute transition: {}", e))
        } else {
            Err("Python executor not initialized".to_string())
        }
    }

    /// Send an event if event sender is configured
    async fn send_event(&self, event: ExplorationEvent) {
        if let Some(ref sender) = self.event_sender {
            if let Err(e) = sender.send(event).await {
                debug!("Failed to send exploration event: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exploration_result_new() {
        let result = ExplorationResult::new(
            "test-run".to_string(),
            "test/config.json".to_string(),
            "exhaustive".to_string(),
        );

        assert_eq!(result.run_id, "test-run");
        assert_eq!(result.config_path, "test/config.json");
        assert_eq!(result.strategy, "exhaustive");
        assert_eq!(result.states_visited, 0);
    }

    #[test]
    fn test_exploration_result_complete() {
        let mut result = ExplorationResult::new(
            "test-run".to_string(),
            "test.json".to_string(),
            "exhaustive".to_string(),
        );

        result.total_states = 3;
        result.states_visited = 3;
        result.states_passed = 2;
        result.states_failed = 1;

        result.complete();

        assert!(matches!(result.overall_status, ExplorationStatus::Failed));
        assert!(result.summary.contains("3/3"));
    }
}
