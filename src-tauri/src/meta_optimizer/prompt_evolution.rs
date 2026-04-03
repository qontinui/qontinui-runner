//! Prompt evolution tracking.
//!
//! Records the history of prompt rewrite attempts and their canary verdicts,
//! enabling the meta-prompt optimizer to learn from past failures and avoid
//! repeating rejected approaches.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

use crate::database::pg::PgDb;

/// A single entry in the prompt evolution history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptEvolutionEntry {
    pub id: String,
    pub agent_type: String,
    pub parent_variant_id: Option<String>,
    pub variant_id: String,
    pub recommendation_id: Option<String>,
    pub critique: Option<String>,
    pub changes_summary: Option<String>,
    pub canary_verdict: Option<String>,
    pub score_before: Option<f64>,
    pub score_after: Option<f64>,
    /// SHA256 hash of the baseline prompt at the time of rewrite.
    /// Detects when the baseline has drifted (e.g., code changes to hardcoded prompts),
    /// which would invalidate pending canary results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_prompt_hash: Option<String>,
    /// Number of consecutive rejections for this agent_type at time of creation.
    /// Used by the diminishing-returns circuit breaker.
    #[serde(default)]
    pub consecutive_rejections: i32,
    pub created_at: String,
}

/// Compute SHA256 hash of a prompt string for baseline drift detection.
pub fn compute_prompt_hash(prompt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Record a new prompt evolution entry when a meta-prompt rewrite is created.
pub fn record_evolution(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
) -> Result<String, String> {
    record_evolution_full(
        pg_db,
        agent_type,
        parent_variant_id,
        variant_id,
        recommendation_id,
        critique,
        changes_summary,
        score_before,
        None,
    )
}

/// Record a new prompt evolution entry with baseline hash and rejection count.
pub fn record_evolution_full(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
    baseline_prompt_hash: Option<&str>,
) -> Result<String, String> {
    let id = format!("pe-{}", uuid::Uuid::new_v4());

    // Count consecutive rejections for this agent_type (for circuit breaker)
    let consecutive_rejections = count_consecutive_rejections(pg_db, agent_type);

    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg_db
                .record_prompt_evolution_full(
                    &id,
                    agent_type,
                    parent_variant_id,
                    variant_id,
                    recommendation_id,
                    critique,
                    changes_summary,
                    score_before,
                    baseline_prompt_hash,
                    consecutive_rejections,
                )
                .await
        })
    })?;

    Ok(id)
}

/// Record evolution (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn record_evolution_with_pg(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
) -> Result<String, String> {
    record_evolution(
        pg_db,
        agent_type,
        parent_variant_id,
        variant_id,
        recommendation_id,
        critique,
        changes_summary,
        score_before,
    )
}

/// Update the canary verdict and post-canary score for an evolution entry.
pub fn update_verdict(
    pg_db: &Arc<PgDb>,
    evolution_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg_db
                .update_evolution_verdict(evolution_id, verdict, score_after)
                .await
        })
    })?;
    info!(
        "Updated prompt evolution {} verdict: {}",
        evolution_id, verdict
    );
    Ok(())
}

/// Update verdict (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn update_verdict_with_pg(
    pg_db: &Arc<PgDb>,
    evolution_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    update_verdict(pg_db, evolution_id, verdict, score_after)
}

/// Update the canary verdict for an evolution entry identified by its variant_id.
pub fn update_verdict_by_variant(
    pg_db: &Arc<PgDb>,
    variant_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg_db
                .update_evolution_verdict_by_variant(variant_id, verdict, score_after)
                .await
        })
    })
}

/// Update verdict by variant with PG dual-write (fire-and-forget).
pub fn update_verdict_by_variant_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    variant_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    update_verdict_by_variant(pg_db, variant_id, verdict, score_after)
}

/// Get evolution history for an agent type (most recent first).
pub fn get_evolution_history(
    pg_db: &Arc<PgDb>,
    agent_type: Option<&str>,
    limit: usize,
) -> Result<Vec<PromptEvolutionEntry>, String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.get_evolution_history(agent_type, limit as i64).await })
    })
}

/// Check whether there is an active (no verdict yet) evolution entry for an agent type.
/// This indicates a canary is in progress and we should NOT create another rewrite.
pub fn has_active_evolution(pg_db: &Arc<PgDb>, agent_type: &str) -> bool {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.has_active_evolution(agent_type).await })
    })
    .unwrap_or(false)
}

/// Get the most recent evolution entry for an agent type that was rejected.
/// Used to feed failure context back into the next optimization round.
pub fn get_latest_rejected(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
) -> Result<Option<PromptEvolutionEntry>, String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.get_latest_rejected_evolution(agent_type).await })
    })
}

/// Check the cooldown: returns true if the last evolution entry for this agent_type
/// was created less than `cooldown_hours` ago.
pub fn is_in_cooldown(pg_db: &Arc<PgDb>, agent_type: &str, cooldown_hours: i64) -> bool {
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(cooldown_hours)).to_rfc3339();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.is_in_evolution_cooldown(agent_type, &cutoff).await })
    })
    .unwrap_or(false)
}

/// Count consecutive recent rejections for an agent_type.
///
/// Looks at the most recent evolution entries and counts how many consecutive
/// "reject" verdicts appear (stopping at the first non-reject). Used by the
/// diminishing-returns circuit breaker.
pub fn count_consecutive_rejections(pg_db: &Arc<PgDb>, agent_type: &str) -> i32 {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.count_consecutive_rejections(agent_type).await })
    })
    .unwrap_or(0)
}

/// Compute the adaptive cooldown hours based on consecutive rejections.
///
/// Implements exponential backoff: 24h → 72h → 168h (1 week) → 336h (2 weeks).
/// After 4+ consecutive rejections, the optimizer should largely leave this
/// agent's prompt alone — it's likely near-optimal or has structural issues
/// that prompt rewriting can't fix.
pub fn adaptive_cooldown_hours(consecutive_rejections: i32) -> i64 {
    match consecutive_rejections {
        0 => 24,  // No rejections: standard 24h cooldown
        1 => 72,  // 1 rejection: 3 days
        2 => 168, // 2 rejections: 1 week
        3 => 336, // 3 rejections: 2 weeks
        _ => 672, // 4+ rejections: 4 weeks
    }
}

/// Get all rejected prompt variant contents for an agent_type.
///
/// Returns (variant_id, prompt_content) pairs for similarity comparison.
/// Only fetches variants that were part of rejected evolution entries.
pub fn get_rejected_prompt_contents(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
) -> Result<Vec<(String, String)>, String> {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(async { pg_db.get_rejected_prompt_contents(agent_type).await })
    })
}

/// Check if the baseline prompt has drifted since a canary was started.
///
/// Compares the stored `baseline_prompt_hash` against the current prompt hash.
/// Returns true if the baseline has changed, indicating the canary results may
/// be invalid (the prompt being compared against is different from when the
/// canary started).
pub fn has_baseline_drifted(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    current_prompt_hash: &str,
) -> bool {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            pg_db
                .has_baseline_drifted(agent_type, current_prompt_hash)
                .await
        })
    })
    .unwrap_or(false)
}

/// Get evolution history (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn get_evolution_history_with_pg(
    pg_db: &Arc<PgDb>,
    agent_type: Option<&str>,
    limit: usize,
) -> Result<Vec<PromptEvolutionEntry>, String> {
    get_evolution_history(pg_db, agent_type, limit)
}

/// Check whether there is an active evolution (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn has_active_evolution_with_pg(pg_db: &Arc<PgDb>, agent_type: &str) -> bool {
    has_active_evolution(pg_db, agent_type)
}

/// Get the most recent rejected evolution entry (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn get_latest_rejected_with_pg(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
) -> Result<Option<PromptEvolutionEntry>, String> {
    get_latest_rejected(pg_db, agent_type)
}

/// Check cooldown (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn is_in_cooldown_with_pg(pg_db: &Arc<PgDb>, agent_type: &str, cooldown_hours: i64) -> bool {
    is_in_cooldown(pg_db, agent_type, cooldown_hours)
}

/// Count consecutive rejections (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn count_consecutive_rejections_with_pg(pg_db: &Arc<PgDb>, agent_type: &str) -> i32 {
    count_consecutive_rejections(pg_db, agent_type)
}

/// Get rejected prompt contents (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn get_rejected_prompt_contents_with_pg(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
) -> Result<Vec<(String, String)>, String> {
    get_rejected_prompt_contents(pg_db, agent_type)
}

/// Check baseline drift (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn has_baseline_drifted_with_pg(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    current_prompt_hash: &str,
) -> bool {
    has_baseline_drifted(pg_db, agent_type, current_prompt_hash)
}

/// Update verdict by variant (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn update_verdict_by_variant_direct_pg(
    pg_db: &Arc<PgDb>,
    variant_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    update_verdict_by_variant(pg_db, variant_id, verdict, score_after)
}

/// Record evolution with full parameters (backward compat, delegates to primary).
#[allow(dead_code)]
pub fn record_evolution_full_with_pg(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    parent_variant_id: Option<&str>,
    variant_id: &str,
    recommendation_id: Option<&str>,
    critique: Option<&str>,
    changes_summary: Option<&str>,
    score_before: Option<f64>,
    baseline_prompt_hash: Option<&str>,
) -> Result<String, String> {
    record_evolution_full(
        pg_db,
        agent_type,
        parent_variant_id,
        variant_id,
        recommendation_id,
        critique,
        changes_summary,
        score_before,
        baseline_prompt_hash,
    )
}

// Tests removed during PG migration: all tests relied on SQLite in-memory
// CheckpointDb which no longer backs these functions. Integration tests
// covering PG prompt_evolution live in tests/integration/.
#[cfg(test)]
mod tests {
    // Placeholder — SQLite-based tests removed during PG migration.
    #[test]
    fn placeholder() {}
}
