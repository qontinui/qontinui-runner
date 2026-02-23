//! Save Workflow Artifact Handler
//!
//! Reads a generated workflow JSON file from the artifact directory
//! and saves it to the database as a new unified workflow. Used as the
//! final completion step in the meta-workflow (AI Workflow Generator).

use async_trait::async_trait;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;
use crate::unified_workflows::CreateUnifiedWorkflowRequest;

/// Handler for save_workflow_artifact steps.
///
/// Reads the workflow JSON produced by the AI builder/fixer agents
/// from the artifact directory, parses it into a `CreateUnifiedWorkflowRequest`,
/// saves it to the database, and records the new workflow ID in the
/// task run's `result_data`.
pub struct SaveWorkflowArtifactHandler;

#[async_trait]
impl StepHandler for SaveWorkflowArtifactHandler {
    fn step_type(&self) -> &'static str {
        "save_workflow_artifact"
    }

    fn display_name(&self) -> &'static str {
        "Save Workflow Artifact"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        let input_path = match &step.artifact_input_path {
            Some(p) => p.clone(),
            None => {
                return StepHandlerResult::failure(
                    "save_workflow_artifact: missing artifact_input_path",
                );
            }
        };

        tracing::info!("Reading workflow artifact from: {}", input_path);

        // Read the workflow JSON file
        let json_content = match tokio::fs::read_to_string(&input_path).await {
            Ok(content) => content,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to read workflow artifact at '{}': {}",
                    input_path, e
                ));
            }
        };

        // Parse as a CreateUnifiedWorkflowRequest
        let mut request: CreateUnifiedWorkflowRequest = match serde_json::from_str(&json_content) {
            Ok(req) => req,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to parse workflow JSON from '{}': {}",
                    input_path, e
                ));
            }
        };

        // Link the generated workflow back to the task run that created it
        if let Some(ref task_run_id) = context.task_run_id {
            request.generated_by_task_run_id = Some(task_run_id.clone());
        }

        // Save to database
        let workflow = match context
            .app_state
            .checkpoint_db
            .create_unified_workflow(&request)
        {
            Ok(wf) => wf,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to save workflow to database: {}",
                    e
                ));
            }
        };

        tracing::info!(
            "Saved generated workflow '{}' (id: {})",
            workflow.name,
            workflow.id
        );

        // Update the task run's result_data with the generated workflow ID
        if let Some(ref task_run_id) = context.task_run_id {
            let result_data = serde_json::json!({
                "generated_workflow_id": workflow.id,
                "generated_workflow_name": workflow.name,
            });
            if let Err(e) = context
                .app_state
                .checkpoint_db
                .update_task_run_result_data(task_run_id, &result_data.to_string())
            {
                tracing::warn!("Failed to update task run result_data: {}", e);
                // Non-fatal: the workflow was saved successfully
            }
        }

        StepHandlerResult::success_with_data(serde_json::json!({
            "workflow_id": workflow.id,
            "workflow_name": workflow.name,
        }))
    }
}
