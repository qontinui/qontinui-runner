//! Contradiction Resolution Engine
//!
//! Detects contradictory observations (same topic_key, different content) and
//! auto-resolves them using a multi-factor scoring heuristic:
//!
//!   Score = 0.4*recency + 0.3*importance + 0.2*evidence_weight + 0.1*access_frequency
//!
//! When the score difference exceeds 0.3, the winner supersedes the loser.
//! Ambiguous cases (diff <= 0.3) are skipped for future LLM resolution.

use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;
use crate::database::types::{ContradictionCandidate, ContradictionStats};

/// Internal scoring result for auto-resolution.
struct ResolutionScore {
    winner_id: i64,
    loser_id: i64,
    resolution_type: String,
    confidence: f64,
    rationale: String,
    evidence: serde_json::Value,
}

/// Compute a normalized recency score (0..1) from an RFC3339 timestamp.
/// Newer timestamps produce higher scores. Half-life is ~30 days.
fn recency_score(valid_from: &str) -> f64 {
    let parsed = chrono::DateTime::parse_from_rfc3339(valid_from);
    match parsed {
        Ok(dt) => {
            let age_days = (chrono::Utc::now() - dt.with_timezone(&chrono::Utc))
                .num_hours() as f64
                / 24.0;
            // Exponential decay with ~30-day half-life
            (-0.0231 * age_days).exp()
        }
        Err(_) => 0.5, // fallback for unparseable dates
    }
}

/// Normalize access_count to 0..1 using a saturating curve.
/// access_count of 10 maps to ~0.67, 50 maps to ~0.91.
fn access_score(access_count: i32) -> f64 {
    let count = access_count.max(0) as f64;
    // Hyperbolic tangent-like normalization: count / (count + 20)
    count / (count + 20.0)
}

/// Auto-resolve a contradiction based on multi-factor scoring.
///
/// Score = 0.4*recency + 0.3*importance + 0.2*evidence_weight + 0.1*access_frequency
///
/// `evidence_weight` is proxied by importance (more vetted observations tend to have
/// higher importance from revision/duplicate boosts).
///
/// Returns `Some(ResolutionScore)` if the score difference exceeds 0.3, `None` if ambiguous.
fn auto_resolve(candidate: &ContradictionCandidate) -> Option<ResolutionScore> {
    // Compute component scores for observation A
    let a_recency = recency_score(&candidate.obs_a_valid_from);
    let a_importance = candidate.obs_a_importance.clamp(0.0, 1.0);
    let a_evidence = a_importance; // proxy: importance reflects revision/dup boosts
    let a_access = access_score(candidate.obs_a_access_count);
    let a_total = 0.4 * a_recency + 0.3 * a_importance + 0.2 * a_evidence + 0.1 * a_access;

    // Compute component scores for observation B
    let b_recency = recency_score(&candidate.obs_b_valid_from);
    let b_importance = candidate.obs_b_importance.clamp(0.0, 1.0);
    let b_evidence = b_importance;
    let b_access = access_score(candidate.obs_b_access_count);
    let b_total = 0.4 * b_recency + 0.3 * b_importance + 0.2 * b_evidence + 0.1 * b_access;

    let diff = (a_total - b_total).abs();

    debug!(
        "Contradiction scoring: obs_a={} ({:.3}) vs obs_b={} ({:.3}), diff={:.3}",
        candidate.obs_a_id, a_total, candidate.obs_b_id, b_total, diff
    );

    if diff <= 0.3 {
        // Ambiguous — skip for potential LLM resolution later
        return None;
    }

    let (winner_id, loser_id, winner_score, loser_score) = if a_total > b_total {
        (candidate.obs_a_id, candidate.obs_b_id, a_total, b_total)
    } else {
        (candidate.obs_b_id, candidate.obs_a_id, b_total, a_total)
    };

    let confidence = diff.min(1.0);

    let rationale = format!(
        "Auto-resolved via multi-factor scoring: winner (id={}) scored {:.3} vs loser (id={}) scored {:.3}. \
         Factors: recency(0.4), importance(0.3), evidence(0.2), access_frequency(0.1). \
         Score difference {:.3} exceeded threshold 0.3.",
        winner_id, winner_score, loser_id, loser_score, diff
    );

    let evidence = serde_json::json!({
        "winner_id": winner_id,
        "loser_id": loser_id,
        "winner_score": (winner_score * 1000.0).round() / 1000.0,
        "loser_score": (loser_score * 1000.0).round() / 1000.0,
        "score_diff": (diff * 1000.0).round() / 1000.0,
        "components": {
            "a": {
                "recency": (a_recency * 1000.0).round() / 1000.0,
                "importance": (a_importance * 1000.0).round() / 1000.0,
                "evidence": (a_evidence * 1000.0).round() / 1000.0,
                "access": (a_access * 1000.0).round() / 1000.0,
                "total": (a_total * 1000.0).round() / 1000.0,
            },
            "b": {
                "recency": (b_recency * 1000.0).round() / 1000.0,
                "importance": (b_importance * 1000.0).round() / 1000.0,
                "evidence": (b_evidence * 1000.0).round() / 1000.0,
                "access": (b_access * 1000.0).round() / 1000.0,
                "total": (b_total * 1000.0).round() / 1000.0,
            },
        },
        "topic_key": candidate.obs_a_topic_key,
    });

    Some(ResolutionScore {
        winner_id,
        loser_id,
        resolution_type: "auto_score".to_string(),
        confidence,
        rationale,
        evidence,
    })
}

/// Apply a resolution: supersede the loser observation and persist the resolution record.
async fn apply_resolution(
    pg: &PgDb,
    candidate: &ContradictionCandidate,
    winner_id: i64,
    loser_id: i64,
    resolution_type: &str,
    confidence: f64,
    rationale: &str,
    evidence_json: Option<&str>,
) -> Result<(), String> {
    // 1. Supersede the loser observation (sets valid_until = NOW(), superseded_by = winner)
    pg.supersede_observation(loser_id, winner_id).await?;

    // 2. Persist the resolution record
    pg.save_contradiction_resolution(
        candidate.obs_a_id,
        candidate.obs_b_id,
        resolution_type,
        Some(winner_id),
        Some(loser_id),
        confidence,
        rationale,
        evidence_json,
        "auto_score",
    )
    .await?;

    debug!(
        "Applied contradiction resolution: winner={}, loser={}, type={}",
        winner_id, loser_id, resolution_type
    );

    Ok(())
}

/// Run a full contradiction scan: detect candidates, score them, and auto-resolve.
///
/// Returns statistics about the scan results.
pub async fn run_contradiction_scan(
    pg: &Arc<PgDb>,
    max_candidates: i64,
) -> Result<ContradictionStats, String> {
    let mut stats = ContradictionStats::default();

    let candidates = pg.detect_contradictions(max_candidates).await?;
    stats.detected = candidates.len() as u32;

    if candidates.is_empty() {
        debug!("Contradiction scan: no candidates found");
        return Ok(stats);
    }

    info!(
        "Contradiction scan: found {} candidate pairs",
        candidates.len()
    );

    for candidate in &candidates {
        match auto_resolve(candidate) {
            Some(resolution) => {
                let evidence_str = resolution.evidence.to_string();
                match apply_resolution(
                    pg,
                    candidate,
                    resolution.winner_id,
                    resolution.loser_id,
                    &resolution.resolution_type,
                    resolution.confidence,
                    &resolution.rationale,
                    Some(&evidence_str),
                )
                .await
                {
                    Ok(()) => {
                        info!(
                            "Auto-resolved contradiction: topic={:?}, winner={}, loser={}",
                            candidate.obs_a_topic_key, resolution.winner_id, resolution.loser_id
                        );
                        stats.auto_resolved += 1;
                    }
                    Err(e) => {
                        warn!("Failed to apply contradiction resolution: {}", e);
                        stats.failed += 1;
                    }
                }
            }
            None => {
                debug!(
                    "Skipping ambiguous contradiction: obs_a={}, obs_b={}, topic={:?}",
                    candidate.obs_a_id, candidate.obs_b_id, candidate.obs_a_topic_key
                );
                // Ambiguous — will be addressed by LLM resolution in a future enhancement
            }
        }
    }

    info!(
        "Contradiction scan complete: detected={}, auto_resolved={}, failed={}",
        stats.detected, stats.auto_resolved, stats.failed
    );

    Ok(stats)
}
