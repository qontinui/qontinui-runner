//! Model capability profiling.
//!
//! Builds model scorecards from historical `phase_token_usage` and `learning_outcomes`
//! data, enabling cost-per-quality optimization and model comparison.

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::database::pg::PgDb;

// =============================================================================
// Types
// =============================================================================

/// Performance profile for a specific AI model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfile {
    pub model_id: String,
    /// Overall pass rate across all runs using this model.
    pub pass_rate: f64,
    /// Average iterations per run.
    pub mean_iterations: f64,
    /// Average duration per run (ms).
    pub mean_duration_ms: f64,
    /// Average cost per run (USD).
    pub avg_cost_per_run_usd: f64,
    /// Cost per successful run (total_cost / successful_runs). Inf if no successes.
    pub cost_per_success_usd: f64,
    /// Cost efficiency score: pass_rate / avg_cost (higher = better bang for buck).
    pub cost_efficiency_score: f64,
    /// Number of runs analyzed.
    pub trial_count: u32,
    /// When this profile was last computed.
    pub last_updated: String,
    /// Average prompt cache hit rate (0.0-1.0) for this model.
    pub avg_cache_hit_rate: f64,
    /// Average USD saved per run through prompt caching.
    pub avg_cache_savings_usd: f64,
    /// Performance breakdown by task type (empty until task categorization data exists).
    pub performance_by_task_type: Vec<TaskTypePerformance>,
}

/// Performance metrics for a specific task type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTypePerformance {
    pub task_type: String,
    pub pass_rate: f64,
    pub trial_count: u32,
}

/// Recommendation for which model to use, optionally constrained by budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRecommendation {
    pub model_id: String,
    pub reason: String,
    pub pass_rate: f64,
    pub cost_efficiency_score: f64,
    pub avg_cost_per_run_usd: f64,
}

// =============================================================================
// Profile computation
// =============================================================================

/// Build a model profile from historical data.
///
/// Joins `phase_token_usage` (for model and cost) with `learning_outcomes`
/// (for success/failure) over the specified number of days.
pub async fn build_model_profile(
    pg: &PgDb,
    model_id: &str,
    days: i64,
) -> Result<Option<ModelProfile>, String> {
    let period_start = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    let data = pg.build_model_profile_data(model_id, &period_start).await?;

    let (run_count, success_count, avg_iterations, avg_duration_ms, avg_cost) = match data {
        Some(d) => d,
        None => return Ok(None),
    };

    if run_count == 0 {
        return Ok(None);
    }

    let pass_rate = success_count as f64 / run_count as f64;
    let cost_per_success = if success_count > 0 {
        (avg_cost * run_count as f64) / success_count as f64
    } else {
        f64::INFINITY
    };
    // Populate cache metrics from phase_token_usage cache columns.
    let (avg_cache_hit_rate, avg_cache_savings_usd) = pg
        .get_model_cache_stats(model_id, &period_start)
        .await
        .unwrap_or((0.0, 0.0));

    // Cost efficiency factors in cache reuse: models that achieve high cache hit
    // rates get a boost because effective cost is lower than nominal cost.
    // cost_efficiency = pass_rate / (avg_cost * (1.0 - avg_cache_hit_rate * 0.5))
    let cost_efficiency = if avg_cost > 0.0 {
        let effective_cost = avg_cost * (1.0 - avg_cache_hit_rate * 0.5);
        if effective_cost > 0.0 {
            pass_rate / effective_cost
        } else {
            pass_rate / avg_cost
        }
    } else {
        0.0
    };

    Ok(Some(ModelProfile {
        model_id: model_id.to_string(),
        pass_rate,
        mean_iterations: avg_iterations,
        mean_duration_ms: avg_duration_ms,
        avg_cost_per_run_usd: avg_cost,
        cost_per_success_usd: cost_per_success,
        cost_efficiency_score: cost_efficiency,
        trial_count: run_count as u32,
        last_updated: chrono::Utc::now().to_rfc3339(),
        avg_cache_hit_rate,
        avg_cache_savings_usd,
        performance_by_task_type: Vec::new(),
    }))
}

/// Build profiles for all models seen in the last N days.
pub async fn build_all_profiles(pg: &PgDb, days: i64) -> Result<Vec<ModelProfile>, String> {
    let period_start = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    let models = pg.get_distinct_models(&period_start).await?;

    let mut profiles = Vec::new();
    for model_id in &models {
        if let Some(profile) = build_model_profile(pg, model_id, days).await? {
            profiles.push(profile);
        }
    }

    // Sort by cost efficiency (best first)
    profiles.sort_by(|a, b| {
        b.cost_efficiency_score
            .partial_cmp(&a.cost_efficiency_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(profiles)
}

/// Get model recommendations, optionally constrained by budget.
pub async fn get_model_recommendation(
    pg: &PgDb,
    budget_constraint_usd: Option<f64>,
) -> Result<Vec<ModelRecommendation>, String> {
    let profiles = build_all_profiles(pg, 30).await?;

    let mut recommendations: Vec<ModelRecommendation> = profiles
        .into_iter()
        .filter(|p| p.trial_count >= 3) // Need at least 3 runs for a recommendation
        .filter(|p| {
            budget_constraint_usd
                .map(|b| p.avg_cost_per_run_usd <= b)
                .unwrap_or(true)
        })
        .map(|p| {
            let reason = if p.cost_efficiency_score > 0.0 {
                format!(
                    "{:.0}% pass rate at ${:.3}/run ({} runs). Cost efficiency: {:.1}",
                    p.pass_rate * 100.0,
                    p.avg_cost_per_run_usd,
                    p.trial_count,
                    p.cost_efficiency_score
                )
            } else {
                format!(
                    "{:.0}% pass rate ({} runs), no cost data",
                    p.pass_rate * 100.0,
                    p.trial_count
                )
            };

            ModelRecommendation {
                model_id: p.model_id,
                reason,
                pass_rate: p.pass_rate,
                cost_efficiency_score: p.cost_efficiency_score,
                avg_cost_per_run_usd: p.avg_cost_per_run_usd,
            }
        })
        .collect();

    // Sort by cost efficiency
    recommendations.sort_by(|a, b| {
        b.cost_efficiency_score
            .partial_cmp(&a.cost_efficiency_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(recommendations)
}

// =============================================================================
// Database persistence
// =============================================================================

/// Save a model profile to the database.
pub async fn save_model_profile(pg: &PgDb, profile: &ModelProfile) -> Result<(), String> {
    let id = format!("mp-{}", uuid::Uuid::new_v4());
    let profile_json = serde_json::to_string(profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;
    let trial_count = profile.trial_count as i64;

    pg.save_model_profile(
        &id,
        &profile.model_id,
        &profile_json,
        trial_count,
        &profile.last_updated,
    )
    .await
}

/// Load all saved model profiles.
pub async fn list_model_profiles(pg: &PgDb) -> Result<Vec<ModelProfile>, String> {
    let jsons = pg.list_model_profiles().await?;
    Ok(jsons
        .into_iter()
        .filter_map(|json| serde_json::from_str(&json).ok())
        .collect())
}

/// Refresh all model profiles (rebuild from historical data and persist).
pub async fn refresh_all_profiles(pg: &PgDb, days: i64) -> Result<Vec<ModelProfile>, String> {
    let profiles = build_all_profiles(pg, days).await?;

    for profile in &profiles {
        save_model_profile(pg, profile).await?;
    }

    info!("Refreshed {} model profiles", profiles.len());
    Ok(profiles)
}
