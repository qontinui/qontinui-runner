//! Workflow executor for the trigger system.
//!
//! Loads a workflow from the database, creates a task_run, builds LoopConfig,
//! and spawns it -- following the same pattern as `auto_run.rs`.

use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tracing::info;

use crate::config_storage::ConfigStorage;
use crate::unified_workflow_executor::LoopConfig;
use crate::AppState;

/// Dependencies needed to spawn triggered workflows.
pub struct TriggerExecutorDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

impl Clone for TriggerExecutorDeps {
    fn clone(&self) -> Self {
        Self {
            app_state: self.app_state.clone(),
            config_storage: self.config_storage.clone(),
            app_handle: self.app_handle.clone(),
            pid_tracker: self.pid_tracker.clone(),
        }
    }
}

/// Spawn a workflow in response to a trigger event.
///
/// Returns the execution_id (task_run_id) on success.
pub async fn execute_triggered_workflow(
    deps: &TriggerExecutorDeps,
    workflow_id: &str,
    workflow_overrides: Option<&serde_json::Value>,
    variables: &HashMap<String, String>,
    trigger_name: &str,
    chain_depth: u32,
) -> Result<String, String> {
    let db = &deps.app_state.pg_db;

    // 1. Load the workflow
    let workflow = db
        .get_unified_workflow(workflow_id)
        .await
        .map_err(|e| format!("Failed to load workflow: {}", e))?
        .ok_or_else(|| format!("Workflow not found: {}", workflow_id))?;

    info!(
        "Trigger '{}' spawning workflow '{}' (chain_depth: {})",
        trigger_name, workflow.name, chain_depth
    );

    // 1b. Check for DAG workflow routing
    if let Some(dag_def) =
        crate::workflow::dag_routing::check_dag_routing(workflow_id, db).await
    {
        let execution_id = format!(
            "trigger-{}-{}-{}",
            workflow_id,
            chrono::Utc::now().timestamp_millis(),
            &uuid::Uuid::new_v4().to_string()[..8]
        );

        let input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&format!("DAG workflow: {}", dag_def.name))
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_workflow_id(&workflow.id)
            .with_workflow_type("dag");

        db.create_task_run(&input)
            .await
            .map_err(|e| format!("Failed to create task run: {}", e))?;

        let dag_deps = crate::workflow::dag_driver::DagDriverDeps {
            app_state: deps.app_state.clone(),
            config_storage: deps.config_storage.clone(),
            app_handle: Some(deps.app_handle.clone()),
            execution_id: execution_id.clone(),
            task_run_id: Some(execution_id.clone()),
        };

        let exec_id = execution_id.clone();
        let pg = db.clone();
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result = crate::workflow::dag_driver::execute_dag_workflow(dag_def, dag_deps).await;
            let duration = start.elapsed().as_millis() as u64;
            let status = match &result {
                Ok(r) if r.success => "completed",
                Ok(_) => "failed",
                Err(_) => "failed",
            };
            if let Err(e) = pg.update_task_run_status(&exec_id, status).await {
                tracing::error!("Failed to update DAG task run status: {}", e);
            }
            // Store dag_node_metrics if available
            if let Ok(ref r) = result {
                if let Ok(metrics_json) = serde_json::to_string(&r.node_metrics) {
                    let _ = pg.set_task_run_result_data(
                        &exec_id,
                        &serde_json::json!({ "dag_node_metrics": metrics_json }).to_string(),
                    ).await;
                }
            }
            tracing::info!(execution_id = %exec_id, ?duration, ?status, "DAG workflow finished");
        });

        return Ok(execution_id);
    }

    // 2. Normalize to stages
    let normalized_stages = workflow.normalize_to_stages();
    let total_stages = normalized_stages.len();

    let stages: Vec<crate::unified_workflow_executor::StageConfig> = normalized_stages
        .iter()
        .enumerate()
        .map(|(idx, stage)| {
            crate::unified_workflows::stage_to_stage_config(
                stage,
                idx,
                total_stages,
                workflow.preflight_check_enabled,
                workflow.log_watch_enabled,
                workflow.health_check_enabled,
                &workflow.health_check_urls,
            )
        })
        .collect();

    // 3. Build combined prompt with variable substitution
    let combined_prompt = {
        let raw_prompt = normalized_stages
            .iter()
            .flat_map(|stage| stage.agentic_steps.iter())
            .filter_map(|step: &serde_json::Value| step.get("content").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        // Substitute {{variable}} patterns from trigger event
        substitute_variables(&raw_prompt, variables)
    };

    // 4. Apply overrides
    let max_iterations = workflow_overrides
        .and_then(|o| o.get("max_iterations"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(workflow.max_iterations);

    let reflection_mode = workflow_overrides
        .and_then(|o| o.get("reflection_mode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(workflow.reflection_mode);

    let stop_on_failure = workflow_overrides
        .and_then(|o| o.get("stop_on_failure"))
        .and_then(|v| v.as_bool())
        .unwrap_or(workflow.stop_on_failure);

    let provider_override = workflow_overrides
        .and_then(|o| o.get("provider"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let model_override = workflow_overrides
        .and_then(|o| o.get("model"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // 5. Create task run
    let execution_id = format!(
        "trigger-{}-{}-{}",
        workflow_id,
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );

    let run_agentic_first = !workflow.targeted_error_ids.is_empty();

    let input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_prompt(&combined_prompt)
        .with_task_type("ai")
        .with_workflow_name(&workflow.name)
        .with_workflow_id(&workflow.id)
        .with_max_sessions(max_iterations)
        .with_auto_continue(true)
        .with_workflow_type("unified");

    db.create_task_run(&input)
        .await
        .map_err(|e| format!("Failed to create task run: {}", e))?;

    // 6. Build LoopConfig
    let loop_config = LoopConfig {
        max_iterations,
        base_prompt: combined_prompt,
        workflow_name: workflow.name.clone(),
        workflow_id: workflow.id.clone(),
        execution_id: execution_id.clone(),
        targeted_error_ids: workflow.targeted_error_ids.clone(),
        starting_iteration: 0,
        run_agentic_first,
        artifact_dir: None,
        is_dev_mode: cfg!(debug_assertions),
        enable_sweep: workflow.enable_sweep,
        max_sweep_iterations: workflow.max_sweep_iterations,
        stages,
        stop_on_failure,
        constraint_overrides: workflow.constraint_overrides.clone(),
        reflection_mode,
        provider_override,
        model_override,
        model_overrides: workflow.model_overrides.clone(),
        stage_index: None,
        max_sessions: Some(max_iterations),
        auto_run_generated: false,
        approval_gate: workflow.approval_gate,
        blocking_approval: false,
        confidence_threshold: 0.85,
        max_context_tokens: 100_000,
        enforce_token_budget: workflow.enforce_token_budget,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
        acceptance_criteria: workflow.acceptance_criteria.clone(),
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
        max_fix_attempts: workflow.max_fix_attempts,
        max_ci_auto_resumes: workflow.max_ci_auto_resumes,
        ci_failure_context: None,
        htn_config: crate::planning_bridge::HtnConfig::default(),
    };

    // 7. Spawn the workflow
    let wf_name = workflow.name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());
    let file_lock = Some(deps.app_state.file_lock_manager.clone());
    let app_state = deps.app_state.clone();
    let config_storage = deps.config_storage.clone();
    let app_handle = deps.app_handle.clone();
    let pid_tracker = deps.pid_tracker.clone();

    // Check if Restate durable execution should be used
    let mut use_legacy = true;
    let restate_settings = crate::settings::load_settings().restate;
    if crate::restate::launch::should_use_restate(&restate_settings).await {
        match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
            Ok(input) => {
                if let Err(e) = deps
                    .app_state
                    .pg_db
                    .save_restate_workflow_execution(&execution_id, &execution_id, None)
                    .await
                {
                    tracing::error!("Failed to record Restate workflow: {}", e);
                }

                if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                    &execution_id,
                    &input,
                    &restate_settings.ingress_url(),
                )
                .await
                {
                    tracing::error!("Restate launch failed, falling back to legacy: {}", e);
                } else {
                    use_legacy = false;
                }
            }
            Err(e) => {
                tracing::error!("Failed to build WorkflowInput for Restate: {}", e);
            }
        }
    }

    if use_legacy {
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            execution_id.clone(),
            wf_name,
            url_lock,
            file_registry,
            file_lock,
            deps.app_state.pg_db.clone(),
            Box::pin(async move {
                let mut controller = crate::unified_workflow_executor::LoopController::new(
                    app_state,
                    config_storage,
                    app_handle.clone(),
                    pid_tracker,
                );

                // Get session manager from app handle
                let session_manager: Arc<crate::claude_session::SessionManager> = app_handle
                    .state::<Arc<crate::claude_session::SessionManager>>()
                    .inner()
                    .clone();
                controller = controller.with_session_manager(session_manager);

                controller
                    .run(
                        loop_config,
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )
                    .await
            }),
        );
    }

    info!(
        "Trigger '{}' spawned workflow '{}' -> task_run: {}",
        trigger_name, workflow.name, execution_id
    );

    Ok(execution_id)
}

/// Substitute {{variable}} patterns in a template string.
fn substitute_variables(template: &str, variables: &HashMap<String, String>) -> String {
    static PATTERN: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"\{\{([^}]+)\}\}").expect("valid regex"));

    let mut result = template.to_string();

    for cap in PATTERN.captures_iter(template) {
        let full_match = cap.get(0).expect("match group 0").as_str();
        let var_name = cap.get(1).expect("match group 1").as_str().trim();

        if let Some(value) = variables.get(var_name) {
            result = result.replace(full_match, value);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_variables() {
        let mut vars = HashMap::new();
        vars.insert("branch".to_string(), "main".to_string());
        vars.insert("message".to_string(), "fix: bug".to_string());

        let result =
            substitute_variables("Deploy {{branch}}: {{message}} ({{unknown}} stays)", &vars);

        assert_eq!(result, "Deploy main: fix: bug ({{unknown}} stays)");
    }

    #[test]
    fn test_substitute_no_variables() {
        let vars = HashMap::new();
        let result = substitute_variables("No variables here", &vars);
        assert_eq!(result, "No variables here");
    }
}
