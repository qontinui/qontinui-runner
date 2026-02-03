//! AWAS Common Module
//!
//! Shared types and helper functions for AWAS (Automated Web Action Specification) step handlers.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::commands::AppState;
use crate::database::CreateTaskRunAwasStepInput;
use crate::executor::with_default_bridge;

/// Response from AWAS command execution
#[derive(Debug)]
pub struct AwasCommandResponse {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Execute an AWAS command via the Python bridge
pub async fn execute_awas_command(
    app_state: Arc<AppState>,
    command: &str,
    params: Option<serde_json::Value>,
    timeout_secs: u64,
) -> Result<AwasCommandResponse, String> {
    let command = command.to_string();
    let timeout_duration = Duration::from_secs(timeout_secs);

    tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let result = bridge.send_command_and_wait(&command, params, timeout_duration)?;

            Ok(AwasCommandResponse {
                success: result.success,
                data: result.data,
                error: result.error,
            })
        })?
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Save an AWAS step result to the database (if task_run_id is set)
///
/// This function is called after each AWAS step execution to persist
/// the results for later analysis and debugging.
pub fn save_awas_step_result(
    app_state: &AppState,
    task_run_id: Option<&str>,
    step_type: &str,
    url: Option<&str>,
    action_id: Option<&str>,
    parameters: Option<&serde_json::Value>,
    response: &AwasCommandResponse,
    duration_ms: i64,
    step_name: Option<&str>,
) {
    // Only save if we have a task_run_id
    let Some(task_run_id) = task_run_id else {
        return;
    };

    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let input = CreateTaskRunAwasStepInput {
        task_run_id: task_run_id.to_string(),
        step_id: None, // Step ID from workflow if available
        step_name: step_name.map(|s| s.to_string()),
        step_type: step_type.to_string(),
        url: url.map(|s| s.to_string()),
        action_id: action_id.map(|s| s.to_string()),
        parameters: parameters.map(|p| serde_json::to_string(p).unwrap_or_default()),
        response_data: response
            .data
            .as_ref()
            .map(|d| serde_json::to_string(d).unwrap_or_default()),
        success: response.success,
        error_message: response.error.clone(),
        duration_ms: Some(duration_ms),
        timestamp,
    };

    match app_state.checkpoint_db.create_task_run_awas_step(&input) {
        Ok(id) => {
            info!(
                "Saved AWAS step result to database: {} (type: {})",
                id, step_type
            );
        }
        Err(e) => {
            warn!("Failed to save AWAS step result to database: {}", e);
        }
    }
}

/// Helper struct for AWAS step execution with common boilerplate
pub struct AwasStepExecutor<'a> {
    pub app_state: Arc<AppState>,
    pub task_run_id: Option<&'a str>,
    pub step_type: &'a str,
    pub step_name: &'a str,
}

impl<'a> AwasStepExecutor<'a> {
    pub fn new(
        app_state: Arc<AppState>,
        task_run_id: Option<&'a str>,
        step_type: &'a str,
        step_name: &'a str,
    ) -> Self {
        Self {
            app_state,
            task_run_id,
            step_type,
            step_name,
        }
    }

    /// Execute an AWAS command with standard timing, logging, and database persistence
    pub async fn execute(
        &self,
        command: &str,
        params: Option<serde_json::Value>,
        params_for_db: Option<serde_json::Value>,
        url: Option<&str>,
        action_id: Option<&str>,
        timeout_secs: u64,
    ) -> (bool, Option<String>, Option<serde_json::Value>) {
        let start_time = Instant::now();

        // Use params_for_db if provided, otherwise clone params for db logging
        let db_params = params_for_db.or_else(|| params.clone());

        match execute_awas_command(self.app_state.clone(), command, params, timeout_secs).await {
            Ok(response) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;

                // Save result to database
                save_awas_step_result(
                    &self.app_state,
                    self.task_run_id,
                    self.step_type,
                    url,
                    action_id,
                    db_params.as_ref(),
                    &response,
                    duration_ms,
                    Some(self.step_name),
                );

                if response.success {
                    info!(
                        "{} '{}' completed successfully in {}ms",
                        self.step_type, self.step_name, duration_ms
                    );
                    (true, None, response.data)
                } else {
                    let error = response
                        .error
                        .unwrap_or_else(|| format!("{} failed", self.step_type));
                    (false, Some(error), response.data)
                }
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as i64;
                let error_msg = format!("{} error: {}", self.step_type, e);

                // Save error result to database
                let error_response = AwasCommandResponse {
                    success: false,
                    data: None,
                    error: Some(error_msg.clone()),
                };
                save_awas_step_result(
                    &self.app_state,
                    self.task_run_id,
                    self.step_type,
                    url,
                    action_id,
                    db_params.as_ref(),
                    &error_response,
                    duration_ms,
                    Some(self.step_name),
                );

                (false, Some(error_msg), None)
            }
        }
    }
}
