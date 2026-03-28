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
}
