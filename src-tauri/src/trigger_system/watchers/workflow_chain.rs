//! Workflow chain watcher: triggers a workflow when another completes.
//!
//! This watcher is "passive" -- it receives events from the post-execution
//! hook system when workflows complete. The TriggerService routes
//! TaskCompleted/TaskFailed events from the event bus to matching chain triggers.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::database::CheckpointDb;

use super::super::storage;
use super::super::types::{TriggerConfig, TriggerEvent};

/// Check all workflow chain triggers against a completed workflow.
///
/// Called from the post-execution path when a workflow finishes.
pub async fn check_workflow_chains(
    db: &Arc<CheckpointDb>,
    tx: &mpsc::Sender<TriggerEvent>,
    completed_workflow_id: &str,
    completed_task_run_id: &str,
    status: &str, // "completed" or "failed"
    output_summary: Option<&str>,
) {
    let triggers = match storage::get_enabled_triggers(db) {
        Ok(t) => t,
        Err(e) => {
            warn!("Failed to load triggers for chain check: {}", e);
            return;
        }
    };

    for trigger in triggers {
        if let TriggerConfig::WorkflowChain {
            ref source_workflow_id,
            ref on_status,
            pass_context,
        } = trigger.trigger_config
        {
            // Check if this trigger matches the completed workflow
            if source_workflow_id != completed_workflow_id {
                continue;
            }

            // Check status filter
            let status_matches = match on_status.as_str() {
                "any" => true,
                "completed" => status == "completed",
                "failed" => status == "failed",
                _ => status == on_status,
            };

            if !status_matches {
                debug!(
                    "Chain trigger '{}' skipped: status '{}' doesn't match filter '{}'",
                    trigger.name, status, on_status
                );
                continue;
            }

            info!(
                "Workflow chain trigger '{}' matched: {} -> {} (status: {})",
                trigger.name, completed_workflow_id, trigger.workflow_id, status
            );

            // Build variables
            let mut variables = HashMap::new();
            variables.insert(
                "source_workflow_id".to_string(),
                completed_workflow_id.to_string(),
            );
            variables.insert(
                "source_task_run_id".to_string(),
                completed_task_run_id.to_string(),
            );
            variables.insert("source_status".to_string(), status.to_string());

            if pass_context {
                if let Some(summary) = output_summary {
                    variables.insert("source_output".to_string(), summary.to_string());
                }
            }

            let event = TriggerEvent {
                trigger_id: trigger.id.clone(),
                event_type: format!("workflow_{}", status),
                event_data: serde_json::json!({
                    "source_workflow_id": completed_workflow_id,
                    "source_task_run_id": completed_task_run_id,
                    "status": status,
                }),
                variables,
                chain_depth: 1, // Start at 1 since this is a chain reaction
            };

            if let Err(e) = tx.send(event).await {
                warn!(
                    "Failed to send chain event for trigger '{}': {}",
                    trigger.name, e
                );
            }
        }
    }
}
