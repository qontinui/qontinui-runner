//! Post-workflow trigger for launching reflection runs.
//!
//! Checks recursion guards and launches a reflection workflow after
//! a dev-mode workflow completes. Uses three layers of recursion prevention:
//! 1. `is_dev_mode: false` on reflection's LoopConfig
//! 2. `is_reflection` column check on source task run
//! 3. Parent hierarchy tracking

use std::sync::Arc;
use tauri::Manager;
use tracing::{debug, info};

use crate::config_storage::ConfigStorage;
use crate::database::CheckpointDb;
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
}

/// Check whether a reflection run should be launched for the given task run.
///
/// Returns false if:
/// - The source task run is itself a reflection run
/// - Reflection is disabled in settings
/// - The workflow failed catastrophically (no useful data to analyze)
pub fn should_launch_reflection(
    db: &CheckpointDb,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let source_id = source_task_run_id.to_string();

    db.with_conn(|conn| {
        // Guard 1: Check if source task run is already a reflection run
        let is_reflection: bool = conn
            .query_row(
                "SELECT COALESCE(is_reflection, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Failed to check is_reflection: {}", e))?;

        if is_reflection {
            debug!(
                "Skipping reflection for {} — source is already a reflection run",
                source_id
            );
            return Ok(false);
        }

        // Guard 2: Check reflection_enabled setting
        let reflection_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(json_extract(value, '$.reflection_enabled'), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true); // Default to enabled if setting doesn't exist

        if !reflection_enabled {
            debug!("Reflection disabled in settings");
            return Ok(false);
        }

        // Guard 3: Check that the source run has meaningful output to analyze
        // Use output chunks table since output_log column may be empty for chunked storage
        let has_output: bool = conn
            .query_row(
                r#"SELECT COALESCE(
                    (SELECT SUM(LENGTH(content)) FROM task_run_output_chunks WHERE task_run_id = ?1),
                    0
                ) + LENGTH(COALESCE((SELECT output_log FROM task_runs WHERE id = ?1), '')) > 100"#,
                rusqlite::params![source_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_output {
            debug!(
                "Skipping reflection for {} — insufficient output to analyze",
                source_id
            );
            return Ok(false);
        }

        Ok(true)
    })
}

/// Launch a reflection workflow to analyze the given source task run.
///
/// Creates a new task run with `is_reflection = true`, builds the reflection
/// workflow programmatically, and spawns it via LoopController.
///
/// This is a synchronous function that spawns the workflow asynchronously.
/// All database operations use `with_conn` (sync) and the workflow is launched
/// via `spawn_workflow_with_panic_guard` (fire-and-forget).
pub fn launch_reflection(
    deps: ReflectionDeps,
    source_task_run_id: String,
) -> Result<String, String> {
    let db = &deps.app_state.checkpoint_db;

    // Final check before launching
    if !should_launch_reflection(db, &source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details
    let source_id = source_task_run_id.clone();
    let workflow_name = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = ?1",
            rusqlite::params![source_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("Failed to get source task run: {}", e))
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

    db.create_task_run(&input)?;

    // Build the reflection workflow
    let loop_config = super::workflow::build_reflection_config(
        &reflection_id,
        &reflection_name,
        &workflow_name,
    );

    let setup_steps = super::workflow::build_setup_steps(&source_task_run_id, &workflow_name);
    let verification_steps = super::workflow::build_verification_steps();
    let completion_prompt_steps = super::workflow::build_completion_steps(&workflow_name);

    // Build LoopController with full deps
    let session_manager: Arc<crate::claude_session::SessionManager> = deps
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    let mut controller = crate::unified_workflow_executor::LoopController::new(
        deps.app_state.clone(),
        deps.config_storage.clone(),
        deps.app_handle.clone(),
        deps.pid_tracker.clone(),
    )
    .with_session_manager(session_manager);

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
    crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
        deps.app_state.checkpoint_db.clone(),
        exec_id,
        wf_name,
        Box::pin(async move {
            controller
                .run(
                    loop_config,
                    setup_steps,             // setup automation steps (API requests)
                    Vec::new(),              // setup prompt steps (none)
                    verification_steps,      // verification steps
                    Vec::new(),              // agentic steps (prompt is in loop_config.base_prompt)
                    Vec::new(),              // completion automation steps (none)
                    completion_prompt_steps,  // completion prompt steps
                )
                .await
        }),
    );

    info!(
        "Reflection workflow '{}' spawned for source {}",
        reflection_name, source_task_run_id
    );

    Ok(reflection_id)
}
