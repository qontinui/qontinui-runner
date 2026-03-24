//! Workflow Step Handler
//!
//! Executes a saved workflow inline as a single step. This enables workflow
//! composition — referencing one workflow from another. The referenced workflow's
//! full verification-agentic loop runs as part of the parent workflow.
//!
//! Includes a recursion guard to detect and prevent circular references.

use async_trait::async_trait;
use std::sync::Arc;
use tauri::Manager;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::{LoopConfig, LoopController};

/// Handler for workflow steps — runs a saved workflow inline.
pub struct WorkflowStepHandler;

#[async_trait]
impl StepHandler for WorkflowStepHandler {
    fn step_type(&self) -> &'static str {
        "workflow"
    }

    fn display_name(&self) -> &'static str {
        "Workflow"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // 1. Extract workflow_id (may come as ref_workflow_id or fall back to ci_cd field)
        let workflow_id = match &step.ref_workflow_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                return StepHandlerResult::failure("Workflow step: missing or empty workflow_id");
            }
        };

        // Note: `workflow_name` JSON key is captured by `ci_cd_workflow_name` (serde alias conflict),
        // so check both `ref_workflow_name` (alias "refWorkflowName") and `ci_cd_workflow_name`
        // (alias "workflow_name"/"workflowName") as sources for the display name.
        let workflow_name = step
            .ref_workflow_name
            .clone()
            .or_else(|| step.ci_cd_workflow_name.clone())
            .unwrap_or_else(|| workflow_id.clone());

        info!(
            "Executing nested workflow '{}' (id: {})",
            workflow_name, workflow_id
        );

        // 2. Check recursion stack to prevent circular references
        {
            let stack = context
                .workflow_recursion_stack
                .lock()
                .expect("recursion stack lock poisoned");
            if stack.contains(&workflow_id) {
                let stack_list: Vec<&String> = stack.iter().collect();
                return StepHandlerResult::failure(format!(
                    "Circular workflow reference detected: '{}' is already on the execution stack ({:?})",
                    workflow_name, stack_list
                ));
            }
        }

        // Push onto recursion stack before execution
        {
            let mut stack = context
                .workflow_recursion_stack
                .lock()
                .expect("recursion stack lock poisoned");
            stack.insert(workflow_id.clone());
        }

        // 3. Load workflow from database
        let workflow = match context
            .app_state
            .checkpoint_db
            .get_unified_workflow(&workflow_id)
        {
            Ok(Some(w)) => w,
            Ok(None) => {
                return StepHandlerResult::failure(format!(
                    "Referenced workflow not found: '{}' (id: {})",
                    workflow_name, workflow_id
                ));
            }
            Err(e) => {
                return StepHandlerResult::failure(format!(
                    "Failed to load workflow '{}': {}",
                    workflow_name, e
                ));
            }
        };

        // 4. Get app_handle and pid_tracker (required for LoopController)
        let app_handle = match &context.app_handle {
            Some(h) => h.clone(),
            None => {
                return StepHandlerResult::failure(
                    "Workflow step: app_handle not available (cannot create LoopController)",
                );
            }
        };

        let pid_tracker = context
            .pid_tracker
            .clone()
            .unwrap_or_else(|| Arc::new(std::sync::Mutex::new(Vec::new())));

        // 5. Convert workflow steps into StageConfig (no preflight for nested workflows)
        let stage = crate::unified_workflows::workflow_to_stage_config(&workflow, 0, 1, false);

        // 6. Create a task run for tracking the nested execution
        let execution_id = format!(
            "nested-{}-{}",
            workflow_id,
            chrono::Utc::now().timestamp_millis()
        );

        let input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(format!("Nested execution of workflow '{}'", workflow.name))
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_workflow_id(&workflow.id)
            .with_auto_continue(true)
            .with_workflow_type("unified");

        if let Err(e) = context.app_state.checkpoint_db.create_task_run(&input) {
            warn!("Failed to create task_run for nested workflow: {}", e);
        }

        // 7. Build LoopConfig with the single stage
        let loop_config = LoopConfig {
            max_iterations: workflow.max_iterations,
            base_prompt: format!(
                "Execute nested workflow: {} - {}",
                workflow.name, workflow.description
            ),
            workflow_name: workflow.name.clone(),
            workflow_id: workflow.id.clone(),
            execution_id: execution_id.clone(),
            targeted_error_ids: Vec::new(),
            starting_iteration: 0,
            run_agentic_first: false,
            artifact_dir: None,
            is_dev_mode: cfg!(debug_assertions),
            enable_sweep: false,
            max_sweep_iterations: 0,
            stages: vec![stage],
            stop_on_failure: true,
            constraint_overrides: std::collections::HashMap::new(),
            reflection_mode: false,
            provider_override: workflow.provider.clone(),
            model_override: workflow.model.clone(),
            model_overrides: workflow.model_overrides.clone(),
            stage_index: None,
            max_sessions: Some(workflow.max_iterations),
            auto_run_generated: false,
            approval_gate: false,
            max_context_tokens: 100_000,
            enforce_token_budget: false,
            cross_workflow_learning: true,
            verification_history: std::collections::HashMap::new(),
            routing_context: Default::default(),
            project_path: crate::mcp::shared::current_project_path(),
            acceptance_criteria: None,
            multi_agent_mode: false,
            strict_cwd: false,
            tool_tags: Vec::new(),
            use_worktree: false,
            worktree_path: None,
            worktree_branch: None,
            workflow_architecture: None,
            agentic_verification_config: None,
            multi_agent_pipeline_config: None,
            rollback_policy: crate::unified_workflow_executor::RollbackPolicy::None,
            iteration_diffs: Vec::new(),
            active_canary: None,
            is_canary_run: false,
            phase_timeout_ms: None,
        };

        // 8. Create LoopController and get session manager
        let session_manager: Arc<crate::claude_session::SessionManager> = app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();

        let mut controller = LoopController::new(
            context.app_state.clone(),
            context.config_storage.clone(),
            app_handle,
            pid_tracker,
        )
        .with_session_manager(session_manager);

        // 9. Run the nested workflow
        let result = controller
            .run(
                loop_config,
                Vec::new(), // No top-level setup (all in stages)
                Vec::new(), // No top-level setup prompts
                Vec::new(), // No top-level verification
                Vec::new(), // No top-level agentic
                Vec::new(), // No top-level completion
                Vec::new(), // No top-level completion prompts
            )
            .await;

        // 10. Pop from recursion stack after execution
        {
            let mut stack = context
                .workflow_recursion_stack
                .lock()
                .expect("recursion stack lock poisoned");
            stack.remove(&workflow_id);
        }

        // 11. Update task run status
        if result.success {
            info!("Nested workflow '{}' completed successfully", workflow.name);
            let _ = context
                .app_state
                .checkpoint_db
                .complete_task_run(&execution_id);

            StepHandlerResult::success_with_data(serde_json::json!({
                "nested_workflow_id": workflow.id,
                "nested_workflow_name": workflow.name,
                "nested_execution_id": execution_id,
                "success": true,
            }))
        } else {
            let error_msg = format!("Nested workflow '{}' failed", workflow.name);
            warn!("{}", error_msg);
            let _ = context
                .app_state
                .checkpoint_db
                .fail_task_run(&execution_id, &error_msg);

            StepHandlerResult::failure_with_data(
                error_msg,
                serde_json::json!({
                    "nested_workflow_id": workflow.id,
                    "nested_workflow_name": workflow.name,
                    "nested_execution_id": execution_id,
                    "success": false,
                }),
            )
        }
    }
}
