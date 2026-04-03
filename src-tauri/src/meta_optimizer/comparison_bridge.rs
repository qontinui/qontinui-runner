//! Bridge between the comparison system and the meta-optimizer.
//!
//! Converts comparison run winners into pending recommendations, and allows
//! triggering comparison runs to validate recommendations before applying them.

use std::sync::Arc;
use tracing::info;

use crate::comparison::{ComparisonRecommendation, ComparisonRun, ComparisonStatus};
use crate::database::pg::PgDb;

/// Convert a completed comparison run's winner into a meta-optimizer recommendation.
///
/// If the comparison has a recommendation with sufficient confidence, creates
/// a pending recommendation for human review.
///
/// Returns the recommendation ID if one was created, None otherwise.
pub fn comparison_to_recommendation(
    pg_db: &Arc<PgDb>,
    comparison_id: &str,
) -> Result<Option<String>, String> {
    let comp_id = comparison_id.to_string();

    // Load the comparison run from PG
    let comparison: ComparisonRun = {
        let row = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(pg_db.get_comparison_run_for_bridge(&comp_id))
        })?
        .ok_or_else(|| format!("Comparison not found: {}", comp_id))?;

        let (entries_json, report, rec_json, status, workflow_id, workflow_name, created_at) = row;

        let entries = serde_json::from_str(&entries_json).unwrap_or_default();
        let recommendation: Option<ComparisonRecommendation> =
            rec_json.and_then(|j| serde_json::from_str(&j).ok());
        let status_enum = match status.as_str() {
            "completed" => ComparisonStatus::Completed,
            "failed" => ComparisonStatus::Failed,
            "comparing" => ComparisonStatus::Comparing,
            _ => ComparisonStatus::Running,
        };

        ComparisonRun {
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
            recommendation_id: None,
            source: None,
        }
    };

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
        pg_db,
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

    // Link the comparison back to the recommendation via PG
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(
            pg_db.update_comparison_recommendation_link(&recommendation.id, &comparison.id),
        )
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
/// PG schema always has these columns; this returns true unconditionally now.
pub fn has_bridge_columns() -> bool {
    true
}

/// Create a comparison config to validate a recommendation via A/B testing.
///
/// Returns a ComparisonConfig that can be passed to the comparison system.
/// The caller is responsible for actually starting the comparison run.
pub fn build_validation_comparison(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<Option<crate::comparison::ComparisonConfig>, String> {
    // Look up the recommendation from PG
    let rec = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.get_recommendation(recommendation_id))
    })?
    .ok_or_else(|| format!("Recommendation not found: {}", recommendation_id))?;

    // Only config_change recommendations can be validated via comparison
    if rec.recommendation_type != "config_change" {
        return Ok(None);
    }

    let recommended_value = rec.recommended_value;

    // Find a recent workflow to use as benchmark
    let workflow_id: Option<String> = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(pg_db.get_most_recent_workflow_id())
    })
    .ok()
    .flatten();

    let workflow_id = match workflow_id {
        Some(id) => id,
        None => return Ok(None),
    };

    // Build custom overrides: one run with current config, one with recommendation applied
    let recommended = recommended_value.unwrap_or_else(|| "{}".to_string());
    let overrides = vec![
        serde_json::json!({"label": "baseline"}),
        serde_json::json!({"label": "candidate", "config_override": recommended}),
    ];

    Ok(Some(crate::comparison::ComparisonConfig {
        workflow_id,
        run_count: 2,
        variation: crate::comparison::ComparisonVariation::Custom { overrides },
        timeout_seconds: 600,
    }))
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Convert a comparison to a recommendation with PG dual-write (fire-and-forget).
#[deprecated(note = "Use comparison_to_recommendation directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn comparison_to_recommendation_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    comparison_id: &str,
) -> Result<Option<String>, String> {
    comparison_to_recommendation(pg_db, comparison_id)
}

/// Build a validation comparison with PG-primary read for the recommendation.
#[allow(dead_code)]
pub fn build_validation_comparison_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    recommendation_id: &str,
) -> Result<Option<crate::comparison::ComparisonConfig>, String> {
    build_validation_comparison(pg_db, recommendation_id)
}

/// Check if bridge columns exist. SQLite-only (uses PRAGMA).
/// PG always has these columns so this always returns true when PG is available.
#[allow(dead_code)]
pub fn has_bridge_columns_with_pg(_pg_db: &std::sync::Arc<crate::database::pg::PgDb>) -> bool {
    // PG schema always has these columns; SQLite may not if migration hasn't run
    true
}
