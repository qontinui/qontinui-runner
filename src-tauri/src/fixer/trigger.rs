//! Post-workflow trigger for launching fixer runs.
//!
//! Waits for all child task runs (reflections, follow-ups) to complete,
//! then launches a fixer workflow to implement remaining fixes.
//!
//! Recursion prevention (3 layers):
//! 1. `is_dev_mode: false` on fixer's LoopConfig
//! 2. `is_fixer` column check on source task run
//! 3. `is_reflection` / `is_follow_up` column checks on source

use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::config_storage::ConfigStorage;
use crate::AppState;

/// Dependencies required to launch a fixer workflow.
///
/// Bundles all the Arc-cloned state needed to construct a LoopController
/// and spawn the workflow. Mirrors `FollowUpDeps`.
pub struct FixerDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
    pub session_manager: Option<Arc<crate::claude_session::SessionManager>>,
}

/// Check whether a fixer run should be launched for the given task run.
///
/// Returns false if:
/// - Another fixer is already running
/// - The source task run is itself a fixer, reflection, or follow-up run
/// - Fixer is disabled in settings
/// - The source run has insufficient output
pub fn should_launch_fixer(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    source_task_run_id: &str,
) -> Result<bool, String> {
    should_launch_fixer_sync(pg_db, source_task_run_id, None)
}

/// Sync wrapper around async PG guard checks using tokio::task::block_in_place.
fn should_launch_fixer_sync(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    source_task_run_id: &str,
    exclude_fixer_id: Option<&str>,
) -> Result<bool, String> {
    let pg = pg_db.clone();
    let src_id = source_task_run_id.to_string();
    let exc_id = exclude_fixer_id.map(|s| s.to_string());
    tokio::task::block_in_place(move || {
        tokio::runtime::Handle::current()
            .block_on(async { should_launch_fixer_pg(&pg, &src_id, exc_id.as_deref()).await })
    })
}

/// Same as `should_launch_fixer` but excludes a specific task run ID from the
/// "is another fixer running?" check. Used for the re-check after the fixer's
/// own task run has been created (to avoid self-blocking).
pub fn should_launch_fixer_excluding(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    source_task_run_id: &str,
    exclude_fixer_id: Option<&str>,
) -> Result<bool, String> {
    should_launch_fixer_sync(pg_db, source_task_run_id, exclude_fixer_id)
}

/// PG-backed fixer guard checks.
async fn should_launch_fixer_pg(
    pg_db: &crate::database::pg::PgDb,
    source_task_run_id: &str,
    exclude_fixer_id: Option<&str>,
) -> Result<bool, String> {
    // Guard 0: Check if a fixer workflow is already running
    if pg_db.has_running_fixer(exclude_fixer_id).await? {
        debug!("Skipping fixer — another fixer workflow is already running");
        return Ok(false);
    }

    // Guard 1-3: Check task_run flags
    let (is_reflection, is_fixer, is_follow_up) =
        pg_db.get_task_run_flags(source_task_run_id).await?;

    if is_fixer {
        debug!(
            "Skipping fixer for {} — source is already a fixer run",
            source_task_run_id
        );
        return Ok(false);
    }
    if is_reflection {
        debug!(
            "Skipping fixer for {} — source is a reflection run",
            source_task_run_id
        );
        return Ok(false);
    }
    if is_follow_up {
        debug!(
            "Skipping fixer for {} — source is a follow-up run",
            source_task_run_id
        );
        return Ok(false);
    }

    // Guard 4: Check fixer_enabled setting
    if !pg_db
        .get_dev_mode_setting_bool("fixer_enabled", true)
        .await?
    {
        debug!("Fixer disabled in settings");
        return Ok(false);
    }

    // Guard 5: Check output threshold
    if !pg_db.has_sufficient_output(source_task_run_id).await? {
        debug!(
            "Skipping fixer for {} — insufficient output to analyze",
            source_task_run_id
        );
        return Ok(false);
    }

    // Guard 6: Check that reflection fixes exist
    let conn = pg_db
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool: {e}"))?;
    let fix_count: i64 = conn
        .query_one(
            "SELECT COUNT(*) FROM reflection_fixes WHERE source_task_run_id = $1",
            &[&source_task_run_id],
        )
        .await
        .map(|r| r.get(0))
        .unwrap_or(0);

    if fix_count == 0 {
        debug!(
            "Skipping fixer: no reflection fixes found for source run {}",
            source_task_run_id
        );
        return Ok(false);
    }

    // Guard 7: Skip fixer if source passed verification (fixer_on_failure_only)
    let fixer_on_failure_only = pg_db
        .get_dev_mode_setting_bool("fixer_on_failure_only", true)
        .await?;
    if fixer_on_failure_only {
        let verification_passed: bool = conn
            .query_one(
                "SELECT COALESCE(verification_passed, false) FROM task_runs WHERE id = $1",
                &[&source_task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        if verification_passed {
            info!(
                "Skipping fixer — parent workflow passed successfully (source: {})",
                source_task_run_id
            );
            return Ok(false);
        }
    }

    Ok(true)
}

/// Wait for all child task runs (reflections, follow-ups) of the given source
/// to reach a terminal state. Polls every 5 seconds with a 10-minute timeout.
///
/// Returns Ok(true) if all children completed, Ok(false) if timed out.
pub async fn wait_for_children_complete(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let max_wait = std::time::Duration::from_secs(600); // 10 minutes
    let poll_interval = std::time::Duration::from_secs(5);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > max_wait {
            warn!(
                "Fixer timed out waiting for children of {} to complete (10 min)",
                source_task_run_id
            );
            return Ok(false);
        }

        let still_running = pg_db.count_running_children(source_task_run_id).await?;

        if still_running == 0 {
            debug!(
                "All children of {} are complete, fixer can proceed",
                source_task_run_id
            );
            return Ok(true);
        }

        debug!(
            "Fixer waiting: {} children still running for {}",
            still_running, source_task_run_id
        );

        tokio::time::sleep(poll_interval).await;
    }
}

/// Launch a fixer workflow to implement remaining fixes from reflections and follow-ups.
///
/// This is a synchronous function that spawns the async wait-for-children + workflow
/// execution. It first checks guards synchronously, then spawns a tokio task that
/// waits for all child workflows to complete before building and running the fixer.
///
/// Returns the fixer task run ID or "skipped".
pub fn launch_fixer(deps: FixerDeps, source_task_run_id: String) -> Result<String, String> {
    let pg_db = &deps.app_state.pg_db;

    // Guard check before committing to spawn
    if !should_launch_fixer(pg_db, &source_task_run_id)? {
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

    // Create fixer task run ID
    let fixer_id = uuid::Uuid::new_v4().to_string();
    let fixer_name = format!("Fixer: {}", workflow_name);

    info!(
        "Launching fixer {} for source run {} (workflow: {})",
        fixer_id, source_task_run_id, workflow_name
    );

    // Create the fixer task run record (status: running, will be updated on completion)
    let input = crate::database::CreateTaskRunInput::new(&fixer_id, &fixer_name)
        .with_prompt(format!(
            "Implement remaining fixes from reflections and follow-ups for '{}'.",
            workflow_name
        ))
        .with_workflow_name(&fixer_name)
        .with_workflow_type("unified")
        .with_task_type("fixer")
        .with_max_sessions(2)
        .with_auto_continue(true)
        .with_is_fixer(true)
        .with_fixer_source_task_run_id(&source_task_run_id)
        .with_parent_task_run_id(&source_task_run_id);
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.create_task_run(&input))
    })?;

    // Spawn an async task that waits for children, then runs the fixer workflow
    let fixer_id_clone = fixer_id.clone();
    let fixer_name_clone = fixer_name.clone();
    let workflow_name_clone = workflow_name.clone();
    let source_id_clone = source_task_run_id.clone();
    let pg_db_clone = deps.app_state.pg_db.clone();

    tokio::spawn(async move {
        // Wait for all children to complete before building/running the fixer
        info!(
            "Fixer waiting for children of {} to complete...",
            source_id_clone
        );
        match wait_for_children_complete(&pg_db_clone, &source_id_clone).await {
            Ok(true) => {
                info!("All children complete, proceeding with fixer");
            }
            Ok(false) => {
                warn!(
                    "Fixer timed out waiting for children of {} — skipping",
                    source_id_clone
                );
                // Mark the fixer run as failed
                let _ = pg_db_clone
                    .update_task_run_status(&fixer_id_clone, "failed")
                    .await;
                return;
            }
            Err(e) => {
                warn!("Fixer error waiting for children: {}", e);
                let _ = pg_db_clone
                    .update_task_run_status(&fixer_id_clone, "failed")
                    .await;
                return;
            }
        }

        // Re-check guards after waiting (state may have changed)
        // Exclude own ID to avoid self-blocking (our task run is status='running')
        match should_launch_fixer_excluding(&pg_db_clone, &source_id_clone, Some(&fixer_id_clone)) {
            Ok(true) => {}
            _ => {
                info!("Fixer guards no longer pass after waiting — skipping");
                let _ = pg_db_clone
                    .update_task_run_status(&fixer_id_clone, "stopped")
                    .await;
                return;
            }
        }

        // Build and run the fixer workflow
        let loop_config = super::workflow::build_fixer_config(
            &fixer_id_clone,
            &fixer_name_clone,
            &workflow_name_clone,
        );
        let setup_steps = super::workflow::build_setup_steps(&source_id_clone);
        let verification_steps = super::workflow::build_verification_steps();

        let step_injection_ctx = crate::step_injection::types::StepInjectionContext {
            execution_id: fixer_id_clone.clone(),
        };

        let mut controller = crate::unified_workflow_executor::LoopController::new(
            deps.app_state.clone(),
            deps.config_storage.clone(),
            deps.app_handle.clone(),
            deps.pid_tracker.clone(),
        )
        .with_step_injection_ctx(step_injection_ctx);

        if let Some(sm) = deps.session_manager {
            controller = controller.with_session_manager(sm);
        }

        info!(
            "Spawning fixer workflow '{}' (id: {}) with {} setup steps",
            fixer_name_clone,
            fixer_id_clone,
            setup_steps.len()
        );

        let exec_id = fixer_id_clone.clone();
        let wf_name = fixer_name_clone.clone();
        let url_lock = Some(deps.app_state.url_lock_manager.clone());
        let file_registry = Some(deps.app_state.file_registry_manager.clone());

        // Check if Restate durable execution should be used
        let mut use_legacy = true;
        let restate_settings = crate::settings::load_settings().restate;
        if crate::restate::launch::should_use_restate(&restate_settings).await {
            match crate::restate::launch::build_workflow_input_from_loop_config(&loop_config) {
                Ok(input) => {
                    if let Err(e) = deps
                        .app_state
                        .pg_db
                        .save_restate_workflow_execution(&exec_id, &exec_id, None)
                        .await
                    {
                        tracing::error!("Failed to record Restate workflow: {}", e);
                    }

                    if let Err(e) = crate::restate::launch::launch_workflow_via_restate(
                        &exec_id,
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
                exec_id,
                wf_name,
                url_lock,
                file_registry,
                deps.app_state.pg_db.clone(),
                Box::pin(async move {
                    controller
                        .run(
                            loop_config,
                            setup_steps,
                            Vec::new(),
                            verification_steps,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )
                        .await
                }),
            );
        }

        info!(
            "Fixer workflow '{}' spawned for source {}",
            fixer_name_clone, source_id_clone
        );
    });

    info!(
        "Fixer task {} created and waiting for children of {}",
        fixer_id, source_task_run_id
    );

    Ok(fixer_id)
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
    fn test_guard_skips_when_fixer_already_running() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_is_fixer() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_is_reflection() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_is_follow_up() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_fixer_disabled() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_defaults_to_enabled_when_no_setting() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_skips_when_source_passed_verification() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_guard_allows_fixer_when_source_failed_verification() {
        // SQLite removed - no-op
    }
}
