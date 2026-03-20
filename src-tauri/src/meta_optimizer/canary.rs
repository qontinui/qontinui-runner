//! Canary rollout system for meta-optimizer recommendations.
//!
//! Allows testing a recommendation on a percentage of runs before full rollout.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::database::CheckpointDb;

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
}

/// Start a canary rollout for a recommendation.
pub fn start_canary(
    db: &CheckpointDb,
    recommendation_id: &str,
    percentage: i64,
) -> Result<String, String> {
    let id = format!("canary-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let rec_id = recommendation_id.to_string();
    let id_clone = id.clone();
    let percentage = percentage.clamp(1, 100);

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO canary_rollouts
               (id, recommendation_id, percentage, status, start_date,
                baseline_run_count, canary_run_count,
                baseline_metrics_json, canary_metrics_json, created_at)
               VALUES (?1, ?2, ?3, 'active', ?4, 0, 0, '{}', '{}', ?4)"#,
            params![id_clone, rec_id, percentage, now],
        )
        .map_err(|e| format!("Failed to create canary rollout: {}", e))?;

        // Update recommendation status to "canary"
        conn.execute(
            "UPDATE meta_optimizer_recommendations SET status = 'canary' WHERE id = ?1 AND status = 'pending'",
            params![rec_id],
        )
        .map_err(|e| format!("Failed to update recommendation status: {}", e))?;

        info!("Started canary rollout {} for recommendation {} at {}%", id_clone, rec_id, percentage);
        Ok(())
    })?;

    Ok(id)
}

/// Get all active canary rollouts.
pub fn get_active_canaries(db: &CheckpointDb) -> Result<Vec<CanaryRollout>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT id, recommendation_id, percentage, status, start_date, end_date,
                          baseline_run_count, canary_run_count,
                          baseline_metrics_json, canary_metrics_json, created_at
                   FROM canary_rollouts
                   WHERE status = 'active'
                   ORDER BY created_at DESC"#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results = stmt
            .query_map([], |row| {
                Ok(CanaryRollout {
                    id: row.get(0)?,
                    recommendation_id: row.get(1)?,
                    percentage: row.get(2)?,
                    status: row.get(3)?,
                    start_date: row.get(4)?,
                    end_date: row.get(5)?,
                    baseline_run_count: row.get(6)?,
                    canary_run_count: row.get(7)?,
                    baseline_metrics_json: row.get(8)?,
                    canary_metrics_json: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })
            .map_err(|e| format!("Failed to query canaries: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    })
}

/// Get the canary prompt overrides for a recommendation.
/// Returns a map of agent_type → prompt_content if the recommendation is a prompt_rewrite,
/// or an empty map otherwise. This is used to inject the canary prompt during pipeline execution.
pub fn get_canary_prompt_overrides(
    db: &CheckpointDb,
    recommendation_id: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let rec_id = recommendation_id.to_string();
    let mut overrides = std::collections::HashMap::new();

    db.with_conn(move |conn| {
        let (rec_type, recommended_value, target_agent): (String, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT recommendation_type, recommended_value, target_agent FROM meta_optimizer_recommendations WHERE id = ?1",
                params![rec_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| format!("Recommendation not found: {}", e))?;

        if rec_type == "prompt_rewrite" {
            if let Some(ref val) = recommended_value {
                // Parse the prompt_rewrite payload: { agent_type, variant_name, prompt_content }
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(val) {
                    if let (Some(agent), Some(content)) = (
                        payload.get("agent_type").and_then(|v| v.as_str()),
                        payload.get("prompt_content").and_then(|v| v.as_str()),
                    ) {
                        overrides.insert(agent.to_string(), content.to_string());
                    }
                }
            }
        } else if rec_type == "config_change" {
            // Config changes could target a specific agent — store the target for reference
            // but config changes are applied differently (via settings), not prompt injection.
            // No prompt override needed.
        }

        // If no payload-based override, check if there's a prompt variant created by this recommendation
        if overrides.is_empty() {
            if let Some(agent) = target_agent {
                let variant_content: Option<String> = conn.query_row(
                    "SELECT prompt_content FROM prompt_registry WHERE source_recommendation_id = ?1 ORDER BY version DESC LIMIT 1",
                    params![rec_id],
                    |row| row.get(0),
                ).ok();

                if let Some(content) = variant_content {
                    overrides.insert(agent, content);
                }
            }
        }

        Ok(overrides)
    })
}

/// Probabilistic check: should this run use the canary config?
pub fn should_apply_canary(db: &CheckpointDb, recommendation_id: &str) -> bool {
    let rec_id = recommendation_id.to_string();
    db.with_conn(move |conn| {
        let percentage: i64 = conn
            .query_row(
                "SELECT percentage FROM canary_rollouts WHERE recommendation_id = ?1 AND status = 'active' LIMIT 1",
                params![rec_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if percentage <= 0 {
            return Ok(false);
        }

        let roll: f64 = rand::random::<f64>() * 100.0;
        Ok(roll < percentage as f64)
    })
    .unwrap_or(false)
}

/// Record a completed run as either baseline or canary.
pub fn record_canary_run(
    db: &CheckpointDb,
    canary_id: &str,
    is_canary: bool,
    success: bool,
    cost: f64,
    duration_ms: f64,
) -> Result<(), String> {
    let canary_id = canary_id.to_string();

    db.with_conn(move |conn| {
        // Load current metrics
        let (baseline_json, canary_json): (String, String) = conn
            .query_row(
                "SELECT baseline_metrics_json, canary_metrics_json FROM canary_rollouts WHERE id = ?1",
                params![canary_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Canary not found: {}", e))?;

        let mut baseline: CanaryMetrics =
            serde_json::from_str(&baseline_json).unwrap_or_default();
        let mut canary: CanaryMetrics =
            serde_json::from_str(&canary_json).unwrap_or_default();

        let metrics = if is_canary { &mut canary } else { &mut baseline };
        if success {
            metrics.success_count += 1;
        } else {
            metrics.failure_count += 1;
        }
        metrics.total_cost_usd += cost;
        metrics.total_duration_ms += duration_ms;

        let count_field = if is_canary { "canary_run_count" } else { "baseline_run_count" };
        let baseline_json = serde_json::to_string(&baseline).unwrap_or_default();
        let canary_json = serde_json::to_string(&canary).unwrap_or_default();

        conn.execute(
            &format!(
                "UPDATE canary_rollouts SET {} = {} + 1, baseline_metrics_json = ?1, canary_metrics_json = ?2 WHERE id = ?3",
                count_field, count_field
            ),
            params![baseline_json, canary_json, canary_id],
        )
        .map_err(|e| format!("Failed to record canary run: {}", e))?;

        Ok(())
    })
}

/// Evaluate a canary: should it be promoted, rolled back, or continue?
pub fn evaluate_canary(db: &CheckpointDb, canary_id: &str) -> Result<CanaryEvaluation, String> {
    let canary_id = canary_id.to_string();

    db.with_conn(move |conn| {
        let (baseline_json, canary_json, canary_count): (String, String, i64) = conn
            .query_row(
                "SELECT baseline_metrics_json, canary_metrics_json, canary_run_count FROM canary_rollouts WHERE id = ?1",
                params![canary_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| format!("Canary not found: {}", e))?;

        let baseline: CanaryMetrics = serde_json::from_str(&baseline_json).unwrap_or_default();
        let canary: CanaryMetrics = serde_json::from_str(&canary_json).unwrap_or_default();

        let min_runs = 20;
        let min_runs_met = canary_count >= min_runs;

        let baseline_total = baseline.success_count + baseline.failure_count;
        let canary_total = canary.success_count + canary.failure_count;

        let baseline_sr = if baseline_total > 0 {
            baseline.success_count as f64 / baseline_total as f64 * 100.0
        } else {
            0.0
        };
        let canary_sr = if canary_total > 0 {
            canary.success_count as f64 / canary_total as f64 * 100.0
        } else {
            0.0
        };

        let delta = canary_sr - baseline_sr;

        let verdict = if !min_runs_met {
            "continue"
        } else if delta < -5.0 {
            "rollback"
        } else if delta > -2.0 {
            "promote"
        } else {
            "continue"
        };

        Ok(CanaryEvaluation {
            verdict: verdict.to_string(),
            baseline_success_rate: baseline_sr,
            canary_success_rate: canary_sr,
            delta,
            min_runs_met,
        })
    })
}

/// Promote a canary: apply the recommendation globally.
pub fn promote_canary(db: &CheckpointDb, canary_id: &str) -> Result<(), String> {
    let canary_id_str = canary_id.to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // Get recommendation_id
    let rec_id: String = db.with_conn({
        let canary_id = canary_id_str.clone();
        move |conn| {
            conn.query_row(
                "SELECT recommendation_id FROM canary_rollouts WHERE id = ?1",
                params![canary_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Canary not found: {}", e))
        }
    })?;

    // Apply the recommendation fully
    super::recommendations::apply_recommendation_with_side_effects(db, &rec_id)?;

    // Update canary status
    db.with_conn({
        let canary_id = canary_id_str;
        move |conn| {
            conn.execute(
                "UPDATE canary_rollouts SET status = 'promoted', end_date = ?1 WHERE id = ?2",
                params![now, canary_id],
            )
            .map_err(|e| format!("Failed to promote canary: {}", e))?;
            Ok(())
        }
    })?;

    info!("Promoted canary {} (recommendation {})", canary_id, rec_id);
    Ok(())
}

/// Roll back a canary: revert to pending state.
pub fn rollback_canary(db: &CheckpointDb, canary_id: &str) -> Result<(), String> {
    let canary_id_str = canary_id.to_string();
    let now = chrono::Utc::now().to_rfc3339();

    db.with_conn({
        let canary_id = canary_id_str.clone();
        move |conn| {
            // Get recommendation_id
            let rec_id: String = conn
                .query_row(
                    "SELECT recommendation_id FROM canary_rollouts WHERE id = ?1",
                    params![canary_id],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Canary not found: {}", e))?;

            // Update canary status
            conn.execute(
                "UPDATE canary_rollouts SET status = 'rolled_back', end_date = ?1 WHERE id = ?2",
                params![now, canary_id],
            )
            .map_err(|e| format!("Failed to rollback canary: {}", e))?;

            // Revert recommendation to pending
            conn.execute(
                "UPDATE meta_optimizer_recommendations SET status = 'pending' WHERE id = ?1",
                params![rec_id],
            )
            .map_err(|e| format!("Failed to revert recommendation: {}", e))?;

            info!("Rolled back canary {} (recommendation {})", canary_id, rec_id);
            Ok(())
        }
    })
}
