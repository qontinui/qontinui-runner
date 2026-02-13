//! Save Workflow Artifact Step Handler
//!
//! Reads a generated workflow JSON file from the artifact directory and
//! saves it to the workflow library (database).

use async_trait::async_trait;
use serde_json::json;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::executor::ExecutionStepConfig;
use crate::unified_workflows::{CreateUnifiedWorkflowRequest, UnifiedWorkflow};

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
        let step_name = step.name.as_deref().unwrap_or("Save Workflow Artifact");

        // Get the input path
        let input_path = match &step.artifact_input_path {
            Some(p) => p.clone(),
            None => {
                return StepHandlerResult::failure(
                    "artifact_input_path is required for save_workflow_artifact step",
                );
            }
        };

        info!(
            "SaveWorkflowArtifact '{}': reading workflow from '{}'",
            step_name, input_path
        );

        // Read the workflow JSON file
        let content = match std::fs::read_to_string(&input_path) {
            Ok(c) => c,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to read workflow file '{}': {}",
                    input_path, e
                ));
            }
        };

        // Parse as UnifiedWorkflow
        let workflow: UnifiedWorkflow = match serde_json::from_str(&content) {
            Ok(w) => w,
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to parse workflow JSON from '{}': {}",
                    input_path, e
                ));
            }
        };

        info!(
            "SaveWorkflowArtifact '{}': parsed workflow '{}' (category: {})",
            step_name, workflow.name, workflow.category
        );

        // Build create request from the parsed workflow
        let create_request = CreateUnifiedWorkflowRequest {
            name: workflow.name.clone(),
            description: workflow.description.clone(),
            category: workflow.category.clone(),
            tags: workflow.tags.clone(),
            setup_steps: workflow.setup_steps.clone(),
            verification_steps: workflow.verification_steps.clone(),
            agentic_steps: workflow.agentic_steps.clone(),
            completion_steps: workflow.completion_steps.clone(),
            max_iterations: workflow.max_iterations,
            timeout_seconds: workflow.timeout_seconds,
            provider: workflow.provider.clone(),
            model: workflow.model.clone(),
            skip_ai_summary: workflow.skip_ai_summary,
            log_source_selection: Some(workflow.log_source_selection.clone()),
            log_watch_enabled: Some(workflow.log_watch_enabled),
            health_check_enabled: Some(workflow.health_check_enabled),
            health_check_urls: Some(workflow.health_check_urls.clone()),
            preflight_check_enabled: Some(workflow.preflight_check_enabled),
            context_ids: Some(workflow.context_ids.clone()),
            disabled_context_ids: Some(workflow.disabled_context_ids.clone()),
            auto_include_contexts: Some(workflow.auto_include_contexts),
            prompt_template: workflow.prompt_template.clone(),
            targeted_error_ids: Some(workflow.targeted_error_ids.clone()),
            generated_by_task_run_id: workflow.generated_by_task_run_id.clone(),
        };

        // Save to database
        let db = &context.app_state.checkpoint_db;
        match db.create_unified_workflow(&create_request) {
            Ok(saved_workflow) => {
                info!(
                    "SaveWorkflowArtifact '{}': saved workflow '{}' with ID '{}'",
                    step_name, saved_workflow.name, saved_workflow.id
                );

                // Store the generated workflow ID in result_data if task_run_id is available
                if let Some(ref task_run_id) = context.task_run_id {
                    let result_data = json!({
                        "generated_workflow_id": saved_workflow.id,
                        "generated_workflow_name": saved_workflow.name,
                    });
                    if let Err(e) = db.update_task_run_result_data(
                        task_run_id,
                        &serde_json::to_string(&result_data).unwrap_or_default(),
                    ) {
                        warn!(
                            "Failed to update task_run result_data: {} - workflow was still saved",
                            e
                        );
                    }
                }

                StepHandlerResult::success_with_data(json!({
                    "generated_workflow_id": saved_workflow.id,
                    "generated_workflow_name": saved_workflow.name,
                }))
            }
            Err(e) => StepHandlerResult::failure(format!("Failed to save workflow: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handler_step_type() {
        let handler = SaveWorkflowArtifactHandler;
        assert_eq!(handler.step_type(), "save_workflow_artifact");
        assert_eq!(handler.display_name(), "Save Workflow Artifact");
    }
}
