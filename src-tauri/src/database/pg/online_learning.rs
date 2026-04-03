//! PostgreSQL persistence for the online learning system.
//!
//! Provides CRUD operations for:
//! - Drift detection signals and detector state
//! - Model routing Q-table, overrides, and decisions
//! - Experience summaries
//! - Step credit assignments
//! - Strategy bank
//!
//! Uses raw SQL queries (not Clorinde) for simplicity during initial development.
//! Can be migrated to Clorinde-generated queries once the schema stabilizes.

use super::PgDb;
use tracing::{debug, warn};

impl PgDb {
    // =========================================================================
    // Drift Detection
    // =========================================================================

    /// Record a drift signal.
    pub async fn insert_drift_signal(
        &self,
        id: &str,
        detector_type: &str,
        metric_name: &str,
        context_key: &str,
        drift_level: &str,
        pre_drift_mean: f64,
        post_drift_mean: f64,
        window_size: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO performance_drift_signals
             (id, detector_type, metric_name, context_key, drift_level,
              pre_drift_mean, post_drift_mean, window_size)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO NOTHING",
            &[
                &id,
                &detector_type,
                &metric_name,
                &context_key,
                &drift_level,
                &pre_drift_mean,
                &post_drift_mean,
                &window_size,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_drift_signal: {}", e))?;
        Ok(())
    }

    /// Get unacknowledged drift signals.
    pub async fn get_unacknowledged_drift_signals(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, detector_type, metric_name, context_key, drift_level,
                        pre_drift_mean, post_drift_mean, window_size, created_at
                 FROM performance_drift_signals
                 WHERE acknowledged = false
                 ORDER BY created_at DESC
                 LIMIT 100",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_unacknowledged_drift_signals: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "detector_type": r.get::<_, String>(1),
                    "metric_name": r.get::<_, String>(2),
                    "context_key": r.get::<_, String>(3),
                    "drift_level": r.get::<_, String>(4),
                    "pre_drift_mean": r.get::<_, f64>(5),
                    "post_drift_mean": r.get::<_, f64>(6),
                    "window_size": r.get::<_, i64>(7),
                })
            })
            .collect())
    }

    /// Acknowledge a drift signal.
    pub async fn acknowledge_drift_signal(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE performance_drift_signals SET acknowledged = true WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| format!("PG acknowledge_drift_signal: {}", e))?;
        Ok(())
    }

    /// Save drift detector state for persistence across restarts.
    pub async fn save_drift_detector_state(
        &self,
        detector_id: &str,
        detector_type: &str,
        state_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO drift_detector_state (detector_id, detector_type, state_json, last_updated)
             VALUES ($1, $2, $3, NOW())
             ON CONFLICT (detector_id) DO UPDATE
             SET state_json = $3, last_updated = NOW()",
            &[&detector_id, &detector_type, &state_json],
        )
        .await
        .map_err(|e| format!("PG save_drift_detector_state: {}", e))?;
        Ok(())
    }

    /// Load drift detector state.
    pub async fn load_drift_detector_state(
        &self,
        detector_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT state_json FROM drift_detector_state WHERE detector_id = $1",
                &[&detector_id],
            )
            .await
            .map_err(|e| format!("PG load_drift_detector_state: {}", e))?;
        Ok(row.map(|r| r.get(0)))
    }

    // =========================================================================
    // Model Routing
    // =========================================================================

    /// Upsert a model routing Q-table entry.
    pub async fn upsert_model_routing_entry(
        &self,
        context_key: &str,
        model_id: &str,
        q_value: f64,
        visit_count: i32,
        sum_of_squares: f64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO model_routing_table
             (context_key, model_id, q_value, visit_count, sum_of_squares, last_updated)
             VALUES ($1, $2, $3, $4, $5, NOW())
             ON CONFLICT (context_key, model_id) DO UPDATE
             SET q_value = $3, visit_count = $4, sum_of_squares = $5, last_updated = NOW()",
            &[
                &context_key,
                &model_id,
                &q_value,
                &visit_count,
                &sum_of_squares,
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_model_routing_entry: {}", e))?;
        Ok(())
    }

    /// Load all model routing Q-table entries.
    pub async fn load_model_routing_table(
        &self,
    ) -> Result<Vec<(String, String, f64, u32, f64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT context_key, model_id, q_value, visit_count, sum_of_squares
                 FROM model_routing_table",
                &[],
            )
            .await
            .map_err(|e| format!("PG load_model_routing_table: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, f64>(2),
                    r.get::<_, i32>(3) as u32,
                    r.get::<_, f64>(4),
                )
            })
            .collect())
    }

    /// Load model routing overrides.
    pub async fn load_model_routing_overrides(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT context_key, forced_model FROM model_routing_overrides",
                &[],
            )
            .await
            .map_err(|e| format!("PG load_model_routing_overrides: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Record a model routing decision.
    pub async fn record_model_routing_decision(
        &self,
        id: &str,
        task_run_id: &str,
        context_key: &str,
        model_selected: &str,
        source: &str,
        exploration: bool,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO model_routing_decisions
             (id, task_run_id, context_key, model_selected, source, exploration)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO NOTHING",
            &[
                &id,
                &task_run_id,
                &context_key,
                &model_selected,
                &source,
                &exploration,
            ],
        )
        .await
        .map_err(|e| format!("PG record_model_routing_decision: {}", e))?;
        Ok(())
    }

    /// Backfill the reward for a model routing decision after the run completes.
    pub async fn backfill_model_routing_reward(
        &self,
        task_run_id: &str,
        reward: f64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE model_routing_decisions SET reward = $2 WHERE task_run_id = $1",
            &[&task_run_id, &reward],
        )
        .await
        .map_err(|e| format!("PG backfill_model_routing_reward: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Experience Summaries
    // =========================================================================

    /// Insert an experience summary.
    pub async fn insert_experience_summary(
        &self,
        id: &str,
        task_run_id: &str,
        domain: &str,
        complexity_tier: &str,
        outcome: &str,
        key_decisions_json: &str,
        failure_points_json: &str,
        effective_patterns_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO experience_summaries
             (id, task_run_id, domain, complexity_tier, outcome,
              key_decisions_json, failure_points_json, effective_patterns_json)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO NOTHING",
            &[
                &id,
                &task_run_id,
                &domain,
                &complexity_tier,
                &outcome,
                &key_decisions_json,
                &failure_points_json,
                &effective_patterns_json,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_experience_summary: {}", e))?;
        Ok(())
    }

    /// Get recent experience summaries for a domain.
    pub async fn get_experience_summaries(
        &self,
        domain: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(d) = domain {
            conn.query(
                "SELECT id, task_run_id, domain, complexity_tier, outcome,
                        key_decisions_json, failure_points_json, effective_patterns_json,
                        created_at
                 FROM experience_summaries
                 WHERE domain = $1
                 ORDER BY created_at DESC
                 LIMIT $2",
                &[&d, &limit],
            )
            .await
        } else {
            conn.query(
                "SELECT id, task_run_id, domain, complexity_tier, outcome,
                        key_decisions_json, failure_points_json, effective_patterns_json,
                        created_at
                 FROM experience_summaries
                 ORDER BY created_at DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
        }
        .map_err(|e| format!("PG get_experience_summaries: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "task_run_id": r.get::<_, String>(1),
                    "domain": r.get::<_, String>(2),
                    "complexity_tier": r.get::<_, String>(3),
                    "outcome": r.get::<_, String>(4),
                    "key_decisions": r.get::<_, String>(5),
                    "failure_points": r.get::<_, String>(6),
                    "effective_patterns": r.get::<_, String>(7),
                })
            })
            .collect())
    }

    // =========================================================================
    // Step Credit Assignments
    // =========================================================================

    /// Save step credit assignments for a completed run.
    pub async fn save_step_credits(
        &self,
        credits: &[crate::online_learning::credit_assignment::StepCredit],
        task_run_id: &str,
    ) -> Result<usize, String> {
        if credits.is_empty() {
            return Ok(0);
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let mut count = 0;

        for credit in credits {
            let id = format!("{}:{}", task_run_id, credit.step_index);
            if let Err(e) = conn
                .execute(
                    "INSERT INTO step_credit_assignments
                     (id, task_run_id, step_index, step_type, agent_type,
                      raw_credit, normalized_credit,
                      temporal_proximity, output_utilization,
                      confidence_delta_signal, downstream_success_signal,
                      cost_efficiency_signal)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                     ON CONFLICT (id) DO NOTHING",
                    &[
                        &id,
                        &task_run_id,
                        &(credit.step_index as i32),
                        &credit.step_type,
                        &credit.agent_type,
                        &credit.raw_credit,
                        &credit.normalized_credit,
                        &credit.signals.temporal_proximity,
                        &credit.signals.output_utilization,
                        &credit.signals.confidence_delta,
                        &credit.signals.downstream_success,
                        &credit.signals.cost_efficiency,
                    ],
                )
                .await
            {
                warn!("Failed to save step credit {}: {}", id, e);
            } else {
                count += 1;
            }
        }

        debug!("Saved {} step credits for run {}", count, task_run_id);
        Ok(count)
    }

    /// Get step credits for a specific task run.
    pub async fn get_step_credits(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT step_index, step_type, agent_type, raw_credit, normalized_credit,
                        temporal_proximity, output_utilization,
                        confidence_delta_signal, downstream_success_signal
                 FROM step_credit_assignments
                 WHERE task_run_id = $1
                 ORDER BY step_index",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_step_credits: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "step_index": r.get::<_, i32>(0),
                    "step_type": r.get::<_, String>(1),
                    "agent_type": r.get::<_, Option<String>>(2),
                    "raw_credit": r.get::<_, f64>(3),
                    "normalized_credit": r.get::<_, f64>(4),
                    "temporal_proximity": r.get::<_, Option<f64>>(5),
                    "output_utilization": r.get::<_, Option<f64>>(6),
                    "confidence_delta_signal": r.get::<_, Option<f64>>(7),
                    "downstream_success_signal": r.get::<_, Option<f64>>(8),
                })
            })
            .collect())
    }

    /// Get step-type scorecard (aggregated credits across runs).
    pub async fn get_step_type_scorecard(
        &self,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT step_type,
                        AVG(normalized_credit) as avg_credit,
                        COUNT(*) as total_count,
                        AVG(raw_credit) as avg_raw_credit
                 FROM step_credit_assignments
                 GROUP BY step_type
                 ORDER BY avg_credit DESC
                 LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG get_step_type_scorecard: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "step_type": r.get::<_, String>(0),
                    "avg_normalized_credit": r.get::<_, f64>(1),
                    "total_count": r.get::<_, i64>(2),
                    "avg_raw_credit": r.get::<_, f64>(3),
                })
            })
            .collect())
    }

    // =========================================================================
    // Strategy Bank
    // =========================================================================

    /// Insert a new strategy.
    pub async fn insert_strategy(
        &self,
        id: &str,
        name: &str,
        description: &str,
        applicability_json: &str,
        components_json: &str,
        provenance_json: &str,
        status: &str,
        parent_strategy_id: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "INSERT INTO strategy_bank
             (id, name, description, applicability_json, components_json,
              provenance_json, status, parent_strategy_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO NOTHING",
            &[
                &id,
                &name,
                &description,
                &applicability_json,
                &components_json,
                &provenance_json,
                &status,
                &parent_strategy_id,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_strategy: {}", e))?;
        Ok(())
    }

    /// Update strategy stats after a run.
    pub async fn update_strategy_stats(
        &self,
        strategy_id: &str,
        stats_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE strategy_bank SET stats_json = $2, updated_at = NOW() WHERE id = $1",
            &[&strategy_id, &stats_json],
        )
        .await
        .map_err(|e| format!("PG update_strategy_stats: {}", e))?;
        Ok(())
    }

    /// Update strategy status (candidate → active, active → degraded, etc.).
    pub async fn update_strategy_status(
        &self,
        strategy_id: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE strategy_bank SET status = $2, updated_at = NOW() WHERE id = $1",
            &[&strategy_id, &status],
        )
        .await
        .map_err(|e| format!("PG update_strategy_status: {}", e))?;
        Ok(())
    }

    /// Get active strategies for a given context.
    pub async fn get_active_strategies(&self) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                "SELECT id, name, description, applicability_json, components_json,
                        stats_json, provenance_json, status, parent_strategy_id,
                        created_at, updated_at
                 FROM strategy_bank
                 WHERE status IN ('active', 'candidate')
                 ORDER BY updated_at DESC",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_active_strategies: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "description": r.get::<_, String>(2),
                    "applicability": r.get::<_, String>(3),
                    "components": r.get::<_, String>(4),
                    "stats": r.get::<_, String>(5),
                    "provenance": r.get::<_, String>(6),
                    "status": r.get::<_, String>(7),
                    "parent_strategy_id": r.get::<_, Option<String>>(8),
                })
            })
            .collect())
    }

    // =========================================================================
    // Model Tracking (Phase 1A)
    // =========================================================================

    /// Get learning outcome data for online learning enrichment.
    /// Returns key fields from the learning_outcomes table for a task_run_id.
    pub async fn get_learning_outcome_for_online_learning(
        &self,
        task_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT status, duration_secs, iterations, tools_used, files_modified,
                        error_type, workflow_architecture, step_count,
                        has_ui_bridge, total_tokens, total_cost_usd,
                        composite_agentic_score, technology_tags, domain_tags,
                        complexity_tier, model_used
                 FROM learning_outcomes
                 WHERE task_id = $1
                 ORDER BY created_at DESC
                 LIMIT 1",
                &[&task_id],
            )
            .await
            .map_err(|e| format!("PG get_learning_outcome_for_online_learning: {}", e))?;

        Ok(row.map(|r| {
            serde_json::json!({
                "status": r.get::<_, Option<String>>(0),
                "duration_secs": r.get::<_, Option<f64>>(1),
                "iterations": r.get::<_, Option<i32>>(2),
                "tools_used": r.get::<_, Option<String>>(3),
                "files_modified": r.get::<_, Option<String>>(4),
                "error_type": r.get::<_, Option<String>>(5),
                "workflow_architecture": r.get::<_, Option<String>>(6),
                "step_count": r.get::<_, Option<i64>>(7),
                "has_ui_bridge": r.get::<_, Option<bool>>(8),
                "total_tokens": r.get::<_, Option<i64>>(9),
                "total_cost_usd": r.get::<_, Option<f64>>(10),
                "composite_agentic_score": r.get::<_, Option<f64>>(11),
                "technology_tags": r.get::<_, Option<String>>(12),
                "domain_tags": r.get::<_, Option<String>>(13),
                "complexity_tier": r.get::<_, Option<String>>(14),
                "model_used": r.get::<_, Option<String>>(15),
            })
        }))
    }

    /// Get the composite score percentile for a context (for strategy extraction).
    ///
    /// Filters by domain and complexity from the context_key (domain:complexity:has_ui).
    /// Falls back to global percentile if no context-specific data exists.
    pub async fn get_score_percentile(
        &self,
        context_key: &str,
        percentile: i32,
    ) -> Result<f64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Parse context_key (domain:complexity:has_ui) to filter
        let parts: Vec<&str> = context_key.split(':').collect();

        // Try context-specific first, fall back to global
        let rows = if parts.len() >= 2 {
            let domain = format!("%{}%", parts[0]);
            let complexity = parts[1].to_string();
            let context_rows = conn
                .query(
                    "SELECT composite_agentic_score
                     FROM learning_outcomes
                     WHERE composite_agentic_score IS NOT NULL
                       AND composite_agentic_score > 0
                       AND status = 'success'
                       AND domain_tags LIKE $1
                       AND complexity_tier = $2
                     ORDER BY composite_agentic_score ASC",
                    &[&domain, &complexity],
                )
                .await
                .map_err(|e| format!("PG get_score_percentile: {}", e))?;

            // Fall back to global if context has <10 samples
            if context_rows.len() >= 10 {
                context_rows
            } else {
                conn.query(
                    "SELECT composite_agentic_score
                     FROM learning_outcomes
                     WHERE composite_agentic_score IS NOT NULL
                       AND composite_agentic_score > 0
                       AND status = 'success'
                     ORDER BY composite_agentic_score ASC",
                    &[],
                )
                .await
                .map_err(|e| format!("PG get_score_percentile (global): {}", e))?
            }
        } else {
            conn.query(
                "SELECT composite_agentic_score
                 FROM learning_outcomes
                 WHERE composite_agentic_score IS NOT NULL
                   AND composite_agentic_score > 0
                   AND status = 'success'
                 ORDER BY composite_agentic_score ASC",
                &[],
            )
            .await
            .map_err(|e| format!("PG get_score_percentile: {}", e))?
        };

        if rows.is_empty() {
            return Ok(1.0);
        }

        let scores: Vec<f64> = rows.iter().map(|r| r.get::<_, f64>(0)).collect();
        let idx = ((percentile as f64 / 100.0) * scores.len() as f64) as usize;
        let idx = idx.min(scores.len() - 1);

        Ok(scores[idx])
    }
}
