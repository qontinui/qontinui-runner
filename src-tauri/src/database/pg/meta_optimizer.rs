//! PostgreSQL meta-optimizer operations.
//!
//! Provides CRUD for `meta_optimizer_runs` and `meta_optimizer_snapshots` using raw SQL.

use super::PgDb;
use crate::meta_optimizer::snapshots::MetaOptimizerSnapshot;
use crate::meta_optimizer::types::MetaOptimizerRun;
use tracing::info;

impl PgDb {
    // ========================================================================
    // Meta-Optimizer Runs
    // ========================================================================

    /// Create a new meta-optimizer run.
    pub async fn create_optimizer_run(
        &self,
        optimizer_type: &str,
        trigger_type: &str,
        task_run_id: Option<&str>,
    ) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("morun-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO meta_optimizer_runs
               (id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                task_run_id, status, created_at)
               VALUES ($1, $2, $3, 0, 0, $4, 'running', $5)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &optimizer_type,
                &trigger_type,
                &task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG create_optimizer_run: {}", e))?;

        info!("Created PG meta-optimizer run {} (type={})", id, optimizer_type);
        Ok(id)
    }

    /// Complete a meta-optimizer run with results.
    pub async fn complete_optimizer_run(
        &self,
        run_id: &str,
        runs_analyzed: i64,
        recommendations_produced: i64,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"UPDATE meta_optimizer_runs
               SET status = 'complete', runs_analyzed = $1, recommendations_produced = $2, completed_at = $3
               WHERE id = $4"#,
            &[
                &runs_analyzed as &(dyn tokio_postgres::types::ToSql + Sync),
                &recommendations_produced,
                &now,
                &run_id,
            ],
        )
        .await
        .map_err(|e| format!("PG complete_optimizer_run: {}", e))?;

        Ok(())
    }

    /// Get recent meta-optimizer runs.
    pub async fn get_recent_optimizer_runs(
        &self,
        limit: i64,
    ) -> Result<Vec<MetaOptimizerRun>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                          task_run_id, status, created_at, completed_at
                   FROM meta_optimizer_runs
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG get_recent_optimizer_runs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| MetaOptimizerRun {
                id: r.get(0),
                optimizer_type: r.get(1),
                trigger_type: r.get(2),
                runs_analyzed: r.get(3),
                recommendations_produced: r.get(4),
                task_run_id: r.get(5),
                status: r.get(6),
                created_at: r.get(7),
                completed_at: r.get(8),
            })
            .collect())
    }

    // ========================================================================
    // Meta-Optimizer Snapshots
    // ========================================================================

    /// Save a meta-optimizer snapshot.
    pub async fn save_optimizer_snapshot(
        &self,
        id: &str,
        snapshot_type: &str,
        period_start: &str,
        period_end: &str,
        metrics_json: &str,
        breakdown_json: Option<&str>,
        recommendation_id: Option<&str>,
        runs_included: i64,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let bd = breakdown_json.unwrap_or("{}");

        conn.execute(
            r#"INSERT INTO meta_optimizer_snapshots
               (id, snapshot_type, period_start, period_end, metrics_json, breakdown_json,
                recommendation_id, runs_included, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &snapshot_type,
                &period_start,
                &period_end,
                &metrics_json,
                &bd,
                &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &runs_included,
                &period_end,
            ],
        )
        .await
        .map_err(|e| format!("PG save_optimizer_snapshot: {}", e))?;

        info!("Saved PG meta-optimizer snapshot {} (type={})", id, snapshot_type);
        Ok(())
    }

    /// Get the latest meta-optimizer snapshot.
    pub async fn get_latest_optimizer_snapshot(
        &self,
    ) -> Result<Option<MetaOptimizerSnapshot>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at
                   FROM meta_optimizer_snapshots
                   ORDER BY created_at DESC
                   LIMIT 1"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_latest_optimizer_snapshot: {}", e))?;

        Ok(rows.first().map(|r| MetaOptimizerSnapshot {
            id: r.get(0),
            snapshot_type: r.get(1),
            period_start: r.get(2),
            period_end: r.get(3),
            metrics_json: r.get(4),
            breakdown_json: r.get(5),
            recommendation_id: r.get(6),
            runs_included: r.get(7),
            created_at: r.get(8),
        }))
    }

    /// Update outcome_after_apply on a recommendation.
    pub async fn update_recommendation_outcome(&self, recommendation_id: &str, outcome_json: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("UPDATE meta_optimizer_recommendations SET outcome_after_apply = $1 WHERE id = $2", &[&outcome_json, &recommendation_id]).await.map_err(|e| format!("PG update_recommendation_outcome: {}", e))?;
        Ok(())
    }

    /// Create a recommendation in PG.
    pub async fn create_recommendation(&self, id: &str, optimizer_type: &str, recommendation_type: &str, target_agent: Option<&str>, title: &str, description: &str, current_value: Option<&str>, recommended_value: Option<&str>, evidence: Option<&str>, confidence: f64, optimizer_run_id: Option<&str>, content_hash: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO meta_optimizer_recommendations (id, optimizer_type, recommendation_type, target_agent, title, description, current_value, recommended_value, evidence, confidence, status, optimizer_run_id, created_at, content_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending', $11, $12, $13) ON CONFLICT (id) DO NOTHING"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &optimizer_type, &recommendation_type, &target_agent as &(dyn tokio_postgres::types::ToSql + Sync), &title, &description, &current_value as &(dyn tokio_postgres::types::ToSql + Sync), &recommended_value as &(dyn tokio_postgres::types::ToSql + Sync), &evidence as &(dyn tokio_postgres::types::ToSql + Sync), &confidence, &optimizer_run_id as &(dyn tokio_postgres::types::ToSql + Sync), &now, &content_hash],
        ).await.map_err(|e| format!("PG create_recommendation: {}", e))?;
        Ok(())
    }

    /// Update recommendation status in PG.
    pub async fn update_recommendation_status(&self, recommendation_id: &str, status: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute("UPDATE meta_optimizer_recommendations SET status = $1, applied_at = CASE WHEN $1 = 'applied' THEN $2 ELSE applied_at END WHERE id = $3", &[&status, &now, &recommendation_id]).await.map_err(|e| format!("PG update_recommendation_status: {}", e))?;
        Ok(())
    }

    /// Save an eval spec in PG.
    pub async fn save_eval_spec(&self, id: &str, name: &str, target_agent: &str, spec_json: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(r#"INSERT INTO eval_specs (id, name, target_agent, spec_json, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT(id) DO UPDATE SET name = EXCLUDED.name, target_agent = EXCLUDED.target_agent, spec_json = EXCLUDED.spec_json, updated_at = EXCLUDED.updated_at"#, &[&id, &name, &target_agent, &spec_json, &now]).await.map_err(|e| format!("PG save_eval_spec: {}", e))?;
        Ok(())
    }

    /// Delete an eval spec in PG.
    pub async fn delete_eval_spec(&self, spec_id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM eval_specs WHERE id = $1", &[&spec_id]).await.map_err(|e| format!("PG delete_eval_spec: {}", e))?;
        Ok(())
    }

    /// Save an eval result in PG.
    pub async fn save_eval_result(&self, id: &str, spec_id: &str, recommendation_id: Option<&str>, status: &str, result_json: &str, p_value: Option<f64>, trials_run: i64) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO eval_results (id, spec_id, recommendation_id, status, result_json, p_value, trials_run, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT(id) DO UPDATE SET status = EXCLUDED.status, result_json = EXCLUDED.result_json, p_value = EXCLUDED.p_value, trials_run = EXCLUDED.trials_run"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &spec_id, &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync), &status, &result_json, &p_value as &(dyn tokio_postgres::types::ToSql + Sync), &trials_run, &now],
        ).await.map_err(|e| format!("PG save_eval_result: {}", e))?;
        Ok(())
    }

    /// Attach eval result to a recommendation in PG.
    pub async fn attach_eval_result(&self, recommendation_id: &str, eval_result_id: &str, eval_status: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("UPDATE meta_optimizer_recommendations SET eval_result_id = $1, eval_status = $2 WHERE id = $3", &[&eval_result_id, &eval_status, &recommendation_id]).await.map_err(|e| format!("PG attach_eval_result: {}", e))?;
        Ok(())
    }

    /// Save a golden dataset in PG.
    pub async fn save_golden_dataset(&self, id: &str, agent_type: &str, name: &str, entries_json: &str, entry_count: i64) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(r#"INSERT INTO golden_datasets (id, agent_type, name, entries_json, entry_count, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $6) ON CONFLICT(id) DO UPDATE SET name = EXCLUDED.name, entries_json = EXCLUDED.entries_json, entry_count = EXCLUDED.entry_count, updated_at = EXCLUDED.updated_at"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &agent_type, &name, &entries_json, &entry_count, &now]).await.map_err(|e| format!("PG save_golden_dataset: {}", e))?;
        Ok(())
    }

    /// Delete a golden dataset in PG.
    pub async fn delete_golden_dataset(&self, dataset_id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM golden_datasets WHERE id = $1", &[&dataset_id]).await.map_err(|e| format!("PG delete_golden_dataset: {}", e))?;
        Ok(())
    }

    /// Save a robustness report in PG.
    pub async fn save_robustness_report(&self, id: &str, prompt_variant_id: Option<&str>, recommendation_id: Option<&str>, total_tests: i64, passed: i64, failed: i64, report_json: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO robustness_reports (id, prompt_variant_id, recommendation_id, total_tests, passed, failed, report_json, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &prompt_variant_id as &(dyn tokio_postgres::types::ToSql + Sync), &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync), &total_tests, &passed, &failed, &report_json, &now],
        ).await.map_err(|e| format!("PG save_robustness_report: {}", e))?;
        Ok(())
    }

    /// Update canary percentage in PG.
    pub async fn update_canary_percentage(&self, canary_id: &str, new_percentage: i64) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("UPDATE canary_rollouts SET percentage = $1 WHERE id = $2 AND status = 'active'", &[&new_percentage, &canary_id]).await.map_err(|e| format!("PG update_canary_percentage: {}", e))?;
        Ok(())
    }

    /// Get snapshots for a specific recommendation.
    pub async fn get_snapshots_for_recommendation(
        &self,
        recommendation_id: &str,
    ) -> Result<Vec<MetaOptimizerSnapshot>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at
                   FROM meta_optimizer_snapshots
                   WHERE recommendation_id = $1
                   ORDER BY created_at DESC"#,
                &[&recommendation_id],
            )
            .await
            .map_err(|e| format!("PG get_snapshots_for_recommendation: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| MetaOptimizerSnapshot {
                id: r.get(0),
                snapshot_type: r.get(1),
                period_start: r.get(2),
                period_end: r.get(3),
                metrics_json: r.get(4),
                breakdown_json: r.get(5),
                recommendation_id: r.get(6),
                runs_included: r.get(7),
                created_at: r.get(8),
            })
            .collect())
    }

    /// Get meta-optimizer run by its associated task_run_id.
    ///
    /// Returns (run_id, optimizer_type) if found.
    pub async fn get_optimizer_run_by_task_run_id(
        &self,
        task_run_id: &str,
    ) -> Result<Option<(String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT id, optimizer_type FROM meta_optimizer_runs WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_optimizer_run_by_task_run_id: {}", e))?;

        Ok(row.map(|r| (r.get::<_, String>(0), r.get::<_, String>(1))))
    }

    // ========================================================================
    // Recommendations — reads
    // ========================================================================

    /// Get a single recommendation by ID.
    pub async fn get_recommendation(&self, recommendation_id: &str) -> Result<Option<crate::meta_optimizer::types::Recommendation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            r#"SELECT id, optimizer_type, recommendation_type, target_agent, title, description,
                      current_value, recommended_value, evidence, confidence, status,
                      applied_at, outcome_after_apply, optimizer_run_id, created_at,
                      eval_result_id, eval_status
               FROM meta_optimizer_recommendations WHERE id = $1"#,
            &[&recommendation_id],
        ).await.map_err(|e| format!("PG get_recommendation: {}", e))?;
        Ok(row.map(|r| crate::meta_optimizer::types::Recommendation {
            id: r.get(0), optimizer_type: r.get(1), recommendation_type: r.get(2),
            target_agent: r.get(3), title: r.get(4), description: r.get(5),
            current_value: r.get(6), recommended_value: r.get(7), evidence: r.get(8),
            confidence: r.get(9), status: r.get(10), applied_at: r.get(11),
            outcome_after_apply: r.get(12), optimizer_run_id: r.get(13), created_at: r.get(14),
            eval_result_id: r.get(15), eval_status: r.get(16),
        }))
    }

    /// List recommendations with optional filters.
    pub async fn list_recommendations(&self, optimizer_type: Option<&str>, status: Option<&str>) -> Result<Vec<crate::meta_optimizer::types::Recommendation>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let mut sql = String::from(
            r#"SELECT id, optimizer_type, recommendation_type, target_agent, title, description,
                      current_value, recommended_value, evidence, confidence, status,
                      applied_at, outcome_after_apply, optimizer_run_id, created_at,
                      eval_result_id, eval_status
               FROM meta_optimizer_recommendations WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut idx = 1u32;
        if let Some(ot) = optimizer_type {
            sql.push_str(&format!(" AND optimizer_type = ${}", idx));
            params.push(Box::new(ot.to_string()));
            idx += 1;
        }
        if let Some(st) = status {
            sql.push_str(&format!(" AND status = ${}", idx));
            params.push(Box::new(st.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC");
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await.map_err(|e| format!("PG list_recommendations: {}", e))?;
        Ok(rows.iter().map(|r| crate::meta_optimizer::types::Recommendation {
            id: r.get(0), optimizer_type: r.get(1), recommendation_type: r.get(2),
            target_agent: r.get(3), title: r.get(4), description: r.get(5),
            current_value: r.get(6), recommended_value: r.get(7), evidence: r.get(8),
            confidence: r.get(9), status: r.get(10), applied_at: r.get(11),
            outcome_after_apply: r.get(12), optimizer_run_id: r.get(13), created_at: r.get(14),
            eval_result_id: r.get(15), eval_status: r.get(16),
        }).collect())
    }

    /// Check if a recommendation with the given content hash already exists in a non-terminal state.
    pub async fn is_content_duplicate(&self, content_hash: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) FROM meta_optimizer_recommendations WHERE content_hash = $1 AND status IN ('pending', 'canary', 'applied')",
            &[&content_hash],
        ).await.map_err(|e| format!("PG is_content_duplicate: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// List all optimizer runs (most recent first, limit 100).
    pub async fn list_optimizer_runs(&self) -> Result<Vec<MetaOptimizerRun>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            r#"SELECT id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                      task_run_id, status, created_at, completed_at
               FROM meta_optimizer_runs ORDER BY created_at DESC LIMIT 100"#,
            &[],
        ).await.map_err(|e| format!("PG list_optimizer_runs: {}", e))?;
        Ok(rows.iter().map(|r| MetaOptimizerRun {
            id: r.get(0), optimizer_type: r.get(1), trigger_type: r.get(2),
            runs_analyzed: r.get(3), recommendations_produced: r.get(4),
            task_run_id: r.get(5), status: r.get(6), created_at: r.get(7), completed_at: r.get(8),
        }).collect())
    }

    /// List snapshots with optional type filter.
    pub async fn list_snapshots(&self, snapshot_type: Option<&str>) -> Result<Vec<MetaOptimizerSnapshot>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(st) = snapshot_type {
            conn.query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at
                   FROM meta_optimizer_snapshots WHERE snapshot_type = $1
                   ORDER BY created_at DESC LIMIT 100"#,
                &[&st],
            ).await
        } else {
            conn.query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at
                   FROM meta_optimizer_snapshots ORDER BY created_at DESC LIMIT 100"#,
                &[],
            ).await
        }.map_err(|e| format!("PG list_snapshots: {}", e))?;
        Ok(rows.iter().map(|r| MetaOptimizerSnapshot {
            id: r.get(0), snapshot_type: r.get(1), period_start: r.get(2),
            period_end: r.get(3), metrics_json: r.get(4), breakdown_json: r.get(5),
            recommendation_id: r.get(6), runs_included: r.get(7), created_at: r.get(8),
        }).collect())
    }

    /// Get the latest baseline snapshot for a given type.
    pub async fn get_latest_baseline_snapshot(&self, snapshot_type: &str) -> Result<Option<MetaOptimizerSnapshot>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                      breakdown_json, recommendation_id, runs_included, created_at
               FROM meta_optimizer_snapshots WHERE snapshot_type = $1
               ORDER BY created_at DESC LIMIT 1"#,
            &[&snapshot_type],
        ).await.map_err(|e| format!("PG get_latest_baseline_snapshot: {}", e))?;
        Ok(row.map(|r| MetaOptimizerSnapshot {
            id: r.get(0), snapshot_type: r.get(1), period_start: r.get(2),
            period_end: r.get(3), metrics_json: r.get(4), breakdown_json: r.get(5),
            recommendation_id: r.get(6), runs_included: r.get(7), created_at: r.get(8),
        }))
    }

    /// Count applied recommendations.
    pub async fn count_applied_recommendations(&self) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) FROM meta_optimizer_recommendations WHERE status = 'applied'",
            &[],
        ).await.map_err(|e| format!("PG count_applied_recommendations: {}", e))?;
        Ok(row.get(0))
    }

    // ========================================================================
    // Eval specs — reads
    // ========================================================================

    /// List eval specs, optionally filtered by target agent.
    pub async fn list_eval_specs(&self, target_agent: Option<&str>) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(ta) = target_agent {
            conn.query("SELECT spec_json FROM eval_specs WHERE target_agent = $1 ORDER BY updated_at DESC", &[&ta]).await
        } else {
            conn.query("SELECT spec_json FROM eval_specs ORDER BY updated_at DESC", &[]).await
        }.map_err(|e| format!("PG list_eval_specs: {}", e))?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// Get a single eval spec by ID (returns the spec_json).
    pub async fn get_eval_spec(&self, spec_id: &str) -> Result<Option<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt("SELECT spec_json FROM eval_specs WHERE id = $1", &[&spec_id])
            .await.map_err(|e| format!("PG get_eval_spec: {}", e))?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    /// List eval results, optionally filtered by spec or recommendation.
    pub async fn list_eval_results(&self, spec_id: Option<&str>, recommendation_id: Option<&str>) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let mut sql = String::from("SELECT result_json FROM eval_results WHERE 1=1");
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut idx = 1u32;
        if let Some(s) = spec_id {
            sql.push_str(&format!(" AND spec_id = ${}", idx));
            params.push(Box::new(s.to_string()));
            idx += 1;
        }
        if let Some(r) = recommendation_id {
            sql.push_str(&format!(" AND recommendation_id = ${}", idx));
            params.push(Box::new(r.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT 50");
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await.map_err(|e| format!("PG list_eval_results: {}", e))?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    // ========================================================================
    // Golden datasets — reads
    // ========================================================================

    /// List golden datasets, optionally filtered by agent type.
    pub async fn list_golden_datasets(&self, agent_type: Option<&str>) -> Result<Vec<(String, String, String, String, String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(at) = agent_type {
            conn.query(
                "SELECT id, agent_type, name, entries_json, created_at, updated_at FROM golden_datasets WHERE agent_type = $1 ORDER BY updated_at DESC",
                &[&at],
            ).await
        } else {
            conn.query(
                "SELECT id, agent_type, name, entries_json, created_at, updated_at FROM golden_datasets ORDER BY updated_at DESC",
                &[],
            ).await
        }.map_err(|e| format!("PG list_golden_datasets: {}", e))?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5))).collect())
    }

    // ========================================================================
    // Robustness reports — reads
    // ========================================================================

    /// List robustness reports, optionally filtered.
    pub async fn list_robustness_reports(&self, prompt_variant_id: Option<&str>, recommendation_id: Option<&str>) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let mut sql = String::from("SELECT report_json FROM robustness_reports WHERE 1=1");
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        let mut idx = 1u32;
        if let Some(v) = prompt_variant_id {
            sql.push_str(&format!(" AND prompt_variant_id = ${}", idx));
            params.push(Box::new(v.to_string()));
            idx += 1;
        }
        if let Some(r) = recommendation_id {
            sql.push_str(&format!(" AND recommendation_id = ${}", idx));
            params.push(Box::new(r.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT 50");
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await.map_err(|e| format!("PG list_robustness_reports: {}", e))?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    // ========================================================================
    // Prompt evolution — reads
    // ========================================================================

    /// Check whether there is an active (no verdict yet) evolution entry for an agent type.
    pub async fn has_active_evolution(&self, agent_type: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) FROM prompt_evolution WHERE agent_type = $1 AND canary_verdict IS NULL",
            &[&agent_type],
        ).await.map_err(|e| format!("PG has_active_evolution: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// Get the most recent rejected evolution entry for an agent type.
    pub async fn get_latest_rejected_evolution(&self, agent_type: &str) -> Result<Option<crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                      critique, changes_summary, canary_verdict, score_before, score_after,
                      baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at::text
               FROM prompt_evolution
               WHERE agent_type = $1 AND canary_verdict = 'reject'
               ORDER BY created_at DESC LIMIT 1"#,
            &[&agent_type],
        ).await.map_err(|e| format!("PG get_latest_rejected_evolution: {}", e))?;
        Ok(row.map(|r| crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry {
            id: r.get(0), agent_type: r.get(1), parent_variant_id: r.get(2),
            variant_id: r.get(3), recommendation_id: r.get(4), critique: r.get(5),
            changes_summary: r.get(6), canary_verdict: r.get(7), score_before: r.get(8),
            score_after: r.get(9), baseline_prompt_hash: r.get(10),
            consecutive_rejections: r.get::<_, Option<i32>>(11).unwrap_or(0),
            created_at: r.get(12),
        }))
    }

    /// Check cooldown: returns true if an evolution entry exists within cutoff.
    pub async fn is_in_evolution_cooldown(&self, agent_type: &str, cutoff: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) FROM prompt_evolution WHERE agent_type = $1 AND created_at > $2",
            &[&agent_type, &cutoff],
        ).await.map_err(|e| format!("PG is_in_evolution_cooldown: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// Count consecutive recent rejections for an agent type.
    pub async fn count_consecutive_rejections(&self, agent_type: &str) -> Result<i32, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            r#"SELECT canary_verdict FROM prompt_evolution
               WHERE agent_type = $1 AND canary_verdict IS NOT NULL
               ORDER BY created_at DESC LIMIT 10"#,
            &[&agent_type],
        ).await.map_err(|e| format!("PG count_consecutive_rejections: {}", e))?;
        let mut count = 0i32;
        for r in &rows {
            let v: String = r.get(0);
            if v == "reject" { count += 1; } else { break; }
        }
        Ok(count)
    }

    /// Get rejected prompt contents for an agent type (for similarity comparison).
    pub async fn get_rejected_prompt_contents(&self, agent_type: &str) -> Result<Vec<(String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            r#"SELECT pe.variant_id, pr.prompt_content
               FROM prompt_evolution pe
               INNER JOIN prompt_registry pr ON pr.id = pe.variant_id
               WHERE pe.agent_type = $1 AND pe.canary_verdict = 'reject'
               ORDER BY pe.created_at DESC LIMIT 10"#,
            &[&agent_type],
        ).await.map_err(|e| format!("PG get_rejected_prompt_contents: {}", e))?;
        Ok(rows.iter().map(|r| (r.get::<_, String>(0), r.get::<_, String>(1))).collect())
    }

    /// Check if baseline prompt has drifted.
    pub async fn has_baseline_drifted(&self, agent_type: &str, current_hash: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            r#"SELECT baseline_prompt_hash FROM prompt_evolution
               WHERE agent_type = $1 AND canary_verdict IS NULL
               ORDER BY created_at DESC LIMIT 1"#,
            &[&agent_type],
        ).await.map_err(|e| format!("PG has_baseline_drifted: {}", e))?;
        match row {
            Some(r) => {
                let h: Option<String> = r.get(0);
                match h {
                    Some(ref hash) if !hash.is_empty() => Ok(hash != current_hash),
                    _ => Ok(false),
                }
            }
            None => Ok(false),
        }
    }

    // ========================================================================
    // Canary — reads
    // ========================================================================

    /// Get completed canary rollouts for history display.
    pub async fn get_canary_history(&self, limit: i64) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn.query(
            r#"SELECT c.id, c.recommendation_id, c.percentage, c.status,
                      c.start_date::text, c.end_date::text,
                      c.baseline_run_count, c.canary_run_count,
                      c.baseline_metrics_json, c.canary_metrics_json, c.created_at::text,
                      r.title, r.target_agent, r.recommendation_type
               FROM canary_rollouts c
               LEFT JOIN meta_optimizer_recommendations r ON r.id = c.recommendation_id
               WHERE c.status IN ('promoted', 'rolled_back')
               ORDER BY c.end_date DESC
               LIMIT $1"#,
            &[&limit],
        ).await.map_err(|e| format!("PG get_canary_history: {}", e))?;
        Ok(rows.iter().map(|row| {
            serde_json::json!({
                "id": row.get::<_, String>(0),
                "recommendation_id": row.get::<_, String>(1),
                "percentage": row.get::<_, i64>(2),
                "status": row.get::<_, String>(3),
                "start_date": row.get::<_, Option<String>>(4),
                "end_date": row.get::<_, Option<String>>(5),
                "baseline_run_count": row.get::<_, i64>(6),
                "canary_run_count": row.get::<_, i64>(7),
                "baseline_metrics_json": row.get::<_, String>(8),
                "canary_metrics_json": row.get::<_, String>(9),
                "created_at": row.get::<_, Option<String>>(10),
                "recommendation_title": row.get::<_, Option<String>>(11),
                "target_agent": row.get::<_, Option<String>>(12),
                "recommendation_type": row.get::<_, Option<String>>(13),
            })
        }).collect())
    }

    /// Probabilistic check: should this run use the canary config?
    pub async fn should_apply_canary(&self, recommendation_id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT percentage FROM canary_rollouts WHERE recommendation_id = $1 AND status = 'active' LIMIT 1",
            &[&recommendation_id],
        ).await.map_err(|e| format!("PG should_apply_canary: {}", e))?;
        match row {
            Some(r) => {
                let percentage: i64 = r.get(0);
                if percentage <= 0 { return Ok(false); }
                let roll: f64 = rand::random::<f64>() * 100.0;
                Ok(roll < percentage as f64)
            }
            None => Ok(false),
        }
    }

    // ========================================================================
    // Tiered query helpers for meta-optimizer API endpoints
    // ========================================================================

    /// Learning outcomes L0 summary: counts and averages grouped by status.
    pub async fn get_learning_outcomes_l0(
        &self,
        status: Option<&str>,
        workflow_architecture: Option<&str>,
    ) -> Result<Vec<crate::meta_optimizer::types::LearningOutcomeSummaryL0>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT status,
                      COUNT(*)::bigint as run_count,
                      COALESCE(AVG(duration_secs), 0.0) as avg_duration_secs,
                      COALESCE(AVG(iterations::double precision), 0.0) as avg_iterations
               FROM learning_outcomes
               WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(s) = status {
            params.push(Box::new(s.to_string()));
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        if let Some(wa) = workflow_architecture {
            params.push(Box::new(wa.to_string()));
            sql.push_str(&format!(" AND workflow_architecture = ${}", params.len()));
        }
        sql.push_str(" GROUP BY status ORDER BY run_count DESC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_learning_outcomes_l0: {}", e))?;

        Ok(rows.iter().map(|r| crate::meta_optimizer::types::LearningOutcomeSummaryL0 {
            status: r.get(0),
            run_count: r.get(1),
            avg_duration_secs: r.get(2),
            avg_iterations: r.get(3),
        }).collect())
    }

    /// Learning outcomes L1 details.
    pub async fn get_learning_outcomes_l1(
        &self,
        status: Option<&str>,
        workflow_architecture: Option<&str>,
        limit: u32,
    ) -> Result<Vec<crate::meta_optimizer::types::LearningOutcomeDetailL1>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, task_id, status, duration_secs, iterations,
                      workflow_architecture, error_type, created_at
               FROM learning_outcomes WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(s) = status {
            params.push(Box::new(s.to_string()));
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        if let Some(wa) = workflow_architecture {
            params.push(Box::new(wa.to_string()));
            sql.push_str(&format!(" AND workflow_architecture = ${}", params.len()));
        }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_learning_outcomes_l1: {}", e))?;

        Ok(rows.iter().map(|r| crate::meta_optimizer::types::LearningOutcomeDetailL1 {
            id: r.get(0),
            task_id: r.get(1),
            status: r.get(2),
            duration_secs: r.get(3),
            iterations: r.get(4),
            workflow_architecture: r.get(5),
            error_type: r.get(6),
            created_at: r.get(7),
        }).collect())
    }

    /// Learning outcomes L2 full records.
    pub async fn get_learning_outcomes_l2(
        &self,
        status: Option<&str>,
        workflow_architecture: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, task_id, status, duration_secs, iterations, strategy,
                      tools_used, files_modified, error_type, error_message,
                      workflow_architecture, created_at, step_count,
                      verification_step_count, agentic_step_count, has_ui_bridge,
                      technology_tags, domain_tags, complexity_tier
               FROM learning_outcomes WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(s) = status {
            params.push(Box::new(s.to_string()));
            sql.push_str(&format!(" AND status = ${}", params.len()));
        }
        if let Some(wa) = workflow_architecture {
            params.push(Box::new(wa.to_string()));
            sql.push_str(&format!(" AND workflow_architecture = ${}", params.len()));
        }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_learning_outcomes_l2: {}", e))?;

        Ok(rows.iter().map(|r| {
            let tech_tags: Option<String> = r.get(16);
            let domain_tags: Option<String> = r.get(17);
            serde_json::json!({
                "id": r.get::<_, String>(0),
                "task_id": r.get::<_, String>(1),
                "status": r.get::<_, String>(2),
                "duration_secs": r.get::<_, Option<f64>>(3),
                "iterations": r.get::<_, Option<i32>>(4),
                "strategy": r.get::<_, Option<String>>(5),
                "tools_used": r.get::<_, Option<String>>(6),
                "files_modified": r.get::<_, Option<String>>(7),
                "error_type": r.get::<_, Option<String>>(8),
                "error_message": r.get::<_, Option<String>>(9),
                "workflow_architecture": r.get::<_, Option<String>>(10),
                "created_at": r.get::<_, String>(11),
                "step_count": r.get::<_, Option<i64>>(12),
                "verification_step_count": r.get::<_, Option<i64>>(13),
                "agentic_step_count": r.get::<_, Option<i64>>(14),
                "has_ui_bridge": r.get::<_, Option<bool>>(15),
                "technology_tags": tech_tags.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "domain_tags": domain_tags.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "complexity_tier": r.get::<_, Option<String>>(18),
            })
        }).collect())
    }

    /// Generation feedback L0 summary.
    pub async fn get_generation_feedback_l0(
        &self,
        feedback_type: Option<&str>,
    ) -> Result<Vec<crate::meta_optimizer::types::GenerationFeedbackSummaryL0>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT feedback_type,
                      COUNT(*)::bigint as total_count,
                      AVG(rating::double precision) as avg_rating
               FROM workflow_generation_feedback WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(ft) = feedback_type {
            params.push(Box::new(ft.to_string()));
            sql.push_str(&format!(" AND feedback_type = ${}", params.len()));
        }
        sql.push_str(" GROUP BY feedback_type ORDER BY total_count DESC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_generation_feedback_l0: {}", e))?;

        Ok(rows.iter().map(|r| crate::meta_optimizer::types::GenerationFeedbackSummaryL0 {
            feedback_type: r.get(0),
            total_count: r.get(1),
            avg_rating: r.get(2),
        }).collect())
    }

    /// Generation feedback L1 details.
    pub async fn get_generation_feedback_l1(
        &self,
        feedback_type: Option<&str>,
        limit: u32,
    ) -> Result<Vec<crate::meta_optimizer::types::GenerationFeedbackDetailL1>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, feedback_type, edited_field, rating,
                      workflow_category, created_at
               FROM workflow_generation_feedback WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(ft) = feedback_type {
            params.push(Box::new(ft.to_string()));
            sql.push_str(&format!(" AND feedback_type = ${}", params.len()));
        }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_generation_feedback_l1: {}", e))?;

        Ok(rows.iter().map(|r| crate::meta_optimizer::types::GenerationFeedbackDetailL1 {
            id: r.get(0),
            feedback_type: r.get(1),
            edited_field: r.get(2),
            rating: r.get(3),
            workflow_category: r.get(4),
            created_at: r.get(5),
        }).collect())
    }

    /// Generation feedback L2 full records.
    pub async fn get_generation_feedback_l2(
        &self,
        feedback_type: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, workflow_id, task_run_id, feedback_type, edited_field,
                      old_value, new_value, delete_reason, rating, rating_comment,
                      workflow_category, workflow_description, created_at
               FROM workflow_generation_feedback WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(ft) = feedback_type {
            params.push(Box::new(ft.to_string()));
            sql.push_str(&format!(" AND feedback_type = ${}", params.len()));
        }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_generation_feedback_l2: {}", e))?;

        Ok(rows.iter().map(|r| serde_json::json!({
            "id": r.get::<_, String>(0),
            "workflow_id": r.get::<_, String>(1),
            "task_run_id": r.get::<_, Option<String>>(2),
            "feedback_type": r.get::<_, String>(3),
            "edited_field": r.get::<_, Option<String>>(4),
            "old_value": r.get::<_, Option<String>>(5),
            "new_value": r.get::<_, Option<String>>(6),
            "delete_reason": r.get::<_, Option<String>>(7),
            "rating": r.get::<_, Option<i32>>(8),
            "rating_comment": r.get::<_, Option<String>>(9),
            "workflow_category": r.get::<_, Option<String>>(10),
            "workflow_description": r.get::<_, Option<String>>(11),
            "created_at": r.get::<_, String>(12),
        })).collect())
    }

    /// Reflection fixes L0 summary (for cross-workflow view).
    pub async fn get_reflection_fixes_l0(
        &self,
        source_agent: Option<&str>,
    ) -> Result<Vec<crate::meta_optimizer::types::ReflectionFixSummaryL0>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT fix_type,
                      COUNT(*)::bigint as total_count,
                      SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END)::bigint as effective_count
               FROM reflection_fixes WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(agent) = source_agent {
            params.push(Box::new(agent.to_string()));
            sql.push_str(&format!(" AND source_agent = ${}", params.len()));
        }
        sql.push_str(" GROUP BY fix_type ORDER BY total_count DESC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_reflection_fixes_l0: {}", e))?;

        Ok(rows.iter().map(|r| crate::meta_optimizer::types::ReflectionFixSummaryL0 {
            fix_type: r.get(0),
            total_count: r.get(1),
            effective_count: r.get(2),
        }).collect())
    }

    /// Reflection fixes L1 details.
    pub async fn get_reflection_fixes_l1(
        &self,
        source_agent: Option<&str>,
        limit: i64,
    ) -> Result<Vec<crate::meta_optimizer::types::ReflectionFixDetailL1>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, fix_type, fix_description, confidence,
                      effectiveness, source_agent, created_at
               FROM reflection_fixes WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(agent) = source_agent {
            params.push(Box::new(agent.to_string()));
            sql.push_str(&format!(" AND source_agent = ${}", params.len()));
        }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_reflection_fixes_l1: {}", e))?;

        Ok(rows.iter().map(|r| crate::meta_optimizer::types::ReflectionFixDetailL1 {
            id: r.get(0),
            fix_type: r.get(1),
            fix_description: r.get(2),
            confidence: r.get(3),
            effectiveness: r.get(4),
            source_agent: r.get(5),
            created_at: r.get(6),
        }).collect())
    }

    /// Reflection fixes L2 full records.
    pub async fn get_reflection_fixes_l2(
        &self,
        source_agent: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, source_task_run_id, reflection_task_run_id,
                      source_finding_id, source_knowledge_id,
                      fix_type, fix_description, file_changed,
                      old_value, new_value, confidence, status,
                      effectiveness, effectiveness_evidence,
                      applied_at, evaluated_at, created_at,
                      content_hash, source_agent, reflection_scope,
                      project_path, target_component, reuse_count,
                      reasoning, alternatives_considered,
                      applicability_context
               FROM reflection_fixes WHERE 1=1"#,
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        if let Some(agent) = source_agent {
            params.push(Box::new(agent.to_string()));
            sql.push_str(&format!(" AND source_agent = ${}", params.len()));
        }
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let rows = conn.query(&sql, &param_refs).await
            .map_err(|e| format!("PG get_reflection_fixes_l2: {}", e))?;

        Ok(rows.iter().map(|r| serde_json::json!({
            "id": r.get::<_, String>(0),
            "source_task_run_id": r.get::<_, String>(1),
            "reflection_task_run_id": r.get::<_, String>(2),
            "source_finding_id": r.get::<_, Option<String>>(3),
            "source_knowledge_id": r.get::<_, Option<String>>(4),
            "fix_type": r.get::<_, String>(5),
            "fix_description": r.get::<_, String>(6),
            "file_changed": r.get::<_, Option<String>>(7),
            "old_value": r.get::<_, Option<String>>(8),
            "new_value": r.get::<_, Option<String>>(9),
            "confidence": r.get::<_, String>(10),
            "status": r.get::<_, String>(11),
            "effectiveness": r.get::<_, Option<String>>(12),
            "effectiveness_evidence": r.get::<_, Option<String>>(13),
            "applied_at": r.get::<_, String>(14),
            "evaluated_at": r.get::<_, Option<String>>(15),
            "created_at": r.get::<_, String>(16),
            "content_hash": r.get::<_, Option<String>>(17),
            "source_agent": r.get::<_, Option<String>>(18),
            "reflection_scope": r.get::<_, Option<String>>(19),
            "project_path": r.get::<_, Option<String>>(20),
            "target_component": r.get::<_, Option<String>>(21),
            "reuse_count": r.get::<_, Option<i32>>(22),
            "reasoning": r.get::<_, Option<String>>(23),
            "alternatives_considered": r.get::<_, Option<String>>(24),
            "applicability_context": r.get::<_, Option<String>>(25),
        })).collect())
    }

    /// Get recommendation metadata (title, target_agent, type) for a recommendation ID.
    pub async fn get_recommendation_metadata(
        &self,
        rec_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>, Option<String>)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT title, target_agent, recommendation_type FROM meta_optimizer_recommendations WHERE id = $1",
            &[&rec_id],
        ).await.map_err(|e| format!("PG get_recommendation_metadata: {}", e))?;

        Ok(row.map(|r| (r.get(0), r.get(1), r.get(2))))
    }

    /// Update evolution verdict by variant_id.
    pub async fn update_evolution_verdict_by_variant(&self, variant_id: &str, verdict: &str, score_after: Option<f64>) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE prompt_evolution SET canary_verdict = $1, score_after = $2 WHERE variant_id = $3 AND canary_verdict IS NULL",
            &[&verdict, &score_after as &(dyn tokio_postgres::types::ToSql + Sync), &variant_id],
        ).await.map_err(|e| format!("PG update_evolution_verdict_by_variant: {}", e))?;
        Ok(())
    }
}
