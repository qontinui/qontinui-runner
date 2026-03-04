//! Post-workflow trigger for launching reflection runs.
//!
//! Checks recursion guards and launches a reflection workflow after
//! a dev-mode workflow completes. Uses three layers of recursion prevention:
//! 1. `is_dev_mode: false` on reflection's LoopConfig
//! 2. `is_reflection` column check on source task run
//! 3. Parent hierarchy tracking

use rusqlite::Connection;
use std::collections::BTreeSet;
use std::sync::Arc;
use tracing::{debug, info};

use crate::config_storage::ConfigStorage;
use crate::database::CheckpointDb;
use crate::AppState;

/// Number of consecutive successful runs (zero findings) before suppressing reflection.
const CONVERGENCE_THRESHOLD: u32 = 5;

/// Number of consecutive reflection runs with identical findings before declaring stall.
const REPEAT_THRESHOLD: u32 = 3;

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
pub fn should_launch_reflection(
    db: &CheckpointDb,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let source_id = source_task_run_id.to_string();

    db.with_conn(|conn| {
        // Guard 0: Check if a reflection workflow is already running
        let has_running: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_reflection = 1 AND workflow_type = 'unified'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if has_running {
            debug!("Skipping reflection — another reflection workflow is already running");
            return Ok(false);
        }

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

        // Guard 4: Convergence suppression — skip reflection if the workflow has
        // had N consecutive successful runs with zero findings. This prevents
        // spawning vacuous reflections that find nothing new.
        let converged = has_converged(conn, &source_id)?;
        if converged {
            info!(
                "Skipping reflection for {} — workflow has converged",
                source_id
            );
            return Ok(false);
        }

        Ok(true)
    })
}

/// Check whether a workflow has converged — i.e., the last N consecutive
/// non-reflection runs of the same workflow all completed successfully with
/// zero findings. When this is true, spawning another reflection is wasteful.
fn has_converged(conn: &Connection, source_task_run_id: &str) -> Result<bool, String> {
    // Get the workflow name for the source run
    let workflow_name: Option<String> = conn
        .query_row(
            "SELECT workflow_name FROM task_runs WHERE id = ?1",
            rusqlite::params![source_task_run_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get workflow_name: {}", e))?;

    let workflow_name = match workflow_name {
        Some(name) => name,
        None => return Ok(false), // No workflow name → can't check convergence
    };

    // Get the last CONVERGENCE_THRESHOLD non-reflection completed runs of this workflow,
    // ordered by most recent first
    let mut stmt = conn
        .prepare(
            r#"
            SELECT tr.id, tr.status,
                   (SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = tr.id) as finding_count
            FROM task_runs tr
            WHERE tr.workflow_name = ?1
              AND tr.is_reflection = 0
              AND tr.status IN ('complete', 'completed')
            ORDER BY tr.completed_at DESC
            LIMIT ?2
            "#,
        )
        .map_err(|e| format!("Failed to prepare convergence query: {}", e))?;

    let rows: Vec<(String, u32)> = stmt
        .query_map(
            rusqlite::params![workflow_name, CONVERGENCE_THRESHOLD],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(2)?)),
        )
        .map_err(|e| format!("Failed to query convergence: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Need at least CONVERGENCE_THRESHOLD runs to declare convergence
    if rows.len() < CONVERGENCE_THRESHOLD as usize {
        return Ok(false);
    }

    // All runs must have zero findings
    let all_clean = rows.iter().all(|(_, count)| *count == 0);
    if all_clean {
        debug!(
            "Workflow '{}' has {} consecutive clean runs — converged (zero findings)",
            workflow_name,
            rows.len()
        );
        return Ok(true);
    }

    // Check for repeated identical findings across reflection runs
    if has_repeated_findings(conn, source_task_run_id, &workflow_name)? {
        info!(
            "Workflow '{}' has converged — last {} reflection runs found identical issues",
            workflow_name, REPEAT_THRESHOLD
        );
        return Ok(true);
    }

    Ok(false)
}

/// Check whether recent reflection runs keep finding the same issues.
/// If the last REPEAT_THRESHOLD consecutive reflections for the same workflow
/// produced identical signature_hash sets, the reflection loop has stalled.
fn has_repeated_findings(
    conn: &Connection,
    _source_task_run_id: &str,
    workflow_name: &str,
) -> Result<bool, String> {
    // Get last REPEAT_THRESHOLD reflection runs for this workflow
    let mut stmt = conn
        .prepare(
            r#"
            SELECT tr.id
            FROM task_runs tr
            WHERE tr.workflow_name LIKE 'Reflection: %'
              AND tr.is_reflection = 1
              AND tr.status IN ('complete', 'completed')
              AND tr.reflection_source_task_run_id IN (
                SELECT id FROM task_runs WHERE workflow_name = ?1
              )
            ORDER BY tr.completed_at DESC
            LIMIT ?2
            "#,
        )
        .map_err(|e| format!("Failed to prepare repeated-findings query: {}", e))?;

    let reflection_ids: Vec<String> = stmt
        .query_map(rusqlite::params![workflow_name, REPEAT_THRESHOLD], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("Failed to query reflection runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Need at least REPEAT_THRESHOLD reflection runs
    if reflection_ids.len() < REPEAT_THRESHOLD as usize {
        return Ok(false);
    }

    // Collect signature sets for each reflection run
    let mut finding_sets: Vec<BTreeSet<String>> = Vec::new();
    let mut sig_stmt = conn
        .prepare(
            r#"
            SELECT COALESCE(signature_hash, title) as sig
            FROM task_run_findings
            WHERE task_run_id = ?1
            ORDER BY sig
            "#,
        )
        .map_err(|e| format!("Failed to prepare signature query: {}", e))?;

    for ref_id in &reflection_ids {
        let sigs: BTreeSet<String> = sig_stmt
            .query_map(rusqlite::params![ref_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query findings for {}: {}", ref_id, e))?
            .filter_map(|r| r.ok())
            .collect();
        finding_sets.push(sigs);
    }

    // All sets must be identical and non-empty
    let first = &finding_sets[0];
    if first.is_empty() {
        return Ok(false);
    }

    let all_identical = finding_sets.iter().all(|s| s == first);
    if all_identical {
        debug!(
            "Last {} reflection runs for '{}' all found identical findings: {:?}",
            REPEAT_THRESHOLD, workflow_name, first
        );
    }

    Ok(all_identical)
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
    let loop_config =
        super::workflow::build_reflection_config(&reflection_id, &reflection_name, &workflow_name);

    let setup_steps = super::workflow::build_setup_steps(&source_task_run_id, &workflow_name);
    let verification_steps = super::workflow::build_verification_steps(&source_task_run_id);
    let mut completion_steps = super::workflow::build_completion_steps(&workflow_name);
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
    crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
        deps.app_state.checkpoint_db.clone(),
        exec_id,
        wf_name,
        url_lock,
        Box::pin(async move {
            controller
                .run(
                    loop_config,
                    setup_steps,                 // setup automation steps (API requests)
                    Vec::new(),                  // setup prompt steps (none)
                    verification_steps,          // verification steps
                    Vec::new(), // agentic steps (prompt is in loop_config.base_prompt)
                    completion_automation_steps, // completion automation steps (batch evaluation)
                    completion_prompt_steps, // completion prompt steps
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
                is_reflection INTEGER DEFAULT 0,
                reflection_source_task_run_id TEXT,
                created_at TEXT NOT NULL,
                completed_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                signature_hash TEXT,
                category TEXT,
                title TEXT,
                detected_at TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    fn insert_clean_run(conn: &Connection, id: &str, workflow_name: &str, completed_at: &str) {
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) \
             VALUES (?1, 'Test', ?2, 'complete', 0, ?3, ?3, ?3)",
            rusqlite::params![id, workflow_name, completed_at],
        )
        .unwrap();
    }

    fn insert_run_with_findings(
        conn: &Connection,
        id: &str,
        workflow_name: &str,
        completed_at: &str,
        finding_count: u32,
    ) {
        insert_clean_run(conn, id, workflow_name, completed_at);
        for i in 0..finding_count {
            conn.execute(
                "INSERT INTO task_run_findings (id, task_run_id, category, title, detected_at) \
                 VALUES (?1, ?2, 'test', 'finding', ?3)",
                rusqlite::params![format!("{}-f{}", id, i), id, completed_at],
            )
            .unwrap();
        }
    }

    #[test]
    fn test_has_converged_true_after_threshold() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        // Insert exactly CONVERGENCE_THRESHOLD clean runs
        for i in 0..CONVERGENCE_THRESHOLD {
            insert_clean_run(
                &conn,
                &format!("run-{}", i),
                wf,
                &format!("2025-01-01T{:02}:00:00Z", i),
            );
        }
        // Source run is the latest one
        let source_id = format!("run-{}", CONVERGENCE_THRESHOLD - 1);
        assert!(has_converged(&conn, &source_id).unwrap());
    }

    #[test]
    fn test_has_converged_false_below_threshold() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        // Insert fewer than threshold
        for i in 0..(CONVERGENCE_THRESHOLD - 1) {
            insert_clean_run(
                &conn,
                &format!("run-{}", i),
                wf,
                &format!("2025-01-01T{:02}:00:00Z", i),
            );
        }
        let source_id = format!("run-{}", CONVERGENCE_THRESHOLD - 2);
        assert!(!has_converged(&conn, &source_id).unwrap());
    }

    #[test]
    fn test_has_converged_false_when_findings_present() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        // Insert CONVERGENCE_THRESHOLD runs, but one has findings
        for i in 0..CONVERGENCE_THRESHOLD {
            if i == 2 {
                insert_run_with_findings(
                    &conn,
                    &format!("run-{}", i),
                    wf,
                    &format!("2025-01-01T{:02}:00:00Z", i),
                    3,
                );
            } else {
                insert_clean_run(
                    &conn,
                    &format!("run-{}", i),
                    wf,
                    &format!("2025-01-01T{:02}:00:00Z", i),
                );
            }
        }
        let source_id = format!("run-{}", CONVERGENCE_THRESHOLD - 1);
        assert!(!has_converged(&conn, &source_id).unwrap());
    }

    #[test]
    fn test_has_converged_false_no_workflow_name() {
        let conn = setup_test_db();
        // Insert a run without a workflow name
        conn.execute(
            "INSERT INTO task_runs (id, task_name, status, is_reflection, completed_at, created_at, updated_at) \
             VALUES ('run-1', 'Test', 'complete', 0, '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        assert!(!has_converged(&conn, "run-1").unwrap());
    }

    // --- Repeated-findings tests ---

    fn insert_reflection_run(
        conn: &Connection,
        id: &str,
        source_workflow: &str,
        source_run_id: &str,
        completed_at: &str,
        finding_hashes: &[&str],
    ) {
        let reflection_name = format!("Reflection: {}", source_workflow);
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, reflection_source_task_run_id, completed_at, created_at, updated_at) \
             VALUES (?1, 'Reflection', ?2, 'complete', 1, ?3, ?4, ?4, ?4)",
            rusqlite::params![id, reflection_name, source_run_id, completed_at],
        )
        .unwrap();
        for (i, hash) in finding_hashes.iter().enumerate() {
            conn.execute(
                "INSERT INTO task_run_findings (id, task_run_id, signature_hash, category, title, detected_at) \
                 VALUES (?1, ?2, ?3, 'test', 'finding', ?4)",
                rusqlite::params![format!("{}-f{}", id, i), id, hash, completed_at],
            )
            .unwrap();
        }
    }

    #[test]
    fn test_has_repeated_findings_true_when_same_hashes() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        // Insert source run
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // Insert 3 reflection runs with identical finding hashes
        for i in 0..REPEAT_THRESHOLD {
            insert_reflection_run(
                &conn,
                &format!("ref-{}", i),
                wf,
                "source-1",
                &format!("2025-01-01T{:02}:00:00Z", i + 1),
                &["hash-a", "hash-b"],
            );
        }

        assert!(has_repeated_findings(&conn, "source-1", wf).unwrap());
    }

    #[test]
    fn test_has_repeated_findings_false_when_different_hashes() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // 3 reflection runs with varying findings
        insert_reflection_run(
            &conn,
            "ref-0",
            wf,
            "source-1",
            "2025-01-01T01:00:00Z",
            &["hash-a", "hash-b"],
        );
        insert_reflection_run(
            &conn,
            "ref-1",
            wf,
            "source-1",
            "2025-01-01T02:00:00Z",
            &["hash-a", "hash-c"],
        );
        insert_reflection_run(
            &conn,
            "ref-2",
            wf,
            "source-1",
            "2025-01-01T03:00:00Z",
            &["hash-a", "hash-b"],
        );

        assert!(!has_repeated_findings(&conn, "source-1", wf).unwrap());
    }

    #[test]
    fn test_has_repeated_findings_false_below_threshold() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // Only 2 reflection runs (below REPEAT_THRESHOLD of 3)
        insert_reflection_run(
            &conn,
            "ref-0",
            wf,
            "source-1",
            "2025-01-01T01:00:00Z",
            &["hash-a"],
        );
        insert_reflection_run(
            &conn,
            "ref-1",
            wf,
            "source-1",
            "2025-01-01T02:00:00Z",
            &["hash-a"],
        );

        assert!(!has_repeated_findings(&conn, "source-1", wf).unwrap());
    }

    #[test]
    fn test_has_converged_ignores_reflection_runs() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        // Insert clean non-reflection runs below threshold
        for i in 0..(CONVERGENCE_THRESHOLD - 1) {
            insert_clean_run(
                &conn,
                &format!("run-{}", i),
                wf,
                &format!("2025-01-01T{:02}:00:00Z", i),
            );
        }
        // Insert a reflection run (should be ignored)
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) \
             VALUES ('ref-1', 'Reflection', ?1, 'complete', 1, '2025-01-01T10:00:00Z', '2025-01-01T10:00:00Z', '2025-01-01T10:00:00Z')",
            rusqlite::params![wf],
        )
        .unwrap();
        // Still below threshold because the reflection run is excluded
        let source_id = format!("run-{}", CONVERGENCE_THRESHOLD - 2);
        assert!(!has_converged(&conn, &source_id).unwrap());
    }
}
