//! Post-workflow trigger for launching reflection runs.
//!
//! Checks recursion guards and launches a reflection workflow after
//! a dev-mode workflow completes. Uses three layers of recursion prevention:
//! 1. `is_dev_mode: false` on reflection's LoopConfig
//! 2. `is_reflection` column check on source task run
//! 3. Parent hierarchy tracking

use serde::Serialize;
use std::sync::Arc;
use tracing::info;

use crate::config_storage::ConfigStorage;
use crate::AppState;

/// Dependencies required to launch a reflection workflow.
///
/// Bundles all the Arc-cloned state needed to construct a LoopController
/// and spawn the workflow. Both automatic (post-workflow) and manual
/// (API trigger) callers build this struct from their available state.
pub struct ReflectionDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    pub session_manager: Option<Arc<crate::claude_session::SessionManager>>,
}

/// Check whether a reflection run should be launched for the given task run.
///
/// Returns false if:
/// - The source task run is itself a reflection run
/// - Reflection is disabled in settings
/// - The workflow failed catastrophically (no useful data to analyze)
pub fn should_launch_reflection(source_task_run_id: &str) -> Result<bool, String> {
    // PG-primary: sync function context, use block_in_place
    let pg = crate::database::pg::PgDb::global();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg.should_launch_reflection(source_task_run_id).await })
    })
}

// =============================================================================
// Convergence Status Analysis
// =============================================================================

/// High-level convergence/stall status with human-readable explanation.
#[derive(Debug, Clone, Serialize)]
pub struct ConvergenceStatus {
    /// One of: "converging", "stalled", "improving", "regressing", "insufficient_data"
    pub status: String,
    /// Human-readable explanation of why this status was determined
    pub reason: String,
    /// Number of consecutive runs with zero findings
    pub consecutive_clean_runs: u32,
    /// Continuous convergence score (0.0-1.0)
    pub convergence_score: f64,
    /// Recurring finding titles if the workflow is stalled
    pub stall_findings: Vec<String>,
    /// Actionable recommendation for what to try next
    pub recommendation: String,
}

/// Launch a reflection workflow to analyze the given source task run.
///
/// Creates a new task run with `is_reflection = true`, builds the reflection
/// workflow programmatically, and spawns it via LoopController.
///
/// This is a synchronous function that spawns the workflow asynchronously.
// PG: complex dynamic SQL — already migrated to PG where possible
/// Database operations use PG (sync via block_on) and the workflow is launched
/// via `spawn_workflow_with_panic_guard` (fire-and-forget).
pub fn launch_reflection(
    deps: ReflectionDeps,
    source_task_run_id: String,
) -> Result<String, String> {
    // Final check before launching
    if !should_launch_reflection(&source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details via PG
    let pg = crate::database::pg::PgDb::global();
    let workflow_name = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg.get_workflow_name_for_task_run(&source_task_run_id).await })
    })?;

    // Create reflection task run ID
    let reflection_id = uuid::Uuid::new_v4().to_string();
    let reflection_name = format!("Reflection: {}", workflow_name);

    info!(
        "Launching reflection {} for source run {} (workflow: {})",
        reflection_id, source_task_run_id, workflow_name
    );

    // Create the reflection task run record
    let input = crate::database::CreateTaskRunInput::new(&reflection_id, &reflection_name)
        .with_prompt(format!(
            "Analyze the completed workflow run '{}' and apply fixes for systemic issues.",
            workflow_name
        ))
        .with_workflow_name(&reflection_name)
        .with_workflow_type("unified")
        .with_task_type("reflection")
        .with_max_sessions(2)
        .with_auto_continue(true)
        .with_is_reflection(true)
        .with_reflection_source_task_run_id(&source_task_run_id)
        .with_parent_task_run_id(&source_task_run_id);

    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg.create_task_run(&input))
    })?;

    // Build the reflection workflow
    let loop_config =
        super::workflow::build_reflection_config(&reflection_id, &reflection_name, &workflow_name);

    let setup_steps =
        super::workflow::build_setup_steps(&source_task_run_id, &workflow_name, &deps.app_state);
    let verification_steps =
        super::workflow::build_verification_steps(&source_task_run_id, &deps.app_state);
    let mut completion_steps = super::workflow::build_completion_steps(
        &workflow_name,
        &source_task_run_id,
        &deps.app_state,
    );
    // Split completion steps: first is automation (api_request), rest are prompt steps
    let completion_prompt_steps = completion_steps.split_off(1);
    let completion_automation_steps = completion_steps;

    // Build LoopController with full deps
    // Note: We do NOT use interactive sessions here (reflection workflows use
    // inline one-shot mode because the Claude CLI interactive session fails
    // during initialization when spawned from a background task).
    // However, we DO pass the session_manager for inline PID registration,
    // which gives the stale task sweep visibility into running inline processes.
    let reflection_fix_ctx = crate::mcp::shared::ReflectionFixContext {
        source_task_run_id: source_task_run_id.clone(),
        reflection_task_run_id: reflection_id.clone(),
        project_path: crate::mcp::shared::current_project_path(),
    };

    let mut controller = crate::unified_workflow_executor::LoopController::new(
        deps.app_state.clone(),
        deps.config_storage.clone(),
        deps.app_handle.clone(),
        deps.pid_tracker.clone(),
    )
    .with_reflection_fix_ctx(reflection_fix_ctx);

    // Pass session_manager for inline PID tracking (not interactive sessions)
    if let Some(sm) = deps.session_manager {
        controller = controller.with_session_manager(sm);
    }

    info!(
        "Spawning reflection workflow '{}' (id: {}) with {} setup steps",
        reflection_name,
        reflection_id,
        setup_steps.len()
    );

    // Spawn with panic guard (fire-and-forget).
    // Use Box::pin to erase the concrete future type containing LoopController::run(),
    // which prevents a recursive type cycle (run → launch_reflection → run).
    let exec_id = reflection_id.clone();
    let wf_name = reflection_name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());
    let file_lock = Some(deps.app_state.file_lock_manager.clone());

    // Check if Restate durable execution should be used
    let mut use_legacy = true;
    let restate_settings = crate::settings::load_settings().restate;
    let pg_db_restate = deps.app_state.pg_db.clone();
    let exec_id_restate = exec_id.clone();
    let restate_workflow_input =
        crate::restate::launch::build_workflow_input_from_loop_config(&loop_config);
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if crate::restate::launch::should_use_restate(&restate_settings).await {
                match restate_workflow_input {
                    Ok(input) => {
                        if let Err(e) = pg_db_restate
                            .save_restate_workflow_execution(
                                &exec_id_restate,
                                &exec_id_restate,
                                None,
                            )
                            .await
                        {
                            tracing::error!("Failed to record Restate workflow: {}", e);
                        }

                        if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                            &exec_id_restate,
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
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            exec_id,
            wf_name,
            url_lock,
            file_registry,
            file_lock,
            deps.app_state.pg_db.clone(),
            Box::pin(async move {
                controller
                    .run(
                        loop_config,
                        setup_steps,        // setup automation steps (API requests)
                        Vec::new(),         // setup prompt steps (none)
                        verification_steps, // verification steps
                        Vec::new(),         // agentic steps (prompt is in loop_config.base_prompt)
                        completion_automation_steps, // completion automation steps (batch evaluation)
                        completion_prompt_steps,     // completion prompt steps
                    )
                    .await
            }),
        );
    }

    info!(
        "Reflection workflow '{}' spawned for source {}",
        reflection_name, source_task_run_id
    );

    Ok(reflection_id)
}

// =============================================================================
// Project Reflection
// =============================================================================

/// Check whether a project reflection should be launched.
///
/// Similar to `should_launch_reflection` but uses project-scoped convergence
/// and has its own "already running" guard keyed by project path.
pub fn should_launch_project_reflection(source_task_run_id: &str) -> Result<bool, String> {
    // PG-primary: sync function context, use block_in_place
    let pg = crate::database::pg::PgDb::global();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg.should_launch_project_reflection(source_task_run_id)
                .await
        })
    })
}

/// Launch a project reflection workflow to learn about the user's project.
///
/// Similar to `launch_reflection` but uses project-scoped workflow builder
/// and stores fixes with `reflection_scope = 'project'`.
pub fn launch_project_reflection(
    deps: ReflectionDeps,
    source_task_run_id: String,
) -> Result<String, String> {
    if !should_launch_project_reflection(&source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details via PG
    let pg = crate::database::pg::PgDb::global();
    let (workflow_name, project_path) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg.get_workflow_name_and_project_path(&source_task_run_id)
                .await
        })
    })?;

    let reflection_id = uuid::Uuid::new_v4().to_string();
    let reflection_name = format!("Project Reflection: {}", workflow_name);

    info!(
        "Launching project reflection {} for source run {} (workflow: {}, project: {:?})",
        reflection_id, source_task_run_id, workflow_name, project_path
    );

    // Create the task run record
    let input = crate::database::CreateTaskRunInput::new(&reflection_id, &reflection_name)
        .with_prompt(format!(
            "Learn about the project from the completed workflow run '{}'.",
            workflow_name
        ))
        .with_workflow_name(&reflection_name)
        .with_workflow_type("unified")
        .with_task_type("reflection")
        .with_max_sessions(2)
        .with_auto_continue(true)
        .with_is_reflection(true)
        .with_reflection_source_task_run_id(&source_task_run_id)
        .with_parent_task_run_id(&source_task_run_id);

    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg.create_task_run(&input))
    })?;

    // Build the project reflection workflow
    let loop_config = super::workflow::build_project_reflection_config(
        &reflection_id,
        &reflection_name,
        &workflow_name,
        project_path.clone(),
    );

    let setup_steps = super::workflow::build_project_setup_steps(
        &source_task_run_id,
        &workflow_name,
        &deps.app_state,
    );
    let verification_steps =
        super::workflow::build_project_verification_steps(&source_task_run_id, &deps.app_state);
    let mut completion_steps =
        super::workflow::build_project_completion_steps(&workflow_name, &deps.app_state);
    let completion_prompt_steps = completion_steps.split_off(1);
    let completion_automation_steps = completion_steps;

    // Build LoopController — same pattern as workflow reflection
    let reflection_fix_ctx = crate::mcp::shared::ReflectionFixContext {
        source_task_run_id: source_task_run_id.clone(),
        reflection_task_run_id: reflection_id.clone(),
        project_path,
    };

    let mut controller = crate::unified_workflow_executor::LoopController::new(
        deps.app_state.clone(),
        deps.config_storage.clone(),
        deps.app_handle.clone(),
        deps.pid_tracker.clone(),
    )
    .with_reflection_fix_ctx(reflection_fix_ctx);

    if let Some(sm) = deps.session_manager {
        controller = controller.with_session_manager(sm);
    }

    info!(
        "Spawning project reflection '{}' (id: {}) with {} setup steps",
        reflection_name,
        reflection_id,
        setup_steps.len()
    );

    let exec_id = reflection_id.clone();
    let wf_name = reflection_name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());
    let file_lock = Some(deps.app_state.file_lock_manager.clone());

    // Check if Restate durable execution should be used
    let mut use_legacy = true;
    let restate_settings = crate::settings::load_settings().restate;
    let pg_db_restate = deps.app_state.pg_db.clone();
    let exec_id_restate = exec_id.clone();
    let restate_workflow_input =
        crate::restate::launch::build_workflow_input_from_loop_config(&loop_config);
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if crate::restate::launch::should_use_restate(&restate_settings).await {
                match restate_workflow_input {
                    Ok(input) => {
                        if let Err(e) = pg_db_restate
                            .save_restate_workflow_execution(
                                &exec_id_restate,
                                &exec_id_restate,
                                None,
                            )
                            .await
                        {
                            tracing::error!("Failed to record Restate workflow: {}", e);
                        }

                        if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                            &exec_id_restate,
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
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            exec_id,
            wf_name,
            url_lock,
            file_registry,
            file_lock,
            deps.app_state.pg_db.clone(),
            Box::pin(async move {
                controller
                    .run(
                        loop_config,
                        setup_steps,
                        Vec::new(),
                        verification_steps,
                        Vec::new(),
                        completion_automation_steps,
                        completion_prompt_steps,
                    )
                    .await
            }),
        );
    }

    info!(
        "Project reflection '{}' spawned for source {}",
        reflection_name, source_task_run_id
    );

    Ok(reflection_id)
}

// =============================================================================
// UI Bridge Reflection
// =============================================================================

/// Check whether a UI Bridge reflection should be launched.
///
/// Similar guards to other reflection types, plus checks for UI Bridge
/// activity in the source workflow (skips if workflow had no UI Bridge usage).
pub fn should_launch_ui_bridge_reflection(source_task_run_id: &str) -> Result<bool, String> {
    // PG-primary: sync function context, use block_in_place
    let pg = crate::database::pg::PgDb::global();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg.should_launch_ui_bridge_reflection(source_task_run_id)
                .await
        })
    })
}

/// Launch a UI Bridge reflection workflow.
///
/// Analyzes UI Bridge usage in the source workflow, identifies improvements,
/// and implements them directly.
pub fn launch_ui_bridge_reflection(
    deps: ReflectionDeps,
    source_task_run_id: String,
) -> Result<String, String> {
    if !should_launch_ui_bridge_reflection(&source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details via PG
    let pg = crate::database::pg::PgDb::global();
    let workflow_name = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg.get_workflow_name_for_task_run(&source_task_run_id).await })
    })?;

    let reflection_id = uuid::Uuid::new_v4().to_string();
    let reflection_name = format!("UI Bridge Reflection: {}", workflow_name);

    info!(
        "Launching UI Bridge reflection {} for source run {} (workflow: {})",
        reflection_id, source_task_run_id, workflow_name
    );

    // Create the task run record
    let input = crate::database::CreateTaskRunInput::new(&reflection_id, &reflection_name)
        .with_prompt(format!(
            "Analyze UI Bridge usage in '{}' and implement improvements.",
            workflow_name
        ))
        .with_workflow_name(&reflection_name)
        .with_workflow_type("unified")
        .with_task_type("reflection")
        .with_max_sessions(2)
        .with_auto_continue(true)
        .with_is_reflection(true)
        .with_reflection_source_task_run_id(&source_task_run_id)
        .with_parent_task_run_id(&source_task_run_id);

    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg.create_task_run(&input))
    })?;

    // Build the UI Bridge reflection workflow
    let loop_config = super::workflow::build_ui_bridge_reflection_config(
        &reflection_id,
        &reflection_name,
        &workflow_name,
    );

    let setup_steps = super::workflow::build_ui_bridge_setup_steps(
        &source_task_run_id,
        &workflow_name,
        &deps.app_state,
    );
    let verification_steps =
        super::workflow::build_ui_bridge_verification_steps(&source_task_run_id, &deps.app_state);
    let mut completion_steps =
        super::workflow::build_ui_bridge_completion_steps(&workflow_name, &deps.app_state);
    let completion_prompt_steps = completion_steps.split_off(1);
    let completion_automation_steps = completion_steps;

    // Build LoopController
    let reflection_fix_ctx = crate::mcp::shared::ReflectionFixContext {
        source_task_run_id: source_task_run_id.clone(),
        reflection_task_run_id: reflection_id.clone(),
        project_path: crate::mcp::shared::current_project_path(),
    };

    let mut controller = crate::unified_workflow_executor::LoopController::new(
        deps.app_state.clone(),
        deps.config_storage.clone(),
        deps.app_handle.clone(),
        deps.pid_tracker.clone(),
    )
    .with_reflection_fix_ctx(reflection_fix_ctx);

    if let Some(sm) = deps.session_manager {
        controller = controller.with_session_manager(sm);
    }

    info!(
        "Spawning UI Bridge reflection '{}' (id: {}) with {} setup steps",
        reflection_name,
        reflection_id,
        setup_steps.len()
    );

    let exec_id = reflection_id.clone();
    let wf_name = reflection_name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());
    let file_lock = Some(deps.app_state.file_lock_manager.clone());

    // Check if Restate durable execution should be used
    let mut use_legacy = true;
    let restate_settings = crate::settings::load_settings().restate;
    let pg_db_restate = deps.app_state.pg_db.clone();
    let exec_id_restate = exec_id.clone();
    let restate_workflow_input =
        crate::restate::launch::build_workflow_input_from_loop_config(&loop_config);
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            if crate::restate::launch::should_use_restate(&restate_settings).await {
                match restate_workflow_input {
                    Ok(input) => {
                        if let Err(e) = pg_db_restate
                            .save_restate_workflow_execution(
                                &exec_id_restate,
                                &exec_id_restate,
                                None,
                            )
                            .await
                        {
                            tracing::error!("Failed to record Restate workflow: {}", e);
                        }

                        if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                            &exec_id_restate,
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
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            exec_id,
            wf_name,
            url_lock,
            file_registry,
            file_lock,
            deps.app_state.pg_db.clone(),
            Box::pin(async move {
                controller
                    .run(
                        loop_config,
                        setup_steps,
                        Vec::new(),
                        verification_steps,
                        Vec::new(),
                        completion_automation_steps,
                        completion_prompt_steps,
                    )
                    .await
            }),
        );
    }

    info!(
        "UI Bridge reflection '{}' spawned for source {}",
        reflection_name, source_task_run_id
    );

    Ok(reflection_id)
}
