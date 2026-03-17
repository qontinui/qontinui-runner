//! Post-workflow trigger for launching follow-up runs.
//!
//! Checks recursion guards and launches a follow-up workflow after
//! a dev-mode workflow completes with unfixed issues.
//!
//! Recursion prevention (3 layers):
//! 1. `is_dev_mode: false` on follow-up's LoopConfig
//! 2. `is_follow_up` column check on source task run
//! 3. `is_reflection` column check on source task run

use rusqlite::Connection;
use std::sync::Arc;
use tracing::{debug, info};

use crate::config_storage::ConfigStorage;
use crate::database::CheckpointDb;
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
    db: &CheckpointDb,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let source_id = source_task_run_id.to_string();

    db.with_conn(|conn| {
        // Guard 0: Check if a follow-up workflow is already running
        let has_running: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_follow_up = 1 AND workflow_type = 'unified'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if has_running {
            debug!("Skipping follow-up — another follow-up workflow is already running");
            return Ok(false);
        }

        // Guard 1: Check if source task run is already a follow-up run
        let is_follow_up: bool = conn
            .query_row(
                "SELECT COALESCE(is_follow_up, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Failed to check is_follow_up: {}", e))?;

        if is_follow_up {
            debug!(
                "Skipping follow-up for {} — source is already a follow-up run",
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
                "Skipping follow-up for {} — source is a reflection run",
                source_id
            );
            return Ok(false);
        }

        // Guard 2b: Check if source task run is a fixer run
        let is_fixer: bool = conn
            .query_row(
                "SELECT COALESCE(is_fixer, 0) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .unwrap_or(false);

        if is_fixer {
            debug!(
                "Skipping follow-up for {} — source is a fixer run",
                source_id
            );
            return Ok(false);
        }

        // Guard 3: Check follow_up_enabled setting (default true)
        // json_extract returns integers for JSON booleans (1/0), so use CAST to normalize
        let follow_up_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.follow_up_enabled') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true); // Default to enabled if setting doesn't exist

        if !follow_up_enabled {
            debug!("Follow-up disabled in settings");
            return Ok(false);
        }

        // Guard 4: Check that the source run has meaningful output to analyze
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
                "Skipping follow-up for {} — insufficient output to analyze",
                source_id
            );
            return Ok(false);
        }

        // Guard 5: Check that source output contains unfixed-issue signals
        let has_signal = check_has_unfixed_signal(conn, &source_id)?;
        if !has_signal {
            debug!(
                "Skipping follow-up for {} — no unfixed-issue signals found in output",
                source_id
            );
            return Ok(false);
        }

        Ok(true)
    })
}

/// Scan the last ~20 output chunks for unfixed-issue signal patterns.
///
/// Returns true if any of the known signal patterns are found in the
/// tail of the source run's output.
fn check_has_unfixed_signal(conn: &Connection, task_run_id: &str) -> Result<bool, String> {
    // Get the last 20 output chunks (most recent output)
    let mut stmt = conn
        .prepare(
            r#"
            SELECT content FROM task_run_output_chunks
            WHERE task_run_id = ?1
            ORDER BY chunk_sequence DESC
            LIMIT 20
            "#,
        )
        .map_err(|e| format!("Failed to prepare unfixed signal query: {}", e))?;

    let chunks: Vec<String> = stmt
        .query_map(rusqlite::params![task_run_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("Failed to query output chunks: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Also check the output_log column as fallback
    let output_log: String = conn
        .query_row(
            "SELECT COALESCE(output_log, '') FROM task_runs WHERE id = ?1",
            rusqlite::params![task_run_id],
            |row| row.get(0),
        )
        .unwrap_or_default();

    // Combine chunks and output_log, then search for signals (case-insensitive)
    let combined = chunks.join(" ");
    let tail_output = if !combined.is_empty() {
        combined.clone()
    } else {
        // Fall back to output_log if no chunks exist
        output_log[output_log.len().saturating_sub(20000)..].to_string()
    };

    let lower = tail_output.to_lowercase();

    for pattern in UNFIXED_SIGNAL_PATTERNS {
        if lower.contains(pattern) {
            debug!(
                "Found unfixed-issue signal '{}' in output for {}",
                pattern, task_run_id
            );
            return Ok(true);
        }
    }

    Ok(false)
}

/// Launch a follow-up workflow to fix unfixed issues from the given source task run.
///
/// Creates a new task run with `is_follow_up = true`, builds the follow-up
/// workflow programmatically, and spawns it via LoopController.
///
/// This is a synchronous function that spawns the workflow asynchronously.
pub fn launch_follow_up(deps: FollowUpDeps, source_task_run_id: String) -> Result<String, String> {
    let db = &deps.app_state.checkpoint_db;

    // Final check before launching
    if !should_launch_follow_up(db, &source_task_run_id)? {
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

    db.create_task_run(&input)?;

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
    crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
        deps.app_state.checkpoint_db.clone(),
        exec_id,
        wf_name,
        url_lock,
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

    info!(
        "Follow-up workflow '{}' spawned for source {}",
        follow_up_name, source_task_run_id
    );

    Ok(follow_up_id)
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
                workflow_type TEXT,
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
    fn test_check_has_unfixed_signal_true() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &[
                "Step 1 completed successfully.",
                "Fixed issue A and issue B.",
                "Lower priority items not fixed: issue C, issue D.",
            ],
        );
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_false_when_all_fixed() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &[
                "Step 1 completed successfully.",
                "All issues have been resolved.",
                "Task complete.",
            ],
        );
        assert!(!check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_deferred() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &["Some items were deferred to a later sprint."],
        );
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_could_not_fix() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &["Could not fix the database migration issue due to schema constraints."],
        );
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_out_of_scope() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &["The CSS regression is out of scope for this workflow."],
        );
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_case_insensitive() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &["REMAINING ISSUES: 3 items need attention."],
        );
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_unfixable_errors_marker() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "run-1", "my-workflow");
        add_output_chunks(
            &conn,
            "run-1",
            &["[unfixable_errors] timeout in authentication service"],
        );
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_check_has_unfixed_signal_uses_output_log_fallback() {
        let conn = setup_test_db();
        // Insert run with output_log containing signal, but no chunks
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, output_log, created_at, updated_at) \
             VALUES ('run-1', 'Test', 'wf', 'complete', 'Some items were not fixed in this run.', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        assert!(check_has_unfixed_signal(&conn, "run-1").unwrap());
    }

    #[test]
    fn test_guard_skips_when_follow_up_already_running() {
        let conn = setup_test_db();
        insert_completed_run(&conn, "source-1", "my-workflow");
        add_output_chunks(&conn, "source-1", &["remaining issues in the codebase"]);

        // Insert a running follow-up
        conn.execute(
            "INSERT INTO task_runs (id, task_name, status, is_follow_up, workflow_type, created_at, updated_at) \
             VALUES ('fu-1', 'Follow-up', 'running', 1, 'unified', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        // We can't easily test through should_launch_follow_up without a full CheckpointDb,
        // but we can test the guard logic directly
        let has_running: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_follow_up = 1 AND workflow_type = 'unified'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_running);
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
    fn test_guard_skips_when_follow_up_disabled() {
        let conn = setup_test_db();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('dev_mode', '{\"follow_up_enabled\": false}')",
            [],
        )
        .unwrap();

        let follow_up_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.follow_up_enabled') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);
        assert!(!follow_up_enabled);
    }

    #[test]
    fn test_guard_defaults_to_enabled_when_no_setting() {
        let conn = setup_test_db();
        // No settings row at all — unwrap_or(true) should kick in
        let follow_up_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(CAST(json_extract(value, '$.follow_up_enabled') AS TEXT), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);
        assert!(follow_up_enabled);
    }
}
