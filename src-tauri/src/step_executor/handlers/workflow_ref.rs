//! Workflow Ref Step Handler
//!
//! Executes a saved workflow inline with input variable substitution.
//! This is a composition-oriented handler that enables workflows to call
//! other workflows, passing variables that are substituted into the child
//! workflow's prompt using `{{key}}` template syntax.
//!
//! Differences from the plain `workflow` handler:
//! - Supports `ref_workflow_inputs`: key-value pairs substituted into the child prompt
//! - Supports `ref_inherit_model_overrides`: inherit parent model config (default: true)
//! - Respects `timeout_seconds` on the step config for execution timeout
//!
//! Includes a recursion guard (shared with `workflow` handler) to detect circular references.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tracing::{info, warn};

use super::{HandlerContext, StepHandler, StepHandlerResult};
use crate::step_executor::ExecutionStepConfig;
use crate::unified_workflow_executor::{LoopConfig, LoopController};

/// Handler for workflow_ref steps — runs a saved workflow inline with input substitution.
pub struct WorkflowRefHandler;

/// Apply `{{key}}` template substitution to a string.
fn substitute_inputs(template: &str, inputs: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in inputs {
        let placeholder = format!("{{{{{}}}}}", key); // produces {{key}}
        result = result.replace(&placeholder, value);
    }
    result
}

#[async_trait]
impl StepHandler for WorkflowRefHandler {
    fn step_type(&self) -> &'static str {
        "workflow_ref"
    }

    fn display_name(&self) -> &'static str {
        "Workflow Ref"
    }

    async fn execute(
        &self,
        step: &ExecutionStepConfig,
        context: &HandlerContext,
    ) -> StepHandlerResult {
        // 1. Extract workflow_id
        let workflow_id = match &step.ref_workflow_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => {
                return StepHandlerResult::failure(
                    "workflow_ref step: missing or empty workflow_id",
                );
            }
        };

        let workflow_name = step
            .ref_workflow_name
            .clone()
            .or_else(|| step.ci_cd_workflow_name.clone())
            .unwrap_or_else(|| workflow_id.clone());

        let inputs = step.ref_workflow_inputs.clone().unwrap_or_default();
        let inherit_model_overrides = step.ref_inherit_model_overrides.unwrap_or(true);
        let timeout_seconds = step.timeout_seconds.unwrap_or(300);

        info!(
            "Executing workflow_ref '{}' (id: {}, inputs: {}, timeout: {}s, inherit_models: {})",
            workflow_name,
            workflow_id,
            inputs.len(),
            timeout_seconds,
            inherit_model_overrides
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

        // Push onto recursion stack
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
            .pg_db
            .get_unified_workflow(&workflow_id)
            .await
        {
            Ok(Some(w)) => w,
            Ok(None) => {
                pop_recursion_stack(context, &workflow_id);
                return StepHandlerResult::failure(format!(
                    "Referenced workflow not found: '{}' (id: {})",
                    workflow_name, workflow_id
                ));
            }
            Err(e) => {
                pop_recursion_stack(context, &workflow_id);
                return StepHandlerResult::failure(format!(
                    "Failed to load workflow '{}': {}",
                    workflow_name, e
                ));
            }
        };

        // 4. Get app_handle and pid_tracker
        let app_handle = match &context.app_handle {
            Some(h) => h.clone(),
            None => {
                pop_recursion_stack(context, &workflow_id);
                return StepHandlerResult::failure(
                    "workflow_ref step: app_handle not available (cannot create LoopController)",
                );
            }
        };

        let pid_tracker = context
            .pid_tracker
            .clone()
            .unwrap_or_else(|| Arc::new(std::sync::Mutex::new(Vec::new())));

        // 5. Apply input variable substitution to the workflow's description/prompt
        let base_prompt = if inputs.is_empty() {
            format!(
                "Execute nested workflow: {} - {}",
                workflow.name, workflow.description
            )
        } else {
            let substituted_desc = substitute_inputs(&workflow.description, &inputs);
            format!(
                "Execute nested workflow: {} - {}",
                workflow.name, substituted_desc
            )
        };

        // 6. Convert workflow to stage config (no preflight for nested workflows)
        let stage = crate::unified_workflows::workflow_to_stage_config(&workflow, 0, 1, false);

        // 7. Create a task run for tracking
        let execution_id = format!(
            "workflow-ref-{}-{}",
            workflow_id,
            chrono::Utc::now().timestamp_millis()
        );

        let input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(base_prompt.clone())
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_workflow_id(&workflow.id)
            .with_auto_continue(true)
            .with_workflow_type("unified");

        // PG-primary: write to PostgreSQL first
        if let Err(e) = context.app_state.pg_db.create_task_run(&input).await {
            warn!("PG create_task_run for workflow_ref failed: {}", e);
        }
        // PG write already done above; SQLite fallback removed

        // 8. Build LoopConfig with model inheritance
        let (provider_override, model_override, model_overrides) = if inherit_model_overrides {
            // Inherit from parent — use workflow's own overrides (which may come from parent context)
            (
                workflow.provider.clone(),
                workflow.model.clone(),
                workflow.model_overrides.clone(),
            )
        } else {
            (None, None, HashMap::new())
        };

        let loop_config = LoopConfig {
            max_iterations: workflow.max_iterations,
            base_prompt,
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
            provider_override,
            model_override,
            model_overrides,
            stage_index: None,
            max_sessions: Some(workflow.max_iterations),
            auto_run_generated: false,
            approval_gate: false,
            blocking_approval: false,
            confidence_threshold: 0.85,
            max_context_tokens: 100_000,
            enforce_token_budget: false,
            cross_workflow_learning: false, // Nested workflows don't need cross-workflow learning
            verification_history: HashMap::new(),
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
            escalation_policy: crate::unified_workflow_executor::blame::EscalationPolicy::default(),
            iteration_diffs: Vec::new(),
            active_canary: None,
            is_canary_run: false,
            phase_timeout_ms: None,
        };

        // 9. Create LoopController with session manager
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

        // 10. Run with timeout
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_seconds),
            controller.run(
                loop_config,
                Vec::new(), // No top-level setup (all in stages)
                Vec::new(), // No top-level setup prompts
                Vec::new(), // No top-level verification
                Vec::new(), // No top-level agentic
                Vec::new(), // No top-level completion
                Vec::new(), // No top-level completion prompts
            ),
        )
        .await;

        // 11. Pop from recursion stack
        pop_recursion_stack(context, &workflow_id);

        // 12. Process result
        match result {
            Ok(workflow_result) => {
                if workflow_result.success {
                    info!("workflow_ref '{}' completed successfully", workflow.name);
                    // PG-primary
                    let _ = context.app_state.pg_db.complete_task_run(&execution_id).await;

                    StepHandlerResult::success_with_data(serde_json::json!({
                        "nested_workflow_id": workflow.id,
                        "nested_workflow_name": workflow.name,
                        "nested_execution_id": execution_id,
                        "success": true,
                        "inputs_applied": inputs.len(),
                    }))
                } else {
                    let error_msg = format!("workflow_ref '{}' failed", workflow.name);
                    warn!("{}", error_msg);
                    // PG-primary
                    let _ = context.app_state.pg_db.fail_task_run(&execution_id, &error_msg).await;

                    StepHandlerResult::failure_with_data(
                        error_msg,
                        serde_json::json!({
                            "nested_workflow_id": workflow.id,
                            "nested_workflow_name": workflow.name,
                            "nested_execution_id": execution_id,
                            "success": false,
                            "inputs_applied": inputs.len(),
                        }),
                    )
                }
            }
            Err(_) => {
                let error_msg = format!(
                    "workflow_ref '{}' timed out after {}s",
                    workflow.name, timeout_seconds
                );
                warn!("{}", error_msg);
                // PG-primary
                let _ = context.app_state.pg_db.fail_task_run(&execution_id, &error_msg).await;

                StepHandlerResult::failure_with_data(
                    error_msg,
                    serde_json::json!({
                        "nested_workflow_id": workflow.id,
                        "nested_workflow_name": workflow.name,
                        "nested_execution_id": execution_id,
                        "success": false,
                        "timed_out": true,
                        "timeout_seconds": timeout_seconds,
                    }),
                )
            }
        }
    }
}

/// Remove a workflow ID from the recursion stack.
fn pop_recursion_stack(context: &HandlerContext, workflow_id: &str) {
    let mut stack = context
        .workflow_recursion_stack
        .lock()
        .expect("recursion stack lock poisoned");
    stack.remove(workflow_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_inputs_empty() {
        let result = substitute_inputs("Hello world", &HashMap::new());
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_substitute_inputs_single() {
        let mut inputs = HashMap::new();
        inputs.insert("name".to_string(), "Alice".to_string());
        let result = substitute_inputs("Hello {{name}}", &inputs);
        assert_eq!(result, "Hello Alice");
    }

    #[test]
    fn test_substitute_inputs_multiple() {
        let mut inputs = HashMap::new();
        inputs.insert("repo".to_string(), "qontinui".to_string());
        inputs.insert("branch".to_string(), "main".to_string());
        let result = substitute_inputs("Clone {{repo}} on branch {{branch}}", &inputs);
        assert_eq!(result, "Clone qontinui on branch main");
    }

    #[test]
    fn test_substitute_inputs_no_match() {
        let mut inputs = HashMap::new();
        inputs.insert("name".to_string(), "Alice".to_string());
        let result = substitute_inputs("Hello {{other}}", &inputs);
        assert_eq!(result, "Hello {{other}}");
    }

    #[test]
    fn test_substitute_inputs_repeated() {
        let mut inputs = HashMap::new();
        inputs.insert("x".to_string(), "42".to_string());
        let result = substitute_inputs("{{x}} + {{x}} = ?", &inputs);
        assert_eq!(result, "42 + 42 = ?");
    }
}
