//! Failure analysis module for the meta-optimizer.
//!
//! Aggregates failure data from multiple existing tables (read-only) to provide
//! a comprehensive view of what's going wrong: abort reasons, verification failures,
//! finding distributions, fix effectiveness, generation quality, and recurring issues.

use serde::{Deserialize, Serialize};

use super::types::WorkflowCategory;
use crate::database::CheckpointDb;
use rusqlite::params;
use tracing::debug;

// ── Data Structures ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureAnalysis {
    pub period_days: u32,
    pub total_runs: i64,
    pub failed_runs: i64,
    pub failure_rate: f64,
    pub abort_reasons: Vec<AbortReason>,
    pub verification_failures: Vec<VerificationFailurePattern>,
    pub finding_distribution: Vec<CategoryCount>,
    pub severity_distribution: Vec<CategoryCount>,
    pub reflection_fix_effectiveness: Vec<FixEffectivenessRecord>,
    pub generation_quality: GenerationQualityMetrics,
    pub pipeline_agent_failures: Vec<PipelineAgentFailureRecord>,
    pub recurring_issues: Vec<RecurringIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbortReason {
    pub reason: String,
    pub count: i64,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFailurePattern {
    pub iteration: i64,
    pub total_checks: i64,
    pub failed_checks: i64,
    pub failure_rate: f64,
    pub critical_failure_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryCount {
    pub category: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixEffectivenessRecord {
    pub fix_type: String,
    pub total: i64,
    pub effective: i64,
    pub ineffective: i64,
    pub caused_regression: i64,
    pub effectiveness_rate: f64,
    pub source_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationQualityMetrics {
    pub total_feedback: i64,
    pub edits: i64,
    pub deletes: i64,
    pub avg_rating: Option<f64>,
    pub most_edited_fields: Vec<CategoryCount>,
    pub delete_reasons: Vec<CategoryCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineAgentFailureRecord {
    pub agent_type: String,
    pub total_runs: i64,
    pub failures: i64,
    pub failure_rate: f64,
    pub avg_duration_ms: f64,
    pub avg_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringIssue {
    pub signature_hash: String,
    pub title: String,
    pub category: String,
    pub severity: String,
    pub occurrence_count: i64,
    pub last_seen: String,
}

// ── Public API ───────────────────────────────────────────────────────────

pub fn get_failure_analysis(
    db: &CheckpointDb,
    days: u32,
    category: WorkflowCategory,
) -> Result<FailureAnalysis, String> {
    db.with_conn(|conn| {
        let since = (chrono::Utc::now() - chrono::Duration::days(days as i64)).to_rfc3339();
        debug!(days, %since, "Running failure analysis");

        let (total_runs, failed_runs) = query_run_totals(conn, &since, category);
        let failure_rate = if total_runs > 0 {
            100.0 * failed_runs as f64 / total_runs as f64
        } else {
            0.0
        };

        Ok(FailureAnalysis {
            period_days: days,
            total_runs,
            failed_runs,
            failure_rate,
            abort_reasons: query_abort_reasons(conn, &since, category),
            verification_failures: query_verification_failures(conn, &since, category),
            finding_distribution: query_finding_distribution(conn, &since, category),
            severity_distribution: query_severity_distribution(conn, &since, category),
            reflection_fix_effectiveness: query_fix_effectiveness(conn, &since),
            generation_quality: query_generation_quality(conn, &since),
            pipeline_agent_failures: query_pipeline_agent_failures(conn, &since, category),
            recurring_issues: query_recurring_issues(conn, &since, category),
        })
    })
}

// ── Private Query Functions ──────────────────────────────────────────────

fn query_run_totals(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> (i64, i64) {
    let filter = category.sql_filter("task_runs");

    let total_runs: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM task_runs \
                 WHERE created_at > ?1{}",
                filter
            ),
            params![since],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let failed_runs: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM task_runs \
                 WHERE status IN ('failed', 'stopped') \
                   AND created_at > ?1{}",
                filter
            ),
            params![since],
            |row| row.get(0),
        )
        .unwrap_or(0);

    (total_runs, failed_runs)
}

fn query_abort_reasons(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> Vec<AbortReason> {
    let mut reasons: Vec<AbortReason> = Vec::new();
    let filter = category.sql_filter("tr");

    // First: learning_outcomes-based reasons
    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT \
                 CASE \
                     WHEN lo.status = 'partial' THEN 'stopped_by_user' \
                     WHEN lo.error_type IS NOT NULL THEN lo.error_type \
                     ELSE 'unknown_failure' \
                 END as reason, \
                 COUNT(*) as count \
             FROM learning_outcomes lo \
             JOIN task_runs tr ON lo.task_id = tr.id \
             WHERE lo.status != 'success' AND lo.created_at > ?1{} \
             GROUP BY reason \
             ORDER BY count DESC",
        filter
    )) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            for row in rows.flatten() {
                reasons.push(AbortReason {
                    reason: row.0,
                    count: row.1,
                    percentage: 0.0, // computed below
                });
            }
        }
    }

    // Second: task_runs-based reasons (more granular)
    let task_runs_filter = category.sql_filter("task_runs");
    if let Ok(mut stmt) = conn.prepare(
        &format!(
            "SELECT \
                 CASE \
                     WHEN error_message LIKE '%max iterations%' OR error_message LIKE '%iteration budget%' THEN 'max_iterations_reached' \
                     WHEN error_message LIKE '%stopped%' OR status = 'stopped' THEN 'stopped_by_user' \
                     WHEN error_message LIKE '%critical%' THEN 'critical_failure' \
                     WHEN error_message IS NOT NULL THEN 'runtime_error' \
                     WHEN goal_achieved = 0 THEN 'goal_not_achieved' \
                     ELSE 'unknown' \
                 END as reason, \
                 COUNT(*) as count \
             FROM task_runs \
             WHERE status IN ('failed', 'stopped') \
               AND created_at > ?1{} \
             GROUP BY reason \
             ORDER BY count DESC",
            task_runs_filter
        ),
    ) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            for row in rows.flatten() {
                // Merge with existing or add new
                if let Some(existing) = reasons.iter_mut().find(|r| r.reason == row.0) {
                    existing.count += row.1;
                } else {
                    reasons.push(AbortReason {
                        reason: row.0,
                        count: row.1,
                        percentage: 0.0,
                    });
                }
            }
        }
    }

    // Calculate percentages
    let total: i64 = reasons.iter().map(|r| r.count).sum();
    if total > 0 {
        for r in &mut reasons {
            r.percentage = 100.0 * r.count as f64 / total as f64;
        }
    }

    reasons.sort_by(|a, b| b.count.cmp(&a.count));
    reasons
}

fn query_verification_failures(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> Vec<VerificationFailurePattern> {
    let mut results = Vec::new();
    let filter = category.sql_filter("tr");

    if let Ok(mut stmt) = conn.prepare(
        &format!(
            "SELECT \
                 vpr.iteration, \
                 SUM(vpr.total_steps) as total_checks, \
                 SUM(vpr.failed_steps) as failed_checks, \
                 ROUND(100.0 * SUM(vpr.failed_steps) / NULLIF(SUM(vpr.total_steps), 0), 1) as failure_rate, \
                 SUM(CASE WHEN vpr.critical_failure THEN 1 ELSE 0 END) as critical_failure_count \
             FROM workflow_verification_phase_results vpr \
             JOIN task_runs tr ON vpr.task_run_id = tr.id \
             WHERE vpr.created_at > ?1{} \
             GROUP BY vpr.iteration \
             ORDER BY vpr.iteration",
            filter
        ),
    ) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok(VerificationFailurePattern {
                iteration: row.get(0).unwrap_or(0),
                total_checks: row.get(1).unwrap_or(0),
                failed_checks: row.get(2).unwrap_or(0),
                failure_rate: row.get(3).unwrap_or(0.0),
                critical_failure_count: row.get(4).unwrap_or(0),
            })
        }) {
            results.extend(rows.flatten());
        }
    }

    results
}

fn query_finding_distribution(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> Vec<CategoryCount> {
    let mut results = Vec::new();
    let filter = category.sql_filter("tr");

    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT trf.category, COUNT(*) as count \
             FROM task_run_findings trf \
             JOIN task_runs tr ON trf.task_run_id = tr.id \
             WHERE trf.detected_at > ?1{} \
             GROUP BY trf.category \
             ORDER BY count DESC",
        filter
    )) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok(CategoryCount {
                category: row.get(0).unwrap_or_default(),
                count: row.get(1).unwrap_or(0),
            })
        }) {
            results.extend(rows.flatten());
        }
    }

    results
}

fn query_severity_distribution(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> Vec<CategoryCount> {
    let mut results = Vec::new();
    let filter = category.sql_filter("tr");

    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT trf.severity, COUNT(*) as count \
             FROM task_run_findings trf \
             JOIN task_runs tr ON trf.task_run_id = tr.id \
             WHERE trf.detected_at > ?1{} \
             GROUP BY trf.severity \
             ORDER BY CASE trf.severity \
                 WHEN 'critical' THEN 1 \
                 WHEN 'high' THEN 2 \
                 WHEN 'medium' THEN 3 \
                 WHEN 'low' THEN 4 \
                 WHEN 'info' THEN 5 \
             END",
        filter
    )) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok(CategoryCount {
                category: row.get(0).unwrap_or_default(),
                count: row.get(1).unwrap_or(0),
            })
        }) {
            results.extend(rows.flatten());
        }
    }

    results
}

fn query_fix_effectiveness(
    conn: &rusqlite::Connection,
    since: &str,
) -> Vec<FixEffectivenessRecord> {
    let mut results = Vec::new();

    if let Ok(mut stmt) = conn.prepare(
        "SELECT \
             fix_type, \
             source_agent, \
             COUNT(*) as total, \
             SUM(CASE WHEN effectiveness IS NULL OR effectiveness = 'effective' THEN 1 ELSE 0 END) as effective, \
             SUM(CASE WHEN effectiveness = 'ineffective' THEN 1 ELSE 0 END) as ineffective, \
             SUM(CASE WHEN effectiveness = 'caused_regression' THEN 1 ELSE 0 END) as caused_regression \
         FROM reflection_fixes \
         WHERE created_at > ?1 \
         GROUP BY fix_type, source_agent \
         ORDER BY total DESC",
    ) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            let total: i64 = row.get(2).unwrap_or(0);
            let effective: i64 = row.get(3).unwrap_or(0);
            let effectiveness_rate = if total > 0 {
                100.0 * effective as f64 / total as f64
            } else {
                0.0
            };
            Ok(FixEffectivenessRecord {
                fix_type: row.get(0).unwrap_or_default(),
                source_agent: row.get(1).ok(),
                total,
                effective,
                ineffective: row.get(4).unwrap_or(0),
                caused_regression: row.get(5).unwrap_or(0),
                effectiveness_rate,
            })
        }) {
            results.extend(rows.flatten());
        }
    }

    results
}

fn query_generation_quality(conn: &rusqlite::Connection, since: &str) -> GenerationQualityMetrics {
    let mut metrics = GenerationQualityMetrics {
        total_feedback: 0,
        edits: 0,
        deletes: 0,
        avg_rating: None,
        most_edited_fields: Vec::new(),
        delete_reasons: Vec::new(),
    };

    // Totals
    if let Ok(row) = conn.query_row(
        "SELECT \
             COUNT(*) as total, \
             SUM(CASE WHEN feedback_type = 'edit' THEN 1 ELSE 0 END) as edits, \
             SUM(CASE WHEN feedback_type = 'delete' THEN 1 ELSE 0 END) as deletes, \
             AVG(CASE WHEN feedback_type = 'rating' THEN rating END) as avg_rating \
         FROM workflow_generation_feedback \
         WHERE created_at > ?1",
        params![since],
        |row| {
            Ok((
                row.get::<_, i64>(0).unwrap_or(0),
                row.get::<_, i64>(1).unwrap_or(0),
                row.get::<_, i64>(2).unwrap_or(0),
                row.get::<_, Option<f64>>(3).unwrap_or(None),
            ))
        },
    ) {
        metrics.total_feedback = row.0;
        metrics.edits = row.1;
        metrics.deletes = row.2;
        metrics.avg_rating = row.3;
    }

    // Most edited fields
    if let Ok(mut stmt) = conn.prepare(
        "SELECT edited_field as category, COUNT(*) as count \
         FROM workflow_generation_feedback \
         WHERE feedback_type = 'edit' AND edited_field IS NOT NULL AND created_at > ?1 \
         GROUP BY edited_field \
         ORDER BY count DESC \
         LIMIT 10",
    ) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok(CategoryCount {
                category: row.get(0).unwrap_or_default(),
                count: row.get(1).unwrap_or(0),
            })
        }) {
            metrics.most_edited_fields = rows.flatten().collect();
        }
    }

    // Delete reasons
    if let Ok(mut stmt) = conn.prepare(
        "SELECT COALESCE(delete_reason, 'unspecified') as category, COUNT(*) as count \
         FROM workflow_generation_feedback \
         WHERE feedback_type = 'delete' AND created_at > ?1 \
         GROUP BY category \
         ORDER BY count DESC",
    ) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok(CategoryCount {
                category: row.get(0).unwrap_or_default(),
                count: row.get(1).unwrap_or(0),
            })
        }) {
            metrics.delete_reasons = rows.flatten().collect();
        }
    }

    metrics
}

fn query_pipeline_agent_failures(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> Vec<PipelineAgentFailureRecord> {
    let mut results = Vec::new();
    let filter = category.sql_filter("tr");

    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT \
                 pat.agent_type, \
                 COUNT(*) as total_runs, \
                 SUM(CASE WHEN pat.downstream_success = 0 THEN 1 ELSE 0 END) as failures, \
                 AVG(pat.duration_ms) as avg_duration_ms, \
                 AVG(pat.cost_usd) as avg_cost_usd \
             FROM pipeline_agent_traces pat \
             JOIN task_runs tr ON pat.task_run_id = tr.id \
             WHERE pat.created_at > ?1 AND pat.downstream_success IS NOT NULL{} \
             GROUP BY pat.agent_type \
             ORDER BY pat.agent_type",
        filter
    )) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            let total_runs: i64 = row.get(1).unwrap_or(0);
            let failures: i64 = row.get(2).unwrap_or(0);
            let failure_rate = if total_runs > 0 {
                100.0 * failures as f64 / total_runs as f64
            } else {
                0.0
            };
            Ok(PipelineAgentFailureRecord {
                agent_type: row.get(0).unwrap_or_default(),
                total_runs,
                failures,
                failure_rate,
                avg_duration_ms: row.get(3).unwrap_or(0.0),
                avg_cost_usd: row.get(4).unwrap_or(0.0),
            })
        }) {
            results.extend(rows.flatten());
        }
    }

    results
}

fn query_recurring_issues(
    conn: &rusqlite::Connection,
    since: &str,
    category: WorkflowCategory,
) -> Vec<RecurringIssue> {
    let mut results = Vec::new();
    let filter = category.sql_filter("tr");

    if let Ok(mut stmt) = conn.prepare(&format!(
        "SELECT \
                 trf.signature_hash, \
                 trf.title, \
                 trf.category, \
                 trf.severity, \
                 COUNT(*) as occurrence_count, \
                 MAX(trf.detected_at) as last_seen \
             FROM task_run_findings trf \
             JOIN task_runs tr ON trf.task_run_id = tr.id \
             WHERE trf.detected_at > ?1 AND trf.signature_hash IS NOT NULL{} \
             GROUP BY trf.signature_hash \
             HAVING COUNT(*) >= 2 \
             ORDER BY occurrence_count DESC \
             LIMIT 20",
        filter
    )) {
        if let Ok(rows) = stmt.query_map(params![since], |row| {
            Ok(RecurringIssue {
                signature_hash: row.get(0).unwrap_or_default(),
                title: row.get(1).unwrap_or_default(),
                category: row.get(2).unwrap_or_default(),
                severity: row.get(3).unwrap_or_default(),
                occurrence_count: row.get(4).unwrap_or(0),
                last_seen: row.get(5).unwrap_or_default(),
            })
        }) {
            results.extend(rows.flatten());
        }
    }

    results
}
