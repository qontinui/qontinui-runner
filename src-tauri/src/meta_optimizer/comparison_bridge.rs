//! Bridge between the comparison system and the meta-optimizer.
//!
//! Converts comparison run winners into pending recommendations, and allows
//! triggering comparison runs to validate recommendations before applying them.

use rusqlite::params;
use tracing::info;

use crate::comparison::{ComparisonRecommendation, ComparisonRun, ComparisonStatus};
use crate::database::CheckpointDb;

/// Convert a completed comparison run's winner into a meta-optimizer recommendation.
///
/// If the comparison has a recommendation with sufficient confidence, creates
/// a pending recommendation for human review.
///
/// Returns the recommendation ID if one was created, None otherwise.
pub fn comparison_to_recommendation(
    db: &CheckpointDb,
    comparison_id: &str,
) -> Result<Option<String>, String> {
    let comp_id = comparison_id.to_string();

    // Load the comparison run
    let comparison: ComparisonRun = db.with_conn({
        let comp_id = comp_id.clone();
        move |conn| {
            let (entries_json, report, rec_json, status, workflow_id, workflow_name, created_at): (
                String,
                Option<String>,
                Option<String>,
                String,
                String,
                String,
                String,
            ) = conn
                .query_row(
                    r#"SELECT entries_json, comparison_report, recommendation_json,
                              status, workflow_id, workflow_name, created_at
                       FROM comparison_runs WHERE id = ?1"#,
                    params![comp_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                        ))
                    },
                )
                .map_err(|e| format!("Comparison not found: {}", e))?;

            let entries = serde_json::from_str(&entries_json).unwrap_or_default();
            let recommendation: Option<ComparisonRecommendation> =
                rec_json.and_then(|j| serde_json::from_str(&j).ok());
            let status_enum = match status.as_str() {
                "completed" => ComparisonStatus::Completed,
                "failed" => ComparisonStatus::Failed,
                "comparing" => ComparisonStatus::Comparing,
                _ => ComparisonStatus::Running,
            };

            Ok(ComparisonRun {
                id: comp_id,
                workflow_id,
                workflow_name,
                source_branch: String::new(),
                source_commit: String::new(),
                entries,
                status: status_enum,
                comparison_report: report,
                recommendation,
                created_at,
                updated_at: String::new(),
            })
        }
    })?;

    // Only process completed comparisons with a recommendation
    if comparison.status != ComparisonStatus::Completed {
        return Ok(None);
    }

    let rec = match &comparison.recommendation {
        Some(r) if r.confidence >= 0.6 => r,
        _ => return Ok(None),
    };

    // Build recommendation title and description
    let title = format!(
        "Comparison winner: {} (workflow: {})",
        rec.branch_name, comparison.workflow_name
    );
    let description = format!(
        "Comparison run {} identified '{}' as the winner with {:.0}% confidence.\n\nReasoning: {}",
        comparison.id,
        rec.branch_name,
        rec.confidence * 100.0,
        rec.reasoning
    );

    let evidence = comparison
        .comparison_report
        .as_deref()
        .unwrap_or("No detailed report available");

    // Create the recommendation
    let recommendation = super::recommendations::create_recommendation(
        db,
        "comparison",
        "config_change",
        None,
        &title,
        &description,
        None,
        Some(
            &serde_json::json!({
                "source": "comparison",
                "comparison_id": comparison.id,
                "winner_branch": rec.branch_name,
                "confidence": rec.confidence,
            })
            .to_string(),
        ),
        Some(evidence),
        rec.confidence,
        None,
    )?;

    // Link the comparison back to the recommendation
    let rec_id = recommendation.id.clone();
    let cid = comparison.id.clone();
    db.with_conn(move |conn| {
        conn.execute(
            "UPDATE comparison_runs SET recommendation_id = ?1, source = 'meta_optimizer' WHERE id = ?2",
            params![rec_id, cid],
        )
        .map_err(|e| format!("Failed to link comparison to recommendation: {}", e))?;
        Ok(())
    })?;

    info!(
        "Created recommendation {} from comparison {} (winner: {}, confidence: {:.0}%)",
        recommendation.id,
        comparison.id,
        rec.branch_name,
        rec.confidence * 100.0
    );

    Ok(Some(recommendation.id))
}

/// Check if a comparison run has the `recommendation_id` and `source` columns.
/// These are added by migration 137. Returns false if columns don't exist yet.
pub fn has_bridge_columns(db: &CheckpointDb) -> bool {
    db.with_conn(|conn| {
        let has_col: bool = conn
            .prepare("PRAGMA table_info(comparison_runs)")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, String>(1))
                    .map(|rows| {
                        rows.filter_map(|r| r.ok())
                            .any(|name| name == "recommendation_id")
                    })
            })
            .unwrap_or(false);
        Ok(has_col)
    })
    .unwrap_or(false)
}
