//! CRUD operations for the meta_optimizer_recommendations table.
//!
//! All optimizer outputs go here with status `pending`. Human reviews from UI.

use std::sync::Arc;
use rusqlite::params;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;
use tracing::{info, warn};

use super::types::{MetaOptimizerRun, Recommendation};
use crate::database::CheckpointDb;
use crate::database::pg::PgDb;

/// Compute a content hash for deduplication based on semantic identity fields.
/// Two recommendations with the same hash are semantically identical regardless
/// of title/description wording.
pub fn compute_content_hash(
    optimizer_type: &str,
    recommendation_type: &str,
    target_agent: Option<&str>,
    recommended_value: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(optimizer_type.as_bytes());
    hasher.update(b"\0");
    hasher.update(recommendation_type.as_bytes());
    hasher.update(b"\0");
    hasher.update(target_agent.unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(recommended_value.unwrap_or("").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Check if a recommendation with the same content hash already exists
/// in a non-terminal state (pending, canary, applied).
pub fn is_content_duplicate(db: &CheckpointDb, pg_db: &Arc<PgDb>, content_hash: &str) -> bool {
    let _ = db;
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.is_content_duplicate(content_hash))
    })
    .unwrap_or(false)
}

// ── JSON payloads expected inside `recommended_value` ────────────────

#[derive(Debug, Deserialize)]
struct PromptRewritePayload {
    agent_type: String,
    variant_name: String,
    prompt_content: String,
}

#[derive(Debug, Deserialize)]
struct ConfigChangePayload {
    key: String,
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RulePayload {
    agent: String,
    section: String,
    title: String,
    content: String,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    rule_number: Option<i32>,
    /// For rule_update: the ID of the existing rule to update.
    #[serde(default)]
    rule_id: Option<String>,
    /// For disabling a rule.
    #[serde(default)]
    status: Option<String>,
    /// JSON array of positive/negative examples (PromptWizard-inspired).
    #[serde(default)]
    examples_json: Option<String>,
}

/// Create a new recommendation.
pub fn create_recommendation(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    optimizer_type: &str,
    recommendation_type: &str,
    target_agent: Option<&str>,
    title: &str,
    description: &str,
    current_value: Option<&str>,
    recommended_value: Option<&str>,
    evidence: Option<&str>,
    confidence: f64,
    optimizer_run_id: Option<&str>,
) -> Result<Recommendation, String> {
    let id = format!("mor-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let content_hash = compute_content_hash(
        optimizer_type,
        recommendation_type,
        target_agent,
        recommended_value,
    );

    let rec = Recommendation {
        id: id.clone(),
        optimizer_type: optimizer_type.to_string(),
        recommendation_type: recommendation_type.to_string(),
        target_agent: target_agent.map(|s| s.to_string()),
        title: title.to_string(),
        description: description.to_string(),
        current_value: current_value.map(|s| s.to_string()),
        recommended_value: recommended_value.map(|s| s.to_string()),
        evidence: evidence.map(|s| s.to_string()),
        confidence,
        status: "pending".to_string(),
        applied_at: None,
        outcome_after_apply: None,
        optimizer_run_id: optimizer_run_id.map(|s| s.to_string()),
        created_at: now.clone(),
        eval_result_id: None,
        eval_status: None,
    };

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.create_recommendation(
            &rec.id, optimizer_type, recommendation_type, target_agent,
            title, description, current_value, recommended_value,
            evidence, confidence, optimizer_run_id, &content_hash,
        ))
    })?;
    info!("Created recommendation {} ({})", rec.id, rec.title);

    Ok(rec)
}

/// List recommendations with optional filters.
pub fn list_recommendations(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    optimizer_type: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Recommendation>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_recommendations(optimizer_type, status))
    })
}

/// Apply a recommendation (updates status to 'applied').
/// This is the simple status-only flip — prefer `apply_recommendation_with_side_effects`.
pub fn apply_recommendation(db: &CheckpointDb, pg_db: &Arc<PgDb>, recommendation_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_recommendation_status(recommendation_id, "applied"))
    })?;
    info!("Applied recommendation {}", recommendation_id);
    Ok(())
}

/// Fetch a single recommendation by ID.
fn get_recommendation(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<Recommendation, String> {
    let result = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_recommendation(recommendation_id))
    })?;
    result.ok_or_else(|| format!("Recommendation not found: {}", recommendation_id))
}

/// Apply a recommendation **and** perform the appropriate side-effect based on
/// `recommendation_type`. If the side-effect fails the status is NOT updated.
pub fn apply_recommendation_with_side_effects(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    let id = recommendation_id.to_string();
    let rec = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_recommendation(&id))
    })?
    .ok_or_else(|| format!("Recommendation not found: {}", id))?;

    if rec.status != "pending" && rec.status != "canary" {
        return Err(format!(
            "Recommendation {} is not pending or canary (status: {})",
            rec.id, rec.status
        ));
    }

    let recommended_value = rec
        .recommended_value
        .as_deref()
        .ok_or_else(|| format!("Recommendation {} has no recommended_value", rec.id))?;

    match rec.recommendation_type.as_str() {
        "prompt_rewrite" => apply_prompt_rewrite(db, pg_db, &rec.id, recommended_value)?,
        "config_change" => apply_config_change(db, recommended_value)?,
        "rule_create" => apply_rule_create(db, pg_db, &rec.id, recommended_value)?,
        "rule_update" => apply_rule_update(db, pg_db, recommended_value)?,
        other => {
            warn!(
                "Unknown recommendation_type '{}' for {}; applying status-only",
                other, rec.id
            );
        }
    }

    // Side-effect succeeded — now flip the status via PG
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.apply_recommendation_with_timestamp(&id))
    })?;

    // Capture a snapshot to measure impact of this recommendation
    if let Err(e) = super::snapshots::capture_post_apply(
        db,
        pg_db,
        recommendation_id,
        super::types::WorkflowCategory::Main,
    ) {
        warn!("Failed to capture post-apply snapshot: {}", e);
    }

    // Evaluate outcome immediately (will likely be "insufficient_data" initially)
    if let Err(e) = super::snapshots::evaluate_recommendation_outcome(db, pg_db, recommendation_id) {
        warn!("Failed to evaluate recommendation outcome: {}", e);
    }

    Ok(())
}

// ── Side-effect helpers ──────────────────────────────────────────────

fn apply_prompt_rewrite(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: PromptRewritePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid prompt_rewrite payload: {}", e))?;

    let variant = super::prompt_registry::create_variant(
        db,
        pg_db,
        &payload.agent_type,
        &payload.variant_name,
        &payload.prompt_content,
        Some(recommendation_id),
    )?;

    super::prompt_registry::activate_variant(db, pg_db, &variant.id)?;

    info!(
        "Applied prompt_rewrite recommendation {}: created and activated variant {}",
        recommendation_id, variant.id
    );
    Ok(())
}

fn apply_config_change(db: &CheckpointDb, recommended_value: &str) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid config_change payload: {}", e))?;

    db.set_setting(&payload.key, &payload.value)?;

    info!(
        "Applied config_change: set '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

fn apply_rule_create(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: RulePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid rule_create payload: {}", e))?;

    let rec_id = recommendation_id.to_string();

    let rule_number = match payload.rule_number {
        Some(n) => n,
        None => tokio::task::block_in_place(|| {
            Handle::current().block_on(pg_db.next_rule_number(&payload.agent, &payload.section))
        })?,
    };

    let input = crate::workflow_generation::rules::InsertRuleInput {
        agent: payload.agent.clone(),
        section: payload.section.clone(),
        rule_number,
        title: payload.title.clone(),
        content: payload.content.clone(),
        condition: payload.condition.clone(),
        provenance: "meta_optimizer".to_string(),
        source_fix_id: Some(rec_id.clone()),
        severity: None,
        examples_json: payload.examples_json.clone(),
    };

    let rule = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.insert_rule(&input))
    })?;
    info!(
        "Applied rule_create recommendation {}: created rule {}",
        rec_id, rule.id
    );
    Ok(())
}

fn apply_rule_update(db: &CheckpointDb, pg_db: &Arc<PgDb>, recommended_value: &str) -> Result<(), String> {
    let payload: RulePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid rule_update payload: {}", e))?;

    let rule_id = payload
        .rule_id
        .ok_or_else(|| "rule_update payload missing 'rule_id'".to_string())?;

    let input = crate::workflow_generation::rules::UpdateRuleInput {
        title: Some(payload.title),
        content: Some(payload.content),
        condition: payload.condition,
        status: payload.status,
        rule_number: payload.rule_number,
        severity: None,
        examples_json: payload.examples_json,
    };

    let rule = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_rule(&rule_id, &input))
    })?;
    info!(
        "Applied rule_update: updated rule {} ({})",
        rule.id, rule.title
    );
    Ok(())
}

/// Reject a recommendation.
pub fn reject_recommendation(db: &CheckpointDb, pg_db: &Arc<PgDb>, recommendation_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_recommendation_status(recommendation_id, "rejected"))
    })?;
    info!("Rejected recommendation {}", recommendation_id);
    Ok(())
}

/// Deduplicate pending recommendations by content hash.
///
/// For each group of pending recs with the same content hash, keeps the oldest
/// and supersedes newer pending duplicates. Canary and applied recs are not touched
/// (they represent active rollouts or already-applied changes). Also backfills
/// content_hash for older rows that lack it.
pub fn dedup_pending_recommendations(db: &CheckpointDb, pg_db: &Arc<PgDb>) -> usize {
    let superseded = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let conn = pg_db.pool().get().await.map_err(|e| format!("PG pool: {e}"))?;
            let affected = conn
                .execute(
                    r#"UPDATE meta_optimizer_recommendations SET status = 'superseded'
                       WHERE id IN (
                           SELECT r.id FROM meta_optimizer_recommendations r
                           WHERE r.status = 'pending' AND r.content_hash IS NOT NULL
                             AND r.created_at > (
                                 SELECT MIN(r2.created_at) FROM meta_optimizer_recommendations r2
                                 WHERE r2.content_hash = r.content_hash
                                   AND r2.status IN ('pending', 'canary', 'applied')
                             )
                       )"#,
                    &[],
                )
                .await
                .map_err(|e| format!("Dedup query failed: {e}"))?;
            Ok::<u64, String>(affected)
        })
    })
    .unwrap_or(0) as usize;

    if superseded > 0 {
        info!("Dedup: superseded {} duplicate pending recommendation(s)", superseded);
    }
    superseded
}

/// Auto-reject pending recommendations older than 30 days.
///
/// Stale recs block the optimizer from regenerating fresh suggestions for the
/// same targets. Rejecting them frees those slots while preserving history.
pub fn auto_reject_stale_recommendations(db: &CheckpointDb, pg_db: &Arc<PgDb>) -> usize {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();

    let rejected = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let conn = pg_db.pool().get().await.map_err(|e| format!("PG pool: {e}"))?;
            let affected = conn.execute(
                "UPDATE meta_optimizer_recommendations SET status = 'rejected' WHERE status = 'pending' AND created_at < $1",
                &[&cutoff],
            ).await.map_err(|e| format!("PG stale rejection: {e}"))?;
            Ok::<u64, String>(affected)
        })
    })
    .unwrap_or(0) as usize;

    if rejected > 0 {
        info!("Auto-rejected {} stale pending recommendation(s) (older than 30 days)", rejected);
    }
    rejected
}

/// Roll back an applied recommendation, undoing side-effects where possible.
pub fn rollback_recommendation(db: &CheckpointDb, pg_db: &Arc<PgDb>, recommendation_id: &str) -> Result<(), String> {
    let rec = get_recommendation(db, pg_db, recommendation_id)?;

    if rec.status != "applied" {
        return Err(format!(
            "Recommendation {} is not applied (status: {})",
            rec.id, rec.status
        ));
    }

    // Attempt to undo the side-effect. Failures here are logged but do not
    // prevent the status from being rolled back — the user explicitly requested
    // the rollback.
    if let Some(ref recommended_value) = rec.recommended_value {
        match rec.recommendation_type.as_str() {
            "rule_create" | "rule_update" => {
                if let Err(e) = rollback_rule(db, pg_db, &rec.id, recommended_value) {
                    warn!("Failed to rollback rule side-effect for {}: {}", rec.id, e);
                }
            }
            "config_change" => {
                if let Some(ref current_value) = rec.current_value {
                    if let Err(e) = rollback_config_change(db, current_value) {
                        warn!(
                            "Failed to rollback config side-effect for {}: {}",
                            rec.id, e
                        );
                    }
                }
            }
            "prompt_rewrite" => {
                // Prompt variants are not automatically deactivated on rollback.
                // The user can manually switch prompts via the prompt registry UI.
                info!(
                    "Prompt rollback for {} — variant left in registry, user can deactivate manually",
                    rec.id
                );
            }
            _ => {}
        }
    }

    // Flip the status to rolled_back
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_recommendation_status(recommendation_id, "rolled_back"))
    })?;
    info!("Rolled back recommendation {}", recommendation_id);
    Ok(())
}

/// Undo a rule side-effect by disabling the rule that was created/updated.
fn rollback_rule(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    // For rule_create, find the rule by source_fix_id matching the recommendation ID.
    // For rule_update, parse the rule_id from the payload.
    let rule_id_from_payload: Option<String> =
        serde_json::from_str::<RulePayload>(recommended_value)
            .ok()
            .and_then(|p| p.rule_id);

    let target_rule_id = if let Some(id) = rule_id_from_payload {
        id
    } else {
        tokio::task::block_in_place(|| {
            Handle::current().block_on(pg_db.find_rule_by_source_fix_id(recommendation_id))
        })?
        .ok_or_else(|| {
            format!(
                "Could not find rule created by recommendation {}",
                recommendation_id
            )
        })?
    };

    let input = crate::workflow_generation::rules::UpdateRuleInput {
        title: None,
        content: None,
        condition: None,
        status: Some("disabled".to_string()),
        rule_number: None,
        severity: None,
        examples_json: None,
    };

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_rule(&target_rule_id, &input))
    })?;
    info!("Rollback: disabled rule {}", target_rule_id);
    Ok(())
}

/// Undo a config change by restoring the `current_value`.
fn rollback_config_change(db: &CheckpointDb, current_value: &str) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(current_value)
        .map_err(|e| format!("Invalid current_value payload for config rollback: {}", e))?;

    db.set_setting(&payload.key, &payload.value)?;
    info!(
        "Rollback: restored config '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

// ── Meta-Optimizer Runs ────────────────────────────────────────────────

/// Create a new optimizer run record.
pub fn create_optimizer_run(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    optimizer_type: &str,
    trigger_type: &str,
    task_run_id: Option<&str>,
) -> Result<String, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.create_optimizer_run(optimizer_type, trigger_type, task_run_id))
    })
}

/// Create an optimizer run with PG dual-write (fire-and-forget).
#[deprecated(note = "Use create_optimizer_run directly — it is now PG-primary")]
pub fn create_optimizer_run_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    optimizer_type: &str,
    trigger_type: &str,
    task_run_id: Option<&str>,
) -> Result<String, String> {
    create_optimizer_run(db, pg_db, optimizer_type, trigger_type, task_run_id)
}

/// Complete an optimizer run, recording how many runs were analyzed and recommendations produced.
pub fn complete_optimizer_run(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    run_id: &str,
    runs_analyzed: i64,
    recommendations_produced: i64,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.complete_optimizer_run(run_id, runs_analyzed, recommendations_produced))
    })
}

/// Complete an optimizer run with PG dual-write (fire-and-forget).
#[deprecated(note = "Use complete_optimizer_run directly — it is now PG-primary")]
pub fn complete_optimizer_run_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    run_id: &str,
    runs_analyzed: i64,
    recommendations_produced: i64,
) -> Result<(), String> {
    complete_optimizer_run(db, pg_db, run_id, runs_analyzed, recommendations_produced)
}

/// List optimizer runs.
pub fn list_optimizer_runs(db: &CheckpointDb, pg_db: &Arc<PgDb>) -> Result<Vec<MetaOptimizerRun>, String> {
    let _ = db;
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_optimizer_runs())
    })
}

/// Evaluate applied recommendations using composite agentic scores.
///
/// Compares the average composite_agentic_score from runs BEFORE the recommendation
/// was applied vs runs AFTER. If the delta is significant and positive, updates
/// outcome_after_apply with the evidence. If negative, flags for rollback.
///
/// Called from the maintenance block in trigger.rs.
pub fn auto_evaluate_with_agentic_scores(db: &CheckpointDb, pg_db: &Arc<PgDb>) {
    let recs = match list_recommendations(db, pg_db, None, Some("applied")) {
        Ok(r) => r,
        Err(_) => return,
    };

    for rec in &recs {
        let applied_at = match &rec.applied_at {
            Some(a) => a.clone(),
            None => continue,
        };

        // Query pre/post agentic scores from PG (learning_outcomes is also in PG)
        let result: Result<Option<(String, f64)>, String> = (|| {
            let eval = tokio::task::block_in_place(|| {
                Handle::current().block_on(pg_db.get_agentic_score_evaluation(&applied_at))
            })?;

            match eval {
                Some((pre, post, post_count)) => {
                    let delta = post - pre;
                    let delta_pct = (delta / pre) * 100.0;
                    let verdict = if delta_pct > 5.0 {
                        "improved"
                    } else if delta_pct < -5.0 {
                        "degraded"
                    } else {
                        "neutral"
                    };

                    let outcome = serde_json::json!({
                        "verdict": verdict,
                        "pre_composite_score": format!("{:.3}", pre),
                        "post_composite_score": format!("{:.3}", post),
                        "delta": format!("{:+.3}", delta),
                        "delta_pct": format!("{:+.1}%", delta_pct),
                        "post_run_count": post_count,
                        "evaluated_by": "agentic_metrics",
                    });

                    tokio::task::block_in_place(|| {
                        Handle::current().block_on(
                            pg_db.update_recommendation_outcome_json(&rec.id, &outcome.to_string()),
                        )
                    })?;

                    Ok(Some((verdict.to_string(), delta_pct)))
                }
                None => Ok(None),
            }
        })();

        match result {
            Ok(Some((verdict, delta_pct))) => {
                if verdict != "neutral" {
                    info!(
                        "Agentic score evaluation for rec {}: verdict={}, delta={:+.1}%",
                        rec.id, verdict, delta_pct
                    );
                }
            }
            Ok(None) => {} // Insufficient data
            Err(e) => {
                tracing::debug!(
                    "Failed to evaluate rec {} with agentic scores: {}",
                    rec.id,
                    e
                );
            }
        }
    }
}

// ── PG dual-write wrappers (now just delegate to PG-primary originals) ───

/// Create a recommendation with PG dual-write (fire-and-forget).
#[deprecated(note = "Use create_recommendation directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn create_recommendation_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    optimizer_type: &str,
    recommendation_type: &str,
    target_agent: Option<&str>,
    title: &str,
    description: &str,
    current_value: Option<&str>,
    recommended_value: Option<&str>,
    evidence: Option<&str>,
    confidence: f64,
    optimizer_run_id: Option<&str>,
) -> Result<Recommendation, String> {
    create_recommendation(
        db, pg_db, optimizer_type, recommendation_type, target_agent, title,
        description, current_value, recommended_value, evidence, confidence, optimizer_run_id,
    )
}

/// Apply a recommendation with PG dual-write (fire-and-forget).
#[deprecated(note = "Use apply_recommendation directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn apply_recommendation_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    apply_recommendation(db, pg_db, recommendation_id)
}

/// Reject a recommendation with PG dual-write (fire-and-forget).
#[deprecated(note = "Use reject_recommendation directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn reject_recommendation_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    reject_recommendation(db, pg_db, recommendation_id)
}

/// Rollback a recommendation with PG dual-write (fire-and-forget).
#[deprecated(note = "Use rollback_recommendation directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn rollback_recommendation_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    rollback_recommendation(db, pg_db, recommendation_id)
}

/// Dedup pending recommendations with PG dual-write.
#[deprecated(note = "Use dedup_pending_recommendations directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn dedup_pending_recommendations_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) -> usize {
    dedup_pending_recommendations(db, pg_db)
}

/// Auto-reject stale recommendations with PG dual-write (fire-and-forget).
#[deprecated(note = "Use auto_reject_stale_recommendations directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn auto_reject_stale_recommendations_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) -> usize {
    auto_reject_stale_recommendations(db, pg_db)
}

// ── PG-primary read wrappers (now just delegate to PG-primary originals) ──

/// Check content duplicate with PG-primary read.
#[deprecated(note = "Use is_content_duplicate directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn is_content_duplicate_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    content_hash: &str,
) -> bool {
    is_content_duplicate(db, pg_db, content_hash)
}

/// List recommendations with PG-primary read.
#[deprecated(note = "Use list_recommendations directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn list_recommendations_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    optimizer_type: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Recommendation>, String> {
    list_recommendations(db, pg_db, optimizer_type, status)
}

/// List optimizer runs with PG-primary read.
#[deprecated(note = "Use list_optimizer_runs directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn list_optimizer_runs_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) -> Result<Vec<MetaOptimizerRun>, String> {
    list_optimizer_runs(db, pg_db)
}

/// Get a single recommendation by ID from PG (falls back to SQLite).
#[deprecated(note = "Use get_recommendation (private) or list_recommendations instead")]
#[allow(dead_code)]
pub fn get_recommendation_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<Recommendation, String> {
    get_recommendation(db, pg_db, recommendation_id)
}

/// Apply a recommendation with side effects and PG dual-write.
#[deprecated(note = "Use apply_recommendation_with_side_effects directly")]
#[allow(dead_code)]
pub fn apply_recommendation_with_side_effects_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    apply_recommendation_with_side_effects(db, pg_db, recommendation_id)
}

/// Dedup pending recommendations with PG dual-write (fire-and-forget).
#[deprecated(note = "Use dedup_pending_recommendations directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn dedup_pending_recommendations_with_pg_sync(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) -> usize {
    dedup_pending_recommendations(db, pg_db)
}

/// Auto-evaluate applied recommendations with agentic scores, with PG dual-write.
#[deprecated(note = "Use auto_evaluate_with_agentic_scores directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn auto_evaluate_with_agentic_scores_with_pg(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
) {
    auto_evaluate_with_agentic_scores(db, pg_db);
}

#[cfg(all(test, feature = "sqlite_tests"))]
mod tests {
    use super::*;
    use crate::database::CheckpointDb;

    fn setup_test_db() -> CheckpointDb {
        CheckpointDb::new_in_memory().unwrap()
    }

    fn make_recommendation(db: &CheckpointDb, title: &str, optimizer_type: &str) -> Recommendation {
        create_recommendation(
            db,
            optimizer_type,
            "prompt_change",
            Some("planner"),
            title,
            "Test description",
            Some(r#"{"old": true}"#),
            Some(r#"{"new": true}"#),
            Some(r#"{"score": 0.9}"#),
            0.85,
            None,
        )
        .unwrap()
    }

    #[test]
    fn test_create_and_list_recommendations() {
        let db = setup_test_db();

        let rec1 = make_recommendation(&db, "Improve planner prompt", "pipeline_prompt");
        let rec2 = make_recommendation(&db, "Adjust architecture", "architecture");

        assert!(rec1.id.starts_with("mor-"));
        assert_eq!(rec1.status, "pending");
        assert_eq!(rec1.confidence, 0.85);

        let all = list_recommendations(&db, None, None).unwrap();
        assert_eq!(all.len(), 2);

        // Verify fields round-trip correctly
        let found = all.iter().find(|r| r.id == rec1.id).unwrap();
        assert_eq!(found.title, "Improve planner prompt");
        assert_eq!(found.optimizer_type, "pipeline_prompt");
        assert_eq!(found.target_agent.as_deref(), Some("planner"));
        assert_eq!(found.current_value.as_deref(), Some(r#"{"old": true}"#));
        assert_eq!(found.recommended_value.as_deref(), Some(r#"{"new": true}"#));
        assert_eq!(found.evidence.as_deref(), Some(r#"{"score": 0.9}"#));

        // rec2 also present
        assert!(all.iter().any(|r| r.id == rec2.id));
    }

    #[test]
    fn test_apply_recommendation() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Apply me", "pipeline_prompt");

        apply_recommendation(&db, &rec.id).unwrap();

        let all = list_recommendations(&db, None, Some("applied")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(all[0].status, "applied");
        assert!(all[0].applied_at.is_some());
    }

    #[test]
    fn test_reject_recommendation() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Reject me", "pipeline_prompt");

        reject_recommendation(&db, &rec.id).unwrap();

        let all = list_recommendations(&db, None, Some("rejected")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(all[0].status, "rejected");
    }

    #[test]
    fn test_rollback_applied() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Rollback me", "pipeline_prompt");

        apply_recommendation(&db, &rec.id).unwrap();
        rollback_recommendation(&db, &rec.id).unwrap();

        let all = list_recommendations(&db, None, Some("rolled_back")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, rec.id);
        assert_eq!(all[0].status, "rolled_back");
    }

    #[test]
    fn test_cannot_apply_non_pending() {
        let db = setup_test_db();
        let rec = make_recommendation(&db, "Already rejected", "pipeline_prompt");

        reject_recommendation(&db, &rec.id).unwrap();

        let result = apply_recommendation(&db, &rec.id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found or not in pending"));
    }

    #[test]
    fn test_list_filters() {
        let db = setup_test_db();

        let rec1 = make_recommendation(&db, "Prompt rec", "pipeline_prompt");
        let _rec2 = make_recommendation(&db, "Arch rec", "architecture");
        let rec3 = make_recommendation(&db, "Another prompt rec", "pipeline_prompt");

        // Apply rec1 so we have mixed statuses
        apply_recommendation(&db, &rec1.id).unwrap();

        // Filter by optimizer_type
        let prompt_recs = list_recommendations(&db, Some("pipeline_prompt"), None).unwrap();
        assert_eq!(prompt_recs.len(), 2);

        let arch_recs = list_recommendations(&db, Some("architecture"), None).unwrap();
        assert_eq!(arch_recs.len(), 1);

        // Filter by status
        let pending = list_recommendations(&db, None, Some("pending")).unwrap();
        assert_eq!(pending.len(), 2); // rec2 and rec3

        let applied = list_recommendations(&db, None, Some("applied")).unwrap();
        assert_eq!(applied.len(), 1);

        // Filter by both
        let prompt_pending =
            list_recommendations(&db, Some("pipeline_prompt"), Some("pending")).unwrap();
        assert_eq!(prompt_pending.len(), 1);
        assert_eq!(prompt_pending[0].id, rec3.id);
    }

    #[test]
    fn test_optimizer_run_lifecycle() {
        let db = setup_test_db();

        let run_id = create_optimizer_run(&db, "pipeline_prompt", "threshold", None).unwrap();
        assert!(run_id.starts_with("morun-"));

        // List runs — should show as running
        let runs = list_optimizer_runs(&db).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run_id);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].optimizer_type, "pipeline_prompt");
        assert_eq!(runs[0].trigger_type, "threshold");
        assert_eq!(runs[0].runs_analyzed, 0);
        assert_eq!(runs[0].recommendations_produced, 0);
        assert!(runs[0].completed_at.is_none());

        // Complete the run
        complete_optimizer_run(&db, &run_id, 10, 3).unwrap();

        let runs = list_optimizer_runs(&db).unwrap();
        assert_eq!(runs[0].status, "complete");
        assert_eq!(runs[0].runs_analyzed, 10);
        assert_eq!(runs[0].recommendations_produced, 3);
        assert!(runs[0].completed_at.is_some());
    }
}
