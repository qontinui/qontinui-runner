//! Bridge between the comparison system and the meta-optimizer.
//!
//! Converts comparison run winners into pending recommendations, and allows
//! triggering comparison runs to validate recommendations before applying them.

use std::sync::Arc;
use tracing::info;

use crate::comparison::{
    axis_adjusted_confidence, classify_axis_drift, ComparisonRecommendation, ComparisonRun,
    ComparisonStatus,
};
use crate::database::pg::PgDb;
use crate::mcp::comparison_api::ComparisonEntryJson;

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

    // Load the comparison run from PG.
    //
    // `variation_type` and the per-arm override blobs come out alongside the
    // `ComparisonRun` because Phase 3 needs the DECLARED label and the ACTUAL
    // arms together — see the axis check below.
    let (comparison, variation_type, arm_overrides): (
        ComparisonRun,
        String,
        Vec<serde_json::Value>,
    ) = {
        let row = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(pg_db.get_comparison_run_for_bridge(&comp_id))
        })?
        .ok_or_else(|| format!("Comparison not found: {}", comp_id))?;

        let (
            entries_json,
            report,
            rec_json,
            status,
            workflow_id,
            workflow_name,
            created_at,
            variation_type,
        ) = row;

        // The per-arm config as it was actually STORED. Note the shape: the
        // live paths persist `Vec<ComparisonEntryJson>`, whose per-arm config
        // field is `overrides`. (`ComparisonRun.entries` below is typed
        // `Vec<ComparisonEntry>`, which cannot deserialize from those bytes and
        // so is always empty — do not read it for the axis.)
        let arm_overrides: Vec<serde_json::Value> =
            serde_json::from_str::<Vec<ComparisonEntryJson>>(&entries_json)
                .map(|arms| arms.into_iter().map(|a| a.overrides).collect())
                .unwrap_or_default();

        let entries = serde_json::from_str(&entries_json).unwrap_or_default();
        let recommendation: Option<ComparisonRecommendation> =
            rec_json.and_then(|j| serde_json::from_str(&j).ok());
        let status_enum = match status.as_str() {
            "completed" => ComparisonStatus::Completed,
            "failed" => ComparisonStatus::Failed,
            "comparing" => ComparisonStatus::Comparing,
            _ => ComparisonStatus::Running,
        };

        let run = ComparisonRun {
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
        };

        (run, variation_type, arm_overrides)
    };

    // Only process completed comparisons with a recommendation
    if comparison.status != ComparisonStatus::Completed {
        return Ok(None);
    }

    let rec = match &comparison.recommendation {
        Some(r) if r.confidence >= 0.6 => r,
        _ => return Ok(None),
    };

    // ---- Phase 3: a non-clean treatment axis may not underwrite a rollout ----
    //
    // `variation_type` is what the run DECLARED would vary; `arm_overrides` is
    // what actually did. When they disagree, this comparison cannot support an
    // autonomous promotion, so its confidence is clamped below the threshold
    // that `meta_optimizer::parser::auto_apply_high_confidence` sweeps at
    // (`parser.rs:1114` -> `start_canary(.., 10)` at `:1122`). A human can still
    // apply it deliberately; it just no longer applies itself.
    let axis_class = classify_axis_drift(&variation_type, &arm_overrides);
    let (effective_confidence, axis_note) = axis_adjusted_confidence(rec.confidence, axis_class);
    if let Some(note) = axis_note.as_deref() {
        info!(
            "Comparison {} treatment axis: {} — {}",
            comparison.id,
            axis_class.as_wire_str(),
            note
        );
    }

    // Build recommendation title and description
    let title = format!(
        "Comparison winner: {} (workflow: {})",
        rec.branch_name, comparison.workflow_name
    );
    let mut description = format!(
        "Comparison run {} identified '{}' as the winner with {:.0}% confidence.\n\nReasoning: {}",
        comparison.id,
        rec.branch_name,
        rec.confidence * 100.0,
        rec.reasoning
    );
    // Record the discrepancy as a fact in the recommendation itself, so a human
    // reading it sees WHY the confidence differs from the comparison's own.
    if let Some(note) = &axis_note {
        description.push_str(&format!("\n\nTreatment-axis check: {}", note));
    }

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
                "confidence": effective_confidence,
                "declared_confidence": rec.confidence,
                "declared_variation_type": variation_type,
                "computed_axis": crate::comparison::computed_treatment_axes(&arm_overrides),
                "axis_drift_class": axis_class.as_wire_str(),
            })
            .to_string(),
        ),
        Some(evidence),
        effective_confidence,
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
