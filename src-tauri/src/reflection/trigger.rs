//! Post-workflow trigger for launching reflection runs.
//!
//! Checks recursion guards and launches a reflection workflow after
//! a dev-mode workflow completes. Uses three layers of recursion prevention:
//! 1. `is_dev_mode: false` on reflection's LoopConfig
//! 2. `is_reflection` column check on source task run
//! 3. Parent hierarchy tracking

use rusqlite::Connection;
use serde::Serialize;
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
        // Guard 0: Check if a reflection for the SAME workflow is already running.
        // Only block if the running reflection's source shares the same workflow_name
        // as our source task run. Unrelated stale reflections must not block new ones.
        let has_running: bool = conn
            .query_row(
                r#"SELECT COUNT(*) > 0 FROM task_runs r
                   WHERE r.status = 'running'
                     AND r.is_reflection = 1
                     AND r.workflow_type = 'unified'
                     AND r.reflection_source_task_run_id IN (
                         SELECT s.id FROM task_runs s
                         WHERE s.workflow_name = (
                             SELECT workflow_name FROM task_runs WHERE id = ?1
                         )
                     )"#,
                rusqlite::params![source_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if has_running {
            debug!("Skipping reflection — a reflection for the same workflow is already running");
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

    // Use the gradient convergence score instead of binary threshold
    let metrics = super::prediction::compute_convergence_score(conn, &workflow_name, "workflow")?;

    // Store snapshot for time-series analysis
    let project_path: Option<String> = conn
        .query_row(
            r#"SELECT COALESCE(
                json_extract(s.value, '$.shell_command_working_directory'),
                json_extract(s.value, '$.working_directory')
            )
            FROM unified_workflows uw, json_each(uw.setup_steps) s
            WHERE uw.name = ?1
              AND COALESCE(
                json_extract(s.value, '$.shell_command_working_directory'),
                json_extract(s.value, '$.working_directory')
              ) IS NOT NULL
            LIMIT 1"#,
            rusqlite::params![workflow_name],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    if let Err(e) = super::prediction::store_convergence_snapshot(
        conn,
        &workflow_name,
        project_path.as_deref(),
        "workflow",
        &metrics,
    ) {
        debug!("Could not store convergence snapshot: {}", e);
    }

    // Suppress reflection when convergence score >= 0.85
    if metrics.score >= 0.85 {
        debug!(
            "Workflow '{}' has converged (score={:.3} >= 0.85, clean_runs={}, eff_rate={:.2})",
            workflow_name,
            metrics.score,
            metrics.consecutive_clean_runs,
            metrics.effective_fix_rate
        );
        return Ok(true);
    }

    // Also check for stalled reflection (repeated identical findings)
    if has_repeated_findings(conn, source_task_run_id, &workflow_name)? {
        info!(
            "Workflow '{}' has converged — last {} reflection runs found identical issues (score={:.3})",
            workflow_name, REPEAT_THRESHOLD, metrics.score
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

/// Extract the recurring finding titles when all recent reflection snapshots
/// share the same set of findings.
///
/// Adapts the logic from `has_repeated_findings()` — instead of returning a
/// bool, returns the finding titles from the first snapshot when all sets
/// (across recent reflection runs) are identical. Returns an empty vec when
/// findings are not repeated.
fn get_stall_finding_titles(conn: &Connection, workflow_name: &str) -> Result<Vec<String>, String> {
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
        .map_err(|e| format!("Failed to prepare stall-findings query: {}", e))?;

    let reflection_ids: Vec<String> = stmt
        .query_map(rusqlite::params![workflow_name, REPEAT_THRESHOLD], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("Failed to query reflection runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // Need at least REPEAT_THRESHOLD reflection runs
    if reflection_ids.len() < REPEAT_THRESHOLD as usize {
        return Ok(Vec::new());
    }

    // Collect (signature_or_title) sets for each reflection run, and also
    // keep the titles from the first set for the response
    let mut sig_stmt = conn
        .prepare(
            r#"
            SELECT COALESCE(signature_hash, title) as sig,
                   COALESCE(title, signature_hash) as display_title
            FROM task_run_findings
            WHERE task_run_id = ?1
            ORDER BY sig
            "#,
        )
        .map_err(|e| format!("Failed to prepare signature query: {}", e))?;

    let mut finding_sets: Vec<BTreeSet<String>> = Vec::new();
    let mut first_titles: Vec<String> = Vec::new();

    for (idx, ref_id) in reflection_ids.iter().enumerate() {
        let mut sigs = BTreeSet::new();
        let rows: Vec<(String, String)> = sig_stmt
            .query_map(rusqlite::params![ref_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("Failed to query findings for {}: {}", ref_id, e))?
            .filter_map(|r| r.ok())
            .collect();

        for (sig, title) in rows {
            sigs.insert(sig);
            if idx == 0 {
                first_titles.push(title);
            }
        }
        finding_sets.push(sigs);
    }

    // All sets must be identical and non-empty
    let first = &finding_sets[0];
    if first.is_empty() {
        return Ok(Vec::new());
    }

    let all_identical = finding_sets.iter().all(|s| s == first);
    if all_identical {
        Ok(first_titles)
    } else {
        Ok(Vec::new())
    }
}

/// Build an actionable recommendation by analyzing which fix types have been
/// attempted and their effectiveness rates.
fn build_stall_recommendation(conn: &Connection, workflow_name: &str) -> Result<String, String> {
    // Query fix_type grouped effectiveness stats for this workflow
    let mut stmt = conn
        .prepare(
            r#"
            SELECT rf.fix_type,
                   COUNT(*) as total,
                   SUM(CASE WHEN rf.effectiveness = 'effective' THEN 1 ELSE 0 END) as effective,
                   SUM(CASE WHEN rf.effectiveness = 'ineffective' THEN 1 ELSE 0 END) as ineffective,
                   SUM(CASE WHEN rf.effectiveness = 'caused_regression' THEN 1 ELSE 0 END) as regression
            FROM reflection_fixes rf
            INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
            WHERE tr.workflow_name = ?1
              AND rf.status = 'applied'
            GROUP BY rf.fix_type
            "#,
        )
        .map_err(|e| format!("Failed to prepare recommendation query: {}", e))?;

    struct FixTypeStats {
        fix_type: String,
        total: u32,
        effective: u32,
        ineffective: u32,
        _regression: u32,
    }

    let stats: Vec<FixTypeStats> = stmt
        .query_map(rusqlite::params![workflow_name], |row| {
            Ok(FixTypeStats {
                fix_type: row.get(0)?,
                total: row.get(1)?,
                effective: row.get(2)?,
                ineffective: row.get(3)?,
                _regression: row.get(4)?,
            })
        })
        .map_err(|e| format!("Failed to query fix type stats: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    // All known fix types
    let all_fix_types = [
        "knowledge_base_update",
        "workflow_step_rewrite",
        "selector_fix",
        "tool_config_update",
        "context_addition",
        "instruction_clarification",
    ];

    let attempted_types: BTreeSet<&str> = stats.iter().map(|s| s.fix_type.as_str()).collect();

    // Find unattempted fix types
    let unattempted: Vec<&str> = all_fix_types
        .iter()
        .filter(|t| !attempted_types.contains(*t))
        .copied()
        .collect();

    // Find fix types with low effectiveness
    let low_effectiveness: Vec<(&str, f64)> = stats
        .iter()
        .filter(|s| {
            let evaluated = s.effective + s.ineffective;
            evaluated > 0 && (s.effective as f64 / evaluated as f64) < 0.4
        })
        .map(|s| {
            let evaluated = s.effective + s.ineffective;
            let rate = if evaluated > 0 {
                s.effective as f64 / evaluated as f64
            } else {
                0.0
            };
            (s.fix_type.as_str(), rate)
        })
        .collect();

    let mut parts: Vec<String> = Vec::new();

    if !unattempted.is_empty() {
        let types_str = unattempted
            .iter()
            .map(|t| format!("'{}'", t))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "Consider trying {} fixes (not yet attempted).",
            types_str
        ));
    }

    for (fix_type, rate) in &low_effectiveness {
        parts.push(format!(
            "'{}' has {:.0}% effectiveness — try a different approach.",
            fix_type,
            rate * 100.0
        ));
    }

    if parts.is_empty() {
        if stats.is_empty() {
            parts.push(
                "No fixes have been attempted yet. Run a reflection workflow to start.".to_string(),
            );
        } else {
            parts.push(
                "All attempted fix types show reasonable effectiveness. \
                 Consider manual review of the recurring findings."
                    .to_string(),
            );
        }
    }

    Ok(parts.join(" "))
}

/// Analyze the convergence status of a workflow, producing a human-readable
/// explanation and actionable recommendation.
pub fn analyze_convergence_status(
    conn: &Connection,
    workflow_name: &str,
) -> Result<ConvergenceStatus, String> {
    // 1. Compute convergence metrics
    let metrics = super::prediction::compute_convergence_score(conn, workflow_name, "workflow")?;

    // 2. Check for stalled findings
    let stall_findings = get_stall_finding_titles(conn, workflow_name)?;

    // 3. Determine status
    let (status, reason, recommendation) = if metrics.score >= 0.85 {
        (
            "converging".to_string(),
            format!(
                "Workflow is converging with score {:.2}. {} consecutive clean runs.",
                metrics.score, metrics.consecutive_clean_runs
            ),
            "The workflow is healthy. Continue monitoring for regressions.".to_string(),
        )
    } else if !stall_findings.is_empty() {
        let titles_display = stall_findings
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let reason = format!(
            "Same findings recurring across last {} snapshots: [{}]. Fixes are not resolving the issues.",
            REPEAT_THRESHOLD, titles_display
        );
        let rec = build_stall_recommendation(conn, workflow_name)?;
        ("stalled".to_string(), reason, rec)
    } else if metrics.effective_fix_rate >= 0.6 {
        (
            "improving".to_string(),
            format!(
                "Fixes are effective ({:.0}% effectiveness rate). Workflow is trending toward convergence.",
                metrics.effective_fix_rate * 100.0
            ),
            "Current fix strategy is working. Continue with the same approach.".to_string(),
        )
    } else if metrics.effective_fix_rate < 0.3 && metrics.total_fixes > 3 {
        let rec = build_stall_recommendation(conn, workflow_name)?;
        (
            "regressing".to_string(),
            format!(
                "Low fix effectiveness ({:.0}%). Most attempted fixes are not resolving issues.",
                metrics.effective_fix_rate * 100.0
            ),
            rec,
        )
    } else {
        (
            "insufficient_data".to_string(),
            format!(
                "Not enough data to determine convergence status ({} fixes total).",
                metrics.total_fixes
            ),
            "Run more workflow iterations to gather convergence data.".to_string(),
        )
    };

    Ok(ConvergenceStatus {
        status,
        reason,
        consecutive_clean_runs: metrics.consecutive_clean_runs,
        convergence_score: metrics.score,
        stall_findings,
        recommendation,
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
    let loop_config =
        super::workflow::build_reflection_config(&reflection_id, &reflection_name, &workflow_name);

    let setup_steps = super::workflow::build_setup_steps(&source_task_run_id, &workflow_name);
    let verification_steps = super::workflow::build_verification_steps(&source_task_run_id);
    let mut completion_steps =
        super::workflow::build_completion_steps(&workflow_name, &source_task_run_id);
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

// =============================================================================
// Project Reflection
// =============================================================================

/// Check whether a project reflection should be launched.
///
/// Similar to `should_launch_reflection` but uses project-scoped convergence
/// and has its own "already running" guard keyed by project path.
pub fn should_launch_project_reflection(
    db: &CheckpointDb,
    source_task_run_id: &str,
) -> Result<bool, String> {
    let source_id = source_task_run_id.to_string();

    db.with_conn(|conn| {
        // Guard 0: Block if a project reflection for the SAME workflow is already running
        let has_running: bool = conn
            .query_row(
                r#"SELECT COUNT(*) > 0 FROM task_runs r
                   WHERE r.status = 'running'
                     AND r.is_reflection = 1
                     AND r.workflow_type = 'unified'
                     AND r.workflow_name LIKE 'Project Reflection:%'
                     AND r.reflection_source_task_run_id IN (
                         SELECT s.id FROM task_runs s
                         WHERE s.workflow_name = (
                             SELECT workflow_name FROM task_runs WHERE id = ?1
                         )
                     )"#,
                rusqlite::params![source_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if has_running {
            debug!("Skipping project reflection — one for the same workflow is already running");
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
                "Skipping project reflection for {} — source is already a reflection run",
                source_id
            );
            return Ok(false);
        }

        // Guard 2: Check project_reflection_enabled setting (default: true)
        let project_reflection_enabled: bool = conn
            .query_row(
                "SELECT COALESCE(json_extract(value, '$.project_reflection_enabled'), 'true') FROM settings WHERE key = 'dev_mode'",
                [],
                |row| {
                    let val: String = row.get(0)?;
                    Ok(val == "true" || val == "1")
                },
            )
            .unwrap_or(true);

        if !project_reflection_enabled {
            debug!("Project reflection disabled in settings");
            return Ok(false);
        }

        // Guard 3: Check output threshold
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
                "Skipping project reflection for {} — insufficient output",
                source_id
            );
            return Ok(false);
        }

        // Guard 4: Project-scoped convergence
        // Check last N project reflections for the same workflow — if all produced 0 fixes,
        // the project is well-documented and we can skip.
        let converged = has_project_converged(conn, &source_id)?;
        if converged {
            info!(
                "Skipping project reflection for {} — project knowledge has converged",
                source_id
            );
            return Ok(false);
        }

        Ok(true)
    })
}

/// Check if project reflection has converged — last N project reflections
/// for the same source workflow produced zero reflection fixes.
fn has_project_converged(conn: &Connection, source_task_run_id: &str) -> Result<bool, String> {
    let workflow_name: Option<String> = conn
        .query_row(
            "SELECT workflow_name FROM task_runs WHERE id = ?1",
            rusqlite::params![source_task_run_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get workflow_name: {}", e))?;

    let workflow_name = match workflow_name {
        Some(name) => name,
        None => return Ok(false),
    };

    // Project convergence checks project reflection runs with zero fixes
    // (different semantics from workflow convergence which checks non-reflection runs)
    let mut stmt = conn
        .prepare(
            r#"
            SELECT tr.id,
                   (SELECT COUNT(*) FROM reflection_fixes rf
                    WHERE rf.reflection_task_run_id = tr.id
                      AND rf.reflection_scope = 'project') as fix_count
            FROM task_runs tr
            WHERE tr.workflow_name LIKE 'Project Reflection:%'
              AND tr.is_reflection = 1
              AND tr.status IN ('complete', 'completed')
              AND tr.reflection_source_task_run_id IN (
                SELECT id FROM task_runs WHERE workflow_name = ?1
              )
            ORDER BY tr.completed_at DESC
            LIMIT ?2
            "#,
        )
        .map_err(|e| format!("Failed to prepare project convergence query: {}", e))?;

    let rows: Vec<u32> = stmt
        .query_map(
            rusqlite::params![workflow_name, CONVERGENCE_THRESHOLD],
            |row| row.get::<_, u32>(1),
        )
        .map_err(|e| format!("Failed to query project convergence: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    if rows.len() < CONVERGENCE_THRESHOLD as usize {
        return Ok(false);
    }

    let all_empty = rows.iter().all(|count| *count == 0);

    // Store convergence snapshot for monitoring
    let consecutive_clean = rows.iter().take_while(|&&c| c == 0).count() as u32;
    let clean_ratio = (consecutive_clean as f64 / CONVERGENCE_THRESHOLD as f64).min(1.0);
    let metrics = super::prediction::ConvergenceMetrics {
        score: clean_ratio,
        consecutive_clean_runs: consecutive_clean,
        novelty_score: 0.0,
        effective_fix_rate: if all_empty { 1.0 } else { 0.0 },
        change_velocity: 0.0,
        total_fixes: 0,
        effective_fixes: 0,
    };
    if let Err(e) = super::prediction::store_convergence_snapshot(
        conn,
        &workflow_name,
        None,
        "project",
        &metrics,
    ) {
        debug!("Could not store project convergence snapshot: {}", e);
    }

    if all_empty {
        debug!(
            "Project reflection for '{}' has {} consecutive runs with zero fixes — converged",
            workflow_name,
            rows.len()
        );
    }

    Ok(all_empty)
}

/// Launch a project reflection workflow to learn about the user's project.
///
/// Similar to `launch_reflection` but uses project-scoped workflow builder
/// and stores fixes with `reflection_scope = 'project'`.
pub fn launch_project_reflection(
    deps: ReflectionDeps,
    source_task_run_id: String,
) -> Result<String, String> {
    let db = &deps.app_state.checkpoint_db;

    if !should_launch_project_reflection(db, &source_task_run_id)? {
        return Ok("skipped".to_string());
    }

    // Get source task run details
    let source_id = source_task_run_id.clone();
    let (workflow_name, project_path) = db.with_conn(|conn| {
        let wf_name: String = conn
            .query_row(
                "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = ?1",
                rusqlite::params![source_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| format!("Failed to get source task run: {}", e))?;

        // Try to get project path from the workflow's step JSON arrays.
        // shell_command_working_directory is a per-step field stored in JSON, not a table column.
        let proj_path: Option<String> = conn
            .query_row(
                r#"SELECT COALESCE(
                    json_extract(s.value, '$.shell_command_working_directory'),
                    json_extract(s.value, '$.working_directory')
                )
                FROM unified_workflows uw, json_each(uw.setup_steps) s
                INNER JOIN task_runs tr ON tr.workflow_name = uw.name
                WHERE tr.id = ?1
                  AND COALESCE(
                    json_extract(s.value, '$.shell_command_working_directory'),
                    json_extract(s.value, '$.working_directory')
                  ) IS NOT NULL
                LIMIT 1"#,
                rusqlite::params![source_id],
                |row| row.get(0),
            )
            .ok()
            .flatten()
            // Fallback: check agentic_steps if setup_steps had nothing
            .or_else(|| {
                conn.query_row(
                    r#"SELECT COALESCE(
                        json_extract(s.value, '$.shell_command_working_directory'),
                        json_extract(s.value, '$.working_directory')
                    )
                    FROM unified_workflows uw, json_each(uw.agentic_steps) s
                    INNER JOIN task_runs tr ON tr.workflow_name = uw.name
                    WHERE tr.id = ?1
                      AND COALESCE(
                        json_extract(s.value, '$.shell_command_working_directory'),
                        json_extract(s.value, '$.working_directory')
                      ) IS NOT NULL
                    LIMIT 1"#,
                    rusqlite::params![source_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten()
            });

        Ok((wf_name, proj_path))
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

    db.create_task_run(&input)?;

    // Build the project reflection workflow
    let loop_config = super::workflow::build_project_reflection_config(
        &reflection_id,
        &reflection_name,
        &workflow_name,
        project_path.clone(),
    );

    let setup_steps =
        super::workflow::build_project_setup_steps(&source_task_run_id, &workflow_name);
    let verification_steps = super::workflow::build_project_verification_steps(&source_task_run_id);
    let mut completion_steps = super::workflow::build_project_completion_steps(&workflow_name);
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
    crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
        deps.app_state.checkpoint_db.clone(),
        exec_id,
        wf_name,
        url_lock,
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

    info!(
        "Project reflection '{}' spawned for source {}",
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
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                reflection_task_run_id TEXT NOT NULL,
                source_task_run_id TEXT,
                fix_type TEXT NOT NULL,
                fix_description TEXT NOT NULL,
                confidence TEXT NOT NULL DEFAULT 'medium',
                content_hash TEXT,
                status TEXT DEFAULT 'applied',
                effectiveness TEXT,
                reasoning TEXT,
                alternatives_considered TEXT,
                reflection_scope TEXT DEFAULT 'workflow',
                project_path TEXT,
                target_component TEXT,
                reuse_count INTEGER DEFAULT 0,
                applied_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                applicability_context TEXT,
                fix_description_embedding BLOB
            );
            CREATE TABLE convergence_snapshots (
                id TEXT PRIMARY KEY,
                workflow_name TEXT NOT NULL,
                project_path TEXT,
                scope TEXT NOT NULL DEFAULT 'workflow',
                convergence_score REAL NOT NULL,
                consecutive_clean_runs INTEGER NOT NULL,
                novelty_score REAL NOT NULL,
                effective_fix_rate REAL NOT NULL,
                change_velocity REAL NOT NULL,
                total_fixes INTEGER NOT NULL,
                effective_fixes INTEGER NOT NULL,
                snapshot_at TEXT NOT NULL
            );
            CREATE TABLE unified_workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                setup_steps TEXT DEFAULT '[]',
                agentic_steps TEXT DEFAULT '[]'
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

    // --- Project convergence tests ---

    fn insert_project_reflection_run(
        conn: &Connection,
        id: &str,
        source_workflow: &str,
        source_run_id: &str,
        completed_at: &str,
        project_fix_count: u32,
    ) {
        let reflection_name = format!("Project Reflection: {}", source_workflow);
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, reflection_source_task_run_id, completed_at, created_at, updated_at) \
             VALUES (?1, 'Project Reflection', ?2, 'complete', 1, ?3, ?4, ?4, ?4)",
            rusqlite::params![id, reflection_name, source_run_id, completed_at],
        )
        .unwrap();
        for i in 0..project_fix_count {
            conn.execute(
                "INSERT INTO reflection_fixes (id, reflection_task_run_id, source_task_run_id, fix_type, fix_description, confidence, reflection_scope, created_at) \
                 VALUES (?1, ?2, ?3, 'project_environment', 'test fix', 'high', 'project', ?4)",
                rusqlite::params![format!("{}-pf{}", id, i), id, source_run_id, completed_at],
            )
            .unwrap();
        }
    }

    #[test]
    fn test_has_project_converged_true_when_all_empty() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // Insert CONVERGENCE_THRESHOLD project reflection runs with zero fixes
        for i in 0..CONVERGENCE_THRESHOLD {
            insert_project_reflection_run(
                &conn,
                &format!("proj-ref-{}", i),
                wf,
                "source-1",
                &format!("2025-01-01T{:02}:00:00Z", i + 1),
                0, // zero fixes
            );
        }

        assert!(has_project_converged(&conn, "source-1").unwrap());
    }

    #[test]
    fn test_has_project_converged_false_below_threshold() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // Insert fewer than threshold project reflection runs
        for i in 0..(CONVERGENCE_THRESHOLD - 1) {
            insert_project_reflection_run(
                &conn,
                &format!("proj-ref-{}", i),
                wf,
                "source-1",
                &format!("2025-01-01T{:02}:00:00Z", i + 1),
                0,
            );
        }

        assert!(!has_project_converged(&conn, "source-1").unwrap());
    }

    #[test]
    fn test_has_project_converged_false_when_fixes_exist() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // Insert CONVERGENCE_THRESHOLD runs, but some have fixes
        for i in 0..CONVERGENCE_THRESHOLD {
            let fix_count = if i == 2 { 1 } else { 0 };
            insert_project_reflection_run(
                &conn,
                &format!("proj-ref-{}", i),
                wf,
                "source-1",
                &format!("2025-01-01T{:02}:00:00Z", i + 1),
                fix_count,
            );
        }

        assert!(!has_project_converged(&conn, "source-1").unwrap());
    }

    #[test]
    fn test_has_project_converged_ignores_workflow_scoped_fixes() {
        let conn = setup_test_db();
        let wf = "my-workflow";
        insert_clean_run(&conn, "source-1", wf, "2025-01-01T00:00:00Z");

        // Insert CONVERGENCE_THRESHOLD project reflection runs with zero project fixes
        for i in 0..CONVERGENCE_THRESHOLD {
            let ref_id = format!("proj-ref-{}", i);
            insert_project_reflection_run(
                &conn,
                &ref_id,
                wf,
                "source-1",
                &format!("2025-01-01T{:02}:00:00Z", i + 1),
                0,
            );
            // Add workflow-scoped fixes (should be ignored by project convergence)
            conn.execute(
                "INSERT INTO reflection_fixes (id, reflection_task_run_id, source_task_run_id, fix_type, fix_description, confidence, reflection_scope, created_at) \
                 VALUES (?1, ?2, 'source-1', 'knowledge_base_update', 'workflow fix', 'high', 'workflow', ?3)",
                rusqlite::params![format!("{}-wf", ref_id), ref_id, format!("2025-01-01T{:02}:00:00Z", i + 1)],
            )
            .unwrap();
        }

        // Should still converge — only project-scoped fixes count
        assert!(has_project_converged(&conn, "source-1").unwrap());
    }
}
