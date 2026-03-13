//! Effectiveness evaluation engine for reflection fixes.
//!
//! Uses timestamp-based comparison to determine whether a fix actually resolved
//! the issue it targeted. Compares finding signature hashes before and after
//! the fix was applied across subsequent workflow runs.

use rusqlite::{params, Connection};
use tracing::{debug, info, warn};

use super::causal;
use super::prediction;
use super::storage;
use super::types::{FixEffectiveness, ReflectionFix};
use crate::str_utils::truncate_str;

/// Result of evaluating a single fix's effectiveness.
#[derive(Debug)]
pub struct EvaluationResult {
    pub fix_id: String,
    pub effectiveness: FixEffectiveness,
    pub evidence: String,
}

/// Evaluate the effectiveness of a single reflection fix.
///
/// Algorithm:
/// 1. Get the source finding's signature_hash
/// 2. Find subsequent runs of the same workflow that completed after fix.applied_at
/// 3. Check if the same signature_hash recurs in those runs
/// 4. Check for new findings that could indicate a regression
pub fn evaluate_fix(conn: &Connection, fix: &ReflectionFix) -> Result<EvaluationResult, String> {
    // Only evaluate applied fixes
    if fix.status != "applied" {
        return Ok(EvaluationResult {
            fix_id: fix.id.clone(),
            effectiveness: FixEffectiveness::Inconclusive,
            evidence: format!("Fix status is '{}', not 'applied'", fix.status),
        });
    }

    // Get the workflow name from the source task run
    let workflow_name: Option<String> = conn
        .query_row(
            "SELECT workflow_name FROM task_runs WHERE id = ?1",
            params![fix.source_task_run_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get source task run: {}", e))?;

    let workflow_name = match workflow_name {
        Some(name) => name,
        None => {
            return Ok(EvaluationResult {
                fix_id: fix.id.clone(),
                effectiveness: FixEffectiveness::Inconclusive,
                evidence: "Source task run has no workflow_name".to_string(),
            });
        }
    };

    // Find subsequent non-reflection runs of the same workflow
    let subsequent_run_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id FROM task_runs
                WHERE workflow_name = ?1
                  AND completed_at > ?2
                  AND is_reflection = 0
                  AND status = 'complete'
                ORDER BY completed_at ASC
                "#,
            )
            .map_err(|e| format!("Failed to prepare subsequent runs query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name, fix.applied_at], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| format!("Failed to query subsequent runs: {}", e))?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }
        ids
    };

    if subsequent_run_ids.is_empty() {
        return Ok(EvaluationResult {
            fix_id: fix.id.clone(),
            effectiveness: FixEffectiveness::Inconclusive,
            evidence: "No subsequent runs of this workflow completed yet".to_string(),
        });
    }

    // If we have a source_finding_id, check by signature hash
    if let Some(ref finding_id) = fix.source_finding_id {
        return evaluate_by_finding_signature(conn, fix, finding_id, &subsequent_run_ids);
    }

    // No source finding — use outcome-based heuristic for knowledge/context fixes
    evaluate_by_workflow_outcome(conn, fix, &workflow_name, &subsequent_run_ids)
}

/// Evaluate a fix by checking if its source finding's signature recurs.
fn evaluate_by_finding_signature(
    conn: &Connection,
    fix: &ReflectionFix,
    finding_id: &str,
    subsequent_run_ids: &[String],
) -> Result<EvaluationResult, String> {
    // Get the signature hash from the source finding
    let signature_hash: Option<String> = conn
        .query_row(
            "SELECT signature_hash FROM task_run_findings WHERE id = ?1",
            params![finding_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to get finding signature: {}", e))?;

    let signature_hash = match signature_hash {
        Some(hash) => hash,
        None => {
            return Ok(EvaluationResult {
                fix_id: fix.id.clone(),
                effectiveness: FixEffectiveness::Inconclusive,
                evidence: "Source finding has no signature_hash".to_string(),
            });
        }
    };

    // Check each subsequent run for recurrence of this signature
    let mut recurrence_count = 0;
    let mut checked_runs = 0;

    for run_id in subsequent_run_ids {
        checked_runs += 1;
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = ?1 AND signature_hash = ?2",
                params![run_id, signature_hash],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check recurrence: {}", e))?;

        if count > 0 {
            recurrence_count += 1;
        }
    }

    if recurrence_count > 0 {
        // Check for regression — new findings not seen before the fix
        let has_regression = check_for_regression(conn, fix, subsequent_run_ids)?;
        if has_regression {
            link_regression_fix(conn, fix);
            return Ok(EvaluationResult {
                fix_id: fix.id.clone(),
                effectiveness: FixEffectiveness::CausedRegression,
                evidence: format!(
                    "Finding recurred in {}/{} subsequent runs AND new issues appeared after fix",
                    recurrence_count, checked_runs
                ),
            });
        }

        let reasoning_ctx = fix
            .reasoning
            .as_ref()
            .map(|r| format!(" Original reasoning: {}", truncate_str(r, 200)))
            .unwrap_or_default();
        return Ok(EvaluationResult {
            fix_id: fix.id.clone(),
            effectiveness: FixEffectiveness::Ineffective,
            evidence: format!(
                "Same finding (signature: {}) recurred in {}/{} subsequent runs.{}",
                truncate_str(&signature_hash, 8),
                recurrence_count,
                checked_runs,
                reasoning_ctx,
            ),
        });
    }

    // Link this effective fix to the error event (if any) and record fix application
    link_effective_fix_to_error(conn, fix, &signature_hash);

    Ok(EvaluationResult {
        fix_id: fix.id.clone(),
        effectiveness: FixEffectiveness::Effective,
        evidence: format!(
            "Finding did not recur in {} subsequent run(s)",
            checked_runs
        ),
    })
}

/// When a fix is evaluated as Effective, link it to any matching error events
/// and record a fix application for accumulation monotonicity tracking.
fn link_effective_fix_to_error(conn: &Connection, fix: &ReflectionFix, signature_hash: &str) {
    // Update error_events that match this signature hash
    if let Err(e) = conn.execute(
        r#"UPDATE error_events SET resolved_by_fix_id = ?1
           WHERE signature_hash = ?2 AND resolved_by_fix_id IS NULL"#,
        params![fix.id, signature_hash],
    ) {
        debug!(
            "Could not link fix to error_events (table may not exist): {}",
            e
        );
    }

    // Record a fix application with 'resolved' outcome
    if let Err(e) = prediction::record_fix_application(
        conn,
        &fix.id,
        &fix.source_task_run_id,
        Some(signature_hash),
        "resolved",
    ) {
        debug!("Could not record fix application: {}", e);
    }

    // Create automated causal link: fix_applied → finding_detected (resolved)
    if let Some(ref finding_id) = fix.source_finding_id {
        if let Err(e) = causal::insert_causal_event(
            conn,
            "fix_applied",
            &fix.id,
            "finding_detected",
            finding_id,
            "resolved",
            "high",
            "automated",
            Some(&fix.source_task_run_id),
            None,
            Some("Fix evaluated as effective — finding did not recur"),
        ) {
            debug!("Could not create causal link for effective fix: {}", e);
        }
    }
}

/// When a fix causes a regression, create a causal link recording the relationship.
fn link_regression_fix(conn: &Connection, fix: &ReflectionFix) {
    if let Some(ref finding_id) = fix.source_finding_id {
        if let Err(e) = causal::insert_causal_event(
            conn,
            "fix_applied",
            &fix.id,
            "finding_detected",
            finding_id,
            "caused",
            "high",
            "automated",
            Some(&fix.source_task_run_id),
            None,
            Some("Fix evaluated as causing regression — new issues appeared"),
        ) {
            debug!("Could not create causal link for regression fix: {}", e);
        }
    }
}

/// Check if any new findings appeared in subsequent runs that weren't present before the fix.
fn check_for_regression(
    conn: &Connection,
    fix: &ReflectionFix,
    subsequent_run_ids: &[String],
) -> Result<bool, String> {
    for run_id in subsequent_run_ids {
        let count: u32 = conn
            .query_row(
                r#"
                SELECT COUNT(*) FROM task_run_findings
                WHERE task_run_id = ?1
                  AND detected_at > ?2
                  AND signature_hash NOT IN (
                      SELECT DISTINCT signature_hash FROM task_run_findings
                      WHERE detected_at < ?2 AND signature_hash IS NOT NULL
                  )
                  AND signature_hash IS NOT NULL
                "#,
                params![run_id, fix.applied_at],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to check for regression: {}", e))?;

        if count > 0 {
            debug!(
                "Found {} potential regression findings in run {}",
                count, run_id
            );
            return Ok(true);
        }
    }

    Ok(false)
}

/// Evaluate a fix without a source finding by comparing workflow outcomes.
///
/// For knowledge_base_update and context_addition fixes, checks if subsequent
/// runs have fewer findings than the source run. This is a weaker signal than
/// signature-based tracking but better than permanent "inconclusive".
fn evaluate_by_workflow_outcome(
    conn: &Connection,
    fix: &ReflectionFix,
    _workflow_name: &str,
    subsequent_run_ids: &[String],
) -> Result<EvaluationResult, String> {
    // Count findings in the source run
    let source_findings: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = ?1",
            params![fix.source_task_run_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Failed to count source findings: {}", e))?;

    // Count findings across subsequent runs (average per run)
    let mut total_subsequent_findings: u32 = 0;
    for run_id in subsequent_run_ids {
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to count subsequent findings: {}", e))?;
        total_subsequent_findings += count;
    }

    let avg_subsequent = total_subsequent_findings as f64 / subsequent_run_ids.len() as f64;

    // For knowledge/context fixes: if subsequent runs are cleaner, mark effective
    let result = match fix.fix_type.as_str() {
        "knowledge_base_update"
        | "context_addition"
        | "instruction_clarification"
        | "project_environment"
        | "project_architecture"
        | "project_test_pattern"
        | "project_recurring_issue" => {
            if source_findings == 0 && total_subsequent_findings == 0 {
                // Source had no findings either — fix may be preventive, mark effective
                // if subsequent runs succeeded
                EvaluationResult {
                    fix_id: fix.id.clone(),
                    effectiveness: FixEffectiveness::Effective,
                    evidence: format!(
                        "Knowledge/context fix with {} subsequent successful run(s), \
                         no findings in source or subsequent runs",
                        subsequent_run_ids.len()
                    ),
                }
            } else if avg_subsequent < source_findings as f64 {
                EvaluationResult {
                    fix_id: fix.id.clone(),
                    effectiveness: FixEffectiveness::Effective,
                    evidence: format!(
                        "Findings decreased: {} in source → {:.1} avg in {} subsequent run(s)",
                        source_findings,
                        avg_subsequent,
                        subsequent_run_ids.len()
                    ),
                }
            } else {
                EvaluationResult {
                    fix_id: fix.id.clone(),
                    effectiveness: FixEffectiveness::Inconclusive,
                    evidence: format!(
                        "Findings did not decrease: {} in source → {:.1} avg in {} subsequent run(s). \
                         No source finding linked for precise tracking.",
                        source_findings, avg_subsequent, subsequent_run_ids.len()
                    ),
                }
            }
        }
        // For other fix types without source findings: if both source and
        // subsequent runs have zero findings, the workflow is clean — mark effective.
        // Otherwise remain inconclusive without signature-based tracking.
        _ => {
            if source_findings == 0 && total_subsequent_findings == 0 {
                EvaluationResult {
                    fix_id: fix.id.clone(),
                    effectiveness: FixEffectiveness::Effective,
                    evidence: format!(
                        "Fix type '{}' with no source finding, but {} subsequent successful \
                         run(s) with zero findings — workflow is clean",
                        fix.fix_type,
                        subsequent_run_ids.len()
                    ),
                }
            } else if avg_subsequent < source_findings as f64 {
                EvaluationResult {
                    fix_id: fix.id.clone(),
                    effectiveness: FixEffectiveness::Effective,
                    evidence: format!(
                        "Fix type '{}': findings decreased {} → {:.1} avg across {} subsequent run(s)",
                        fix.fix_type, source_findings, avg_subsequent, subsequent_run_ids.len()
                    ),
                }
            } else {
                EvaluationResult {
                    fix_id: fix.id.clone(),
                    effectiveness: FixEffectiveness::Inconclusive,
                    evidence: format!(
                        "Fix type '{}' has no source finding — cannot track recurrence. \
                         {} subsequent run(s) exist.",
                        fix.fix_type,
                        subsequent_run_ids.len()
                    ),
                }
            }
        }
    };

    // Record fix application for accumulation monotonicity tracking
    // (outcome-based path has no signature_hash, so pass None)
    if result.effectiveness == FixEffectiveness::Effective {
        if let Err(e) = prediction::record_fix_application(
            conn,
            &fix.id,
            &fix.source_task_run_id,
            None,
            "resolved",
        ) {
            debug!("Could not record outcome-based fix application: {}", e);
        }
    }

    Ok(result)
}

/// Batch evaluate all unevaluated fixes for a workflow.
/// Also re-evaluates fixes previously marked 'inconclusive' in case new
/// subsequent runs now provide enough signal to determine effectiveness.
///
/// Called during the completion phase of each reflection run.
pub fn evaluate_pending_fixes(
    conn: &Connection,
    workflow_name: &str,
) -> Result<Vec<EvaluationResult>, String> {
    // Get all applied, unevaluated fixes for this workflow
    let mut fixes = storage::get_fixes_by_workflow_name(
        conn,
        workflow_name,
        Some("applied"),
        Some("unevaluated"),
    )?;

    // Also re-evaluate inconclusive fixes — they may now have enough
    // subsequent run data to transition to 'effective' or 'ineffective'
    let inconclusive_fixes = storage::get_fixes_by_workflow_name(
        conn,
        workflow_name,
        Some("applied"),
        Some("inconclusive"),
    )?;

    if !inconclusive_fixes.is_empty() {
        info!(
            "Re-evaluating {} previously inconclusive fixes for workflow '{}'",
            inconclusive_fixes.len(),
            workflow_name
        );
    }
    fixes.extend(inconclusive_fixes);

    if fixes.is_empty() {
        debug!(
            "No pending fixes to evaluate for workflow '{}'",
            workflow_name
        );
        return Ok(Vec::new());
    }

    info!(
        "Evaluating {} total fixes for workflow '{}'",
        fixes.len(),
        workflow_name
    );

    let mut results = Vec::new();
    for fix in &fixes {
        match evaluate_fix(conn, fix) {
            Ok(result) => {
                // Only persist if the evaluation changed (avoid unnecessary writes
                // for fixes that remain inconclusive)
                let changed = fix.effectiveness.as_deref() != Some(result.effectiveness.as_str());

                if changed {
                    if let Err(e) = storage::update_fix_effectiveness(
                        conn,
                        &result.fix_id,
                        result.effectiveness.as_str(),
                        Some(&result.evidence),
                    ) {
                        warn!(
                            "Failed to persist evaluation for fix {}: {}",
                            result.fix_id, e
                        );
                    }
                }
                results.push(result);
            }
            Err(e) => {
                warn!("Failed to evaluate fix {}: {}", fix.id, e);
            }
        }
    }

    let effective_count = results
        .iter()
        .filter(|r| r.effectiveness == FixEffectiveness::Effective)
        .count();
    let ineffective_count = results
        .iter()
        .filter(|r| r.effectiveness == FixEffectiveness::Ineffective)
        .count();
    let inconclusive_count = results
        .iter()
        .filter(|r| r.effectiveness == FixEffectiveness::Inconclusive)
        .count();

    info!(
        "Evaluated {} fixes: {} effective, {} ineffective, {} inconclusive",
        results.len(),
        effective_count,
        ineffective_count,
        inconclusive_count,
    );

    // Check for fixes that should be auto-promoted to universal scope
    if let Ok(promoted) = check_cross_project_promotion(conn) {
        if !promoted.is_empty() {
            info!("Auto-promoted {} fixes to universal scope", promoted.len());
        }
    }

    Ok(results)
}

/// Auto-promote fixes that are effective across 2+ different project_paths.
///
/// When the same fix (by content_hash) independently proves effective in
/// multiple projects, it's a strong signal that the pattern is universal.
pub fn check_cross_project_promotion(conn: &Connection) -> Result<Vec<String>, String> {
    let sql = r#"
        SELECT content_hash, GROUP_CONCAT(DISTINCT project_path) as paths, MIN(id) as canonical_id
        FROM reflection_fixes
        WHERE reflection_scope IN ('workflow', 'project')
          AND effectiveness = 'effective'
          AND status = 'applied'
          AND content_hash IS NOT NULL
          AND project_path IS NOT NULL
        GROUP BY content_hash
        HAVING COUNT(DISTINCT project_path) >= 2
    "#;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to prepare cross-project promotion query: {}", e))?;

    let candidates: Vec<(String, String, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("Failed to query cross-project candidates: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let mut promoted = Vec::new();
    for (content_hash, paths, canonical_id) in &candidates {
        let applicability = format!("Proven effective across: {}", paths);
        if let Err(e) = storage::promote_to_universal(conn, canonical_id, Some(&applicability)) {
            warn!(
                "Failed to promote fix {} (hash: {}): {}",
                canonical_id, content_hash, e
            );
        } else {
            info!(
                "Auto-promoted fix {} to universal (hash: {}, projects: {})",
                canonical_id, content_hash, paths
            );
            promoted.push(canonical_id.clone());
        }
    }

    Ok(promoted)
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
                detected_at TEXT,
                reflection_fix_id TEXT
            );
            CREATE TABLE task_knowledge (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                reflection_fix_id TEXT
            );
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                source_knowledge_id TEXT,
                fix_type TEXT NOT NULL,
                fix_description TEXT NOT NULL,
                file_changed TEXT,
                old_value TEXT,
                new_value TEXT,
                confidence TEXT NOT NULL DEFAULT 'medium',
                content_hash TEXT,
                status TEXT NOT NULL DEFAULT 'applied',
                effectiveness TEXT,
                effectiveness_evidence TEXT,
                applied_at TEXT NOT NULL,
                evaluated_at TEXT,
                created_at TEXT NOT NULL,
                reuse_count INTEGER DEFAULT 0,
                reasoning TEXT,
                alternatives_considered TEXT,
                reflection_scope TEXT DEFAULT 'workflow',
                project_path TEXT,
                applicability_context TEXT,
                fix_description_embedding BLOB
            );
            CREATE TABLE error_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                signature_hash TEXT,
                resolved_by_fix_id TEXT
            );
            CREATE TABLE fix_applications (
                id TEXT PRIMARY KEY,
                fix_id TEXT NOT NULL,
                task_run_id TEXT NOT NULL,
                error_signature_hash TEXT,
                outcome TEXT DEFAULT 'pending',
                applied_at TEXT NOT NULL,
                evaluated_at TEXT
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_evaluate_fix_no_subsequent_runs() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: Some("f-1".to_string()),
            source_knowledge_id: None,
            fix_type: "selector_fix".to_string(),
            fix_description: "Fixed selector".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Inconclusive);
    }

    #[test]
    fn test_evaluate_fix_effective() {
        let conn = setup_test_db();

        // Source run
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        // Source finding
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-1', 'src-1', 'hash-abc', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        // Subsequent run (after fix applied at 01:00) with NO recurrence
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) VALUES ('run-2', 'Test', 'wf-1', 'complete', 0, '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: Some("f-1".to_string()),
            source_knowledge_id: None,
            fix_type: "selector_fix".to_string(),
            fix_description: "Fixed selector".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Effective);
        assert!(result.evidence.contains("did not recur"));
    }

    #[test]
    fn test_evaluate_fix_ineffective() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-1', 'src-1', 'hash-abc', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        // Subsequent run WITH recurrence of the same finding
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) VALUES ('run-2', 'Test', 'wf-1', 'complete', 0, '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-2', 'run-2', 'hash-abc', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: Some("f-1".to_string()),
            source_knowledge_id: None,
            fix_type: "selector_fix".to_string(),
            fix_description: "Fixed selector".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Ineffective);
        assert!(result.evidence.contains("recurred"));
    }

    #[test]
    fn test_evaluate_knowledge_fix_effective_when_findings_decrease() {
        let conn = setup_test_db();

        // Source run with 5 findings
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();
        for i in 0..5 {
            conn.execute(
                &format!("INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-{}', 'src-1', 'hash-{}', '2025-01-01T00:00:00Z')", i, i),
                [],
            ).unwrap();
        }

        // Subsequent run with 1 finding (improvement)
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) VALUES ('run-2', 'Test', 'wf-1', 'complete', 0, '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-sub', 'run-2', 'hash-new', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: None, // No source finding!
            source_knowledge_id: None,
            fix_type: "knowledge_base_update".to_string(),
            fix_description: "Added knowledge".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Effective);
        assert!(result.evidence.contains("decreased"));
    }

    #[test]
    fn test_evaluate_knowledge_fix_effective_when_zero_findings() {
        let conn = setup_test_db();

        // Source run with 0 findings
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        // Subsequent run with 0 findings
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) VALUES ('run-2', 'Test', 'wf-1', 'complete', 0, '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "context_addition".to_string(),
            fix_description: "Added context".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "medium".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Effective);
        assert!(result.evidence.contains("no findings"));
    }

    #[test]
    fn test_evaluate_other_fix_type_effective_when_zero_findings() {
        let conn = setup_test_db();

        // Source run with 0 findings
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();

        // Subsequent run with 0 findings
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) VALUES ('run-2', 'Test', 'wf-1', 'complete', 0, '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "selector_fix".to_string(),
            fix_description: "Fixed selector".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        // Now marks effective since both source and subsequent have zero findings
        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Effective);
        assert!(result.evidence.contains("workflow is clean"));
    }

    #[test]
    fn test_evaluate_other_fix_type_inconclusive_when_findings_persist() {
        let conn = setup_test_db();

        // Source run with 2 findings
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, completed_at, created_at, updated_at) VALUES ('src-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        ).unwrap();
        for i in 0..2 {
            conn.execute(
                &format!("INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-src-{}', 'src-1', 'hash-{}', '2025-01-01T00:00:00Z')", i, i),
                [],
            ).unwrap();
        }

        // Subsequent run with 3 findings (more than source)
        conn.execute(
            "INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection, completed_at, created_at, updated_at) VALUES ('run-2', 'Test', 'wf-1', 'complete', 0, '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z', '2025-01-01T02:00:00Z')",
            [],
        ).unwrap();
        for i in 0..3 {
            conn.execute(
                &format!("INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at) VALUES ('f-sub-{}', 'run-2', 'hash-sub-{}', '2025-01-01T02:00:00Z')", i, i),
                [],
            ).unwrap();
        }

        let fix = ReflectionFix {
            id: "fix-1".to_string(),
            source_task_run_id: "src-1".to_string(),
            reflection_task_run_id: "ref-1".to_string(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: "selector_fix".to_string(),
            fix_description: "Fixed selector".to_string(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: "high".to_string(),
            content_hash: None,
            status: "applied".to_string(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2025-01-01T01:00:00Z".to_string(),
            evaluated_at: None,
            created_at: "2025-01-01T01:00:00Z".to_string(),
            source_agent: None,
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        };

        let result = evaluate_fix(&conn, &fix).unwrap();
        assert_eq!(result.effectiveness, FixEffectiveness::Inconclusive);
    }
}
