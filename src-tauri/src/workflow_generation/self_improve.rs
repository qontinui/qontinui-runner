//! Self-improvement analyzer for workflow generation.
//!
//! Analyzes historical generation patterns, common issues, success/failure
//! rates, and user feedback to build context that improves future generations.
//! This is the planned-but-never-created module referenced in the codebase.

use crate::database::pg::PgDb;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
    /// Specification agent insights from prompt analysis.
    pub specification_insights: Vec<String>,
    /// Verification blind spots from prompt analysis.
    pub verification_blind_spots: Vec<String>,
    /// Hardener conversion patterns from prompt analysis.
    pub hardener_patterns: Vec<String>,
    /// Builder construction patterns from prompt analysis.
    pub builder_insights: Vec<String>,
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
            && self.specification_insights.is_empty()
            && self.verification_blind_spots.is_empty()
            && self.hardener_patterns.is_empty()
            && self.builder_insights.is_empty()
    }
}

// ============================================================================
// Per-Agent Formatting Functions
// ============================================================================

/// Format specification insights as markdown for the specification prompt.
pub fn format_specification_insights(context: &SelfImprovementContext) -> String {
    if context.specification_insights.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Lessons from Past Generations\n\n");
    output.push_str(
        "Based on analysis of previous workflow generations, pay attention to these patterns:\n\n",
    );
    for insight in &context.specification_insights {
        output.push_str(&format!("- {}\n", insight));
    }
    output.push('\n');
    output
}

/// Format verification blind spots as markdown for the verification prompt.
pub fn format_verification_insights(context: &SelfImprovementContext) -> String {
    if context.verification_blind_spots.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Known Blind Spots\n\n");
    output.push_str("Previous analysis found that the verification agent tends to miss these issue types. Check for them explicitly:\n\n");
    for blind_spot in &context.verification_blind_spots {
        output.push_str(&format!("- {}\n", blind_spot));
    }
    output.push('\n');
    output
}

/// Format hardener patterns as markdown for the hardener prompt.
pub fn format_hardener_insights(context: &SelfImprovementContext) -> String {
    if context.hardener_patterns.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Patterns from Past Conversions\n\n");
    output
        .push_str("Based on analysis of previous hardener runs, watch out for these patterns:\n\n");
    for pattern in &context.hardener_patterns {
        output.push_str(&format!("- {}\n", pattern));
    }
    output.push('\n');
    output
}

/// Format builder insights as markdown for the builder prompt.
pub fn format_builder_insights(context: &SelfImprovementContext) -> String {
    if context.builder_insights.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Lessons for Workflow Construction\n\n");
    output.push_str(
        "Based on analysis of previous builder runs, pay attention to these patterns:\n\n",
    );
    for insight in &context.builder_insights {
        output.push_str(&format!("- {}\n", insight));
    }
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_specification_insights_empty() {
        let ctx = SelfImprovementContext::default();
        let result = format_specification_insights(&ctx);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_specification_insights_with_data() {
        let ctx = SelfImprovementContext {
            specification_insights: vec!["insight 1".into(), "insight 2".into()],
            ..Default::default()
        };
        let result = format_specification_insights(&ctx);
        assert!(result.contains("Lessons from Past Generations"));
        assert!(result.contains("insight 1"));
        assert!(result.contains("insight 2"));
    }

    #[test]
    fn test_format_verification_insights_empty() {
        let ctx = SelfImprovementContext::default();
        let result = format_verification_insights(&ctx);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_verification_insights_with_data() {
        let ctx = SelfImprovementContext {
            verification_blind_spots: vec!["blind spot 1".into()],
            ..Default::default()
        };
        let result = format_verification_insights(&ctx);
        assert!(result.contains("Known Blind Spots"));
        assert!(result.contains("blind spot 1"));
    }

    #[test]
    fn test_format_hardener_insights_empty() {
        let ctx = SelfImprovementContext::default();
        let result = format_hardener_insights(&ctx);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_hardener_insights_with_data() {
        let ctx = SelfImprovementContext {
            hardener_patterns: vec!["pattern 1".into()],
            ..Default::default()
        };
        let result = format_hardener_insights(&ctx);
        assert!(result.contains("Patterns from Past Conversions"));
        assert!(result.contains("pattern 1"));
    }

    #[test]
    fn test_format_builder_insights_empty() {
        let ctx = SelfImprovementContext::default();
        let result = format_builder_insights(&ctx);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_builder_insights_with_data() {
        let ctx = SelfImprovementContext {
            builder_insights: vec!["lesson 1".into(), "lesson 2".into()],
            ..Default::default()
        };
        let result = format_builder_insights(&ctx);
        assert!(result.contains("Lessons for Workflow Construction"));
        assert!(result.contains("lesson 1"));
        assert!(result.contains("lesson 2"));
    }

    #[test]
    fn test_is_empty_default() {
        let ctx = SelfImprovementContext::default();
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_is_empty_with_builder_insights() {
        let ctx = SelfImprovementContext {
            builder_insights: vec!["x".to_string()],
            ..Default::default()
        };
        assert!(!ctx.is_empty());
    }

    #[test]
    fn test_format_improvement_context_empty() {
        let ctx = SelfImprovementContext::default();
        let result = format_improvement_context(&ctx);
        assert!(result.is_empty());
    }

    #[test]
    fn test_format_improvement_context_with_data() {
        let ctx = SelfImprovementContext {
            common_verifier_issues: vec![("issue".into(), 5)],
            ..Default::default()
        };
        let result = format_improvement_context(&ctx);
        assert!(result.contains("Historical Generation Patterns"));
        assert!(result.contains("issue"));
    }
}

// ============================================================================
// PG-based analysis (async, called from sync context via block_on)
// ============================================================================

/// Analyze historical generation patterns from PostgreSQL.
///
/// PG equivalent of `analyze_generation_patterns`. Queries the same tables
/// (task_run_findings, learning_outcomes, workflow_generation_feedback) via raw SQL.
pub async fn analyze_generation_patterns_pg(
    pg: &Arc<PgDb>,
) -> Result<SelfImprovementContext, String> {
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    // Common verifier issues
    let common_verifier_issues: Vec<(String, i64)> = conn
        .query(
            r#"SELECT f.title, COUNT(*) as cnt
               FROM task_run_findings f
               JOIN task_runs tr ON tr.id = f.task_run_id
               WHERE tr.workflow_name LIKE 'AI Generate:%'
                 AND f.category IN ('verification_issue', 'code_bug', 'structural_issue')
               GROUP BY f.title
               ORDER BY cnt DESC
               LIMIT 10"#,
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1)))
                .collect()
        })
        .unwrap_or_default();

    // Fixer failures
    let fixer_failures: Vec<(String, i64)> = conn
        .query(
            r#"SELECT COALESCE(lo.strategy, 'unknown'), COUNT(*) as cnt
               FROM learning_outcomes lo
               WHERE lo.status = 'failure'
               GROUP BY lo.strategy
               ORDER BY cnt DESC
               LIMIT 10"#,
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1)))
                .collect()
        })
        .unwrap_or_default();

    // Success rates by category
    let success_rates: Vec<(String, i64, i64)> = conn
        .query(
            r#"SELECT
                COALESCE(
                    CASE
                        WHEN strategy LIKE '%cat=%' THEN
                            SUBSTRING(strategy FROM 'cat=([^)]+)')
                        ELSE 'unknown'
                    END,
                    'unknown'
                ) as category,
                SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
                COUNT(*) as total_count
               FROM learning_outcomes
               GROUP BY category
               HAVING COUNT(*) >= 2
               ORDER BY total_count DESC
               LIMIT 10"#,
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    (
                        r.get::<_, String>(0),
                        r.get::<_, i64>(1),
                        r.get::<_, i64>(2),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    // Commonly edited fields
    let commonly_edited_fields: Vec<(String, i64)> = conn
        .query(
            r#"SELECT edited_field, COUNT(*) as cnt
               FROM workflow_generation_feedback
               WHERE feedback_type = 'edit' AND edited_field IS NOT NULL
               GROUP BY edited_field
               ORDER BY cnt DESC
               LIMIT 5"#,
            &[],
        )
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1)))
                .collect()
        })
        .unwrap_or_default();

    // Delete rate
    let generated: i64 = conn
        .query_one(
            "SELECT COUNT(DISTINCT workflow_id) FROM workflow_generation_feedback",
            &[],
        )
        .await
        .map(|r| r.get(0))
        .unwrap_or(0);

    let deleted: i64 = conn
        .query_one(
            "SELECT COUNT(*) FROM workflow_generation_feedback WHERE feedback_type = 'delete'",
            &[],
        )
        .await
        .map(|r| r.get(0))
        .unwrap_or(0);

    // Average rating.
    //
    // NOTE: `rating` is INTEGER, and PostgreSQL's `AVG(integer)` returns
    // `numeric`, which tokio-postgres does NOT deserialize into `f64`.
    // Attempting `row.get::<_, Option<f64>>(0)` on a numeric column panics
    // with "error deserializing column 0: cannot convert between the Rust
    // type `core::option::Option<f64>` and the Postgres type `numeric`".
    // That panic bubbles up through `spawn_blocking` and surfaces to the
    // frontend as:
    //     "Generation task failed: task NNN panicked with message
    //      \"error retrieving column 0: error deserializing column 0\""
    // The cast to DOUBLE PRECISION forces the server to return float8,
    // which `f64` accepts. `try_get` adds a belt-and-suspenders guard
    // so any future type drift logs a warning instead of crashing.
    let avg_rating: Option<f64> = conn
        .query_one(
            "SELECT AVG(rating)::double precision FROM workflow_generation_feedback WHERE feedback_type = 'rating' AND rating IS NOT NULL",
            &[],
        )
        .await
        .ok()
        .and_then(|r| match r.try_get::<_, Option<f64>>(0) {
            Ok(v) => v,
            Err(e) => {
                debug!("avg_rating deserialization failed: {}", e);
                None
            }
        });

    let ctx = SelfImprovementContext {
        common_verifier_issues,
        fixer_failures,
        success_rates,
        commonly_edited_fields,
        delete_rate: (generated, deleted),
        avg_rating,
        // Agent insights require prompt_analysis PG migration (skipped for now)
        specification_insights: vec![],
        verification_blind_spots: vec![],
        hardener_patterns: vec![],
        builder_insights: vec![],
    };

    debug!(
        "Self-improvement context (PG): {} verifier issues, {} fixer failures, {} categories tracked",
        ctx.common_verifier_issues.len(),
        ctx.fixer_failures.len(),
        ctx.success_rates.len(),
    );

    Ok(ctx)
}
