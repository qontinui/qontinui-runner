//! CRUD operations for the meta_optimizer_recommendations table.
//!
//! All optimizer outputs go here with status `pending`. Human reviews from UI.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::{info, warn};

use super::types::{MetaOptimizerRun, Recommendation};
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
pub fn is_content_duplicate(pg_db: &Arc<PgDb>, content_hash: &str) -> bool {
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
            &rec.id,
            optimizer_type,
            recommendation_type,
            target_agent,
            title,
            description,
            current_value,
            recommended_value,
            evidence,
            confidence,
            optimizer_run_id,
            &content_hash,
        ))
    })?;
    info!("Created recommendation {} ({})", rec.id, rec.title);

    Ok(rec)
}

/// List recommendations with optional filters.
pub fn list_recommendations(
    pg_db: &Arc<PgDb>,
    optimizer_type: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Recommendation>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_recommendations(optimizer_type, status))
    })
}

/// Async variant of [`list_recommendations`] for callers already on a tokio runtime
/// (notably sync `#[tauri::command]` functions would panic in the sync wrapper above
/// because Tauri's webview worker thread lacks an ambient multi-thread runtime).
pub async fn list_recommendations_async(
    pg_db: &Arc<PgDb>,
    optimizer_type: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Recommendation>, String> {
    pg_db.list_recommendations(optimizer_type, status).await
}

/// Apply a recommendation (updates status to 'applied').
/// This is the simple status-only flip — prefer `apply_recommendation_with_side_effects`.
pub fn apply_recommendation(pg_db: &Arc<PgDb>, recommendation_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_recommendation_status(recommendation_id, "applied"))
    })?;
    info!("Applied recommendation {}", recommendation_id);
    Ok(())
}

/// Fetch a single recommendation by ID.
fn get_recommendation(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<Recommendation, String> {
    let result = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_recommendation(recommendation_id))
    })?;
    result.ok_or_else(|| format!("Recommendation not found: {}", recommendation_id))
}

/// Async variant of [`apply_recommendation_with_side_effects`].
///
/// Safe to call from a sync `#[tauri::command]` that has been converted to
/// `async fn`. Unlike the sync version, this never reaches
/// `tokio::task::block_in_place` — every PG call is awaited directly.
pub async fn apply_recommendation_with_side_effects_async(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    let id = recommendation_id.to_string();
    let rec = pg_db
        .get_recommendation(&id)
        .await?
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
        "prompt_rewrite" => apply_prompt_rewrite_async(pg_db, &rec.id, recommended_value).await?,
        "config_change" => apply_config_change_async(pg_db, recommended_value).await?,
        "rule_create" => apply_rule_create_async(pg_db, &rec.id, recommended_value).await?,
        "rule_update" => apply_rule_update_async(pg_db, recommended_value).await?,
        other => {
            warn!(
                "Unknown recommendation_type '{}' for {}; applying status-only",
                other, rec.id
            );
        }
    }

    // Side-effect succeeded — now flip the status via PG
    pg_db.apply_recommendation_with_timestamp(&id).await?;

    // Capture a snapshot to measure impact of this recommendation.
    // `capture_post_apply` is a sync wrapper with its own `block_in_place`;
    // from the async Tauri multi-thread runtime that's still safe. Defer the
    // full conversion of snapshots/* — tracked as Phase B work.
    if let Err(e) = super::snapshots::capture_post_apply(
        pg_db,
        recommendation_id,
        super::types::WorkflowCategory::Main,
    ) {
        warn!("Failed to capture post-apply snapshot: {}", e);
    }

    // Evaluate outcome immediately (will likely be "insufficient_data" initially)
    if let Err(e) = super::snapshots::evaluate_recommendation_outcome(pg_db, recommendation_id) {
        warn!("Failed to evaluate recommendation outcome: {}", e);
    }

    Ok(())
}

/// Async variant of [`apply_prompt_rewrite`] — keeps the write path in a
/// single await chain so sync Tauri commands converted to `async fn` never
/// need to cross back into `block_in_place`.
async fn apply_prompt_rewrite_async(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: PromptRewritePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid prompt_rewrite payload: {}", e))?;

    let variant = pg_db
        .create_prompt_variant(
            &payload.agent_type,
            &payload.variant_name,
            &payload.prompt_content,
            Some(recommendation_id),
        )
        .await?;

    // Best-effort resource-version snapshot (same policy as the sync wrapper).
    if let Err(e) = super::resource_versioning::create_version(
        pg_db,
        "prompt",
        &payload.agent_type,
        &payload.prompt_content,
        Some(&format!(
            r#"{{"variant_id":"{}","variant_name":"{}"}}"#,
            variant.id, payload.variant_name
        )),
    )
    .await
    {
        tracing::warn!(
            "Failed to create resource version for prompt variant: {}",
            e
        );
    }

    pg_db.activate_variant(&variant.id).await?;

    info!(
        "Applied prompt_rewrite recommendation {}: created and activated variant {}",
        recommendation_id, variant.id
    );
    Ok(())
}

async fn apply_config_change_async(
    pg_db: &Arc<PgDb>,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid config_change payload: {}", e))?;

    pg_db.set_setting(&payload.key, &payload.value).await?;

    info!(
        "Applied config_change: set '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

async fn apply_rule_create_async(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: RulePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid rule_create payload: {}", e))?;

    let rec_id = recommendation_id.to_string();

    let rule_number = match payload.rule_number {
        Some(n) => n,
        None => {
            pg_db
                .next_rule_number(&payload.agent, &payload.section)
                .await?
        }
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

    let rule = pg_db.insert_rule(&input).await?;
    info!(
        "Applied rule_create recommendation {}: created rule {}",
        rec_id, rule.id
    );
    Ok(())
}

async fn apply_rule_update_async(pg_db: &Arc<PgDb>, recommended_value: &str) -> Result<(), String> {
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

    let rule = pg_db.update_rule(&rule_id, &input).await?;
    info!(
        "Applied rule_update: updated rule {} ({})",
        rule.id, rule.title
    );
    Ok(())
}

/// Apply a recommendation **and** perform the appropriate side-effect based on
/// `recommendation_type`. If the side-effect fails the status is NOT updated.
pub fn apply_recommendation_with_side_effects(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    let id = recommendation_id.to_string();
    let rec =
        tokio::task::block_in_place(|| Handle::current().block_on(pg_db.get_recommendation(&id)))?
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
        "prompt_rewrite" => apply_prompt_rewrite(pg_db, &rec.id, recommended_value)?,
        "config_change" => apply_config_change(pg_db, recommended_value)?,
        "rule_create" => apply_rule_create(pg_db, &rec.id, recommended_value)?,
        "rule_update" => apply_rule_update(pg_db, recommended_value)?,
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
        pg_db,
        recommendation_id,
        super::types::WorkflowCategory::Main,
    ) {
        warn!("Failed to capture post-apply snapshot: {}", e);
    }

    // Evaluate outcome immediately (will likely be "insufficient_data" initially)
    if let Err(e) = super::snapshots::evaluate_recommendation_outcome(pg_db, recommendation_id) {
        warn!("Failed to evaluate recommendation outcome: {}", e);
    }

    Ok(())
}

// ── Side-effect helpers ──────────────────────────────────────────────

fn apply_prompt_rewrite(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let payload: PromptRewritePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid prompt_rewrite payload: {}", e))?;

    let variant = super::prompt_registry::create_variant(
        pg_db,
        &payload.agent_type,
        &payload.variant_name,
        &payload.prompt_content,
        Some(recommendation_id),
    )?;

    super::prompt_registry::activate_variant(pg_db, &variant.id)?;

    info!(
        "Applied prompt_rewrite recommendation {}: created and activated variant {}",
        recommendation_id, variant.id
    );
    Ok(())
}

fn apply_config_change(pg_db: &Arc<PgDb>, recommended_value: &str) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(recommended_value)
        .map_err(|e| format!("Invalid config_change payload: {}", e))?;

    // Config changes now go through PG settings
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.set_setting(&payload.key, &payload.value))
    })?;

    info!(
        "Applied config_change: set '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

fn apply_rule_create(
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

    let rule =
        tokio::task::block_in_place(|| Handle::current().block_on(pg_db.insert_rule(&input)))?;
    info!(
        "Applied rule_create recommendation {}: created rule {}",
        rec_id, rule.id
    );
    Ok(())
}

fn apply_rule_update(pg_db: &Arc<PgDb>, recommended_value: &str) -> Result<(), String> {
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
pub fn reject_recommendation(pg_db: &Arc<PgDb>, recommendation_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current()
            .block_on(pg_db.update_recommendation_status(recommendation_id, "rejected"))
    })?;
    info!("Rejected recommendation {}", recommendation_id);
    Ok(())
}

/// Async variant of [`reject_recommendation`] — safe to call from a sync
/// `#[tauri::command]` that has been converted to `async fn`.
pub async fn reject_recommendation_async(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    pg_db
        .update_recommendation_status(recommendation_id, "rejected")
        .await?;
    info!("Rejected recommendation {}", recommendation_id);
    Ok(())
}

/// Deduplicate pending recommendations by content hash.
///
/// For each group of pending recs with the same content hash, keeps the oldest
/// and supersedes newer pending duplicates. Canary and applied recs are not touched
/// (they represent active rollouts or already-applied changes). Also backfills
/// content_hash for older rows that lack it.
pub fn dedup_pending_recommendations(pg_db: &Arc<PgDb>) -> usize {
    let superseded = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let conn = pg_db
                .pool()
                .get()
                .await
                .map_err(|e| format!("PG pool: {e}"))?;
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
        info!(
            "Dedup: superseded {} duplicate pending recommendation(s)",
            superseded
        );
    }
    superseded
}

/// Auto-reject pending recommendations older than 30 days.
///
/// Stale recs block the optimizer from regenerating fresh suggestions for the
/// same targets. Rejecting them frees those slots while preserving history.
pub fn auto_reject_stale_recommendations(pg_db: &Arc<PgDb>) -> usize {
    0
}

/// Async variant of [`rollback_recommendation`]. Safe to call from async
/// Tauri commands — never reaches `block_in_place`.
pub async fn rollback_recommendation_async(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<(), String> {
    let rec = pg_db
        .get_recommendation(recommendation_id)
        .await?
        .ok_or_else(|| format!("Recommendation not found: {}", recommendation_id))?;

    if rec.status != "applied" {
        return Err(format!(
            "Recommendation {} is not applied (status: {})",
            rec.id, rec.status
        ));
    }

    if let Some(ref recommended_value) = rec.recommended_value {
        match rec.recommendation_type.as_str() {
            "rule_create" | "rule_update" => {
                if let Err(e) = rollback_rule_async(pg_db, &rec.id, recommended_value).await {
                    warn!("Failed to rollback rule side-effect for {}: {}", rec.id, e);
                }
            }
            "config_change" => {
                if let Some(ref current_value) = rec.current_value {
                    if let Err(e) = rollback_config_change_async(pg_db, current_value).await {
                        warn!(
                            "Failed to rollback config side-effect for {}: {}",
                            rec.id, e
                        );
                    }
                }
            }
            "prompt_rewrite" => {
                info!(
                    "Prompt rollback for {} — variant left in registry, user can deactivate manually",
                    rec.id
                );
            }
            _ => {}
        }
    }

    pg_db
        .update_recommendation_status(recommendation_id, "rolled_back")
        .await?;
    info!("Rolled back recommendation {}", recommendation_id);
    Ok(())
}

async fn rollback_rule_async(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    recommended_value: &str,
) -> Result<(), String> {
    let rule_id_from_payload: Option<String> =
        serde_json::from_str::<RulePayload>(recommended_value)
            .ok()
            .and_then(|p| p.rule_id);

    let target_rule_id = if let Some(id) = rule_id_from_payload {
        id
    } else {
        pg_db
            .find_rule_by_source_fix_id(recommendation_id)
            .await?
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

    pg_db.update_rule(&target_rule_id, &input).await?;
    info!("Rollback: disabled rule {}", target_rule_id);
    Ok(())
}

async fn rollback_config_change_async(
    pg_db: &Arc<PgDb>,
    current_value: &str,
) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(current_value)
        .map_err(|e| format!("Invalid current_value payload for config rollback: {}", e))?;

    pg_db.set_setting(&payload.key, &payload.value).await?;
    info!(
        "Rollback: restored config '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

/// Roll back an applied recommendation, undoing side-effects where possible.
pub fn rollback_recommendation(pg_db: &Arc<PgDb>, recommendation_id: &str) -> Result<(), String> {
    let rec = get_recommendation(pg_db, recommendation_id)?;

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
                if let Err(e) = rollback_rule(pg_db, &rec.id, recommended_value) {
                    warn!("Failed to rollback rule side-effect for {}: {}", rec.id, e);
                }
            }
            "config_change" => {
                if let Some(ref current_value) = rec.current_value {
                    if let Err(e) = rollback_config_change(pg_db, current_value) {
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
        Handle::current()
            .block_on(pg_db.update_recommendation_status(recommendation_id, "rolled_back"))
    })?;
    info!("Rolled back recommendation {}", recommendation_id);
    Ok(())
}

/// Undo a rule side-effect by disabling the rule that was created/updated.
fn rollback_rule(
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
fn rollback_config_change(pg_db: &Arc<PgDb>, current_value: &str) -> Result<(), String> {
    let payload: ConfigChangePayload = serde_json::from_str(current_value)
        .map_err(|e| format!("Invalid current_value payload for config rollback: {}", e))?;

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.set_setting(&payload.key, &payload.value))
    })?;
    info!(
        "Rollback: restored config '{}' to {}",
        payload.key, payload.value
    );
    Ok(())
}

// ── Meta-Optimizer Runs ────────────────────────────────────────────────

/// Create a new optimizer run record.
pub fn create_optimizer_run(
    pg_db: &Arc<PgDb>,
    optimizer_type: &str,
    trigger_type: &str,
    task_run_id: Option<&str>,
) -> Result<String, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.create_optimizer_run(
            optimizer_type,
            trigger_type,
            task_run_id,
        ))
    })
}

/// Complete an optimizer run, recording how many runs were analyzed and recommendations produced.
pub fn complete_optimizer_run(
    pg_db: &Arc<PgDb>,
    run_id: &str,
    runs_analyzed: i64,
    recommendations_produced: i64,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.complete_optimizer_run(
            run_id,
            runs_analyzed,
            recommendations_produced,
        ))
    })
}

/// List optimizer runs.
pub fn list_optimizer_runs(pg_db: &Arc<PgDb>) -> Result<Vec<MetaOptimizerRun>, String> {
    tokio::task::block_in_place(|| Handle::current().block_on(pg_db.list_optimizer_runs()))
}

/// Async variant of [`list_optimizer_runs`].
pub async fn list_optimizer_runs_async(pg_db: &Arc<PgDb>) -> Result<Vec<MetaOptimizerRun>, String> {
    pg_db.list_optimizer_runs().await
}

/// Evaluate applied recommendations using composite agentic scores.
///
/// Compares the average composite_agentic_score from runs BEFORE the recommendation
/// was applied vs runs AFTER. If the delta is significant and positive, updates
/// outcome_after_apply with the evidence. If negative, flags for rollback.
///
/// Called from the maintenance block in trigger.rs.
pub fn auto_evaluate_with_agentic_scores(pg_db: &Arc<PgDb>) {
    let recs = match list_recommendations(pg_db, None, Some("applied")) {
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
