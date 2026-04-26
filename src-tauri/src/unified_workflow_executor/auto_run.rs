//! Auto-run trigger for generated workflows.
//!
//! After a meta-workflow ("Generate & Run") completes, this module reads the
//! `result_data.generated_workflow_id` from the task run and spawns the
//! generated workflow for execution.
//!
//! This is a separate module (like `reflection/trigger.rs`) to avoid recursive
//! async type cycles in the compiler: `run_multi_stage` → spawn → `LoopController::run`.

use std::sync::Arc;
use tauri::Manager;
use tracing::info;

use crate::config_storage::ConfigStorage;
use crate::unified_workflows::UnifiedWorkflowExt;
use crate::AppState;

use super::types::LoopConfig;

/// Dependencies required to launch a generated workflow.
pub struct AutoRunDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

/// Launch the generated workflow after a meta-workflow ("Generate & Run") completes.
///
/// Reads `result_data.generated_workflow_id` from the meta-workflow's task run,
/// loads the generated workflow, creates a new task run, and spawns it.
///
/// This is a synchronous function (like `launch_reflection`) to avoid recursive
/// async type cycles.
pub fn launch_generated_workflow(
    deps: AutoRunDeps,
    meta_task_run_id: &str,
) -> Result<String, String> {
    // 1. Get the meta-workflow's task run to read result_data
    let pg_db = &deps.app_state.pg_db;
    let task_run = {
        let pg = pg_db.clone();
        let id = meta_task_run_id.to_string();
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| "No tokio runtime".to_string())?;
        handle
            .block_on(async move { pg.get_task_run(&id).await })
            .map_err(|e| format!("Failed to get task run: {}", e))?
            .ok_or_else(|| format!("Meta task run not found: {}", meta_task_run_id))?
    };

    // 2. Extract generated_workflow_id from result_data
    let result_data = task_run
        .result_data
        .as_ref()
        .ok_or("No result_data on meta-workflow task run")?;

    let result_json: serde_json::Value = serde_json::from_str(result_data)
        .map_err(|e| format!("Failed to parse result_data: {}", e))?;

    let generated_workflow_id = result_json
        .get("generated_workflow_id")
        .and_then(|v| v.as_str())
        .ok_or("No generated_workflow_id in result_data")?
        .to_string();

    launch_workflow_by_id(deps, &generated_workflow_id, Some(meta_task_run_id))
}

/// Launch a generated workflow by ID, optionally linked to a parent task run.
///
/// Loads the workflow from PG, builds a `LoopConfig`, and dispatches it via
/// Restate (if enabled) or the legacy executor. Returns the execution ID of
/// the spawned workflow.
///
/// This helper is called from `launch_generated_workflow` (meta-workflow flow)
/// and from the server-mode HTTP dispatch endpoint.
pub fn launch_workflow_by_id(
    deps: AutoRunDeps,
    generated_workflow_id: &str,
    parent_task_run_id: Option<&str>,
) -> Result<String, String> {
    let pg_db = &deps.app_state.pg_db;

    info!(
        "Auto-running generated workflow {} (parent={:?})",
        generated_workflow_id, parent_task_run_id
    );

    // 2b. Check for DAG workflow routing
    {
        let pg = pg_db.clone();
        let wf_id = generated_workflow_id.to_string();
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| "No tokio runtime".to_string())?;
        let dag_def = handle.block_on(async move {
            crate::workflow::dag_routing::check_dag_routing(&wf_id, &pg).await
        });

        if let Some(dag_def) = dag_def {
            let execution_id = format!(
                "unified-workflow-{}-{}",
                generated_workflow_id,
                chrono::Utc::now().timestamp_millis()
            );

            let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &dag_def.name)
                .with_prompt(format!("DAG workflow: {}", dag_def.name))
                .with_task_type("ai")
                .with_workflow_name(&dag_def.name)
                .with_workflow_id(generated_workflow_id)
                .with_workflow_type("dag");
            if let Some(parent) = parent_task_run_id {
                input = input.with_parent_task_run_id(parent);
            }

            let pg_create = pg_db.clone();
            let input_clone = input.clone();
            let handle = tokio::runtime::Handle::try_current()
                .map_err(|_| "No tokio runtime".to_string())?;
            handle
                .block_on(async move { pg_create.create_task_run(&input_clone).await })
                .map_err(|e| format!("Failed to create task run: {}", e))?;

            let dag_deps = crate::workflow::dag_driver::DagDriverDeps {
                app_state: deps.app_state.clone(),
                config_storage: deps.config_storage.clone(),
                app_handle: Some(deps.app_handle.clone()),
                execution_id: execution_id.clone(),
                task_run_id: Some(execution_id.clone()),
            };

            let exec_id = execution_id.clone();
            let pg_spawn = pg_db.clone();
            tokio::spawn(async move {
                let start = std::time::Instant::now();
                let result =
                    crate::workflow::dag_driver::execute_dag_workflow(dag_def, dag_deps).await;
                let duration = start.elapsed().as_millis() as u64;
                let status = match &result {
                    Ok(r) if r.success => "completed",
                    Ok(_) => "failed",
                    Err(_) => "failed",
                };
                let _ = pg_spawn.update_task_run_status(&exec_id, status).await;
                if let Ok(ref r) = result {
                    if let Ok(metrics_json) = serde_json::to_string(&r.node_metrics) {
                        let _ = pg_spawn
                            .set_task_run_result_data(
                                &exec_id,
                                &serde_json::json!({ "dag_node_metrics": metrics_json })
                                    .to_string(),
                            )
                            .await;
                    }
                }
                tracing::info!(execution_id = %exec_id, ?duration, ?status, "DAG workflow finished");
            });

            return Ok(execution_id);
        }
    }

    // 3. Load the generated workflow (via PG)
    let workflow = {
        let pg = pg_db.clone();
        let wf_id = generated_workflow_id.to_string();
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| "No tokio runtime".to_string())?;
        handle
            .block_on(async move { pg.get_unified_workflow(&wf_id).await })
            .map_err(|e| format!("Failed to load generated workflow: {}", e))?
            .ok_or_else(|| format!("Generated workflow not found: {}", generated_workflow_id))?
    };

    // 4. Normalize to stages and build config
    let normalized_stages = workflow.normalize_to_stages();
    let total_stages = normalized_stages.len();

    let stages: Vec<super::types::StageConfig> = normalized_stages
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

    let combined_prompt = normalized_stages
        .iter()
        .flat_map(|stage| stage.agentic_steps.iter())
        .filter_map(|step| step.get("content").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    // 5. Create task run for the generated workflow
    let execution_id = format!(
        "unified-workflow-{}-{}",
        generated_workflow_id,
        chrono::Utc::now().timestamp_millis()
    );

    let run_agentic_first = !workflow.targeted_error_ids.is_empty();

    let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_prompt(&combined_prompt)
        .with_task_type("ai")
        .with_workflow_name(&workflow.name)
        .with_workflow_id(&workflow.id)
        .with_max_sessions(workflow.iter_cap())
        .with_auto_continue(true)
        .with_workflow_type("unified");
    if let Some(parent) = parent_task_run_id {
        input = input.with_parent_task_run_id(parent);
    }

    {
        let pg = pg_db.clone();
        let input_clone = input.clone();
        let handle =
            tokio::runtime::Handle::try_current().map_err(|_| "No tokio runtime".to_string())?;
        handle
            .block_on(async move { pg.create_task_run(&input_clone).await })
            .map_err(|e| format!("Failed to create task run: {}", e))?;
    }

    let loop_config = LoopConfig {
        max_iterations: workflow.iter_cap(),
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
        stop_on_failure: workflow.stop_on_failure,
        constraint_overrides: workflow.constraint_overrides.clone(),
        reflection_mode: workflow.reflection_mode,
        provider_override: None,
        model_override: None,
        model_overrides: workflow.model_overrides.clone(),
        stage_index: None,
        max_sessions: workflow.max_iterations,
        auto_run_generated: false, // Don't cascade auto-run
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
        auto_commit_subagents: None,
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

    // 6. Spawn the workflow
    let wf_name = workflow.name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());
    let file_lock = Some(deps.app_state.file_lock_manager.clone());
    let pg_db_spawn = deps.app_state.pg_db.clone();
    let app_state = deps.app_state;
    let config_storage = deps.config_storage;
    let app_handle = deps.app_handle;
    let pid_tracker = deps.pid_tracker;

    // Check if Restate durable execution should be used
    let mut use_legacy = true;
    let restate_settings = crate::settings::load_settings().restate;
    let restate_workflow_input =
        crate::restate::launch::build_workflow_input_from_loop_config(&loop_config);
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if crate::restate::launch::should_use_restate(&restate_settings).await {
                match restate_workflow_input {
                    Ok(input) => {
                        if let Err(e) = pg_db_spawn
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
        })
    });

    if use_legacy {
        super::spawn_workflow_with_panic_guard(
            execution_id.clone(),
            wf_name,
            url_lock,
            file_registry,
            file_lock,
            pg_db_spawn,
            Box::pin(async move {
                let mut controller = super::LoopController::new(
                    app_state,
                    config_storage,
                    app_handle.clone(),
                    pid_tracker,
                );

                // Get session manager from app handle for interactive mode
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
        "Auto-run launched: workflow '{}' (task_run: {})",
        workflow.name, execution_id
    );

    Ok(execution_id)
}
