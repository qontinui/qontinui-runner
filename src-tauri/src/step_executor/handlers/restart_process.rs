//! Handler for the `restart_process` step type.
//!
//! Restarts a managed process by ID or name, optionally waiting for health.

use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;

pub struct RestartProcessHandler;

#[async_trait]
impl StepHandler for RestartProcessHandler {
    fn step_type(&self) -> &'static str {
        "restart_process"
    }

    fn display_name(&self) -> &'static str {
        "Restart Process"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // Resolve the process ID
        let process_id = if let Some(ref id) = step.restart_process_id {
            id.clone()
        } else if let Some(ref name) = step.restart_process_name {
            // Look up by name
            let manager_lock = context.app_state.process_capture_manager.lock().await;
            let manager = match manager_lock.as_ref() {
                Some(m) => m,
                None => {
                    return StepHandlerResult::failure("Process capture manager not initialized");
                }
            };
            match manager.find_process_id_by_name(name).await {
                Some(id) => id,
                None => {
                    return StepHandlerResult::failure(format!(
                        "No process found with name '{}'",
                        name
                    ));
                }
            }
        } else {
            return StepHandlerResult::failure(
                "restart_process step requires either restart_process_id or restart_process_name",
            );
        };

        let step_name = step
            .name
            .clone()
            .unwrap_or_else(|| format!("Restart process {}", process_id));

        info!("Restarting process: {}", process_id);

        // Emit action started
        let action_id = format!("restart-{}", process_id);
        let action_name = format!("RESTART: {}", step_name);
        let metadata = json!({
            "step_type": "restart_process",
            "process_id": process_id,
        });

        let (start_timestamp, sequence) = context
            .event_emitter
            .emit_action_started(&action_id, &action_name, metadata)
            .await;

        // Restart the process
        let restart_result = {
            let manager_lock = context.app_state.process_capture_manager.lock().await;
            let manager = match manager_lock.as_ref() {
                Some(m) => m,
                None => {
                    let error = "Process capture manager not initialized";
                    context
                        .event_emitter
                        .emit_action_failed(
                            &action_id,
                            &action_name,
                            start_timestamp,
                            sequence,
                            error,
                            None,
                        )
                        .await;
                    return StepHandlerResult::failure(error);
                }
            };
            manager.restart_process(&process_id).await
        };

        match restart_result {
            Ok(()) => {
                // Optionally wait for health
                let wait_for_health = step.restart_wait_for_health.unwrap_or(true);
                let mut port_healthy = false;

                if wait_for_health {
                    // Check if the process has a health port configured
                    let health_port = {
                        let manager_lock = context.app_state.process_capture_manager.lock().await;
                        if let Some(ref manager) = *manager_lock {
                            let configs = manager.get_configs().await;
                            configs
                                .iter()
                                .find(|c| c.id == process_id)
                                .and_then(|c| c.health_port)
                        } else {
                            None
                        }
                    };

                    if let Some(port) = health_port {
                        info!("Waiting for port {} to become healthy...", port);
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                        while tokio::time::Instant::now() < deadline {
                            if crate::process_capture::health::is_port_in_use(port) {
                                port_healthy = true;
                                info!("Port {} is healthy after restart", port);
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                        if !port_healthy {
                            warn!(
                                "Port {} did not become healthy within 30s after restart",
                                port
                            );
                        }
                    }
                }

                // Emit action completed
                let result_metadata = json!({
                    "process_id": process_id,
                    "port_healthy": port_healthy,
                });
                context
                    .event_emitter
                    .emit_action_completed(
                        &action_id,
                        &action_name,
                        start_timestamp,
                        sequence,
                        result_metadata,
                    )
                    .await;

                StepHandlerResult::success_with_data(json!({
                    "success": true,
                    "process_id": process_id,
                    "port_healthy": port_healthy,
                }))
            }
            Err(e) => {
                // Emit action failed
                let error_msg = format!("Failed to restart process: {}", e);
                context
                    .event_emitter
                    .emit_action_failed(
                        &action_id,
                        &action_name,
                        start_timestamp,
                        sequence,
                        &error_msg,
                        None,
                    )
                    .await;

                StepHandlerResult::failure(error_msg)
            }
        }
    }
}
