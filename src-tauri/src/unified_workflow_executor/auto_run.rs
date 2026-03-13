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
    let db = &deps.app_state.checkpoint_db;

    // 1. Get the meta-workflow's task run to read result_data
    let task_run = db
        .get_task_run(meta_task_run_id)
        .map_err(|e| format!("Failed to get task run: {}", e))?
        .ok_or_else(|| format!("Meta task run not found: {}", meta_task_run_id))?;

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

    info!(
        "Auto-running generated workflow {} from meta-workflow {}",
        generated_workflow_id, meta_task_run_id
    );

    // 3. Load the generated workflow
    let workflow = db
        .get_unified_workflow(&generated_workflow_id)
        .map_err(|e| format!("Failed to load generated workflow: {}", e))?
        .ok_or_else(|| format!("Generated workflow not found: {}", generated_workflow_id))?;

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

    let input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_prompt(&combined_prompt)
        .with_task_type("ai")
        .with_workflow_name(&workflow.name)
        .with_workflow_id(&workflow.id)
        .with_max_sessions(workflow.max_iterations)
        .with_auto_continue(true)
        .with_workflow_type("unified")
        .with_parent_task_run_id(meta_task_run_id);

    db.create_task_run(&input)
        .map_err(|e| format!("Failed to create task run: {}", e))?;

    let loop_config = LoopConfig {
        max_iterations: workflow.max_iterations,
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
        max_sessions: Some(workflow.max_iterations),
        auto_run_generated: false, // Don't cascade auto-run
        approval_gate: workflow.approval_gate,
        max_context_tokens: 100_000,
        cross_workflow_learning: true,
        verification_history: std::collections::HashMap::new(),
        routing_context: Default::default(),
        project_path: crate::mcp::shared::current_project_path(),
    };

    // 6. Spawn the workflow
    let wf_name = workflow.name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let checkpoint_db = deps.app_state.checkpoint_db.clone();
    let app_state = deps.app_state;
    let config_storage = deps.config_storage;
    let app_handle = deps.app_handle;
    let pid_tracker = deps.pid_tracker;

    super::spawn_workflow_with_panic_guard(
        checkpoint_db,
        execution_id.clone(),
        wf_name,
        url_lock,
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

    info!(
        "Auto-run launched: workflow '{}' (task_run: {})",
        workflow.name, execution_id
    );

    Ok(execution_id)
}
