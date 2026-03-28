//! Save Workflow Artifact Handler
//!
//! Reads a generated workflow JSON file from the artifact directory
//! and saves it to the database as a new unified workflow. Used as the
//! final completion step in the meta-workflow (AI Workflow Generator).
//!
//! When `artifact_capture_prompts` is enabled, also creates a `PipelineArtifact`
//! by reading investigation.md, criteria.json, and prompt data from the artifact
//! directory. This captures training data from the async meta-workflow path.

use async_trait::async_trait;
use std::path::Path;

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflows::CreateUnifiedWorkflowRequest;
use crate::workflow_generation::pipeline_artifacts::PipelineArtifactBuilder;

/// Handler for save_workflow_artifact steps.
///
/// Reads the workflow JSON produced by the AI builder/fixer agents
/// from the artifact directory, parses it into a `CreateUnifiedWorkflowRequest`,
/// saves it to the database, and records the new workflow ID in the
/// task run's `result_data`.
///
/// When `artifact_capture_prompts` is set on the step config, additionally
/// creates a `PipelineArtifact` from the artifact directory contents.
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

        // Default ai_reviewed to false for meta-workflow-generated artifacts.
        // The AI fixer agent writes the workflow JSON but doesn't have access to
        // verification pass/fail status, so if ai_reviewed is absent from the JSON,
        // we conservatively mark it as not reviewed rather than defaulting to true.
        if request.ai_reviewed.is_none() {
            request.ai_reviewed = Some(false);
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

        // Capture PipelineArtifact if enabled
        if step.artifact_capture_prompts.unwrap_or(false) {
            self.capture_pipeline_artifact(
                &input_path,
                &workflow.id,
                &workflow.name,
                &json_content,
                context,
            )
            .await;
        }

        StepHandlerResult::success_with_data(serde_json::json!({
            "workflow_id": workflow.id,
            "workflow_name": workflow.name,
        }))
    }
}

impl SaveWorkflowArtifactHandler {
    /// Extract prompt texts from the meta-workflow definition stored in the DB.
    ///
    /// The meta-workflow's setup/completion steps contain the specification,
    /// builder, and hardener prompts in their `content` fields. We look up
    /// the task run → workflow_id → workflow definition to extract them.
    fn extract_prompts_from_meta_workflow(
        &self,
        context: &HandlerContext,
        task_run_id: &str,
        builder: &mut PipelineArtifactBuilder,
    ) {
        // Look up the task run to get the workflow_name
        let task_run = match tokio::runtime::Handle::try_current()
            .ok()
            .and_then(|h| {
                let pg = context.app_state.pg_db.clone();
                let id = task_run_id.to_string();
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    h.block_on(async move { pg.get_task_run(&id).await })
                })).ok()
            })
            .and_then(|r| r.ok())
            .flatten()
        {
            Some(tr) => tr,
            None => {
                tracing::debug!(
                    "Could not find task_run {} for prompt extraction",
                    task_run_id
                );
                return;
            }
        };

        let workflow_name = match &task_run.workflow_name {
            Some(name) => name.clone(),
            None => return,
        };

        // Load the meta-workflow definition by name
        let workflow = match context
            .app_state
            .checkpoint_db
            .get_unified_workflow_by_name(&workflow_name)
        {
            Ok(Some(wf)) => wf,
            _ => return,
        };

        // Extract prompts from setup steps by name
        for step in &workflow.setup_steps {
            let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let content = step.get("content").and_then(|v| v.as_str());

            if let Some(content) = content {
                if name.contains("acceptance criteria") || name.contains("specification") {
                    builder.specification_prompt = Some(content.to_string());
                } else if name.contains("Generate workflow") || name.contains("builder") {
                    builder.builder_prompt = Some(content.to_string());
                }
            }
        }

        // Extract hardener prompt from completion steps
        for step in &workflow.completion_steps {
            let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let content = step.get("content").and_then(|v| v.as_str());

            if let Some(content) = content {
                if name.contains("Harden") || name.contains("hardener") {
                    builder.hardener_prompt = Some(content.to_string());
                    break;
                }
            }
        }

        // Extract verification prompt from verification steps
        for step in &workflow.verification_steps {
            let prompt = step.get("ai_review_prompt").and_then(|v| v.as_str());
            if let Some(prompt) = prompt {
                builder.verification_prompts.push(prompt.to_string());
            }
        }
    }

    /// Create a PipelineArtifact from the artifact directory contents.
    ///
    /// Reads investigation.md, criteria.json, and the final workflow JSON
    /// to populate a PipelineArtifact for training data capture.
    /// Best-effort: logs warnings on failure but does not fail the step.
    async fn capture_pipeline_artifact(
        &self,
        input_path: &str,
        workflow_id: &str,
        workflow_name: &str,
        final_json: &str,
        context: &HandlerContext,
    ) {
        let artifact_dir = match Path::new(input_path).parent() {
            Some(dir) => dir,
            None => {
                tracing::warn!(
                    "Cannot determine artifact_dir from input_path: {}",
                    input_path
                );
                return;
            }
        };

        let description = workflow_name.to_string();

        let mut builder = PipelineArtifactBuilder::new(&description, Some("meta"));

        // Read investigation.md (optional)
        let investigation_path = artifact_dir.join("investigation.md");
        if let Ok(content) = tokio::fs::read_to_string(&investigation_path).await {
            if !content.is_empty() {
                builder.investigation_enriched_description = Some(content);
            }
        }

        // Read criteria.json (optional)
        let criteria_path = artifact_dir.join("criteria.json");
        if let Ok(content) = tokio::fs::read_to_string(&criteria_path).await {
            if let Ok(criteria_value) = serde_json::from_str::<serde_json::Value>(&content) {
                builder.specification_criteria = Some(criteria_value);
            }
        }

        // Extract prompts from the meta-workflow definition (stored in DB).
        // The meta-workflow's step `content` fields contain the specification,
        // builder, and hardener prompts used during generation.
        if let Some(ref task_run_id) = context.task_run_id {
            self.extract_prompts_from_meta_workflow(context, task_run_id, &mut builder);
        }

        // Set the final workflow JSON
        if let Ok(final_value) = serde_json::from_str::<serde_json::Value>(final_json) {
            builder.final_json = Some(final_value);
        }

        // Build the artifact (total_duration_ms = 0 since we don't have timing data
        // from the meta-workflow; the task_run has its own duration tracking)
        let mut artifact = builder.build(0);
        artifact.workflow_id = Some(workflow_id.to_string());
        artifact.task_run_id = context.task_run_id.clone();

        // Save to database
        if let Err(e) = context
            .app_state
            .checkpoint_db
            .save_pipeline_artifact(&artifact)
        {
            tracing::warn!("Failed to save PipelineArtifact: {}", e);
        } else {
            tracing::info!(
                "Saved PipelineArtifact for meta-workflow (artifact_id: {})",
                artifact.id
            );
        }
    }
}
