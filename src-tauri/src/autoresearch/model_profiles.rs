//! Model capability profiling for autoresearch.
//!
//! Builds model scorecards from historical `phase_token_usage` and `learning_outcomes`
//! data, enabling cost-per-quality optimization and model comparison.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::database::CheckpointDb;

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
pub fn build_model_profile(
    db: &CheckpointDb,
    model_id: &str,
    days: i64,
) -> Result<Option<ModelProfile>, String> {
    let model = model_id.to_string();
    let period_start = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    db.with_conn(move |conn| {
        // Get runs that used this model (via phase_token_usage) and their outcomes
        let result = conn.query_row(
            r#"SELECT
                   COUNT(DISTINCT ptu.task_run_id) as run_count,
                   COUNT(DISTINCT CASE WHEN lo.status = 'success' THEN ptu.task_run_id END) as success_count,
                   COALESCE(AVG(lo.iterations), 0) as avg_iterations,
                   COALESCE(AVG(lo.duration_secs * 1000), 0) as avg_duration_ms
               FROM phase_token_usage ptu
               JOIN learning_outcomes lo ON ptu.task_run_id = lo.task_id
               WHERE ptu.model_used = ?1
                 AND ptu.created_at > ?2"#,
            params![model, period_start],
            |row| {
                let run_count: i64 = row.get(0)?;
                let success_count: i64 = row.get(1)?;
                let avg_iterations: f64 = row.get(2)?;
                let avg_duration_ms: f64 = row.get(3)?;
                Ok((run_count, success_count, avg_iterations, avg_duration_ms))
            },
        );

        let (run_count, success_count, avg_iterations, avg_duration_ms) = match result {
            Ok(r) => r,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(format!("Failed to query model data: {}", e)),
        };

        if run_count == 0 {
            return Ok(None);
        }

        // Get average cost per run for this model
        let avg_cost: f64 = conn
            .query_row(
                r#"SELECT COALESCE(AVG(run_cost), 0) FROM (
                       SELECT ptu.task_run_id, SUM(ptu.cost_cents) / 100.0 as run_cost
                       FROM phase_token_usage ptu
                       WHERE ptu.model_used = ?1 AND ptu.created_at > ?2
                       GROUP BY ptu.task_run_id
                   )"#,
                params![model, period_start],
                |row| row.get(0),
            )
            .unwrap_or(0.0);

        let pass_rate = success_count as f64 / run_count as f64;
        let cost_per_success = if success_count > 0 {
            (avg_cost * run_count as f64) / success_count as f64
        } else {
            f64::INFINITY
        };
        let cost_efficiency = if avg_cost > 0.0 {
            pass_rate / avg_cost
        } else {
            0.0
        };

        Ok(Some(ModelProfile {
            model_id: model,
            pass_rate,
            mean_iterations: avg_iterations,
            mean_duration_ms: avg_duration_ms,
            avg_cost_per_run_usd: avg_cost,
            cost_per_success_usd: cost_per_success,
            cost_efficiency_score: cost_efficiency,
            trial_count: run_count as u32,
            last_updated: chrono::Utc::now().to_rfc3339(),
        }))
    })
}

/// Build profiles for all models seen in the last N days.
pub fn build_all_profiles(db: &CheckpointDb, days: i64) -> Result<Vec<ModelProfile>, String> {
    let period_start = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    // Get distinct models
    let models: Vec<String> = db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT DISTINCT model_used FROM phase_token_usage
                   WHERE model_used IS NOT NULL AND created_at > ?1"#,
            )
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows: Vec<String> = stmt
            .query_map(params![period_start], |row| row.get(0))
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    })?;

    let mut profiles = Vec::new();
    for model_id in &models {
        if let Some(profile) = build_model_profile(db, model_id, days)? {
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
pub fn get_model_recommendation(
    db: &CheckpointDb,
    budget_constraint_usd: Option<f64>,
) -> Result<Vec<ModelRecommendation>, String> {
    let profiles = build_all_profiles(db, 30)?;

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
pub fn save_model_profile(db: &CheckpointDb, profile: &ModelProfile) -> Result<(), String> {
    let id = format!("mp-{}", uuid::Uuid::new_v4());
    let model_id = profile.model_id.clone();
    let profile_json = serde_json::to_string(profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;
    let trial_count = profile.trial_count as i64;
    let last_updated = profile.last_updated.clone();

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO model_profiles (id, model_id, profile_json, trial_count, last_updated)
               VALUES (?1, ?2, ?3, ?4, ?5)
               ON CONFLICT(model_id) DO UPDATE SET
                   profile_json = excluded.profile_json,
                   trial_count = excluded.trial_count,
                   last_updated = excluded.last_updated"#,
            params![id, model_id, profile_json, trial_count, last_updated],
        )
        .map_err(|e| format!("Failed to save model profile: {}", e))?;

        info!(
            "Saved model profile for {} ({} trials)",
            model_id, trial_count
        );
        Ok(())
    })
}

/// Load all saved model profiles.
pub fn list_model_profiles(db: &CheckpointDb) -> Result<Vec<ModelProfile>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn
            .prepare("SELECT profile_json FROM model_profiles ORDER BY trial_count DESC")
            .map_err(|e| format!("Failed to prepare: {}", e))?;

        let rows: Vec<ModelProfile> = stmt
            .query_map([], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            })
            .map_err(|e| format!("Failed to query: {}", e))?
            .filter_map(|r| r.ok())
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect();

        Ok(rows)
    })
}

/// Refresh all model profiles (rebuild from historical data and persist).
pub fn refresh_all_profiles(db: &CheckpointDb, days: i64) -> Result<Vec<ModelProfile>, String> {
    let profiles = build_all_profiles(db, days)?;

    for profile in &profiles {
        save_model_profile(db, profile)?;
    }

    info!("Refreshed {} model profiles", profiles.len());
    Ok(profiles)
}
