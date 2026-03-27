//! Canary rollout system for meta-optimizer recommendations.
//!
//! Allows testing a recommendation on a percentage of runs before full rollout.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

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

/// Update the traffic percentage for an active canary rollout.
///
/// Used to extend canary evaluation by increasing traffic when results are
/// inconclusive after many runs.
pub fn update_canary_percentage(
    db: &CheckpointDb,
    canary_id: &str,
    new_percentage: i64,
) -> Result<(), String> {
    let canary_id = canary_id.to_string();
    let pct = new_percentage.clamp(1, 100);

    db.with_conn(move |conn| {
        conn.execute(
            "UPDATE canary_rollouts SET percentage = ?1 WHERE id = ?2 AND status = 'active'",
            params![pct, canary_id],
        )
        .map_err(|e| format!("Failed to update canary percentage: {}", e))?;
        Ok(())
    })
}

/// Get completed canary rollouts (promoted or rolled back) for history display.
pub fn get_canary_history(db: &CheckpointDb, limit: u32) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.min(100) as i64;
    db.with_conn(move |conn| {
        let mut stmt = conn
            .prepare(
                r#"SELECT c.id, c.recommendation_id, c.percentage, c.status,
                          c.start_date, c.end_date,
                          c.baseline_run_count, c.canary_run_count,
                          c.baseline_metrics_json, c.canary_metrics_json, c.created_at,
                          r.title, r.target_agent, r.recommendation_type
                   FROM canary_rollouts c
                   LEFT JOIN meta_optimizer_recommendations r ON r.id = c.recommendation_id
                   WHERE c.status IN ('promoted', 'rolled_back')
                   ORDER BY c.end_date DESC
                   LIMIT ?1"#,
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let results: Vec<serde_json::Value> = stmt
            .query_map(params![limit], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "recommendation_id": row.get::<_, String>(1)?,
                    "percentage": row.get::<_, i64>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "start_date": row.get::<_, String>(4)?,
                    "end_date": row.get::<_, Option<String>>(5)?,
                    "baseline_run_count": row.get::<_, i64>(6)?,
                    "canary_run_count": row.get::<_, i64>(7)?,
                    "baseline_metrics_json": row.get::<_, String>(8)?,
                    "canary_metrics_json": row.get::<_, String>(9)?,
                    "created_at": row.get::<_, String>(10)?,
                    "recommendation_title": row.get::<_, Option<String>>(11)?,
                    "target_agent": row.get::<_, Option<String>>(12)?,
                    "recommendation_type": row.get::<_, Option<String>>(13)?,
                }))
            })
            .map_err(|e| format!("Failed to query canary history: {}", e))?
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

/// Get config overrides for a canary rollout of a config_change recommendation.
/// Returns a vec of (key, serde_json::Value) pairs to apply as temporary settings during canary runs.
pub fn get_canary_config_overrides(
    db: &CheckpointDb,
    recommendation_id: &str,
) -> Result<Vec<(String, serde_json::Value)>, String> {
    let rec_id = recommendation_id.to_string();
    db.with_conn(move |conn| {
        let (rec_type, recommended_value): (String, Option<String>) = conn
            .query_row(
                "SELECT recommendation_type, recommended_value FROM meta_optimizer_recommendations WHERE id = ?1",
                params![rec_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Recommendation not found: {}", e))?;

        if rec_type != "config_change" {
            return Ok(Vec::new());
        }

        let Some(val) = recommended_value else {
            return Ok(Vec::new());
        };

        // Parse ConfigChangePayload: { key, value }
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&val) {
            if let (Some(key), Some(value)) = (
                payload.get("key").and_then(|v| v.as_str()),
                payload.get("value").cloned(),
            ) {
                return Ok(vec![(key.to_string(), value)]);
            }
        }

        Ok(Vec::new())
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

        // Best-effort: insert a per-run record into canary_run_records
        let record_id = format!("crr-{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();
        if let Err(e) = conn.execute(
            r#"INSERT INTO canary_run_records
               (id, canary_id, is_canary, task_run_id, success, cost_usd, duration_ms, created_at)
               VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)"#,
            params![
                record_id,
                canary_id,
                is_canary as i32,
                success as i32,
                cost,
                duration_ms,
                created_at,
            ],
        ) {
            warn!("Failed to insert canary_run_record: {}", e);
        }

        Ok(())
    })
}

/// Evaluate a canary: should it be promoted, rolled back, or continue?
///
/// Uses statistical tests (proportion z-test, confidence intervals, effect size)
/// instead of simple threshold-based verdicts.
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

        let min_runs = 10;
        let min_runs_met = canary_count >= min_runs;

        let baseline_total = (baseline.success_count + baseline.failure_count) as u64;
        let canary_total = (canary.success_count + canary.failure_count) as u64;

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

        // Statistical analysis (requires at least 2 runs per group)
        let analysis = crate::stats::proportion_analysis(
            (canary.success_count as u64, canary_total),
            (baseline.success_count as u64, baseline_total),
            2,
        );
        let p_value = analysis.p_value;
        let confidence_interval = analysis.confidence_interval;
        let effect_size = analysis.effect_size;

        // Cost and duration deltas
        let cost_delta_pct = if baseline_total > 0 && canary_total > 0 {
            let baseline_avg_cost = baseline.total_cost_usd / baseline_total as f64;
            let canary_avg_cost = canary.total_cost_usd / canary_total as f64;
            if baseline_avg_cost > 0.0 {
                Some((canary_avg_cost - baseline_avg_cost) / baseline_avg_cost * 100.0)
            } else {
                None
            }
        } else {
            None
        };

        let duration_delta_pct = if baseline_total > 0 && canary_total > 0 {
            let baseline_avg_dur = baseline.total_duration_ms / baseline_total as f64;
            let canary_avg_dur = canary.total_duration_ms / canary_total as f64;
            if baseline_avg_dur > 0.0 {
                Some((canary_avg_dur - baseline_avg_dur) / baseline_avg_dur * 100.0)
            } else {
                None
            }
        } else {
            None
        };

        // Verdict using shared statistical verdict with canary thresholds
        let verdict_enum = if !min_runs_met {
            crate::stats::Verdict::Neutral // will map to "continue"
        } else {
            crate::stats::compute_verdict(
                delta,
                &analysis,
                canary_total,
                &crate::stats::VerdictThresholds::canary(),
            )
        };
        let verdict = verdict_enum.as_canary_str();

        Ok(CanaryEvaluation {
            verdict: verdict.to_string(),
            baseline_success_rate: baseline_sr,
            canary_success_rate: canary_sr,
            delta,
            min_runs_met,
            p_value,
            confidence_interval,
            effect_size,
            cost_delta_pct,
            duration_delta_pct,
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

    // Update prompt evolution verdict if this was a meta-prompt rewrite canary
    if let Err(e) = update_evolution_for_recommendation(db, &rec_id, "adopt", None) {
        debug!("No prompt evolution entry for recommendation {}: {}", rec_id, e);
    }

    info!("Promoted canary {} (recommendation {})", canary_id, rec_id);
    Ok(())
}

/// Roll back a canary: mark recommendation as rolled_back and record evaluation metrics.
///
/// Unlike the previous behavior (reverting to "pending"), this sets the recommendation
/// status to "rolled_back" so it is not re-attempted, and records the canary evaluation
/// metrics that triggered the rollback decision.
pub fn rollback_canary(db: &CheckpointDb, canary_id: &str) -> Result<(), String> {
    rollback_canary_with_eval(db, canary_id, None)
}

/// Roll back a canary with optional evaluation metrics to record on the recommendation.
pub fn rollback_canary_with_eval(
    db: &CheckpointDb,
    canary_id: &str,
    eval: Option<&CanaryEvaluation>,
) -> Result<(), String> {
    let canary_id_str = canary_id.to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let eval_json = eval
        .and_then(|e| serde_json::to_string(e).ok())
        .unwrap_or_else(|| "{}".to_string());

    // Get rec_id outside the closure so we can use it for evolution update
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

    db.with_conn({
        let canary_id = canary_id_str.clone();
        let rec_id = rec_id.clone();
        move |conn| {
            // Update canary status
            conn.execute(
                "UPDATE canary_rollouts SET status = 'rolled_back', end_date = ?1 WHERE id = ?2",
                params![now, canary_id],
            )
            .map_err(|e| format!("Failed to rollback canary: {}", e))?;

            // Mark recommendation as rolled_back (not pending) and record eval metrics
            conn.execute(
                "UPDATE meta_optimizer_recommendations SET status = 'rolled_back', outcome_after_apply = ?1 WHERE id = ?2",
                params![eval_json, rec_id],
            )
            .map_err(|e| format!("Failed to update recommendation status: {}", e))?;

            info!(
                "Rolled back canary {} (recommendation {} -> rolled_back)",
                canary_id, rec_id
            );
            Ok(())
        }
    })?;

    // Compute canary success rate as score_after for evolution tracking
    let score_after = eval.map(|e| e.canary_success_rate / 100.0);

    // Update prompt evolution verdict if this was a meta-prompt rewrite canary
    if let Err(e) = update_evolution_for_recommendation(db, &rec_id, "reject", score_after) {
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
    db: &CheckpointDb,
    recommendation_id: &str,
    verdict: &str,
    score_after: Option<f64>,
) -> Result<(), String> {
    let rec_id = recommendation_id.to_string();
    let verdict_str = verdict.to_string();

    db.with_conn(move |conn| {
        let updated = conn
            .execute(
                "UPDATE prompt_evolution SET canary_verdict = ?1, score_after = ?2 WHERE recommendation_id = ?3 AND canary_verdict IS NULL",
                params![verdict_str, score_after, rec_id],
            )
            .map_err(|e| format!("Failed to update evolution verdict: {}", e))?;

        if updated > 0 {
            info!(
                "Updated prompt evolution verdict for recommendation {}: {}",
                rec_id, verdict_str
            );
        }
        Ok(())
    })
}

/// Auto-rollback canary rollouts that have been active for more than 30 days
/// without reaching a promote/rollback verdict. Stale canaries block the optimizer
/// from generating fresh recommendations for the same target.
pub fn auto_rollback_stale_canaries(db: &CheckpointDb) -> usize {
    let canaries = match get_active_canaries(db) {
        Ok(c) => c,
        Err(_) => return 0,
    };

    let cutoff = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
    let mut rolled_back = 0;

    for canary in &canaries {
        // Only rollback canaries started more than 30 days ago
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
        if let Err(e) = rollback_canary(db, &canary.id) {
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
    db: &CheckpointDb,
    template_id: &str,
    baseline_version: i32,
    candidate_version: i32,
    traffic_pct: f64,
) -> Result<String, String> {
    let id = format!("ptc-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let traffic_pct = traffic_pct.clamp(0.0, 1.0);
    let empty_metrics = serde_json::to_string(&PromptVersionMetrics::default())
        .unwrap_or_else(|_| "{}".to_string());

    let id_clone = id.clone();
    let template_id = template_id.to_string();

    db.with_conn(move |conn| {
        conn.execute(
            r#"INSERT INTO prompt_template_canaries
               (id, template_id, baseline_version, candidate_version,
                traffic_percentage, status,
                baseline_metrics_json, candidate_metrics_json,
                created_at)
               VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6, ?7)"#,
            params![
                id_clone,
                template_id,
                baseline_version,
                candidate_version,
                traffic_pct,
                empty_metrics,
                now,
            ],
        )
        .map_err(|e| format!("Failed to create prompt template canary: {}", e))?;

        info!(
            "Created prompt template canary {} for template {} (v{} vs v{}, {}% candidate traffic)",
            id_clone,
            template_id,
            baseline_version,
            candidate_version,
            (traffic_pct * 100.0) as i64,
        );
        Ok(())
    })?;

    Ok(id)
}

/// Load a prompt template canary from the database by id.
pub fn get_prompt_template_canary(
    db: &CheckpointDb,
    canary_id: &str,
) -> Result<PromptTemplateCanary, String> {
    let canary_id = canary_id.to_string();
    db.with_conn(move |conn| {
        conn.query_row(
            r#"SELECT id, template_id, baseline_version, candidate_version,
                      traffic_percentage, status,
                      baseline_metrics_json, candidate_metrics_json,
                      created_at, ended_at
               FROM prompt_template_canaries WHERE id = ?1"#,
            params![canary_id],
            |row| {
                let baseline_json: String = row.get(6)?;
                let candidate_json: String = row.get(7)?;
                Ok(PromptTemplateCanary {
                    id: row.get(0)?,
                    template_id: row.get(1)?,
                    baseline_version: row.get(2)?,
                    candidate_version: row.get(3)?,
                    traffic_percentage: row.get(4)?,
                    status: row.get(5)?,
                    baseline_metrics: serde_json::from_str(&baseline_json).unwrap_or_default(),
                    candidate_metrics: serde_json::from_str(&candidate_json).unwrap_or_default(),
                    created_at: row.get(8)?,
                    ended_at: row.get(9)?,
                })
            },
        )
        .map_err(|e| format!("Prompt template canary not found: {}", e))
    })
}

/// Record a result for a prompt template canary and persist updated metrics.
pub fn record_prompt_canary_run(
    db: &CheckpointDb,
    canary_id: &str,
    used_candidate: bool,
    success: bool,
    cost: f64,
    latency_ms: f64,
    tokens: i64,
) -> Result<(), String> {
    let canary_id = canary_id.to_string();

    db.with_conn(move |conn| {
        let (baseline_json, candidate_json): (String, String) = conn
            .query_row(
                "SELECT baseline_metrics_json, candidate_metrics_json FROM prompt_template_canaries WHERE id = ?1",
                params![canary_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Prompt template canary not found: {}", e))?;

        let mut baseline: PromptVersionMetrics =
            serde_json::from_str(&baseline_json).unwrap_or_default();
        let mut candidate: PromptVersionMetrics =
            serde_json::from_str(&candidate_json).unwrap_or_default();

        let metrics = if used_candidate {
            &mut candidate
        } else {
            &mut baseline
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

        let baseline_json = serde_json::to_string(&baseline).unwrap_or_default();
        let candidate_json = serde_json::to_string(&candidate).unwrap_or_default();

        conn.execute(
            "UPDATE prompt_template_canaries SET baseline_metrics_json = ?1, candidate_metrics_json = ?2 WHERE id = ?3",
            params![baseline_json, candidate_json, canary_id],
        )
        .map_err(|e| format!("Failed to record prompt canary run: {}", e))?;

        Ok(())
    })
}

/// Evaluate a prompt template canary from persisted DB state.
pub fn evaluate_prompt_canary_from_db(
    db: &CheckpointDb,
    canary_id: &str,
) -> Result<CanaryEvaluation, String> {
    let canary = get_prompt_template_canary(db, canary_id)?;
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
    db: &CheckpointDb,
    template_id: &str,
    default_content: &str,
) -> CanaryResolvedPrompt {
    // Look up an active canary for this template_id
    let template_id_owned = template_id.to_string();
    let canary_opt: Option<PromptTemplateCanary> = db
        .with_conn(move |conn| {
            let result = conn.query_row(
                r#"SELECT id, template_id, baseline_version, candidate_version,
                          traffic_percentage, status,
                          baseline_metrics_json, candidate_metrics_json,
                          created_at, ended_at
                   FROM prompt_template_canaries
                   WHERE template_id = ?1 AND status = 'active'
                   LIMIT 1"#,
                params![template_id_owned],
                |row| {
                    let baseline_json: String = row.get(6)?;
                    let candidate_json: String = row.get(7)?;
                    Ok(PromptTemplateCanary {
                        id: row.get(0)?,
                        template_id: row.get(1)?,
                        baseline_version: row.get(2)?,
                        candidate_version: row.get(3)?,
                        traffic_percentage: row.get(4)?,
                        status: row.get(5)?,
                        baseline_metrics: serde_json::from_str(&baseline_json).unwrap_or_default(),
                        candidate_metrics: serde_json::from_str(&candidate_json)
                            .unwrap_or_default(),
                        created_at: row.get(8)?,
                        ended_at: row.get(9)?,
                    })
                },
            );
            match result {
                Ok(c) => Ok(Some(c)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => {
                    warn!("Failed to query prompt template canary: {}", e);
                    Ok(None)
                }
            }
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
        db,
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
///
/// The original SQLite version is kept for fallback when PG is unavailable.
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
    db: &CheckpointDb,
    canary_id: &str,
    used_candidate: bool,
    success: bool,
    cost_usd: f64,
    latency_ms: f64,
    tokens: i64,
) {
    if let Err(e) = record_prompt_canary_run(
        db,
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
