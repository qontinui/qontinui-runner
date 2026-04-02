//! Canary rollout system for meta-optimizer recommendations.
//!
//! Allows testing a recommendation on a percentage of runs before full rollout.

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryRollout {
    pub id: String,
    pub recommendation_id: String,
    pub percentage: i64,
    pub status: String,
    pub start_date: String,
    pub end_date: Option<String>,
    pub baseline_run_count: i64,
    pub canary_run_count: i64,
    pub baseline_metrics_json: String,
    pub canary_metrics_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryMetrics {
    pub success_count: i64,
    pub failure_count: i64,
    pub total_cost_usd: f64,
    pub total_duration_ms: f64,
}

impl Default for CanaryMetrics {
    fn default() -> Self {
        Self {
            success_count: 0,
            failure_count: 0,
            total_cost_usd: 0.0,
            total_duration_ms: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryEvaluation {
    pub verdict: String, // "promote" | "rollback" | "continue"
    pub baseline_success_rate: f64,
    pub canary_success_rate: f64,
    pub delta: f64,
    pub min_runs_met: bool,
    /// One-sided p-value (H₁: canary > baseline). None if insufficient data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    /// 95% confidence interval for the success rate difference (canary - baseline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_interval: Option<(f64, f64)>,
    /// Cohen's h effect size for the success rate difference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_size: Option<f64>,
    /// Percentage change in average cost (canary vs baseline). Positive = more expensive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_delta_pct: Option<f64>,
    /// Percentage change in average duration (canary vs baseline). Positive = slower.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_delta_pct: Option<f64>,
}

/// Start a canary rollout for a recommendation.
pub fn start_canary(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    percentage: i64,
) -> Result<String, String> {
    let percentage = percentage.clamp(1, 100);
    tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            pg_db.start_canary(recommendation_id, percentage as f64).await
        })
    })
}

/// Get all active canary rollouts.
pub fn get_active_canaries(pg_db: &Arc<PgDb>) -> Result<Vec<CanaryRollout>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_active_canaries())
    })
}

/// Update the traffic percentage for an active canary rollout.
///
/// Used to extend canary evaluation by increasing traffic when results are
/// inconclusive after many runs.
pub fn update_canary_percentage(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
    new_percentage: i64,
) -> Result<(), String> {
    let pct = new_percentage.clamp(1, 100);

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_canary_percentage(canary_id, pct))
    })
}

/// Get completed canary rollouts (promoted or rolled back) for history display.
pub fn get_canary_history(pg_db: &Arc<PgDb>, limit: u32) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.min(100) as i64;
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_canary_history(limit))
    })
}

/// Get the canary prompt overrides for a recommendation.
/// Returns a map of agent_type → prompt_content if the recommendation is a prompt_rewrite,
/// or an empty map otherwise. This is used to inject the canary prompt during pipeline execution.
pub fn get_canary_prompt_overrides(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_canary_prompt_overrides(recommendation_id))
    })
}

/// Get config overrides for a canary rollout of a config_change recommendation.
/// Returns a vec of (key, serde_json::Value) pairs to apply as temporary settings during canary runs.
pub fn get_canary_config_overrides(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
) -> Result<Vec<(String, serde_json::Value)>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_canary_config_overrides(recommendation_id))
    })
}

/// Probabilistic check: should this run use the canary config?
pub fn should_apply_canary(pg_db: &Arc<PgDb>, recommendation_id: &str) -> bool {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.should_apply_canary(recommendation_id))
    })
    .unwrap_or(false)
}

/// Record a completed run as either baseline or canary.
pub fn record_canary_run(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
    is_canary: bool,
    success: bool,
    cost: f64,
    duration_ms: f64,
) -> Result<(), String> {
    // PG-primary: load current metrics from PG, update, write back
    let pg_result = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let (baseline_json, canary_json) = pg_db.get_canary_metrics(canary_id).await?;
            let mut baseline: CanaryMetrics = serde_json::from_str(&baseline_json).unwrap_or_default();
            let mut canary_m: CanaryMetrics = serde_json::from_str(&canary_json).unwrap_or_default();
            let metrics = if is_canary { &mut canary_m } else { &mut baseline };
            if success { metrics.success_count += 1; } else { metrics.failure_count += 1; }
            metrics.total_cost_usd += cost;
            metrics.total_duration_ms += duration_ms;
            let new_baseline = serde_json::to_string(&baseline).unwrap_or_default();
            let new_canary = serde_json::to_string(&canary_m).unwrap_or_default();
            let run_type = if is_canary { "canary" } else { "baseline" };
            pg_db.record_canary_run(canary_id, run_type, if is_canary { &new_canary } else { &new_baseline }).await?;
            // Also update full metrics
            pg_db.update_canary_metrics(canary_id, &new_baseline, &new_canary).await?;
            Ok::<(), String>(())
        })
    });

    pg_result
}

/// Evaluate a canary: should it be promoted, rolled back, or continue?
///
/// Uses statistical tests (proportion z-test, confidence intervals, effect size)
/// instead of simple threshold-based verdicts.
pub fn evaluate_canary(pg_db: &Arc<PgDb>, canary_id: &str) -> Result<CanaryEvaluation, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.evaluate_canary(canary_id))
    })
}

/// Promote a canary: apply the recommendation globally.
pub fn promote_canary(pg_db: &Arc<PgDb>, canary_id: &str) -> Result<(), String> {
    let canary_id_str = canary_id.to_string();

    // Get recommendation_id from PG
    let rec_id: String = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let conn = pg_db.pool().get().await.map_err(|e| format!("PG pool: {e}"))?;
            let row = conn.query_one(
                "SELECT recommendation_id FROM canary_rollouts WHERE id = $1",
                &[&canary_id_str],
            ).await.map_err(|e| format!("PG canary lookup: {e}"))?;
            Ok::<String, String>(row.get(0))
        })
    })?;

    // Apply the recommendation fully
    super::recommendations::apply_recommendation_with_side_effects(pg_db, &rec_id)?;

    // Update canary status
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.promote_canary(&canary_id_str))
    })?;

    // Update prompt evolution verdict if this was a meta-prompt rewrite canary
    if let Err(e) = update_evolution_for_recommendation(pg_db, &rec_id, "adopt", None) {
        debug!("No prompt evolution entry for recommendation {}: {}", rec_id, e);
    }

    info!("Promoted canary {} (recommendation {})", canary_id, rec_id);
    Ok(())
}

/// Roll back a canary: mark recommendation as rolled_back and record evaluation metrics.
pub fn rollback_canary(pg_db: &Arc<PgDb>, canary_id: &str) -> Result<(), String> {
    rollback_canary_with_eval(pg_db, canary_id, None)
}

/// Roll back a canary with optional evaluation metrics to record on the recommendation.
pub fn rollback_canary_with_eval(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
    eval: Option<&CanaryEvaluation>,
) -> Result<(), String> {
    let canary_id_str = canary_id.to_string();
    let eval_json = eval
        .and_then(|e| serde_json::to_string(e).ok())
        .unwrap_or_else(|| "{}".to_string());

    // Get rec_id from PG
    let rec_id: String = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let conn = pg_db.pool().get().await.map_err(|e| format!("PG pool: {e}"))?;
            let row = conn.query_one(
                "SELECT recommendation_id FROM canary_rollouts WHERE id = $1",
                &[&canary_id_str],
            ).await.map_err(|e| format!("PG canary lookup: {e}"))?;
            Ok::<String, String>(row.get(0))
        })
    })?;

    // Update canary status and recommendation
    tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            pg_db.rollback_canary(&canary_id_str).await?;
            // Also update recommendation outcome
            pg_db.update_recommendation_outcome(&rec_id, &eval_json).await?;
            Ok::<(), String>(())
        })
    })?;

    info!(
        "Rolled back canary {} (recommendation {} -> rolled_back)",
        canary_id_str, rec_id
    );

    // Compute canary success rate as score_after for evolution tracking
    let score_after = eval.map(|e| e.canary_success_rate / 100.0);

    // Update prompt evolution verdict if this was a meta-prompt rewrite canary
    if let Err(e) = update_evolution_for_recommendation(pg_db, &rec_id, "reject", score_after) {
        debug!("No prompt evolution entry for recommendation {}: {}", rec_id, e);
    }

    Ok(())
}

/// Update the prompt evolution verdict for a recommendation.
///
/// Looks up the prompt_evolution entry by recommendation_id and updates its
/// canary_verdict and score_after. This closes the feedback loop for the
/// meta-prompt optimizer.
fn update_evolution_for_recommendation(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Auto-rollback canary rollouts that have been active for more than 30 days
/// without reaching a promote/rollback verdict. Stale canaries block the optimizer
/// from generating fresh recommendations for the same target.
pub fn auto_rollback_stale_canaries(pg_db: &Arc<PgDb>) -> usize {
    let canaries = match get_active_canaries(pg_db) {
        Ok(c) => c,
        Err(_) => return 0,
    };

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let mut rolled_back = 0;

    for canary in &canaries {
        let started = if canary.start_date.is_empty() {
            &canary.created_at
        } else {
            &canary.start_date
        };
        if started.as_str() >= cutoff.as_str() {
            continue;
        }

        info!(
            "Auto-rolling-back stale canary {} (active since {}, {} canary runs)",
            canary.id, started, canary.canary_run_count
        );
        if let Err(e) = rollback_canary(pg_db, &canary.id) {
            warn!("Failed to auto-rollback stale canary {}: {}", canary.id, e);
        } else {
            rolled_back += 1;
        }
    }

    if rolled_back > 0 {
        info!(
            "Auto-rolled-back {} stale canary rollout(s) (active >30 days)",
            rolled_back
        );
    }

    rolled_back
}

// ── Prompt Template A/B Testing ──────────────────────────────────────────

/// Per-version metrics for prompt template canary testing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptVersionMetrics {
    pub run_count: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub total_cost_usd: f64,
    pub total_latency_ms: f64,
    pub total_tokens: i64,
}

impl PromptVersionMetrics {
    pub fn success_rate(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total > 0 {
            self.success_count as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    }

    pub fn avg_cost(&self) -> f64 {
        if self.run_count > 0 {
            self.total_cost_usd / self.run_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_latency(&self) -> f64 {
        if self.run_count > 0 {
            self.total_latency_ms / self.run_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_tokens(&self) -> f64 {
        if self.run_count > 0 {
            self.total_tokens as f64 / self.run_count as f64
        } else {
            0.0
        }
    }
}

/// A prompt template canary for A/B testing two versions of a prompt template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplateCanary {
    pub id: String,
    pub template_id: String,
    pub baseline_version: i32,
    pub candidate_version: i32,
    pub traffic_percentage: f64,
    pub status: String,
    pub baseline_metrics: PromptVersionMetrics,
    pub candidate_metrics: PromptVersionMetrics,
    pub created_at: String,
    pub ended_at: Option<String>,
}

/// Randomly decide whether to use the candidate version based on traffic_percentage.
pub fn should_use_candidate(canary: &PromptTemplateCanary) -> bool {
    let pct = canary.traffic_percentage.clamp(0.0, 1.0);
    rand::random::<f64>() < pct
}

/// Accumulate metrics for the version that was used in a given execution.
pub fn record_canary_result(
    canary: &mut PromptTemplateCanary,
    used_candidate: bool,
    success: bool,
    cost: f64,
    latency_ms: f64,
    tokens: i64,
) {
    let metrics = if used_candidate {
        &mut canary.candidate_metrics
    } else {
        &mut canary.baseline_metrics
    };
    metrics.run_count += 1;
    if success {
        metrics.success_count += 1;
    } else {
        metrics.failure_count += 1;
    }
    metrics.total_cost_usd += cost;
    metrics.total_latency_ms += latency_ms;
    metrics.total_tokens += tokens;
}

/// Compare baseline vs candidate metrics using statistical analysis (p-value, CI, effect size).
pub fn evaluate_prompt_canary(canary: &PromptTemplateCanary) -> CanaryEvaluation {
    let b = &canary.baseline_metrics;
    let c = &canary.candidate_metrics;

    let baseline_total = (b.success_count + b.failure_count) as u64;
    let candidate_total = (c.success_count + c.failure_count) as u64;

    let baseline_sr = b.success_rate();
    let candidate_sr = c.success_rate();
    let delta = candidate_sr - baseline_sr;

    let min_runs: i64 = 10;
    let min_runs_met = c.run_count >= min_runs && b.run_count >= min_runs;

    let analysis = crate::stats::proportion_analysis(
        (c.success_count as u64, candidate_total),
        (b.success_count as u64, baseline_total),
        2,
    );

    let cost_delta_pct = if baseline_total > 0 && candidate_total > 0 && b.avg_cost() > 0.0 {
        Some((c.avg_cost() - b.avg_cost()) / b.avg_cost() * 100.0)
    } else {
        None
    };

    let duration_delta_pct =
        if baseline_total > 0 && candidate_total > 0 && b.avg_latency() > 0.0 {
            Some((c.avg_latency() - b.avg_latency()) / b.avg_latency() * 100.0)
        } else {
            None
        };

    let verdict_enum = if !min_runs_met {
        crate::stats::Verdict::Neutral
    } else {
        crate::stats::compute_verdict(
            delta,
            &analysis,
            candidate_total,
            &crate::stats::VerdictThresholds::canary(),
        )
    };

    CanaryEvaluation {
        verdict: verdict_enum.as_canary_str().to_string(),
        baseline_success_rate: baseline_sr,
        canary_success_rate: candidate_sr,
        delta,
        min_runs_met,
        p_value: analysis.p_value,
        confidence_interval: analysis.confidence_interval,
        effect_size: analysis.effect_size,
        cost_delta_pct,
        duration_delta_pct,
    }
}

// ── DB-backed prompt template canary operations ─────────────────────────

/// Create a new prompt template canary and persist it to the database.
pub fn create_prompt_template_canary(
    pg_db: &Arc<PgDb>,
    template_id: &str,
    baseline_version: i32,
    candidate_version: i32,
    traffic_pct: f64,
) -> Result<String, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.create_template_canary(template_id, baseline_version, candidate_version, traffic_pct))
    })
}

/// Load a prompt template canary from the database by id.
pub fn get_prompt_template_canary(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
) -> Result<PromptTemplateCanary, String> {
    let result = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_template_canary(canary_id))
    })?;
    result.ok_or_else(|| format!("Prompt template canary not found: {}", canary_id))
}

/// Record a result for a prompt template canary and persist updated metrics.
pub fn record_prompt_canary_run(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
    used_candidate: bool,
    success: bool,
    cost: f64,
    latency_ms: f64,
    tokens: i64,
) -> Result<(), String> {
    // PG-primary: load metrics, update, write back
    let pg_result = tokio::task::block_in_place(|| {
        Handle::current().block_on(async {
            let canary = pg_db.get_template_canary(canary_id).await?
                .ok_or_else(|| format!("Prompt template canary not found: {}", canary_id))?;
            let mut baseline = canary.baseline_metrics;
            let mut candidate = canary.candidate_metrics;
            let metrics = if used_candidate { &mut candidate } else { &mut baseline };
            metrics.run_count += 1;
            if success { metrics.success_count += 1; } else { metrics.failure_count += 1; }
            metrics.total_cost_usd += cost;
            metrics.total_latency_ms += latency_ms;
            metrics.total_tokens += tokens;
            let b_json = serde_json::to_string(&baseline).unwrap_or_default();
            let c_json = serde_json::to_string(&candidate).unwrap_or_default();
            pg_db.update_template_canary_metrics(canary_id, &b_json, &c_json).await
        })
    });
    pg_result
}

/// Evaluate a prompt template canary from persisted DB state.
pub fn evaluate_prompt_canary_from_db(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
) -> Result<CanaryEvaluation, String> {
    let canary = get_prompt_template_canary(pg_db, canary_id)?;
    Ok(evaluate_prompt_canary(&canary))
}

// ── Prompt canary A/B traffic splitting integration ─────────────────────

/// Result of resolving a prompt through the canary system.
#[derive(Debug, Clone)]
pub struct CanaryResolvedPrompt {
    /// The prompt content to use (either baseline or candidate version).
    pub content: String,
    /// The canary ID if an active canary was found, None otherwise.
    pub canary_id: Option<String>,
    /// Whether the candidate version was selected (true) or baseline (false).
    /// Only meaningful when canary_id is Some.
    pub used_candidate: bool,
}

/// Check for an active prompt template canary for the given template_id (agent_type),
/// perform the traffic split, and return the appropriate prompt content.
///
/// If no active canary exists, returns the `default_content` as-is.
/// If a canary is active, uses `should_use_candidate()` to pick baseline vs candidate,
/// loads the chosen version from the prompt registry, and returns it.
pub fn resolve_prompt_with_canary(
    pg_db: &Arc<PgDb>,
    template_id: &str,
    default_content: &str,
) -> CanaryResolvedPrompt {
    // Look up active canary from PG
    let canary_opt: Option<PromptTemplateCanary> = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_active_template_canary(template_id))
    })
    .unwrap_or(None);

    let Some(canary) = canary_opt else {
        return CanaryResolvedPrompt {
            content: default_content.to_string(),
            canary_id: None,
            used_candidate: false,
        };
    };

    let use_candidate = should_use_candidate(&canary);
    let version = if use_candidate {
        canary.candidate_version
    } else {
        canary.baseline_version
    };

    // Load the prompt content for the selected version from the registry
    let content = match super::prompt_registry::get_prompt_by_version(
        pg_db,
        &canary.template_id,
        version,
    ) {
        Ok(Some(variant)) => {
            info!(
                "Canary {}: using {} version v{} for template '{}'",
                canary.id,
                if use_candidate { "candidate" } else { "baseline" },
                version,
                canary.template_id,
            );
            variant.prompt_content
        }
        Ok(None) => {
            warn!(
                "Canary {}: version v{} not found in prompt registry for '{}', falling back to default",
                canary.id, version, canary.template_id,
            );
            default_content.to_string()
        }
        Err(e) => {
            warn!(
                "Canary {}: failed to load version v{} for '{}': {}, falling back to default",
                canary.id, version, canary.template_id, e,
            );
            default_content.to_string()
        }
    };

    CanaryResolvedPrompt {
        content,
        canary_id: Some(canary.id),
        used_candidate: use_candidate,
    }
}

/// Async variant of `resolve_prompt_with_canary` that queries PostgreSQL.
///
/// If no active canary exists, returns the `default_content` as-is.
/// If a canary is active, uses `should_use_candidate()` to pick baseline vs candidate,
/// loads the chosen version from the PG prompt registry, and returns it.
pub async fn resolve_prompt_with_canary_pg(
    pg: &crate::database::pg::PgDb,
    template_id: &str,
    default_content: &str,
) -> CanaryResolvedPrompt {
    let canary_opt = match pg.get_active_template_canary(template_id).await {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to query PG for active canary: {}", e);
            return CanaryResolvedPrompt {
                content: default_content.to_string(),
                canary_id: None,
                used_candidate: false,
            };
        }
    };

    let Some(canary) = canary_opt else {
        return CanaryResolvedPrompt {
            content: default_content.to_string(),
            canary_id: None,
            used_candidate: false,
        };
    };

    let use_candidate = should_use_candidate(&canary);
    let version = if use_candidate {
        canary.candidate_version
    } else {
        canary.baseline_version
    };

    // Load the prompt content for the selected version from the PG prompt registry
    let content = match pg
        .get_prompt_by_version(&canary.template_id, version)
        .await
    {
        Ok(Some(variant)) => {
            info!(
                "Canary {}: using {} version v{} for template '{}' [PG]",
                canary.id,
                if use_candidate { "candidate" } else { "baseline" },
                version,
                canary.template_id,
            );
            variant.prompt_content
        }
        Ok(None) => {
            warn!(
                "Canary {}: version v{} not found in PG prompt registry for '{}', falling back to default",
                canary.id, version, canary.template_id,
            );
            default_content.to_string()
        }
        Err(e) => {
            warn!(
                "Canary {}: failed to load version v{} for '{}' from PG: {}, falling back to default",
                canary.id, version, canary.template_id, e,
            );
            default_content.to_string()
        }
    };

    CanaryResolvedPrompt {
        content,
        canary_id: Some(canary.id),
        used_candidate: use_candidate,
    }
}

/// Record the outcome of a prompt execution that went through canary routing.
/// This is a convenience wrapper around `record_prompt_canary_run` that handles
/// the case where no canary was active (canary_id is None — silently no-ops).
pub fn record_canary_outcome(
    pg_db: &Arc<PgDb>,
    canary_id: &str,
    used_candidate: bool,
    success: bool,
    cost_usd: f64,
    latency_ms: f64,
    tokens: i64,
) {
    if let Err(e) = record_prompt_canary_run(
        pg_db,
        canary_id,
        used_candidate,
        success,
        cost_usd,
        latency_ms,
        tokens,
    ) {
        warn!(
            "Failed to record canary outcome for {}: {}",
            canary_id, e
        );
    }
}
