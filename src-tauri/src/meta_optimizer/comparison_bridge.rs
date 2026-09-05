//! Bridge between the comparison system and the meta-optimizer.
//!
//! Converts comparison run winners into pending recommendations, and allows
//! triggering comparison runs to validate recommendations before applying them.

use std::sync::Arc;
use tracing::info;

use crate::comparison::{
    axis_adjusted_confidence, axis_facts_from_entries_json, recommendation_from_entries_json,
    ComparisonStatus, BRIDGE_MIN_CONFIDENCE,
};
use crate::database::pg::PgDb;

/// Convert a completed comparison run's winner into a meta-optimizer recommendation.
///
/// Returns the recommendation ID if one was created, None otherwise.
///
/// ## What this reads, and what it no longer pretends to read
///
/// This function used to load a `ComparisonRun` carrying a `comparison_report`,
/// a `recommendation_json` and a `workflow_name` — three columns
/// `project.comparison_runs` has never had in any alembic revision, so the very
/// first statement failed `42703` and the Tauri command
/// `convert_comparison_to_recommendation` could never succeed. It also wrote a
/// `recommendation_id` / `source` pair back to two more columns that do not
/// exist. Plan
/// `2026-08-22-comparison-to-recommendation-bridge-references-columns-that-never-existed`.
///
/// The repair is *drop and derive*, not *add columns*:
///
/// * the report is the `report` column that does exist;
/// * the workflow name is a join, not a column;
/// * the recommendation is **derived from the arms the run actually stored**
///   ([`recommendation_from_entries_json`]) rather than read from a column
///   nothing in the tree would ever have written;
/// * the comparison to recommendation link lives on the RECOMMENDATION row,
///   where `create_recommendation` already records `source: "comparison"` and
///   the `comparison_id`. A second copy on the comparison side was a second
///   source of truth, so the write is deleted rather than repointed.
pub fn comparison_to_recommendation(
    pg_db: &Arc<PgDb>,
    comparison_id: &str,
) -> Result<Option<String>, String> {
    let comp_id = comparison_id.to_string();

    let (entries_json, report, status, workflow_name, variation_type) =
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(pg_db.get_comparison_run_for_bridge(&comp_id))
        })?
        .ok_or_else(|| format!("Comparison not found: {}", comp_id))?;

    let status = match status.as_str() {
        "completed" => ComparisonStatus::Completed,
        "failed" => ComparisonStatus::Failed,
        "comparing" => ComparisonStatus::Comparing,
        _ => ComparisonStatus::Running,
    };
    if status != ComparisonStatus::Completed {
        return Ok(None);
    }

    // The axis facts, derived from the arms as they were actually STORED by the
    // SAME function every writer persists them with — so what this reads can
    // never disagree with the row's own `computed_axis` / `axis_drift_class`.
    let axis_facts = axis_facts_from_entries_json(&variation_type, &entries_json);

    let Some(rec) = recommendation_from_entries_json(&entries_json) else {
        info!(
            "Comparison {} produced no derivable winner (fewer than two completed arms, \
             or no arm won more metrics than every other)",
            comp_id
        );
        return Ok(None);
    };
    if rec.confidence < BRIDGE_MIN_CONFIDENCE {
        return Ok(None);
    }

    // ---- A non-clean treatment axis may not underwrite a rollout ----
    //
    // `variation_type` is what the run DECLARED would vary; the arms are what
    // actually did. When they disagree, this comparison cannot support an
    // autonomous promotion, so its confidence is clamped below the threshold
    // that `meta_optimizer::parser::auto_apply_high_confidence` sweeps at. A
    // human can still apply it deliberately; it just no longer applies itself.
    //
    // This composes with the cap `recommendation_from_entries_json` already
    // applied: that one says "a three-metric heuristic is not an AI judgement",
    // this one says "and the arms did not vary as declared". The lower wins.
    let axis_class = axis_facts.drift_class;
    let (effective_confidence, axis_note) = axis_adjusted_confidence(rec.confidence, axis_class);
    if let Some(note) = axis_note.as_deref() {
        info!(
            "Comparison {} treatment axis: {} — {}",
            comp_id,
            axis_class.as_wire_str(),
            note
        );
    }

    let title = format!(
        "Comparison winner: {} (workflow: {})",
        rec.branch_name, workflow_name
    );
    let mut description = format!(
        "Comparison run {} identified '{}' as the winner with {:.0}% confidence.\n\nReasoning: {}",
        comp_id,
        rec.branch_name,
        rec.confidence * 100.0,
        rec.reasoning
    );
    // Record the discrepancy as a fact in the recommendation itself, so a human
    // reading it sees WHY the confidence differs from the comparison's own.
    if let Some(note) = &axis_note {
        description.push_str(&format!("\n\nTreatment-axis check: {}", note));
    }

    // `report` is the column that exists. Nothing in the tree writes it today,
    // so this is the fallback on every current row — see the plan's closing
    // risk, which keeps building a producer OUT of this defect fix.
    let evidence = report
        .as_deref()
        .unwrap_or("No detailed report available");

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
                "comparison_id": comp_id,
                "winner_branch": rec.branch_name,
                "confidence": effective_confidence,
                "declared_confidence": rec.confidence,
                "declared_variation_type": variation_type,
                // Null, not `[]`, when no axis could be computed — the same
                // absence-is-not-zero distinction the column carries.
                "computed_axis": axis_facts.computed_axis_json(),
                "axis_drift_class": axis_class.as_wire_str(),
            })
            .to_string(),
        ),
        Some(evidence),
        effective_confidence,
        None,
    )?;

    info!(
        "Created recommendation {} from comparison {} (winner: {}, confidence: {:.0}%)",
        recommendation.id,
        comp_id,
        rec.branch_name,
        effective_confidence * 100.0
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

