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
use crate::database::CheckpointDb;
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
pub fn should_launch_fixer(db: &CheckpointDb, source_task_run_id: &str) -> Result<bool, String> {
    should_launch_fixer_excluding(db, source_task_run_id, None)
}

/// Same as `should_launch_fixer` but excludes a specific task run ID from the
/// "is another fixer running?" check. Used for the re-check after the fixer's
/// own task run has been created (to avoid self-blocking).
pub fn should_launch_fixer_excluding(
    db: &CheckpointDb,
    source_task_run_id: &str,
    exclude_fixer_id: Option<&str>,
) -> Result<bool, String> {
    let source_id = source_task_run_id.to_string();
    let exclude_id = exclude_fixer_id.map(|s| s.to_string());

    // PG: complex dynamic SQL
    db.with_conn(|conn| {
        // Guard 0: Check if a fixer workflow is already running (exclude self if provided)
        let has_running: bool = if let Some(ref eid) = exclude_id {
            conn.query_row(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_fixer = 1 AND workflow_type = 'unified' AND id != ?1",
                rusqlite::params![eid],
                |row| row.get(0),
            )
            .unwrap_or(false)
        } else {
            conn.query_row(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_fixer = 1 AND workflow_type = 'unified'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false)
        };

        if has_running {
            debug!("Skipping fixer — another fixer workflow is already running");
            return Ok(false);
        }

        // Guard 1: Check if source task run is already a fixer run
        let is_fixer: bool = conn
            .query_row(
                "SELECT COALESCE(is_fixer, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Failed to check is_fixer: {}", e))?;

        if is_fixer {
            debug!(
                "Skipping fixer for {} — source is already a fixer run",
                source_id
            );
            return Ok(false);
        }

        // Guard 2: Check if source task run is a reflection run
        let is_reflection: bool = conn
            .query_row(
                "SELECT COALESCE(is_reflection, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Failed to check is_reflection: {}", e))?;

        if is_reflection {
            debug!(
                "Skipping fixer for {} — source is a reflection run",
                source_id
            );
            return Ok(false);
        }

        // Guard 3: Check if source task run is a follow-up run
        let is_follow_up: bool = conn
            .query_row(
                "SELECT COALESCE(is_follow_up, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Failed to check is_follow_up: {}", e))?;

        if is_follow_up {
            debug!(
                "Skipping fixer for {} — source is a follow-up run",
                source_id
            );
            return Ok(false);
        }

        // Guard 4: Check fixer_enabled setting (default true)
        let fixer_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.fixer_enabled') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);

        if !fixer_enabled {
            debug!("Fixer disabled in settings");
            return Ok(false);
        }

        // Guard 5: Check that the source run has meaningful output to analyze
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
                "Skipping fixer for {} — insufficient output to analyze",
                source_id
            );
            return Ok(false);
        }

        // Guard 6: Check that reflection fixes exist for the source run
        let fix_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reflection_fixes WHERE source_task_run_id = ?1",
                rusqlite::params![source_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if fix_count == 0 {
            debug!(
                "Skipping fixer: no reflection fixes found for source run {}",
                source_id
            );
            return Ok(false);
        }

        // Guard 7: Skip fixer if the source run passed verification (fixer_on_failure_only)
        let fixer_on_failure_only: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.fixer_on_failure_only') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);

        if fixer_on_failure_only {
            let verification_passed: bool = conn
                .query_row(
                    "SELECT COALESCE(verification_passed, 0) FROM task_runs WHERE id = ?1",
                    rusqlite::params![source_id],
                    |row| row.get::<_, i32>(0).map(|v| v != 0),
                )
                .unwrap_or(false);

            if verification_passed {
                info!(
                    "Skipping fixer — parent workflow passed successfully (source: {})",
                    source_id
                );
                return Ok(false);
            }
        }

        Ok(true)
    })
}

/// Wait for all child task runs (reflections, follow-ups) of the given source
/// to reach a terminal state. Polls every 5 seconds with a 10-minute timeout.
///
/// Returns Ok(true) if all children completed, Ok(false) if timed out.
pub async fn wait_for_children_complete(
    db: &CheckpointDb,
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

        let source_id = source_task_run_id.to_string();
        // PG: complex dynamic SQL
        let still_running = db.with_conn(|conn| {
            let count: i64 = conn
                .query_row(
                    r#"SELECT COUNT(*) FROM task_runs
                       WHERE parent_task_run_id = ?1
                         AND status IN ('running', 'pending')
                         AND id != ?1
                         AND COALESCE(is_fixer, 0) = 0"#,
                    rusqlite::params![source_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            Ok(count)
        })?;

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
    let db = &deps.app_state.checkpoint_db;

    // Synchronous guard check before committing to spawn
    if !should_launch_fixer(db, &source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details
    let source_id = source_task_run_id.clone();
    // PG: complex dynamic SQL
    let workflow_name = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = ?1",
            rusqlite::params![source_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| format!("Failed to get source task run: {}", e))
    })?;

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

    db.create_task_run(&input)?;

    // Spawn an async task that waits for children, then runs the fixer workflow
    let fixer_id_clone = fixer_id.clone();
    let fixer_name_clone = fixer_name.clone();
    let workflow_name_clone = workflow_name.clone();
    let source_id_clone = source_task_run_id.clone();
    let db_clone = deps.app_state.checkpoint_db.clone();

    tokio::spawn(async move {
        // Wait for all children to complete before building/running the fixer
        info!(
            "Fixer waiting for children of {} to complete...",
            source_id_clone
        );
        match wait_for_children_complete(&db_clone, &source_id_clone).await {
            Ok(true) => {
                info!("All children complete, proceeding with fixer");
            }
            Ok(false) => {
                warn!(
                    "Fixer timed out waiting for children of {} — skipping",
                    source_id_clone
                );
                // Mark the fixer run as failed
                let _ = db_clone.update_task_run_status(&fixer_id_clone, "failed");
                return;
            }
            Err(e) => {
                warn!("Fixer error waiting for children: {}", e);
                let _ = db_clone.update_task_run_status(&fixer_id_clone, "failed");
                return;
            }
        }

        // Re-check guards after waiting (state may have changed)
        // Exclude own ID to avoid self-blocking (our task run is status='running')
        match should_launch_fixer_excluding(&db_clone, &source_id_clone, Some(&fixer_id_clone)) {
            Ok(true) => {}
            _ => {
                info!("Fixer guards no longer pass after waiting — skipping");
                let _ = db_clone.update_task_run_status(&fixer_id_clone, "stopped");
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
        crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
            deps.app_state.checkpoint_db.clone(),
            exec_id,
            wf_name,
            url_lock,
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
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL,
                workflow_name TEXT,
                status TEXT DEFAULT 'running',
                output_log TEXT DEFAULT '',
                is_reflection INTEGER DEFAULT 0,
                reflection_source_task_run_id TEXT,
                is_follow_up INTEGER DEFAULT 0,
                follow_up_source_task_run_id TEXT,
                is_fixer INTEGER DEFAULT 0,
                fixer_source_task_run_id TEXT,
                workflow_type TEXT,
                parent_task_run_id TEXT,
                verification_passed INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE task_run_output_chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_run_id TEXT NOT NULL,
                chunk_sequence INTEGER NOT NULL,
                content TEXT NOT NULL
            );
            CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_completed_run(conn: &Connection, id: &str, workflow_name: &str) {
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, output_log, created_at, updated_at, completed_at) \
             VALUES (?1, 'Test', ?2, 'complete', '', datetime('now'), datetime('now'), datetime('now'))",
            rusqlite::params![id, workflow_name],
        )
        .unwrap();
    }

    fn add_output_chunks(conn: &Connection, task_run_id: &str, chunks: &[&str]) {
        for (i, chunk) in chunks.iter().enumerate() {
            conn.execute(
                "INSERT INTO task_run_output_chunks (task_run_id, chunk_sequence, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![task_run_id, i as i64, chunk],
            )
            .unwrap();
        }
    }

    #[test]
    fn test_guard_skips_when_fixer_already_running() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "source-1", "my-workflow");
        add_output_chunks(
            &conn,
            "source-1",
            &["some meaningful output that is long enough to pass the threshold check"],
        );

        conn.execute(
            "INSERT INTO task_runs (id, task_name, status, is_fixer, workflow_type, created_at, updated_at) \
             VALUES ('fx-1', 'Fixer', 'running', 1, 'unified', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let has_running: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_fixer = 1 AND workflow_type = 'unified'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_running);
    }

    #[test]
    fn test_guard_skips_when_source_is_fixer() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_fixer, output_log, created_at, updated_at) \
             VALUES ('fx-1', 'Fixer: X', 'Fixer: X', 'complete', 1, '', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let is_fixer: bool = conn
            .query_row(
                "SELECT COALESCE(is_fixer, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params!["fx-1"],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .unwrap();
        assert!(is_fixer);
    }

    #[test]
    fn test_guard_skips_when_source_is_reflection() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, output_log, created_at, updated_at) \
             VALUES ('ref-1', 'Reflection: X', 'Reflection: X', 'complete', 1, '', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let is_reflection: bool = conn
            .query_row(
                "SELECT COALESCE(is_reflection, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params!["ref-1"],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .unwrap();
        assert!(is_reflection);
    }

    #[test]
    fn test_guard_skips_when_source_is_follow_up() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_follow_up, output_log, created_at, updated_at) \
             VALUES ('fu-1', 'Follow-up: X', 'Follow-up: X', 'complete', 1, '', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let is_follow_up: bool = conn
            .query_row(
                "SELECT COALESCE(is_follow_up, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params!["fu-1"],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .unwrap();
        assert!(is_follow_up);
    }

    #[test]
    fn test_guard_skips_when_fixer_disabled() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('dev_mode', '{\"fixer_enabled\": false}')",
            [],
        )
        .unwrap();

        let fixer_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.fixer_enabled') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);
        assert!(!fixer_enabled);
    }

    #[test]
    fn test_guard_defaults_to_enabled_when_no_setting() {
        let conn = setup_test_db();
        let fixer_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.fixer_enabled') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);
        assert!(fixer_enabled);
    }

    #[test]
    fn test_guard_skips_when_source_passed_verification() {
        let conn = setup_test_db();

        // Create a source run that passed verification
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, output_log, verification_passed, created_at, updated_at, completed_at) \
             VALUES ('src-pass', 'Test', 'my-workflow', 'complete', '', 1, datetime('now'), datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        add_output_chunks(
            &conn,
            "src-pass",
            &["some meaningful output that is long enough to pass the threshold check"],
        );
        // Add a reflection fix so Guard 6 passes
        conn.execute(
            "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id) VALUES ('fix-1', 'src-pass', 'ref-1')",
            [],
        )
        .unwrap();

        // Guard 7 should skip because verification_passed = 1
        let verification_passed: bool = conn
            .query_row(
                "SELECT COALESCE(verification_passed, 0) FROM task_runs WHERE id = 'src-pass'",
                [],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .unwrap();
        assert!(
            verification_passed,
            "Source run should have verification_passed = 1"
        );
    }

    #[test]
    fn test_guard_allows_fixer_when_source_failed_verification() {
        let conn = setup_test_db();

        // Create a source run that failed verification
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, output_log, verification_passed, created_at, updated_at, completed_at) \
             VALUES ('src-fail', 'Test', 'my-workflow', 'complete', '', 0, datetime('now'), datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        add_output_chunks(
            &conn,
            "src-fail",
            &["some meaningful output that is long enough to pass the threshold check"],
        );
        // Add a reflection fix so Guard 6 passes
        conn.execute(
            "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id) VALUES ('fix-2', 'src-fail', 'ref-2')",
            [],
        )
        .unwrap();

        // Guard 7 should NOT skip because verification_passed = 0
        let verification_passed: bool = conn
            .query_row(
                "SELECT COALESCE(verification_passed, 0) FROM task_runs WHERE id = 'src-fail'",
                [],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .unwrap();
        assert!(
            !verification_passed,
            "Source run should have verification_passed = 0"
        );
    }
}
