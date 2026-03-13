//! Prediction engine for the cognitive system model.
//!
//! Computes three knowledge properties and uses them for prediction:
//! - **Accumulation monotonicity**: Track fix applications → predict fixes for known errors
//! - **Convergence gradient**: Compute continuous 0.0–1.0 score instead of binary threshold
//! - **Relevance decay**: Score knowledge by effectiveness × recency × stability

use rusqlite::{params, Connection};
use serde::Serialize;
use tracing::{debug, info};

/// Number of consecutive clean runs at which convergence reaches 1.0 for the clean_ratio component.
const CONVERGENCE_THRESHOLD: f64 = 5.0;

/// Default sliding window size for change velocity computation.
const DEFAULT_VELOCITY_WINDOW: u32 = 5;

// =============================================================================
// Types
// =============================================================================

/// A predicted fix for an error, based on historical fix application data.
#[derive(Debug, Clone, Serialize)]
pub struct PredictedFix {
    pub fix_id: String,
    pub fix_description: String,
    pub confidence: f64,
    pub reuse_count: u32,
    pub effective_rate: f64,
}

/// Convergence metrics for a workflow or project scope.
#[derive(Debug, Clone, Serialize)]
pub struct ConvergenceMetrics {
    pub score: f64,
    pub consecutive_clean_runs: u32,
    pub novelty_score: f64,
    pub effective_fix_rate: f64,
    pub change_velocity: f64,
    pub total_fixes: u32,
    pub effective_fixes: u32,
}

/// A knowledge entry scored by relevance for token-budget prioritization.
#[derive(Debug, Clone, Serialize)]
pub struct ScoredKnowledge {
    pub knowledge_id: String,
    pub content: String,
    pub category: String,
    pub relevance_score: f64,
    pub effectiveness_weight: f64,
    pub recency_weight: f64,
    pub velocity_factor: f64,
}

// =============================================================================
// Fix Prediction (Accumulation Monotonicity)
// =============================================================================

/// Predict which fix is most likely to resolve an error with the given signature hash.
///
/// Queries `fix_applications` for previous fixes applied to the same error signature,
/// filters to resolved outcomes, and scores candidates by reuse_count × effective_rate × recency.
pub fn predict_fix_for_error(
    conn: &Connection,
    error_signature_hash: &str,
) -> Result<Option<PredictedFix>, String> {
    // Find fixes that have previously resolved this error signature
    let mut stmt = conn
        .prepare(
            r#"
            SELECT fa.fix_id,
                   rf.fix_description,
                   rf.reuse_count,
                   rf.applied_at,
                   COUNT(*) as resolved_count,
                   (SELECT COUNT(*) FROM fix_applications fa2
                    WHERE fa2.fix_id = fa.fix_id) as total_applications
            FROM fix_applications fa
            INNER JOIN reflection_fixes rf ON rf.id = fa.fix_id
            WHERE fa.error_signature_hash = ?1
              AND fa.outcome = 'resolved'
              AND rf.status = 'applied'
            GROUP BY fa.fix_id
            ORDER BY resolved_count DESC
            LIMIT 5
            "#,
        )
        .map_err(|e| format!("Failed to prepare fix prediction query: {}", e))?;

    struct Candidate {
        fix_id: String,
        description: String,
        reuse_count: u32,
        applied_at: String,
        resolved_count: u32,
        total_applications: u32,
    }

    let candidates: Vec<Candidate> = stmt
        .query_map(params![error_signature_hash], |row| {
            Ok(Candidate {
                fix_id: row.get(0)?,
                description: row.get(1)?,
                reuse_count: row.get(2)?,
                applied_at: row.get(3)?,
                resolved_count: row.get(4)?,
                total_applications: row.get(5)?,
            })
        })
        .map_err(|e| format!("Failed to query fix predictions: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    if candidates.is_empty() {
        return Ok(None);
    }

    // Score each candidate: effective_rate × reuse_count × recency_weight
    let now = chrono::Utc::now();
    let mut best: Option<PredictedFix> = None;
    let mut best_score = 0.0f64;

    for c in &candidates {
        let effective_rate = if c.total_applications > 0 {
            c.resolved_count as f64 / c.total_applications as f64
        } else {
            0.0
        };

        let days_old = chrono::DateTime::parse_from_rfc3339(&c.applied_at)
            .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days() as f64)
            .unwrap_or(30.0);
        let recency_weight = 1.0 / (1.0 + days_old * 0.1);

        let score = effective_rate * (c.reuse_count as f64 + 1.0) * recency_weight;

        if score > best_score {
            best_score = score;
            best = Some(PredictedFix {
                fix_id: c.fix_id.clone(),
                fix_description: c.description.clone(),
                confidence: (effective_rate * recency_weight).min(1.0),
                reuse_count: c.reuse_count,
                effective_rate,
            });
        }
    }

    if let Some(ref fix) = best {
        debug!(
            "Predicted fix {} for signature {} (confidence: {:.2}, reuse_count: {})",
            fix.fix_id, error_signature_hash, fix.confidence, fix.reuse_count
        );
    }

    Ok(best)
}

// =============================================================================
// Convergence Score (Convergence Gradient)
// =============================================================================

/// Compute a continuous convergence score (0.0–1.0) for a workflow.
///
/// Formula: `score = clean_ratio × (1.0 - novelty_score) × effective_fix_rate`
///
/// Components:
/// - `clean_ratio`: min(consecutive_clean_runs / CONVERGENCE_THRESHOLD, 1.0)
/// - `novelty_score`: ratio of unique new fix content hashes in last 3 runs vs total
/// - `effective_fix_rate`: effective / (effective + ineffective + regression)
/// - `change_velocity`: fixes per run in sliding window
pub fn compute_convergence_score(
    conn: &Connection,
    workflow_name: &str,
    scope: &str,
) -> Result<ConvergenceMetrics, String> {
    // 1. Count consecutive clean runs from most recent
    let consecutive_clean = count_consecutive_clean_runs(conn, workflow_name)?;
    let clean_ratio = (consecutive_clean as f64 / CONVERGENCE_THRESHOLD).min(1.0);

    // 2. Compute novelty score: new unique fix hashes in last 3 runs vs total
    let novelty_score = compute_novelty_score(conn, workflow_name)?;

    // 3. Compute effective fix rate
    let (total_fixes, effective_fixes, effective_rate) =
        compute_effective_fix_rate(conn, workflow_name, scope)?;

    // 4. Compute change velocity
    let change_velocity =
        compute_change_velocity_for_workflow(conn, workflow_name, DEFAULT_VELOCITY_WINDOW)?;

    // Composite score
    let score = clean_ratio * (1.0 - novelty_score) * effective_rate.max(0.1); // floor at 0.1 so zero-fix workflows can still converge

    let metrics = ConvergenceMetrics {
        score: score.min(1.0),
        consecutive_clean_runs: consecutive_clean,
        novelty_score,
        effective_fix_rate: effective_rate,
        change_velocity,
        total_fixes,
        effective_fixes,
    };

    debug!(
        "Convergence for '{}' (scope={}): score={:.3}, clean_runs={}, novelty={:.2}, eff_rate={:.2}, velocity={:.2}",
        workflow_name, scope, metrics.score, consecutive_clean, novelty_score, effective_rate, change_velocity
    );

    Ok(metrics)
}

/// Store a convergence snapshot for time-series analysis.
pub fn store_convergence_snapshot(
    conn: &Connection,
    workflow_name: &str,
    project_path: Option<&str>,
    scope: &str,
    metrics: &ConvergenceMetrics,
) -> Result<(), String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        r#"INSERT INTO convergence_snapshots
           (id, workflow_name, project_path, scope, convergence_score,
            consecutive_clean_runs, novelty_score, effective_fix_rate,
            change_velocity, total_fixes, effective_fixes, snapshot_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, datetime('now'))"#,
        params![
            id,
            workflow_name,
            project_path,
            scope,
            metrics.score,
            metrics.consecutive_clean_runs,
            metrics.novelty_score,
            metrics.effective_fix_rate,
            metrics.change_velocity,
            metrics.total_fixes,
            metrics.effective_fixes,
        ],
    )
    .map_err(|e| format!("Failed to store convergence snapshot: {}", e))?;

    Ok(())
}

fn count_consecutive_clean_runs(conn: &Connection, workflow_name: &str) -> Result<u32, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT (SELECT COUNT(*) FROM task_run_findings WHERE task_run_id = tr.id) as finding_count
            FROM task_runs tr
            WHERE tr.workflow_name = ?1
              AND tr.is_reflection = 0
              AND tr.status IN ('complete', 'completed')
            ORDER BY tr.completed_at DESC
            LIMIT 20
            "#,
        )
        .map_err(|e| format!("Failed to prepare clean runs query: {}", e))?;

    let counts: Vec<u32> = stmt
        .query_map(params![workflow_name], |row| row.get::<_, u32>(0))
        .map_err(|e| format!("Failed to query clean runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let consecutive = counts.iter().take_while(|&&c| c == 0).count() as u32;
    Ok(consecutive)
}

fn compute_novelty_score(conn: &Connection, workflow_name: &str) -> Result<f64, String> {
    // Get task run IDs for the last 3 runs
    let recent_run_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id FROM task_runs
                WHERE workflow_name = ?1
                  AND is_reflection = 0
                  AND status IN ('complete', 'completed')
                ORDER BY completed_at DESC
                LIMIT 3
                "#,
            )
            .map_err(|e| format!("Failed to prepare recent runs query: {}", e))?;

        let rows = stmt
            .query_map(params![workflow_name], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query recent runs: {}", e))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    if recent_run_ids.is_empty() {
        return Ok(0.0);
    }

    // Count unique fix content_hashes in recent runs
    let placeholders: Vec<String> = recent_run_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect();
    let query = format!(
        r#"SELECT COUNT(DISTINCT content_hash)
           FROM reflection_fixes
           WHERE source_task_run_id IN ({})
             AND content_hash IS NOT NULL"#,
        placeholders.join(",")
    );

    let recent_unique: u32 = {
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("Failed to prepare novelty query: {}", e))?;

        let params: Vec<&dyn rusqlite::ToSql> = recent_run_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        stmt.query_row(params.as_slice(), |row| row.get::<_, u32>(0))
            .unwrap_or(0)
    };

    // Count total unique content_hashes for the workflow
    let total_unique: u32 = conn
        .query_row(
            r#"SELECT COUNT(DISTINCT rf.content_hash)
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name = ?1
                 AND rf.content_hash IS NOT NULL"#,
            params![workflow_name],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if total_unique == 0 {
        return Ok(0.0);
    }

    Ok(recent_unique as f64 / total_unique as f64)
}

fn compute_effective_fix_rate(
    conn: &Connection,
    workflow_name: &str,
    scope: &str,
) -> Result<(u32, u32, f64), String> {
    let row = conn
        .query_row(
            r#"SELECT
                 COUNT(*) as total,
                 SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END) as effective,
                 SUM(CASE WHEN effectiveness = 'ineffective' THEN 1 ELSE 0 END) as ineffective,
                 SUM(CASE WHEN effectiveness = 'caused_regression' THEN 1 ELSE 0 END) as regression
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name = ?1
                 AND rf.reflection_scope = ?2
                 AND rf.effectiveness IS NOT NULL"#,
            params![workflow_name, scope],
            |row| {
                Ok((
                    row.get::<_, u32>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            },
        )
        .unwrap_or((0, 0, 0, 0));

    let (total, effective, ineffective, regression) = row;
    let denominator = effective + ineffective + regression;
    let rate = if denominator > 0 {
        effective as f64 / denominator as f64
    } else {
        1.0 // No evaluated fixes → assume good
    };

    Ok((total, effective, rate))
}

fn compute_change_velocity_for_workflow(
    conn: &Connection,
    workflow_name: &str,
    window_size: u32,
) -> Result<f64, String> {
    // Count fixes across the last `window_size` runs
    let fix_count: u32 = conn
        .query_row(
            r#"SELECT COUNT(*)
               FROM reflection_fixes rf
               INNER JOIN task_runs tr ON tr.id = rf.source_task_run_id
               WHERE tr.workflow_name = ?1
                 AND tr.is_reflection = 0
                 AND tr.completed_at >= (
                     SELECT COALESCE(MIN(completed_at), '1970-01-01')
                     FROM (
                         SELECT completed_at FROM task_runs
                         WHERE workflow_name = ?1
                           AND is_reflection = 0
                           AND status IN ('complete', 'completed')
                         ORDER BY completed_at DESC
                         LIMIT ?2
                     )
                 )"#,
            params![workflow_name, window_size],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok(fix_count as f64 / window_size as f64)
}

// =============================================================================
// Change Velocity (per-component)
// =============================================================================

/// Compute change velocity for a specific component (file/module).
///
/// Returns fixes per run targeting `component_path` over the last `window_size` runs.
/// Higher values indicate more volatile areas whose knowledge decays faster.
pub fn compute_change_velocity(
    conn: &Connection,
    component_path: &str,
    window_size: u32,
) -> Result<f64, String> {
    let fix_count: u32 = conn
        .query_row(
            r#"SELECT COUNT(*)
               FROM reflection_fixes
               WHERE target_component = ?1
                 AND applied_at >= (
                     SELECT COALESCE(MIN(applied_at), '1970-01-01')
                     FROM (
                         SELECT DISTINCT applied_at FROM reflection_fixes
                         WHERE target_component = ?1
                         ORDER BY applied_at DESC
                         LIMIT ?2
                     )
                 )"#,
            params![component_path, window_size],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok(fix_count as f64 / window_size as f64)
}

// =============================================================================
// Knowledge Relevance Scoring (Relevance Decay)
// =============================================================================

/// Score knowledge entries by relevance for token-budget prioritization.
///
/// For each entry computes:
/// - `effectiveness_weight`: 1.0 (from effective fix), 0.5 (inconclusive), 0.0 (ineffective)
/// - `recency_weight`: 1.0 / (1.0 + days_since_last_validation × 0.1)
/// - `velocity_factor`: 1.0 / (1.0 + change_velocity) for the target component
/// - `relevance_score = effectiveness_weight × recency_weight × velocity_factor`
///
/// Returns entries sorted by relevance_score descending.
pub fn score_knowledge_relevance(
    conn: &Connection,
    workflow_name: &str,
    limit: u32,
) -> Result<Vec<ScoredKnowledge>, String> {
    // Load knowledge entries for this workflow's project
    let mut stmt = conn
        .prepare(
            r#"
            SELECT tk.id, tk.content, tk.category, tk.last_validated_at,
                   tk.reflection_fix_id, tk.created_at
            FROM task_knowledge tk
            INNER JOIN task_runs tr ON tr.id = tk.task_run_id
            WHERE tr.workflow_name = ?1
              AND tk.is_resolved = 0
              AND tk.category IN ('recurring_pattern', 'context', 'finding', 'root_cause',
                                  'project_environment', 'project_architecture',
                                  'project_test_pattern', 'project_recurring_issue')
            ORDER BY tk.created_at DESC
            LIMIT 50
            "#,
        )
        .map_err(|e| format!("Failed to prepare knowledge scoring query: {}", e))?;

    struct RawEntry {
        id: String,
        content: String,
        category: String,
        last_validated_at: Option<String>,
        reflection_fix_id: Option<String>,
        created_at: String,
    }

    let entries: Vec<RawEntry> = stmt
        .query_map(params![workflow_name], |row| {
            Ok(RawEntry {
                id: row.get(0)?,
                content: row.get(1)?,
                category: row.get(2)?,
                last_validated_at: row.get(3)?,
                reflection_fix_id: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(|e| format!("Failed to query knowledge entries: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let now = chrono::Utc::now();
    let mut scored: Vec<ScoredKnowledge> = Vec::with_capacity(entries.len());

    for entry in &entries {
        // Effectiveness weight: check linked fix if any
        let effectiveness_weight = if let Some(ref fix_id) = entry.reflection_fix_id {
            let eff: Option<String> = conn
                .query_row(
                    "SELECT effectiveness FROM reflection_fixes WHERE id = ?1",
                    params![fix_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten();

            match eff.as_deref() {
                Some("effective") => 1.0,
                Some("ineffective") | Some("caused_regression") => 0.0,
                Some("inconclusive") => 0.5,
                _ => 0.5, // unevaluated
            }
        } else {
            0.7 // No linked fix — moderately relevant
        };

        // Recency weight: based on last validation or creation date
        let reference_date = entry
            .last_validated_at
            .as_deref()
            .unwrap_or(&entry.created_at);
        let days_old = chrono::DateTime::parse_from_rfc3339(reference_date)
            .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_days() as f64)
            .unwrap_or(30.0);
        let recency_weight = 1.0 / (1.0 + days_old * 0.1);

        // Velocity factor: check linked fix's target_component
        let velocity_factor = if let Some(ref fix_id) = entry.reflection_fix_id {
            let component: Option<String> = conn
                .query_row(
                    "SELECT target_component FROM reflection_fixes WHERE id = ?1",
                    params![fix_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten();

            if let Some(comp) = component {
                let velocity =
                    compute_change_velocity(conn, &comp, DEFAULT_VELOCITY_WINDOW).unwrap_or(0.0);
                1.0 / (1.0 + velocity)
            } else {
                1.0 // No target component → no velocity penalty
            }
        } else {
            1.0
        };

        let relevance_score = effectiveness_weight * recency_weight * velocity_factor;

        scored.push(ScoredKnowledge {
            knowledge_id: entry.id.clone(),
            content: entry.content.clone(),
            category: entry.category.clone(),
            relevance_score,
            effectiveness_weight,
            recency_weight,
            velocity_factor,
        });
    }

    // Sort by relevance descending
    scored.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored.truncate(limit as usize);

    if !scored.is_empty() {
        info!(
            "Scored {} knowledge entries for '{}': top relevance={:.3}, bottom={:.3}",
            scored.len(),
            workflow_name,
            scored.first().map(|s| s.relevance_score).unwrap_or(0.0),
            scored.last().map(|s| s.relevance_score).unwrap_or(0.0),
        );
    }

    Ok(scored)
}

// =============================================================================
// Fix Application Recording
// =============================================================================

/// Record that a fix was applied to resolve an error.
///
/// Called when the prediction engine's suggested fix is applied, or when
/// effectiveness evaluation links a fix to a resolved error.
pub fn record_fix_application(
    conn: &Connection,
    fix_id: &str,
    task_run_id: &str,
    error_signature_hash: Option<&str>,
    outcome: &str,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        r#"INSERT INTO fix_applications (id, fix_id, task_run_id, error_signature_hash, outcome, applied_at)
           VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))"#,
        params![id, fix_id, task_run_id, error_signature_hash, outcome],
    )
    .map_err(|e| format!("Failed to record fix application: {}", e))?;

    // Increment reuse_count on the fix
    if outcome == "resolved" {
        conn.execute(
            "UPDATE reflection_fixes SET reuse_count = reuse_count + 1 WHERE id = ?1",
            params![fix_id],
        )
        .map_err(|e| format!("Failed to increment reuse_count: {}", e))?;
    }

    Ok(id)
}

/// Update the outcome of a fix application after evaluation.
pub fn update_fix_application_outcome(
    conn: &Connection,
    application_id: &str,
    outcome: &str,
) -> Result<(), String> {
    conn.execute(
        r#"UPDATE fix_applications SET outcome = ?1, evaluated_at = datetime('now') WHERE id = ?2"#,
        params![outcome, application_id],
    )
    .map_err(|e| format!("Failed to update fix application outcome: {}", e))?;

    // If resolved, increment reuse_count
    if outcome == "resolved" {
        conn.execute(
            r#"UPDATE reflection_fixes SET reuse_count = reuse_count + 1
               WHERE id = (SELECT fix_id FROM fix_applications WHERE id = ?1)"#,
            params![application_id],
        )
        .map_err(|e| format!("Failed to increment reuse_count: {}", e))?;
    }

    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
                source_agent TEXT,
                reasoning TEXT,
                alternatives_considered TEXT,
                reflection_scope TEXT DEFAULT 'workflow',
                project_path TEXT,
                target_component TEXT,
                reuse_count INTEGER DEFAULT 0,
                applicability_context TEXT,
                fix_description_embedding BLOB
            );
            CREATE TABLE task_knowledge (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                category TEXT NOT NULL,
                content TEXT NOT NULL,
                confidence TEXT DEFAULT 'medium',
                is_resolved INTEGER DEFAULT 0,
                reflection_fix_id TEXT,
                last_validated_at TEXT,
                validation_count INTEGER DEFAULT 0,
                created_at TEXT NOT NULL
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
            CREATE INDEX idx_fix_applications_fix ON fix_applications(fix_id);
            CREATE INDEX idx_fix_applications_sig ON fix_applications(error_signature_hash);
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
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_predict_fix_no_history() {
        let conn = setup_test_db();
        let result = predict_fix_for_error(&conn, "hash-unknown").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_predict_fix_with_resolved_history() {
        let conn = setup_test_db();

        // Create a fix
        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id,
               fix_type, fix_description, status, applied_at, created_at, reuse_count)
               VALUES ('fix-1', 'src-1', 'ref-1', 'selector_fix', 'Fixed the button selector',
                       'applied', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', 2)"#,
            [],
        )
        .unwrap();

        // Record resolved applications
        conn.execute(
            r#"INSERT INTO fix_applications (id, fix_id, task_run_id, error_signature_hash, outcome, applied_at)
               VALUES ('app-1', 'fix-1', 'run-1', 'hash-abc', 'resolved', '2025-01-01T01:00:00Z')"#,
            [],
        ).unwrap();
        conn.execute(
            r#"INSERT INTO fix_applications (id, fix_id, task_run_id, error_signature_hash, outcome, applied_at)
               VALUES ('app-2', 'fix-1', 'run-2', 'hash-abc', 'resolved', '2025-01-01T02:00:00Z')"#,
            [],
        ).unwrap();

        let result = predict_fix_for_error(&conn, "hash-abc").unwrap();
        assert!(result.is_some());
        let fix = result.unwrap();
        assert_eq!(fix.fix_id, "fix-1");
        assert!(fix.confidence > 0.0);
        assert_eq!(fix.reuse_count, 2);
    }

    #[test]
    fn test_convergence_score_no_runs() {
        let conn = setup_test_db();
        let metrics = compute_convergence_score(&conn, "nonexistent-workflow", "workflow").unwrap();
        assert_eq!(metrics.consecutive_clean_runs, 0);
        assert_eq!(metrics.score, 0.0);
    }

    #[test]
    fn test_convergence_score_clean_runs() {
        let conn = setup_test_db();

        // Create 5 clean runs
        for i in 0..5 {
            conn.execute(
                &format!(
                    r#"INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection,
                       completed_at, created_at, updated_at)
                       VALUES ('run-{}', 'Test', 'wf-1', 'complete', 0,
                               '2025-01-0{}T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')"#,
                    i,
                    i + 1
                ),
                [],
            ).unwrap();
        }

        let metrics = compute_convergence_score(&conn, "wf-1", "workflow").unwrap();
        assert_eq!(metrics.consecutive_clean_runs, 5);
        assert!(
            metrics.score > 0.0,
            "Score should be positive with clean runs"
        );
    }

    #[test]
    fn test_convergence_score_with_findings() {
        let conn = setup_test_db();

        // Create a run with findings
        conn.execute(
            r#"INSERT INTO task_runs (id, task_name, workflow_name, status, is_reflection,
               completed_at, created_at, updated_at)
               VALUES ('run-0', 'Test', 'wf-1', 'complete', 0,
                       '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')"#,
            [],
        )
        .unwrap();
        conn.execute(
            r#"INSERT INTO task_run_findings (id, task_run_id, signature_hash, detected_at)
               VALUES ('f-1', 'run-0', 'hash-1', '2025-01-01T00:00:00Z')"#,
            [],
        )
        .unwrap();

        let metrics = compute_convergence_score(&conn, "wf-1", "workflow").unwrap();
        assert_eq!(metrics.consecutive_clean_runs, 0);
        // Score should be low because clean_ratio is 0
        assert!(metrics.score < 0.5);
    }

    #[test]
    fn test_record_fix_application() {
        let conn = setup_test_db();

        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id,
               fix_type, fix_description, status, applied_at, created_at, reuse_count)
               VALUES ('fix-1', 'src-1', 'ref-1', 'selector_fix', 'Fixed selector',
                       'applied', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', 0)"#,
            [],
        )
        .unwrap();

        let app_id =
            record_fix_application(&conn, "fix-1", "run-1", Some("hash-abc"), "resolved").unwrap();
        assert!(!app_id.is_empty());

        // Check reuse_count was incremented
        let reuse: u32 = conn
            .query_row(
                "SELECT reuse_count FROM reflection_fixes WHERE id = 'fix-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reuse, 1);
    }

    #[test]
    fn test_score_knowledge_relevance_empty() {
        let conn = setup_test_db();
        let scored = score_knowledge_relevance(&conn, "wf-1", 10).unwrap();
        assert!(scored.is_empty());
    }

    #[test]
    fn test_score_knowledge_relevance_ordering() {
        let conn = setup_test_db();

        // Create task run
        conn.execute(
            r#"INSERT INTO task_runs (id, task_name, workflow_name, status, created_at, updated_at)
               VALUES ('run-1', 'Test', 'wf-1', 'complete', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')"#,
            [],
        ).unwrap();

        // Create knowledge entries with different ages
        conn.execute(
            r#"INSERT INTO task_knowledge (id, task_run_id, category, content, is_resolved,
               last_validated_at, created_at)
               VALUES ('k-recent', 'run-1', 'recurring_pattern', 'Recent knowledge', 0,
                       '2025-06-01T00:00:00Z', '2025-06-01T00:00:00Z')"#,
            [],
        )
        .unwrap();
        conn.execute(
            r#"INSERT INTO task_knowledge (id, task_run_id, category, content, is_resolved, created_at)
               VALUES ('k-old', 'run-1', 'context', 'Old knowledge', 0, '2024-01-01T00:00:00Z')"#,
            [],
        ).unwrap();

        let scored = score_knowledge_relevance(&conn, "wf-1", 10).unwrap();
        assert_eq!(scored.len(), 2);
        // Recent entry should score higher
        assert!(scored[0].relevance_score >= scored[1].relevance_score);
        assert_eq!(scored[0].knowledge_id, "k-recent");
    }

    #[test]
    fn test_store_convergence_snapshot() {
        let conn = setup_test_db();
        let metrics = ConvergenceMetrics {
            score: 0.75,
            consecutive_clean_runs: 3,
            novelty_score: 0.2,
            effective_fix_rate: 0.8,
            change_velocity: 0.5,
            total_fixes: 10,
            effective_fixes: 8,
        };

        store_convergence_snapshot(
            &conn,
            "wf-1",
            Some("/path/to/project"),
            "workflow",
            &metrics,
        )
        .unwrap();

        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM convergence_snapshots WHERE workflow_name = 'wf-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_change_velocity() {
        let conn = setup_test_db();

        // Create fixes targeting a component
        for i in 0..3 {
            conn.execute(
                &format!(
                    r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id,
                       fix_type, fix_description, status, target_component, applied_at, created_at)
                       VALUES ('fix-{}', 'src-1', 'ref-1', 'selector_fix', 'Fix {}',
                               'applied', 'src/auth/login.rs', '2025-01-0{}T00:00:00Z', '2025-01-01T00:00:00Z')"#,
                    i, i, i + 1
                ),
                [],
            ).unwrap();
        }

        let velocity = compute_change_velocity(&conn, "src/auth/login.rs", 5).unwrap();
        assert!(velocity > 0.0);
    }
}
