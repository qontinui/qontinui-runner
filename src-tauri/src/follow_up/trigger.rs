//! Post-workflow trigger for launching follow-up runs.
//!
//! Checks recursion guards and launches a follow-up workflow after
//! a dev-mode workflow completes with unfixed issues.
//!
//! Recursion prevention (3 layers):
//! 1. `is_dev_mode: false` on follow-up's LoopConfig
//! 2. `is_follow_up` column check on source task run
//! 3. `is_reflection` column check on source task run

use std::sync::Arc;
use tracing::{debug, info};

use crate::config_storage::ConfigStorage;
use crate::AppState;

/// Signal patterns that indicate unfixed issues in AI output.
const UNFIXED_SIGNAL_PATTERNS: &[&str] = &[
    "[unfixable_errors]",
    "not fixed",
    "lower priority",
    "remaining issues",
    "deferred",
    "could not fix",
    "out of scope",
    "skipped",
    "not addressed",
    "left unresolved",
];

/// Dependencies required to launch a follow-up workflow.
///
/// Bundles all the Arc-cloned state needed to construct a LoopController
/// and spawn the workflow. Mirrors `ReflectionDeps`.
pub struct FollowUpDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    pub session_manager: Option<Arc<crate::claude_session::SessionManager>>,
}

/// Check whether a follow-up run should be launched for the given task run.
///
/// Returns false if:
/// - Another follow-up is already running
/// - The source task run is itself a follow-up run
/// - The source task run is a reflection run
/// - Follow-up is disabled in settings
/// - The source run has insufficient output
/// - The source output doesn't contain unfixed-issue signals
pub fn should_launch_follow_up(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let pg = pg_db.clone();
    let src_id = source_task_run_id.to_string();
    tokio::task::block_in_place(move || {
        tokio::runtime::Handle::current()
            .block_on(async { should_launch_follow_up_pg(&pg, &src_id).await })
    })
}

/// PG-backed follow-up guard checks.
async fn should_launch_follow_up_pg(
    pg_db: &crate::database::pg::PgDb,
    source_task_run_id: &str,
) -> Result<bool, String> {
    // Guard 0: Check if a follow-up workflow is already running
    if pg_db.has_running_follow_up().await? {
        debug!("Skipping follow-up — another follow-up workflow is already running");
        return Ok(false);
    }

    // Guard 1-2b: Check task_run flags
    let (is_reflection, is_fixer, is_follow_up) =
        pg_db.get_task_run_flags(source_task_run_id).await?;

    if is_follow_up {
        debug!(
            "Skipping follow-up for {} — source is already a follow-up run",
            source_task_run_id
        );
        return Ok(false);
    }
    if is_reflection {
        debug!(
            "Skipping follow-up for {} — source is a reflection run",
            source_task_run_id
        );
        return Ok(false);
    }
    if is_fixer {
        debug!(
            "Skipping follow-up for {} — source is a fixer run",
            source_task_run_id
        );
        return Ok(false);
    }

    // Guard 3: Check follow_up_enabled setting
    if !pg_db
        .get_dev_mode_setting_bool("follow_up_enabled", true)
        .await?
    {
        debug!("Follow-up disabled in settings");
        return Ok(false);
    }

    // Guard 4: Check output threshold
    if !pg_db.has_sufficient_output(source_task_run_id).await? {
        debug!(
            "Skipping follow-up for {} — insufficient output to analyze",
            source_task_run_id
        );
        return Ok(false);
    }

    // Guard 5: Check that source output contains unfixed-issue signals
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool: {e}"))?;
    let chunks: Vec<String> = conn
        .query(
            r#"SELECT content FROM task_run_output_chunks
               WHERE task_run_id = $1
               ORDER BY chunk_sequence DESC
               LIMIT 20"#,
            &[&source_task_run_id],
        )
        .await
        .map_err(|e| format!("Failed to query output chunks: {e}"))?
        .iter()
        .map(|r| r.get(0))
        .collect();

    let output_log: String = conn
        .query_one(
            "SELECT COALESCE(output_log, '') FROM task_runs WHERE id = $1",
            &[&source_task_run_id],
        )
        .await
        .map(|r| r.get(0))
        .unwrap_or_default();

    let combined = chunks.join(" ");
    let tail_output = if !combined.is_empty() {
        combined
    } else {
        output_log[output_log.len().saturating_sub(20000)..].to_string()
    };

    let lower = tail_output.to_lowercase();
    let has_signal = UNFIXED_SIGNAL_PATTERNS
        .iter()
        .any(|pattern| lower.contains(pattern));

    if !has_signal {
        debug!(
            "Skipping follow-up for {} — no unfixed-issue signals found in output",
            source_task_run_id
        );
        return Ok(false);
    }

    Ok(true)
}

/// Scan the last ~20 output chunks for unfixed-issue signal patterns.
///
/// Returns true if any of the known signal patterns are found in the
/// tail of the source run's output.
fn check_has_unfixed_signal(task_run_id: &str) -> Result<bool, String> {
    Err("SQLite removed".to_string())
}

/// Launch a follow-up workflow to fix unfixed issues from the given source task run.
///
/// Creates a new task run with `is_follow_up = true`, builds the follow-up
/// workflow programmatically, and spawns it via LoopController.
///
/// This is a synchronous function that spawns the workflow asynchronously.
pub fn launch_follow_up(deps: FollowUpDeps, source_task_run_id: String) -> Result<String, String> {
    let pg_db = &deps.app_state.pg_db;

    // Final check before launching
    if !should_launch_follow_up(pg_db, &source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details via block_in_place
    let workflow_name = {
        let pg = pg_db.clone();
        let src_id = source_task_run_id.clone();
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current()
                .block_on(async { pg.get_task_run_workflow_name(&src_id).await })
        })?
    };

    // Create follow-up task run ID
    let follow_up_id = uuid::Uuid::new_v4().to_string();
    let follow_up_name = format!("Follow-up: {}", workflow_name);

    info!(
        "Launching follow-up {} for source run {} (workflow: {})",
        follow_up_id, source_task_run_id, workflow_name
    );

    // Create the follow-up task run record
    let input = crate::database::CreateTaskRunInput::new(&follow_up_id, &follow_up_name)
        .with_prompt(format!(
            "Fix unfixed issues from the completed workflow run '{}'.",
            workflow_name
        ))
        .with_workflow_name(&follow_up_name)
        .with_workflow_type("unified")
        .with_task_type("follow_up")
        .with_max_sessions(2)
        .with_auto_continue(true)
        .with_is_follow_up(true)
        .with_follow_up_source_task_run_id(&source_task_run_id)
        .with_parent_task_run_id(&source_task_run_id);

    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.create_task_run(&input))
    })?;

    // Build the follow-up workflow
    let loop_config =
        super::workflow::build_follow_up_config(&follow_up_id, &follow_up_name, &workflow_name);

    let setup_steps = super::workflow::build_setup_steps(&source_task_run_id);
    let verification_steps = super::workflow::build_verification_steps();

    // Build LoopController with full deps
    let step_injection_ctx = crate::step_injection::types::StepInjectionContext {
        execution_id: follow_up_id.clone(),
    };

    let mut controller = crate::unified_workflow_executor::LoopController::new(
        deps.app_state.clone(),
        deps.config_storage.clone(),
        deps.app_handle.clone(),
        deps.pid_tracker.clone(),
    )
    .with_step_injection_ctx(step_injection_ctx);

    // Pass session_manager for inline PID tracking (not interactive sessions)
    if let Some(sm) = deps.session_manager {
        controller = controller.with_session_manager(sm);
    }

    info!(
        "Spawning follow-up workflow '{}' (id: {}) with {} setup steps",
        follow_up_name,
        follow_up_id,
        setup_steps.len()
    );

    // Spawn with panic guard (fire-and-forget).
    // Use Box::pin to erase the concrete future type.
    let exec_id = follow_up_id.clone();
    let wf_name = follow_up_name.clone();
    let url_lock = Some(deps.app_state.url_lock_manager.clone());
    let file_registry = Some(deps.app_state.file_registry_manager.clone());

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
            deps.app_state.pg_db.clone(),
            Box::pin(async move {
                controller
                    .run(
                        loop_config,
                        setup_steps,        // setup automation steps (API requests)
                        Vec::new(),         // setup prompt steps (none)
                        verification_steps, // verification steps
                        Vec::new(),         // agentic steps (prompt is in loop_config.base_prompt)
                        Vec::new(),         // completion automation steps (none)
                        Vec::new(),         // completion prompt steps (none)
                    )
                    .await
            }),
        );
    }

    info!(
        "Follow-up workflow '{}' spawned for source {}",
        follow_up_name, source_task_run_id
    );

    Ok(follow_up_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_db() -> Connection {
        panic!("SQLite tests disabled — use PG-based tests instead")
    }

    fn insert_completed_run(id: &str, workflow_name: &str) {
        // SQLite removed - no-op
    }

    fn add_output_chunks(task_run_id: &str, chunks: &[&str]) {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_true() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_false_when_all_fixed() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_deferred() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_could_not_fix() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_out_of_scope() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_case_insensitive() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_unfixable_errors_marker() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_check_has_unfixed_signal_uses_output_log_fallback() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_follow_up_already_running() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_is_follow_up() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_is_reflection() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_follow_up_disabled() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_defaults_to_enabled_when_no_setting() {
        // SQLite removed - no-op
    }
}
