//! Self-improvement analyzer for workflow generation.
//!
//! Analyzes historical generation patterns, common issues, success/failure
//! rates, and user feedback to build context that improves future generations.
//! This is the planned-but-never-created module referenced in the codebase.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Aggregated self-improvement context for prompt injection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SelfImprovementContext {
    /// Most common verifier issues (title, count).
    pub common_verifier_issues: Vec<(String, i64)>,
    /// Fixer failure patterns (category, count).
    pub fixer_failures: Vec<(String, i64)>,
    /// Success rate by category (category, success_count, total_count).
    pub success_rates: Vec<(String, i64, i64)>,
    /// Most edited fields from feedback (field, count).
    pub commonly_edited_fields: Vec<(String, i64)>,
    /// Delete rate (total generated, total deleted).
    pub delete_rate: (i64, i64),
    /// Average rating if available.
    pub avg_rating: Option<f64>,
}

/// Analyze historical generation patterns from the database.
///
/// Queries across `task_run_findings`, `learning_outcomes`, `learning_patterns`,
/// and `workflow_generation_feedback` to build a comprehensive improvement context.
pub fn analyze_generation_patterns(conn: &Connection) -> Result<SelfImprovementContext, String> {
    let ctx = SelfImprovementContext {
        common_verifier_issues: query_common_verifier_issues(conn)?,
        fixer_failures: query_fixer_failures(conn)?,
        success_rates: query_success_rates(conn)?,
        commonly_edited_fields: query_commonly_edited_fields(conn)?,
        delete_rate: query_delete_rate(conn)?,
        avg_rating: query_avg_rating(conn)?,
    };

    debug!(
        "Self-improvement context: {} verifier issues, {} fixer failures, {} categories tracked",
        ctx.common_verifier_issues.len(),
        ctx.fixer_failures.len(),
        ctx.success_rates.len(),
    );

    Ok(ctx)
}

/// Format the improvement context as markdown for prompt injection.
pub fn format_improvement_context(context: &SelfImprovementContext) -> String {
    let mut output = String::new();

    if context.is_empty() {
        return output;
    }

    output.push_str("## Historical Generation Patterns\n\n");

    // Common verifier issues
    if !context.common_verifier_issues.is_empty() {
        output.push_str("### Common Verification Issues (avoid these)\n\n");
        for (issue, count) in &context.common_verifier_issues {
            output.push_str(&format!("- **{}** (occurred {} times)\n", issue, count));
        }
        output.push('\n');
    }

    // Success rates
    if !context.success_rates.is_empty() {
        output.push_str("### Success Rates by Category\n\n");
        for (category, success, total) in &context.success_rates {
            let rate = if *total > 0 {
                (*success as f64 / *total as f64) * 100.0
            } else {
                0.0
            };
            output.push_str(&format!(
                "- **{}**: {:.0}% ({}/{})\n",
                category, rate, success, total
            ));
        }
        output.push('\n');
    }

    // Commonly edited fields (things the builder gets wrong)
    if !context.commonly_edited_fields.is_empty() {
        output.push_str("### Fields Users Commonly Edit After Generation\n\n");
        for (field, count) in &context.commonly_edited_fields {
            output.push_str(&format!("- **{}** edited {} times\n", field, count));
        }
        output.push_str("\nPay extra attention to getting these fields right.\n\n");
    }

    // Delete/rating stats
    let (generated, deleted) = context.delete_rate;
    if generated > 0 {
        let delete_pct = (deleted as f64 / generated as f64) * 100.0;
        output.push_str(&format!(
            "### Overall Stats\n\n- Generated: {}, Deleted: {} ({:.0}% rejection rate)\n",
            generated, deleted, delete_pct
        ));
        if let Some(avg) = context.avg_rating {
            output.push_str(&format!("- Average user rating: {:.1}/5\n", avg));
        }
        output.push('\n');
    }

    output
}

impl SelfImprovementContext {
    /// Returns true if there's no data to show.
    pub fn is_empty(&self) -> bool {
        self.common_verifier_issues.is_empty()
            && self.fixer_failures.is_empty()
            && self.success_rates.is_empty()
            && self.commonly_edited_fields.is_empty()
            && self.delete_rate == (0, 0)
            && self.avg_rating.is_none()
    }
}

// ============================================================================
// Query helpers
// ============================================================================

fn query_common_verifier_issues(conn: &Connection) -> Result<Vec<(String, i64)>, String> {
    let sql = r#"
        SELECT f.title, COUNT(*) as cnt
        FROM task_run_findings f
        JOIN task_runs tr ON tr.id = f.task_run_id
        WHERE tr.workflow_name LIKE 'AI Generate:%'
          AND f.category IN ('verification_issue', 'code_bug', 'structural_issue')
        GROUP BY f.title
        ORDER BY cnt DESC
        LIMIT 10
    "#;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to query verifier issues: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| format!("Failed to execute verifier issues query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn query_fixer_failures(conn: &Connection) -> Result<Vec<(String, i64)>, String> {
    let sql = r#"
        SELECT lo.strategy, COUNT(*) as cnt
        FROM learning_outcomes lo
        WHERE lo.status = 'failure'
        GROUP BY lo.strategy
        ORDER BY cnt DESC
        LIMIT 10
    "#;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to query fixer failures: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| format!("Failed to execute fixer failures query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn query_success_rates(conn: &Connection) -> Result<Vec<(String, i64, i64)>, String> {
    let sql = r#"
        SELECT
            COALESCE(
                CASE
                    WHEN strategy LIKE '%cat=%' THEN
                        SUBSTR(strategy, INSTR(strategy, 'cat=') + 4,
                            CASE WHEN INSTR(SUBSTR(strategy, INSTR(strategy, 'cat=') + 4), ')') > 0
                                THEN INSTR(SUBSTR(strategy, INSTR(strategy, 'cat=') + 4), ')') - 1
                                ELSE LENGTH(strategy)
                            END
                        )
                    ELSE 'unknown'
                END,
                'unknown'
            ) as category,
            SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
            COUNT(*) as total_count
        FROM learning_outcomes
        GROUP BY category
        HAVING total_count >= 2
        ORDER BY total_count DESC
        LIMIT 10
    "#;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to query success rates: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| format!("Failed to execute success rates query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn query_commonly_edited_fields(conn: &Connection) -> Result<Vec<(String, i64)>, String> {
    let sql = r#"
        SELECT edited_field, COUNT(*) as cnt
        FROM workflow_generation_feedback
        WHERE feedback_type = 'edit' AND edited_field IS NOT NULL
        GROUP BY edited_field
        ORDER BY cnt DESC
        LIMIT 5
    "#;

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("Failed to query edited fields: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| format!("Failed to execute edited fields query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn query_delete_rate(conn: &Connection) -> Result<(i64, i64), String> {
    let generated: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT workflow_id) FROM workflow_generation_feedback",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let deleted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM workflow_generation_feedback WHERE feedback_type = 'delete'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok((generated, deleted))
}

fn query_avg_rating(conn: &Connection) -> Result<Option<f64>, String> {
    let avg: Option<f64> = conn
        .query_row(
            "SELECT AVG(rating) FROM workflow_generation_feedback WHERE feedback_type = 'rating' AND rating IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    Ok(avg)
}
