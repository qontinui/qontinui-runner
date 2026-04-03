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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        info!(
            "Created PG meta-optimizer run {} (type={})",
            id, optimizer_type
        );
        Ok(id)
    }

    /// Complete a meta-optimizer run with results.
    pub async fn complete_optimizer_run(
        &self,
        run_id: &str,
        runs_analyzed: i64,
        recommendations_produced: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                          task_run_id, status, created_at::TEXT, completed_at::TEXT
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        info!(
            "Saved PG meta-optimizer snapshot {} (type={})",
            id, snapshot_type
        );
        Ok(())
    }

    /// Get the latest meta-optimizer snapshot.
    pub async fn get_latest_optimizer_snapshot(
        &self,
    ) -> Result<Option<MetaOptimizerSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at::TEXT
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
    pub async fn update_recommendation_outcome(
        &self,
        recommendation_id: &str,
        outcome_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE meta_optimizer_recommendations SET outcome_after_apply = $1 WHERE id = $2",
            &[&outcome_json, &recommendation_id],
        )
        .await
        .map_err(|e| format!("PG update_recommendation_outcome: {}", e))?;
        Ok(())
    }

    /// Create a recommendation in PG.
    pub async fn create_recommendation(
        &self,
        id: &str,
        optimizer_type: &str,
        recommendation_type: &str,
        target_agent: Option<&str>,
        title: &str,
        description: &str,
        current_value: Option<&str>,
        recommended_value: Option<&str>,
        evidence: Option<&str>,
        confidence: f64,
        optimizer_run_id: Option<&str>,
        content_hash: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO meta_optimizer_recommendations (id, optimizer_type, recommendation_type, target_agent, title, description, current_value, recommended_value, evidence, confidence, status, optimizer_run_id, created_at, content_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'pending', $11, $12, $13) ON CONFLICT (id) DO NOTHING"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &optimizer_type, &recommendation_type, &target_agent as &(dyn tokio_postgres::types::ToSql + Sync), &title, &description, &current_value as &(dyn tokio_postgres::types::ToSql + Sync), &recommended_value as &(dyn tokio_postgres::types::ToSql + Sync), &evidence as &(dyn tokio_postgres::types::ToSql + Sync), &confidence, &optimizer_run_id as &(dyn tokio_postgres::types::ToSql + Sync), &now, &content_hash],
        ).await.map_err(|e| format!("PG create_recommendation: {}", e))?;
        Ok(())
    }

    /// Update recommendation status in PG.
    pub async fn update_recommendation_status(
        &self,
        recommendation_id: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute("UPDATE meta_optimizer_recommendations SET status = $1, applied_at = CASE WHEN $1 = 'applied' THEN $2 ELSE applied_at END WHERE id = $3", &[&status, &now, &recommendation_id]).await.map_err(|e| format!("PG update_recommendation_status: {}", e))?;
        Ok(())
    }

    /// Save an eval spec in PG.
    pub async fn save_eval_spec(
        &self,
        id: &str,
        name: &str,
        target_agent: &str,
        spec_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(r#"INSERT INTO eval_specs (id, name, target_agent, spec_json, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT(id) DO UPDATE SET name = EXCLUDED.name, target_agent = EXCLUDED.target_agent, spec_json = EXCLUDED.spec_json, updated_at = EXCLUDED.updated_at"#, &[&id, &name, &target_agent, &spec_json, &now]).await.map_err(|e| format!("PG save_eval_spec: {}", e))?;
        Ok(())
    }

    /// Delete an eval spec in PG.
    pub async fn delete_eval_spec(&self, spec_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM eval_specs WHERE id = $1", &[&spec_id])
            .await
            .map_err(|e| format!("PG delete_eval_spec: {}", e))?;
        Ok(())
    }

    /// Save an eval result in PG.
    pub async fn save_eval_result(
        &self,
        id: &str,
        spec_id: &str,
        recommendation_id: Option<&str>,
        status: &str,
        result_json: &str,
        p_value: Option<f64>,
        trials_run: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO eval_results (id, spec_id, recommendation_id, status, result_json, p_value, trials_run, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT(id) DO UPDATE SET status = EXCLUDED.status, result_json = EXCLUDED.result_json, p_value = EXCLUDED.p_value, trials_run = EXCLUDED.trials_run"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &spec_id, &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync), &status, &result_json, &p_value as &(dyn tokio_postgres::types::ToSql + Sync), &trials_run, &now],
        ).await.map_err(|e| format!("PG save_eval_result: {}", e))?;
        Ok(())
    }

    /// Attach eval result to a recommendation in PG.
    pub async fn attach_eval_result(
        &self,
        recommendation_id: &str,
        eval_result_id: &str,
        eval_status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("UPDATE meta_optimizer_recommendations SET eval_result_id = $1, eval_status = $2 WHERE id = $3", &[&eval_result_id, &eval_status, &recommendation_id]).await.map_err(|e| format!("PG attach_eval_result: {}", e))?;
        Ok(())
    }

    /// Save a golden dataset in PG.
    pub async fn save_golden_dataset(
        &self,
        id: &str,
        agent_type: &str,
        name: &str,
        entries_json: &str,
        entry_count: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(r#"INSERT INTO golden_datasets (id, agent_type, name, entries_json, entry_count, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $6) ON CONFLICT(id) DO UPDATE SET name = EXCLUDED.name, entries_json = EXCLUDED.entries_json, entry_count = EXCLUDED.entry_count, updated_at = EXCLUDED.updated_at"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &agent_type, &name, &entries_json, &entry_count, &now]).await.map_err(|e| format!("PG save_golden_dataset: {}", e))?;
        Ok(())
    }

    /// Delete a golden dataset in PG.
    pub async fn delete_golden_dataset(&self, dataset_id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute("DELETE FROM golden_datasets WHERE id = $1", &[&dataset_id])
            .await
            .map_err(|e| format!("PG delete_golden_dataset: {}", e))?;
        Ok(())
    }

    /// Save a robustness report in PG.
    pub async fn save_robustness_report(
        &self,
        id: &str,
        prompt_variant_id: Option<&str>,
        recommendation_id: Option<&str>,
        total_tests: i64,
        passed: i64,
        failed: i64,
        report_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO robustness_reports (id, prompt_variant_id, recommendation_id, total_tests, passed, failed, report_json, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
            &[&id as &(dyn tokio_postgres::types::ToSql + Sync), &prompt_variant_id as &(dyn tokio_postgres::types::ToSql + Sync), &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync), &total_tests, &passed, &failed, &report_json, &now],
        ).await.map_err(|e| format!("PG save_robustness_report: {}", e))?;
        Ok(())
    }

    /// Update canary percentage in PG.
    pub async fn update_canary_percentage(
        &self,
        canary_id: &str,
        new_percentage: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE canary_rollouts SET percentage = $1 WHERE id = $2 AND status = 'active'",
            &[&new_percentage, &canary_id],
        )
        .await
        .map_err(|e| format!("PG update_canary_percentage: {}", e))?;
        Ok(())
    }

    /// Get snapshots for a specific recommendation.
    pub async fn get_snapshots_for_recommendation(
        &self,
        recommendation_id: &str,
    ) -> Result<Vec<MetaOptimizerSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at::TEXT
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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
    pub async fn get_recommendation(
        &self,
        recommendation_id: &str,
    ) -> Result<Option<crate::meta_optimizer::types::Recommendation>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, optimizer_type, recommendation_type, target_agent, title, description,
                      current_value, recommended_value, evidence, confidence, status,
                      applied_at::TEXT, outcome_after_apply, optimizer_run_id, created_at::TEXT,
                      eval_result_id, eval_status
               FROM meta_optimizer_recommendations WHERE id = $1"#,
                &[&recommendation_id],
            )
            .await
            .map_err(|e| format!("PG get_recommendation: {}", e))?;
        Ok(row.map(|r| crate::meta_optimizer::types::Recommendation {
            id: r.get(0),
            optimizer_type: r.get(1),
            recommendation_type: r.get(2),
            target_agent: r.get(3),
            title: r.get(4),
            description: r.get(5),
            current_value: r.get(6),
            recommended_value: r.get(7),
            evidence: r.get(8),
            confidence: r.get(9),
            status: r.get(10),
            applied_at: r.get(11),
            outcome_after_apply: r.get(12),
            optimizer_run_id: r.get(13),
            created_at: r.get(14),
            eval_result_id: r.get(15),
            eval_status: r.get(16),
        }))
    }

    /// List recommendations with optional filters.
    pub async fn list_recommendations(
        &self,
        optimizer_type: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<crate::meta_optimizer::types::Recommendation>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let mut sql = String::from(
            r#"SELECT id, optimizer_type, recommendation_type, target_agent, title, description,
                      current_value, recommended_value, evidence, confidence, status,
                      applied_at::TEXT, outcome_after_apply, optimizer_run_id, created_at::TEXT,
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
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_recommendations: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| crate::meta_optimizer::types::Recommendation {
                id: r.get(0),
                optimizer_type: r.get(1),
                recommendation_type: r.get(2),
                target_agent: r.get(3),
                title: r.get(4),
                description: r.get(5),
                current_value: r.get(6),
                recommended_value: r.get(7),
                evidence: r.get(8),
                confidence: r.get(9),
                status: r.get(10),
                applied_at: r.get(11),
                outcome_after_apply: r.get(12),
                optimizer_run_id: r.get(13),
                created_at: r.get(14),
                eval_result_id: r.get(15),
                eval_status: r.get(16),
            })
            .collect())
    }

    /// Check if a recommendation with the given content hash already exists in a non-terminal state.
    pub async fn is_content_duplicate(&self, content_hash: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) FROM meta_optimizer_recommendations WHERE content_hash = $1 AND status IN ('pending', 'canary', 'applied')",
            &[&content_hash],
        ).await.map_err(|e| format!("PG is_content_duplicate: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// List all optimizer runs (most recent first, limit 100).
    pub async fn list_optimizer_runs(&self) -> Result<Vec<MetaOptimizerRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, optimizer_type, trigger_type, runs_analyzed, recommendations_produced,
                      task_run_id, status, created_at::TEXT, completed_at::TEXT
               FROM meta_optimizer_runs ORDER BY created_at DESC LIMIT 100"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_optimizer_runs: {}", e))?;
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

    /// List snapshots with optional type filter.
    pub async fn list_snapshots(
        &self,
        snapshot_type: Option<&str>,
    ) -> Result<Vec<MetaOptimizerSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(st) = snapshot_type {
            conn.query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at::TEXT
                   FROM meta_optimizer_snapshots WHERE snapshot_type = $1
                   ORDER BY created_at DESC LIMIT 100"#,
                &[&st],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                          breakdown_json, recommendation_id, runs_included, created_at::TEXT
                   FROM meta_optimizer_snapshots ORDER BY created_at DESC LIMIT 100"#,
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG list_snapshots: {}", e))?;
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

    /// Get the latest baseline snapshot for a given type.
    pub async fn get_latest_baseline_snapshot(
        &self,
        snapshot_type: &str,
    ) -> Result<Option<MetaOptimizerSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                      breakdown_json, recommendation_id, runs_included, created_at::TEXT
               FROM meta_optimizer_snapshots WHERE snapshot_type = $1
               ORDER BY created_at DESC LIMIT 1"#,
                &[&snapshot_type],
            )
            .await
            .map_err(|e| format!("PG get_latest_baseline_snapshot: {}", e))?;
        Ok(row.map(|r| MetaOptimizerSnapshot {
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

    /// Count applied recommendations.
    pub async fn count_applied_recommendations(&self) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*) FROM meta_optimizer_recommendations WHERE status = 'applied'",
                &[],
            )
            .await
            .map_err(|e| format!("PG count_applied_recommendations: {}", e))?;
        Ok(row.get(0))
    }

    // ========================================================================
    // Eval specs — reads
    // ========================================================================

    /// List eval specs, optionally filtered by target agent.
    pub async fn list_eval_specs(&self, target_agent: Option<&str>) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = if let Some(ta) = target_agent {
            conn.query(
                "SELECT spec_json FROM eval_specs WHERE target_agent = $1 ORDER BY updated_at DESC",
                &[&ta],
            )
            .await
        } else {
            conn.query(
                "SELECT spec_json FROM eval_specs ORDER BY updated_at DESC",
                &[],
            )
            .await
        }
        .map_err(|e| format!("PG list_eval_specs: {}", e))?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    /// Get a single eval spec by ID (returns the spec_json).
    pub async fn get_eval_spec(&self, spec_id: &str) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                "SELECT spec_json FROM eval_specs WHERE id = $1",
                &[&spec_id],
            )
            .await
            .map_err(|e| format!("PG get_eval_spec: {}", e))?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    /// List eval results, optionally filtered by spec or recommendation.
    pub async fn list_eval_results(
        &self,
        spec_id: Option<&str>,
        recommendation_id: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_eval_results: {}", e))?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    // ========================================================================
    // Golden datasets — reads
    // ========================================================================

    /// List golden datasets, optionally filtered by agent type.
    pub async fn list_golden_datasets(
        &self,
        agent_type: Option<&str>,
    ) -> Result<Vec<(String, String, String, String, String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        Ok(rows
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
            .collect())
    }

    // ========================================================================
    // Robustness reports — reads
    // ========================================================================

    /// List robustness reports, optionally filtered.
    pub async fn list_robustness_reports(
        &self,
        prompt_variant_id: Option<&str>,
        recommendation_id: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
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
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG list_robustness_reports: {}", e))?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    // ========================================================================
    // Prompt evolution — reads
    // ========================================================================

    /// Check whether there is an active (no verdict yet) evolution entry for an agent type.
    pub async fn has_active_evolution(&self, agent_type: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT COUNT(*) FROM prompt_evolution WHERE agent_type = $1 AND canary_verdict IS NULL",
            &[&agent_type],
        ).await.map_err(|e| format!("PG has_active_evolution: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// Get the most recent rejected evolution entry for an agent type.
    pub async fn get_latest_rejected_evolution(
        &self,
        agent_type: &str,
    ) -> Result<Option<crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, agent_type, parent_variant_id, variant_id, recommendation_id,
                      critique, changes_summary, canary_verdict, score_before, score_after,
                      baseline_prompt_hash, COALESCE(consecutive_rejections, 0), created_at::text
               FROM prompt_evolution
               WHERE agent_type = $1 AND canary_verdict = 'reject'
               ORDER BY created_at DESC LIMIT 1"#,
                &[&agent_type],
            )
            .await
            .map_err(|e| format!("PG get_latest_rejected_evolution: {}", e))?;
        Ok(row.map(
            |r| crate::meta_optimizer::prompt_evolution::PromptEvolutionEntry {
                id: r.get(0),
                agent_type: r.get(1),
                parent_variant_id: r.get(2),
                variant_id: r.get(3),
                recommendation_id: r.get(4),
                critique: r.get(5),
                changes_summary: r.get(6),
                canary_verdict: r.get(7),
                score_before: r.get(8),
                score_after: r.get(9),
                baseline_prompt_hash: r.get(10),
                consecutive_rejections: r.get::<_, Option<i32>>(11).unwrap_or(0),
                created_at: r.get(12),
            },
        ))
    }

    /// Check cooldown: returns true if an evolution entry exists within cutoff.
    pub async fn is_in_evolution_cooldown(
        &self,
        agent_type: &str,
        cutoff: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*) FROM prompt_evolution WHERE agent_type = $1 AND created_at > $2",
                &[&agent_type, &cutoff],
            )
            .await
            .map_err(|e| format!("PG is_in_evolution_cooldown: {}", e))?;
        let count: i64 = row.get(0);
        Ok(count > 0)
    }

    /// Count consecutive recent rejections for an agent type.
    pub async fn count_consecutive_rejections(&self, agent_type: &str) -> Result<i32, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT canary_verdict FROM prompt_evolution
               WHERE agent_type = $1 AND canary_verdict IS NOT NULL
               ORDER BY created_at DESC LIMIT 10"#,
                &[&agent_type],
            )
            .await
            .map_err(|e| format!("PG count_consecutive_rejections: {}", e))?;
        let mut count = 0i32;
        for r in &rows {
            let v: String = r.get(0);
            if v == "reject" {
                count += 1;
            } else {
                break;
            }
        }
        Ok(count)
    }

    /// Get rejected prompt contents for an agent type (for similarity comparison).
    pub async fn get_rejected_prompt_contents(
        &self,
        agent_type: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT pe.variant_id, pr.prompt_content
               FROM prompt_evolution pe
               INNER JOIN prompt_registry pr ON pr.id = pe.variant_id
               WHERE pe.agent_type = $1 AND pe.canary_verdict = 'reject'
               ORDER BY pe.created_at DESC LIMIT 10"#,
                &[&agent_type],
            )
            .await
            .map_err(|e| format!("PG get_rejected_prompt_contents: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Check if baseline prompt has drifted.
    pub async fn has_baseline_drifted(
        &self,
        agent_type: &str,
        current_hash: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT baseline_prompt_hash FROM prompt_evolution
               WHERE agent_type = $1 AND canary_verdict IS NULL
               ORDER BY created_at DESC LIMIT 1"#,
                &[&agent_type],
            )
            .await
            .map_err(|e| format!("PG has_baseline_drifted: {}", e))?;
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
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
            )
            .await
            .map_err(|e| format!("PG get_canary_history: {}", e))?;
        Ok(rows
            .iter()
            .map(|row| {
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
            })
            .collect())
    }

    /// Probabilistic check: should this run use the canary config?
    pub async fn should_apply_canary(&self, recommendation_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT percentage FROM canary_rollouts WHERE recommendation_id = $1 AND status = 'active' LIMIT 1",
            &[&recommendation_id],
        ).await.map_err(|e| format!("PG should_apply_canary: {}", e))?;
        match row {
            Some(r) => {
                let percentage: i64 = r.get(0);
                if percentage <= 0 {
                    return Ok(false);
                }
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_learning_outcomes_l0: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::meta_optimizer::types::LearningOutcomeSummaryL0 {
                status: r.get(0),
                run_count: r.get(1),
                avg_duration_secs: r.get(2),
                avg_iterations: r.get(3),
            })
            .collect())
    }

    /// Learning outcomes L1 details.
    pub async fn get_learning_outcomes_l1(
        &self,
        status: Option<&str>,
        workflow_architecture: Option<&str>,
        limit: u32,
    ) -> Result<Vec<crate::meta_optimizer::types::LearningOutcomeDetailL1>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_learning_outcomes_l1: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::meta_optimizer::types::LearningOutcomeDetailL1 {
                id: r.get(0),
                task_id: r.get(1),
                status: r.get(2),
                duration_secs: r.get(3),
                iterations: r.get(4),
                workflow_architecture: r.get(5),
                error_type: r.get(6),
                created_at: r.get(7),
            })
            .collect())
    }

    /// Learning outcomes L2 full records.
    pub async fn get_learning_outcomes_l2(
        &self,
        status: Option<&str>,
        workflow_architecture: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_generation_feedback_l0: {}", e))?;

        Ok(rows
            .iter()
            .map(
                |r| crate::meta_optimizer::types::GenerationFeedbackSummaryL0 {
                    feedback_type: r.get(0),
                    total_count: r.get(1),
                    avg_rating: r.get(2),
                },
            )
            .collect())
    }

    /// Generation feedback L1 details.
    pub async fn get_generation_feedback_l1(
        &self,
        feedback_type: Option<&str>,
        limit: u32,
    ) -> Result<Vec<crate::meta_optimizer::types::GenerationFeedbackDetailL1>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_generation_feedback_l1: {}", e))?;

        Ok(rows
            .iter()
            .map(
                |r| crate::meta_optimizer::types::GenerationFeedbackDetailL1 {
                    id: r.get(0),
                    feedback_type: r.get(1),
                    edited_field: r.get(2),
                    rating: r.get(3),
                    workflow_category: r.get(4),
                    created_at: r.get(5),
                },
            )
            .collect())
    }

    /// Generation feedback L2 full records.
    pub async fn get_generation_feedback_l2(
        &self,
        feedback_type: Option<&str>,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_generation_feedback_l2: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
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
                })
            })
            .collect())
    }

    /// Reflection fixes L0 summary (for cross-workflow view).
    pub async fn get_reflection_fixes_l0(
        &self,
        source_agent: Option<&str>,
    ) -> Result<Vec<crate::meta_optimizer::types::ReflectionFixSummaryL0>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_reflection_fixes_l0: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::meta_optimizer::types::ReflectionFixSummaryL0 {
                fix_type: r.get(0),
                total_count: r.get(1),
                effective_count: r.get(2),
            })
            .collect())
    }

    /// Reflection fixes L1 details.
    pub async fn get_reflection_fixes_l1(
        &self,
        source_agent: Option<&str>,
        limit: i64,
    ) -> Result<Vec<crate::meta_optimizer::types::ReflectionFixDetailL1>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_reflection_fixes_l1: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| crate::meta_optimizer::types::ReflectionFixDetailL1 {
                id: r.get(0),
                fix_type: r.get(1),
                fix_description: r.get(2),
                confidence: r.get(3),
                effectiveness: r.get(4),
                source_agent: r.get(5),
                created_at: r.get(6),
            })
            .collect())
    }

    /// Reflection fixes L2 full records.
    pub async fn get_reflection_fixes_l2(
        &self,
        source_agent: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut sql = String::from(
            r#"SELECT id, source_task_run_id, reflection_task_run_id,
                      source_finding_id, source_knowledge_id,
                      fix_type, fix_description, file_changed,
                      old_value, new_value, confidence, status,
                      effectiveness, effectiveness_evidence,
                      applied_at::TEXT, evaluated_at::TEXT, created_at::TEXT,
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

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG get_reflection_fixes_l2: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                serde_json::json!({
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
                })
            })
            .collect())
    }

    /// Get recommendation metadata (title, target_agent, type) for a recommendation ID.
    pub async fn get_recommendation_metadata(
        &self,
        rec_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>, Option<String>)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT title, target_agent, recommendation_type FROM meta_optimizer_recommendations WHERE id = $1",
            &[&rec_id],
        ).await.map_err(|e| format!("PG get_recommendation_metadata: {}", e))?;

        Ok(row.map(|r| (r.get(0), r.get(1), r.get(2))))
    }

    /// Update evolution verdict by variant_id.
    pub async fn update_evolution_verdict_by_variant(
        &self,
        variant_id: &str,
        verdict: &str,
        score_after: Option<f64>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE prompt_evolution SET canary_verdict = $1, score_after = $2 WHERE variant_id = $3 AND canary_verdict IS NULL",
            &[&verdict, &score_after as &(dyn tokio_postgres::types::ToSql + Sync), &variant_id],
        ).await.map_err(|e| format!("PG update_evolution_verdict_by_variant: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Optimizer Context (PG equivalent of meta_optimizer_api context builder)
    // =========================================================================

    /// PG equivalent of the optimizer context builder in meta_optimizer_api.rs.
    /// Returns a markdown-formatted context string for AI consumption.
    pub async fn get_optimizer_context(&self, is_pipeline: bool) -> Result<String, String> {
        use std::fmt::Write;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;
        let since = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let mut out = String::new();

        // 1. Performance Summary
        let result = conn
            .query_one(
                r#"SELECT
                    COUNT(*) as total_runs,
                    SUM(CASE WHEN status='success' THEN 1 ELSE 0 END) as successful,
                    SUM(CASE WHEN status='partial' THEN 1 ELSE 0 END) as partial,
                    SUM(CASE WHEN status='failure' THEN 1 ELSE 0 END) as failed,
                    COALESCE(ROUND(AVG(duration_secs)::numeric, 1), 0) as avg_duration,
                    COALESCE(ROUND(AVG(iterations)::numeric, 1), 0) as avg_iterations
                FROM learning_outcomes
                WHERE created_at > $1
                  AND (iterations IS NULL OR iterations > 0)"#,
                &[&since],
            )
            .await;

        if let Ok(row) = result {
            let total: i64 = row.get(0);
            if total > 0 {
                let success: i64 = row.get(1);
                let partial: i64 = row.get(2);
                let failed: i64 = row.get(3);
                let success_pct = 100.0 * success as f64 / total as f64;
                let partial_pct = 100.0 * partial as f64 / total as f64;
                let failed_pct = 100.0 * failed as f64 / total as f64;

                let _ = writeln!(out, "## Performance Summary (last 30 days)");
                let _ = writeln!(
                    out,
                    "- {} total runs: {} successful ({:.1}%), {} partial ({:.1}%), {} failed ({:.1}%)",
                    total, success, success_pct, partial, partial_pct, failed, failed_pct
                );
            }
        }

        // 2. Recent optimizer runs
        let runs = conn
            .query(
                r#"SELECT id, optimizer_type, status, improvements_found,
                          recommendations_count, duration_ms, created_at::TEXT
                   FROM meta_optimizer_runs
                   ORDER BY created_at DESC LIMIT 5"#,
                &[],
            )
            .await
            .unwrap_or_default();

        if !runs.is_empty() {
            let _ = writeln!(out, "\n## Recent Optimizer Runs");
            for row in &runs {
                let opt_type: String = row.get(1);
                let status: String = row.get(2);
                let improvements: i32 = row.get(3);
                let recs: i32 = row.get(4);
                let _ = writeln!(
                    out,
                    "- {} optimizer: {} ({} improvements, {} recommendations)",
                    opt_type, status, improvements, recs
                );
            }
        }

        // 3. Active recommendations
        let recs = conn
            .query(
                r#"SELECT optimizer_type, recommendation_type, title, status
                   FROM meta_optimizer_recommendations
                   WHERE status IN ('pending', 'applied')
                   ORDER BY created_at DESC LIMIT 10"#,
                &[],
            )
            .await
            .unwrap_or_default();

        if !recs.is_empty() {
            let _ = writeln!(out, "\n## Active Recommendations");
            for row in &recs {
                let opt_type: String = row.get(0);
                let title: String = row.get(2);
                let status: String = row.get(3);
                let _ = writeln!(out, "- [{}] {} ({})", opt_type, title, status);
            }
        }

        if out.is_empty() {
            let _ = writeln!(out, "No optimizer data available for the last 30 days.");
        }

        Ok(out)
    }

    /// PG equivalent of the autoresearch campaigns query.
    pub async fn get_autoresearch_campaigns_with_experiments(
        &self,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let campaigns = conn
            .query(
                r#"SELECT id, name, config_json, current_control_json, status,
                          experiment_count, accepted_count, created_at
                   FROM autoresearch_campaigns
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG autoresearch_campaigns: {}", e))?;

        let mut result = Vec::new();
        for row in &campaigns {
            let id: String = row.get(0);
            let name: String = row.get(1);
            let config_json: String = row.get(2);
            let current_control_json: String = row.get(3);
            let status: String = row.get(4);
            let experiment_count: i64 = row.get(5);
            let accepted_count: i64 = row.get(6);
            let created_at: String = row.get(7);

            // Fetch experiments for this campaign
            let experiments = conn
                .query(
                    r#"SELECT id, experiment_number, config_json, aggregate_json,
                              accepted, reason, p_value, created_at
                       FROM autoresearch_experiments
                       WHERE campaign_id = $1
                       ORDER BY experiment_number DESC
                       LIMIT 10"#,
                    &[&id],
                )
                .await
                .unwrap_or_default();

            let exp_values: Vec<serde_json::Value> = experiments
                .iter()
                .map(|er| {
                    serde_json::json!({
                        "id": er.get::<_, String>(0),
                        "experiment_number": er.get::<_, i32>(1),
                        "config_json": er.get::<_, String>(2),
                        "aggregate_json": er.get::<_, Option<String>>(3),
                        "accepted": er.get::<_, Option<bool>>(4),
                        "reason": er.get::<_, Option<String>>(5),
                        "p_value": er.get::<_, Option<f64>>(6),
                        "created_at": er.get::<_, String>(7),
                    })
                })
                .collect();

            result.push(serde_json::json!({
                "id": id,
                "name": name,
                "config_json": config_json,
                "current_control_json": current_control_json,
                "status": status,
                "experiment_count": experiment_count,
                "accepted_count": accepted_count,
                "created_at": created_at,
                "experiments": exp_values,
            }));
        }

        Ok(result)
    }

    /// PG equivalent of iteration history queries (L0 summary).
    pub async fn get_iteration_history_l0(
        &self,
        limit: i64,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(status) = status_filter {
            conn.query(
                r#"SELECT status, iteration_history
                   FROM task_runs
                   WHERE iteration_history IS NOT NULL
                     AND iteration_history != '[]'
                     AND status = $1
                   ORDER BY created_at DESC LIMIT $2"#,
                &[&status, &limit],
            )
            .await
        } else {
            conn.query(
                r#"SELECT status, iteration_history
                   FROM task_runs
                   WHERE iteration_history IS NOT NULL
                     AND iteration_history != '[]'
                   ORDER BY created_at DESC LIMIT $1"#,
                &[&limit],
            )
            .await
        }
        .map_err(|e| format!("PG iteration_history_l0: {}", e))?;

        // Group by status and compute aggregates
        let mut groups: std::collections::HashMap<String, Vec<Vec<serde_json::Value>>> =
            std::collections::HashMap::new();
        for row in &rows {
            let status: String = row.get(0);
            let history_json: String = row.get(1);
            if let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&history_json) {
                groups.entry(status).or_default().push(entries);
            }
        }

        let mut summaries = Vec::new();
        for (status, runs) in &groups {
            let run_count = runs.len() as i64;
            let mut total_iterations: usize = 0;
            let mut total_confidence_delta: f64 = 0.0;
            let mut approach_counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for entries in runs {
                total_iterations += entries.len();
                for e in entries {
                    if let Some(d) = e.get("confidence_delta").and_then(|d| d.as_f64()) {
                        total_confidence_delta += d;
                    }
                    if let Some(a) = e.get("approach").and_then(|a| a.as_str()) {
                        *approach_counts.entry(a.to_string()).or_default() += 1;
                    }
                }
            }

            let avg_iterations = if run_count > 0 {
                total_iterations as f64 / run_count as f64
            } else {
                0.0
            };
            let avg_confidence_delta = if total_iterations > 0 {
                total_confidence_delta / total_iterations as f64
            } else {
                0.0
            };

            summaries.push(serde_json::json!({
                "status": status,
                "run_count": run_count,
                "avg_iterations": avg_iterations,
                "avg_confidence_delta": avg_confidence_delta,
                "top_approaches": approach_counts,
            }));
        }

        Ok(summaries)
    }

    /// PG equivalent of iteration history queries (L1 per-run details).
    pub async fn get_iteration_history_l1(
        &self,
        limit: i64,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(status) = status_filter {
            conn.query(
                r#"SELECT id, task_name, status, iteration_history, created_at
                   FROM task_runs
                   WHERE iteration_history IS NOT NULL
                     AND iteration_history != '[]'
                     AND status = $1
                   ORDER BY created_at DESC LIMIT $2"#,
                &[&status, &limit],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, task_name, status, iteration_history, created_at
                   FROM task_runs
                   WHERE iteration_history IS NOT NULL
                     AND iteration_history != '[]'
                   ORDER BY created_at DESC LIMIT $1"#,
                &[&limit],
            )
            .await
        }
        .map_err(|e| format!("PG iteration_history_l1: {}", e))?;

        let details: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let id: String = row.get(0);
                let task_name: String = row.get(1);
                let status: String = row.get(2);
                let history_json: String = row.get(3);
                let created_at: String = row.get(4);

                let entries: Vec<serde_json::Value> =
                    serde_json::from_str(&history_json).unwrap_or_default();
                let iteration_count = entries.len();
                let total_confidence_delta: f64 = entries
                    .iter()
                    .filter_map(|e| e.get("confidence_delta").and_then(|d| d.as_f64()))
                    .sum();
                let approaches: Vec<String> = entries
                    .iter()
                    .filter_map(|e| e.get("approach").and_then(|a| a.as_str()).map(String::from))
                    .collect();

                serde_json::json!({
                    "task_run_id": id,
                    "task_name": task_name,
                    "status": status,
                    "iteration_count": iteration_count,
                    "total_confidence_delta": total_confidence_delta,
                    "approaches": approaches,
                    "created_at": created_at,
                })
            })
            .collect();

        Ok(details)
    }

    /// PG equivalent of iteration history queries (L2 full raw data).
    pub async fn get_iteration_history_l2(
        &self,
        limit: i64,
        status_filter: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool: {}", e))?;

        let rows = if let Some(status) = status_filter {
            conn.query(
                r#"SELECT id, task_name, status, iteration_history, created_at
                   FROM task_runs
                   WHERE iteration_history IS NOT NULL
                     AND iteration_history != '[]'
                     AND status = $1
                   ORDER BY created_at DESC LIMIT $2"#,
                &[&status, &limit],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, task_name, status, iteration_history, created_at
                   FROM task_runs
                   WHERE iteration_history IS NOT NULL
                     AND iteration_history != '[]'
                   ORDER BY created_at DESC LIMIT $1"#,
                &[&limit],
            )
            .await
        }
        .map_err(|e| format!("PG iteration_history_l2: {}", e))?;

        let full: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let id: String = row.get(0);
                let task_name: String = row.get(1);
                let status: String = row.get(2);
                let history_json: String = row.get(3);
                let created_at: String = row.get(4);
                let entries: serde_json::Value =
                    serde_json::from_str(&history_json).unwrap_or_default();

                serde_json::json!({
                    "task_run_id": id,
                    "task_name": task_name,
                    "status": status,
                    "iteration_history": entries,
                    "created_at": created_at,
                })
            })
            .collect();

        Ok(full)
    }

    // ========================================================================
    // Cost optimizer — pipeline_agent_traces queries
    // ========================================================================

    /// Query per-agent token/cost averages over the last 30 days (non-zero tokens only).
    pub async fn query_agent_cost_stats(
        &self,
        since: &str,
    ) -> Result<Vec<(String, i64, f64, f64, f64, f64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT
                    agent_type,
                    COUNT(*)::bigint as run_count,
                    ROUND(AVG(tokens_in)::numeric, 0)::double precision as avg_tokens_in,
                    ROUND(AVG(tokens_out)::numeric, 0)::double precision as avg_tokens_out,
                    ROUND(AVG(cost_usd)::numeric, 6)::double precision as avg_cost,
                    ROUND(SUM(cost_usd)::numeric, 4)::double precision as total_cost
                FROM pipeline_agent_traces
                WHERE created_at > $1
                    AND (tokens_in > 0 OR tokens_out > 0)
                GROUP BY agent_type
                ORDER BY total_cost DESC"#,
                &[&since],
            )
            .await
            .map_err(|e| format!("PG query_agent_cost_stats: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, i64>(1),
                    r.get::<_, f64>(2),
                    r.get::<_, f64>(3),
                    r.get::<_, f64>(4),
                    r.get::<_, f64>(5),
                )
            })
            .collect())
    }

    /// Query agents that have traces but zero tokens (instrumentation gap).
    pub async fn query_zero_token_agents(&self, since: &str) -> Result<Vec<(String, i64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT agent_type, COUNT(*)::bigint as run_count
                FROM pipeline_agent_traces
                WHERE created_at > $1
                    AND tokens_in = 0 AND tokens_out = 0
                GROUP BY agent_type
                HAVING COUNT(*) >= 5
                ORDER BY run_count DESC"#,
                &[&since],
            )
            .await
            .map_err(|e| format!("PG query_zero_token_agents: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1)))
            .collect())
    }

    /// Query agents that primarily use CLI sessions (expensive) vs API.
    ///
    /// Joins `pipeline_agent_traces` with `iteration_logs` to detect high-cost
    /// CLI provider usage. Returns `(agent_type, cli_runs, total_runs, avg_cli_cost, avg_api_cost)`.
    pub async fn query_cli_heavy_agents(
        &self,
        since: &str,
    ) -> Result<Vec<(String, i64, i64, f64, f64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Check if iteration_logs table exists (graceful if not yet migrated)
        let exists: bool = conn
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'iteration_logs')",
                &[],
            )
            .await
            .map(|r| r.get::<_, bool>(0))
            .unwrap_or(false);

        if !exists {
            return Ok(Vec::new());
        }

        let rows = conn
            .query(
                r#"SELECT
                    pat.agent_type,
                    SUM(CASE WHEN il.provider_used LIKE '%cli%' THEN 1 ELSE 0 END)::bigint AS cli_runs,
                    COUNT(*)::bigint AS total_runs,
                    COALESCE(AVG(CASE WHEN il.provider_used LIKE '%cli%' THEN pat.cost_usd END), 0)::double precision AS avg_cli_cost,
                    COALESCE(AVG(CASE WHEN il.provider_used NOT LIKE '%cli%' THEN pat.cost_usd END), 0)::double precision AS avg_api_cost
                FROM pipeline_agent_traces pat
                LEFT JOIN iteration_logs il ON il.task_run_id = pat.task_run_id
                WHERE pat.created_at > $1
                    AND (pat.tokens_in > 0 OR pat.tokens_out > 0)
                GROUP BY pat.agent_type
                HAVING COUNT(*) >= 5"#,
                &[&since],
            )
            .await
            .map_err(|e| format!("PG query_cli_heavy_agents: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, i64>(1),
                    r.get::<_, i64>(2),
                    r.get::<_, f64>(3),
                    r.get::<_, f64>(4),
                )
            })
            .collect())
    }

    /// Compare cost over recent vs previous 15-day window.
    pub async fn compute_cost_trend(&self, mid: &str, start: &str) -> Result<(f64, f64), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let recent_row = conn
            .query_one(
                "SELECT COALESCE(SUM(cost_usd), 0)::double precision FROM pipeline_agent_traces WHERE created_at > $1",
                &[&mid],
            )
            .await
            .map_err(|e| format!("PG compute_cost_trend recent: {}", e))?;
        let recent_cost: f64 = recent_row.get(0);

        let previous_row = conn
            .query_one(
                "SELECT COALESCE(SUM(cost_usd), 0)::double precision FROM pipeline_agent_traces WHERE created_at > $1 AND created_at <= $2",
                &[&start, &mid],
            )
            .await
            .map_err(|e| format!("PG compute_cost_trend previous: {}", e))?;
        let previous_cost: f64 = previous_row.get(0);
        Ok((recent_cost, previous_cost))
    }

    // ========================================================================
    // Comparison bridge queries
    // ========================================================================

    /// Load a comparison run by ID (for meta-optimizer bridge).
    pub async fn get_comparison_run_for_bridge(
        &self,
        comparison_id: &str,
    ) -> Result<
        Option<(
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
            String,
        )>,
        String,
    > {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT entries_json, comparison_report, recommendation_json,
                          status, workflow_id, workflow_name, created_at
                   FROM comparison_runs WHERE id = $1"#,
                &[&comparison_id],
            )
            .await
            .map_err(|e| format!("PG get_comparison_run: {}", e))?;
        Ok(row.map(|r| {
            (
                r.get::<_, String>(0),
                r.get::<_, Option<String>>(1),
                r.get::<_, Option<String>>(2),
                r.get::<_, String>(3),
                r.get::<_, String>(4),
                r.get::<_, String>(5),
                r.get::<_, String>(6),
            )
        }))
    }

    /// Update comparison_runs to link recommendation.
    pub async fn update_comparison_recommendation_link(
        &self,
        recommendation_id: &str,
        comparison_id: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE comparison_runs SET recommendation_id = $1, source = 'meta_optimizer' WHERE id = $2",
            &[&recommendation_id, &comparison_id],
        )
        .await
        .map_err(|e| format!("PG update_comparison_recommendation_link: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Parser — auto-apply queries
    // ========================================================================

    /// Auto-reject finding recommendations.
    pub async fn reject_finding_recommendations(
        &self,
        optimizer_run_id: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let affected = conn
            .execute(
                r#"UPDATE meta_optimizer_recommendations
                   SET status = 'rejected',
                       outcome_after_apply = 'Auto-rejected: findings are diagnostic observations, not actionable changes'
                   WHERE status = 'pending'
                     AND recommendation_type = 'finding'
                     AND ($1::text IS NULL OR optimizer_run_id = $1)"#,
                &[&optimizer_run_id as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| format!("PG reject_finding_recommendations: {}", e))?;
        Ok(affected as i64)
    }

    /// Query pending recommendations by type and min confidence.
    pub async fn query_pending_recommendations_by_type(
        &self,
        recommendation_types: &[&str],
        min_confidence: f64,
        optimizer_run_id: Option<&str>,
    ) -> Result<Vec<(String, f64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let types_str: Vec<String> = recommendation_types.iter().map(|s| s.to_string()).collect();
        // Build IN clause dynamically
        let placeholders: Vec<String> = (0..types_str.len())
            .map(|i| format!("${}", i + 3))
            .collect();
        let in_clause = placeholders.join(", ");
        let sql = format!(
            r#"SELECT id, confidence FROM meta_optimizer_recommendations
               WHERE status = 'pending'
                 AND recommendation_type IN ({})
                 AND confidence >= $1
                 AND ($2::text IS NULL OR optimizer_run_id = $2)
               ORDER BY confidence DESC"#,
            in_clause
        );

        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
        params.push(Box::new(min_confidence));
        params.push(Box::new(
            optimizer_run_id.map(|s| s.to_string()) as Option<String>
        ));
        for t in &types_str {
            params.push(Box::new(t.clone()));
        }
        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = conn
            .query(&sql, &param_refs)
            .await
            .map_err(|e| format!("PG query_pending_recommendations_by_type: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, f64>(1)))
            .collect())
    }

    /// Count rejected recommendations with a given title.
    pub async fn count_rejected_by_title(&self, title: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*)::bigint FROM meta_optimizer_recommendations WHERE title = $1 AND status = 'rejected'",
                &[&title],
            )
            .await
            .map_err(|e| format!("PG count_rejected_by_title: {}", e))?;
        Ok(row.get(0))
    }

    // ========================================================================
    // Golden dataset — build from history (pipeline_agent_traces)
    // ========================================================================

    /// Query recent successful pipeline traces for golden dataset building.
    pub async fn query_golden_entries_from_traces(
        &self,
        agent_type: &str,
        since: &str,
        limit: i64,
    ) -> Result<Vec<(String, String, i64, bool)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT pat.task_run_id, tr.name, pat.duration_ms, pat.success
                   FROM pipeline_agent_traces pat
                   JOIN task_runs tr ON pat.task_run_id = tr.id
                   WHERE pat.agent_type = $1
                     AND pat.success = true
                     AND pat.created_at > $2
                   ORDER BY pat.created_at DESC
                   LIMIT $3"#,
                &[&agent_type, &since, &limit],
            )
            .await
            .map_err(|e| format!("PG query_golden_entries_from_traces: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, i64>(2),
                    r.get::<_, bool>(3),
                )
            })
            .collect())
    }

    /// List golden datasets for a specific agent type (for robustness regression cases).
    pub async fn query_golden_datasets_by_agent(
        &self,
        agent_type: &str,
    ) -> Result<Vec<(String, String, String, String, String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, agent_type, name, entries_json, created_at, updated_at
                   FROM golden_datasets WHERE agent_type = $1 ORDER BY updated_at DESC"#,
                &[&agent_type],
            )
            .await
            .map_err(|e| format!("PG query_golden_datasets_by_agent: {}", e))?;
        Ok(rows
            .iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
            .collect())
    }

    // ========================================================================
    // Trigger — optimizer run count
    // ========================================================================

    /// Count optimizer runs for a given type (for style rotation).
    pub async fn count_optimizer_runs_by_type(&self, optimizer_type: &str) -> Result<i64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                "SELECT COUNT(*)::bigint FROM meta_optimizer_runs WHERE optimizer_type = $1",
                &[&optimizer_type],
            )
            .await
            .map_err(|e| format!("PG count_optimizer_runs_by_type: {}", e))?;
        Ok(row.get(0))
    }

    // ========================================================================
    // Recommendations — agentic score evaluation
    // ========================================================================

    /// Get pre/post agentic scores for a recommendation.
    pub async fn get_agentic_score_evaluation(
        &self,
        applied_at: &str,
    ) -> Result<Option<(f64, f64, i64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Count post-apply runs
        let post_count_row = conn
            .query_one(
                r#"SELECT COUNT(*)::bigint FROM learning_outcomes
                   WHERE created_at > $1 AND composite_agentic_score IS NOT NULL
                     AND (iterations IS NULL OR iterations > 0)"#,
                &[&applied_at],
            )
            .await
            .map_err(|e| format!("PG agentic score post count: {}", e))?;
        let post_count: i64 = post_count_row.get(0);

        if post_count < 5 {
            return Ok(None);
        }

        // Pre-apply average
        let pre_row = conn
            .query_one(
                r#"SELECT AVG(composite_agentic_score)::double precision FROM learning_outcomes
                   WHERE created_at <= $1 AND composite_agentic_score IS NOT NULL
                     AND (iterations IS NULL OR iterations > 0)"#,
                &[&applied_at],
            )
            .await
            .map_err(|e| format!("PG agentic score pre avg: {}", e))?;
        let pre_avg: Option<f64> = pre_row.get(0);

        // Post-apply average
        let post_row = conn
            .query_one(
                r#"SELECT AVG(composite_agentic_score)::double precision FROM learning_outcomes
                   WHERE created_at > $1 AND composite_agentic_score IS NOT NULL
                     AND (iterations IS NULL OR iterations > 0)"#,
                &[&applied_at],
            )
            .await
            .map_err(|e| format!("PG agentic score post avg: {}", e))?;
        let post_avg: Option<f64> = post_row.get(0);

        match (pre_avg, post_avg) {
            (Some(pre), Some(post)) if pre > 0.0 => Ok(Some((pre, post, post_count))),
            _ => Ok(None),
        }
    }

    /// Update recommendation outcome with agentic score evaluation.
    pub async fn update_recommendation_outcome_json(
        &self,
        recommendation_id: &str,
        outcome_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE meta_optimizer_recommendations SET outcome_after_apply = $1 WHERE id = $2",
            &[&outcome_json, &recommendation_id],
        )
        .await
        .map_err(|e| format!("PG update_recommendation_outcome_json: {}", e))?;
        Ok(())
    }

    /// Update recommendation status with applied_at timestamp.
    pub async fn apply_recommendation_with_timestamp(
        &self,
        recommendation_id: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE meta_optimizer_recommendations SET status = 'applied', applied_at = $1 WHERE id = $2 AND status IN ('pending', 'canary')",
            &[&now, &recommendation_id],
        )
        .await
        .map_err(|e| format!("PG apply_recommendation_with_timestamp: {}", e))?;
        Ok(())
    }

    // ========================================================================
    // Snapshot capture (PG-primary)
    // ========================================================================

    /// Capture a performance snapshot from learning_outcomes + phase_token_usage (PG-primary).
    pub async fn pg_capture_snapshot(
        &self,
        snapshot_type: &str,
        recommendation_id: Option<&str>,
        lookback_days: i64,
        category_filter: &str,
    ) -> Result<MetaOptimizerSnapshot, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let id = format!("mos-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now();
        let period_end = now.to_rfc3339();
        let period_start = (now - chrono::Duration::days(lookback_days)).to_rfc3339();

        // Aggregate metrics from learning_outcomes
        let agg_sql = format!(
            r#"SELECT
                   COUNT(*),
                   SUM(CASE WHEN lo.status = 'success' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN lo.status = 'failure' THEN 1 ELSE 0 END),
                   SUM(CASE WHEN lo.status = 'partial' THEN 1 ELSE 0 END),
                   COALESCE(AVG(lo.duration_secs), 0.0),
                   COALESCE(AVG(lo.iterations::double precision), 0.0)
               FROM learning_outcomes lo
               JOIN task_runs tr ON lo.task_id = tr.id
               WHERE lo.created_at > $1
                 AND (lo.iterations IS NULL OR lo.iterations > 0){}"#,
            category_filter
        );
        let agg_row = conn
            .query_one(&agg_sql, &[&period_start])
            .await
            .map_err(|e| format!("PG capture_snapshot agg: {}", e))?;
        let total_runs: i64 = agg_row.get(0);
        let successful_runs: i64 = agg_row.get(1);
        let failed_runs: i64 = agg_row.get(2);
        let partial_runs: i64 = agg_row.get(3);
        let avg_duration: f64 = agg_row.get(4);
        let avg_iterations: f64 = agg_row.get(5);

        let success_rate = if total_runs > 0 {
            successful_runs as f64 / total_runs as f64
        } else {
            0.0
        };

        // Average cost per run
        let cost_sql = format!(
            r#"SELECT COALESCE(AVG(run_cost), 0.0) FROM (
                   SELECT lo.task_id, COALESCE(SUM(ptu.cost_cents), 0) as run_cost
                   FROM learning_outcomes lo
                   JOIN task_runs tr ON lo.task_id = tr.id
                   LEFT JOIN phase_token_usage ptu ON ptu.task_run_id = lo.task_id
                   WHERE lo.created_at > $1{}
                   GROUP BY lo.task_id
               ) sub"#,
            category_filter
        );
        let cost_row = conn
            .query_one(&cost_sql, &[&period_start])
            .await
            .map_err(|e| format!("PG capture_snapshot cost: {}", e))?;
        let avg_cost_cents: f64 = cost_row.get(0);

        // Spec compliance average
        let sc_row = conn
            .query_one(
                "SELECT AVG(overall_score) FROM spec_compliance_results WHERE created_at > $1",
                &[&period_start],
            )
            .await
            .ok();
        let avg_spec_compliance: Option<f64> = sc_row.and_then(|r| r.get(0));

        // Composite agentic score average
        let cas_row = conn.query_one(
            "SELECT AVG(composite_agentic_score) FROM learning_outcomes WHERE created_at > $1 AND composite_agentic_score IS NOT NULL",
            &[&period_start],
        ).await.ok();
        let avg_composite_agentic_score: Option<f64> = cas_row.and_then(|r| r.get(0));

        let metrics = crate::meta_optimizer::snapshots::SnapshotMetrics {
            success_rate,
            avg_duration_secs: avg_duration,
            avg_iterations,
            avg_cost_cents,
            total_runs,
            successful_runs,
            failed_runs,
            partial_runs,
            avg_spec_compliance,
            avg_composite_agentic_score,
        };
        let metrics_json = serde_json::to_string(&metrics)
            .map_err(|e| format!("Failed to serialize metrics: {}", e))?;

        // Per-architecture breakdown
        let breakdown_sql = format!(
            r#"SELECT lo.workflow_architecture, COUNT(*) as cnt,
                      SUM(CASE WHEN lo.status = 'success' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) as sr,
                      AVG(lo.duration_secs) as avg_dur,
                      AVG(lo.iterations::double precision) as avg_iter
               FROM learning_outcomes lo
               JOIN task_runs tr ON lo.task_id = tr.id
               WHERE lo.created_at > $1 AND lo.workflow_architecture IS NOT NULL
                 AND (lo.iterations IS NULL OR lo.iterations > 0){}
               GROUP BY lo.workflow_architecture"#,
            category_filter
        );
        let breakdown_rows = conn
            .query(&breakdown_sql, &[&period_start])
            .await
            .map_err(|e| format!("PG capture_snapshot breakdown: {}", e))?;
        let breakdown_json: Option<String> = if breakdown_rows.is_empty() {
            None
        } else {
            let entries: Vec<serde_json::Value> = breakdown_rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "architecture": row.get::<_, String>(0),
                        "count": row.get::<_, i64>(1),
                        "success_rate": row.get::<_, f64>(2),
                        "avg_duration_secs": row.get::<_, f64>(3),
                        "avg_iterations": row.get::<_, f64>(4),
                    })
                })
                .collect();
            Some(
                serde_json::to_string(&entries)
                    .map_err(|e| format!("Failed to serialize breakdown: {}", e))?,
            )
        };

        // Insert snapshot row
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
                &breakdown_json as &(dyn tokio_postgres::types::ToSql + Sync),
                &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &total_runs,
                &period_end,
            ],
        )
        .await
        .map_err(|e| format!("PG capture_snapshot insert: {}", e))?;

        info!(
            "PG: Captured {} snapshot {} ({} runs, success_rate={:.1}%)",
            snapshot_type,
            id,
            total_runs,
            success_rate * 100.0
        );

        Ok(MetaOptimizerSnapshot {
            id,
            snapshot_type: snapshot_type.to_string(),
            period_start,
            period_end: period_end.clone(),
            metrics_json,
            breakdown_json,
            recommendation_id: recommendation_id.map(|s| s.to_string()),
            runs_included: total_runs,
            created_at: period_end,
        })
    }

    /// Get recommendation target_agent and applied_at for outcome evaluation.
    pub async fn get_recommendation_outcome_info(
        &self,
        recommendation_id: &str,
    ) -> Result<(Option<String>, Option<String>), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_one(
            "SELECT target_agent, applied_at::TEXT FROM meta_optimizer_recommendations WHERE id = $1",
            &[&recommendation_id],
        ).await.map_err(|e| format!("Recommendation not found: {}", e))?;
        Ok((row.get(0), row.get(1)))
    }

    /// Get a post_apply snapshot for a recommendation.
    pub async fn get_post_apply_snapshot(
        &self,
        recommendation_id: &str,
    ) -> Result<Option<MetaOptimizerSnapshot>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, snapshot_type, period_start, period_end, metrics_json,
                      breakdown_json, recommendation_id, runs_included, created_at::TEXT
               FROM meta_optimizer_snapshots
               WHERE recommendation_id = $1 AND snapshot_type = 'post_apply'
               ORDER BY created_at DESC LIMIT 1"#,
                &[&recommendation_id],
            )
            .await
            .map_err(|e| format!("PG get_post_apply_snapshot: {}", e))?;
        Ok(row.map(|r| MetaOptimizerSnapshot {
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

    /// Get agent trace aggregates for a time period (PG equivalent of pipeline_traces::get_agent_aggregates_for_period).
    pub async fn get_agent_aggregates_for_period(
        &self,
        agent_type: &str,
        start: &str,
        end: &str,
    ) -> Result<Option<crate::database::pipeline_traces::AgentTraceAggregate>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                r#"SELECT
                agent_type,
                COUNT(*) as run_count,
                AVG(duration_ms) as avg_duration_ms,
                AVG(cost_usd) as avg_cost_usd,
                SUM(CASE WHEN downstream_success = 1 THEN 1 ELSE 0 END) as success_count,
                SUM(CASE WHEN downstream_success = 0 THEN 1 ELSE 0 END) as failure_count,
                AVG(tokens_in::double precision) as avg_tokens_in,
                AVG(tokens_out::double precision) as avg_tokens_out
            FROM pipeline_agent_traces
            WHERE agent_type = $1 AND created_at >= $2 AND created_at <= $3"#,
                &[&agent_type, &start, &end],
            )
            .await
            .map_err(|e| format!("PG get_agent_aggregates_for_period: {}", e))?;
        let run_count: i64 = row.get(1);
        if run_count == 0 {
            return Ok(None);
        }
        Ok(Some(
            crate::database::pipeline_traces::AgentTraceAggregate {
                agent_type: row.get(0),
                run_count,
                avg_duration_ms: row.get(2),
                avg_cost_usd: row.get(3),
                success_count: row.get(4),
                failure_count: row.get(5),
                avg_tokens_in: row.get(6),
                avg_tokens_out: row.get(7),
            },
        ))
    }

    /// Record prompt evolution with full parameters (including baseline_prompt_hash and consecutive_rejections).
    pub async fn record_prompt_evolution_full(
        &self,
        id: &str,
        agent_type: &str,
        parent_variant_id: Option<&str>,
        variant_id: &str,
        recommendation_id: Option<&str>,
        critique: Option<&str>,
        changes_summary: Option<&str>,
        score_before: Option<f64>,
        baseline_prompt_hash: Option<&str>,
        consecutive_rejections: i32,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            r#"INSERT INTO prompt_evolution
               (id, agent_type, parent_variant_id, variant_id, recommendation_id,
                critique, changes_summary, canary_verdict, score_before, score_after,
                baseline_prompt_hash, consecutive_rejections, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, $8, NULL, $9, $10, $11)
               ON CONFLICT (id) DO NOTHING"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &agent_type,
                &parent_variant_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &variant_id,
                &recommendation_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &critique as &(dyn tokio_postgres::types::ToSql + Sync),
                &changes_summary as &(dyn tokio_postgres::types::ToSql + Sync),
                &score_before as &(dyn tokio_postgres::types::ToSql + Sync),
                &baseline_prompt_hash as &(dyn tokio_postgres::types::ToSql + Sync),
                &consecutive_rejections,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG record_prompt_evolution_full: {}", e))?;
        info!(
            "PG: Recorded prompt evolution {} for agent {} (consecutive_rejections={})",
            id, agent_type, consecutive_rejections
        );
        Ok(())
    }

    /// Look up recommendation_id for a variant (used by update_verdict_by_variant dual-write).
    pub async fn get_recommendation_id_for_variant(
        &self,
        variant_id: &str,
        verdict: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT recommendation_id FROM prompt_evolution WHERE variant_id = $1 AND canary_verdict = $2 ORDER BY created_at DESC LIMIT 1",
            &[&variant_id, &verdict],
        ).await.map_err(|e| format!("PG get_recommendation_id_for_variant: {}", e))?;
        Ok(row.and_then(|r| r.get(0)))
    }

    // ========================================================================
    // Trigger — should_launch_optimizer (PG-primary)
    // ========================================================================

    /// Check whether a specific optimizer should be launched (PG version).
    ///
    /// Guards:
    /// 1. No optimizer of this type already running
    /// 2. Meta-optimizer enabled in settings
    /// 3. Threshold met: completed runs since last optimizer run >= N
    /// 4. Source run is not meta_optimizer/fixer/reflection/follow_up
    pub async fn should_launch_optimizer(
        &self,
        optimizer_type: &str,
        source_task_run_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Guard 0: Check if any meta-optimizer is already running
        let row = conn.query_one(
            "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_meta_optimizer = true AND workflow_type = 'unified'",
            &[],
        ).await.map_err(|e| format!("PG should_launch: {}", e))?;
        let has_running: bool = row.get(0);
        if has_running {
            return Ok(false);
        }

        // Guard 1: Source must not be a meta-optimizer, fixer, reflection, or follow-up run
        let row = conn
            .query_opt(
                r#"SELECT (COALESCE(is_meta_optimizer, false)
                    OR COALESCE(is_fixer, false)
                    OR COALESCE(is_reflection, false)
                    OR COALESCE(is_follow_up, false))
               FROM task_runs WHERE id = $1"#,
                &[&source_task_run_id],
            )
            .await
            .map_err(|e| format!("PG should_launch: {}", e))?;
        let is_excluded: bool = row.map(|r| r.get(0)).unwrap_or(true);
        if is_excluded {
            return Ok(false);
        }

        // Guard 2: Meta-optimizer must be enabled in settings
        let dev_mode_row = conn
            .query_opt("SELECT value FROM settings WHERE key = 'dev_mode'", &[])
            .await
            .map_err(|e| format!("PG should_launch: {}", e))?;

        let (enabled, threshold) = match dev_mode_row {
            Some(r) => {
                let val: String = r.get(0);
                let parsed = serde_json::from_str::<serde_json::Value>(&val).ok();
                let en = parsed
                    .as_ref()
                    .and_then(|v| v.get("meta_optimizer_enabled"))
                    .and_then(|v| {
                        v.as_bool()
                            .or_else(|| v.as_str().map(|s| s == "true" || s == "1"))
                    })
                    .unwrap_or(true);
                let th = parsed
                    .as_ref()
                    .and_then(|v| v.get("meta_optimizer_threshold"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(2);
                (en, th)
            }
            None => (true, 2i64),
        };
        if !enabled {
            return Ok(false);
        }

        // Guard 3: Threshold check
        let last_run_row = conn.query_opt(
            "SELECT MAX(created_at)::TEXT FROM meta_optimizer_runs WHERE optimizer_type = $1 AND status = 'complete'",
            &[&optimizer_type],
        ).await.map_err(|e| format!("PG should_launch: {}", e))?;
        let last_optimizer_run_at: Option<String> = last_run_row.and_then(|r| r.get(0));

        let completed_since: i64 = if let Some(ref since) = last_optimizer_run_at {
            let row = conn
                .query_one(
                    r#"SELECT COUNT(*) FROM task_runs
                   WHERE status IN ('complete', 'failed')
                     AND COALESCE(is_meta_optimizer, false) = false
                     AND COALESCE(is_fixer, false) = false
                     AND COALESCE(is_reflection, false) = false
                     AND COALESCE(is_follow_up, false) = false
                     AND workflow_type = 'unified'
                     AND created_at > $1"#,
                    &[since],
                )
                .await
                .map_err(|e| format!("PG should_launch: {}", e))?;
            row.get(0)
        } else {
            let row = conn
                .query_one(
                    r#"SELECT COUNT(*) FROM task_runs
                   WHERE status IN ('complete', 'failed')
                     AND COALESCE(is_meta_optimizer, false) = false
                     AND COALESCE(is_fixer, false) = false
                     AND COALESCE(is_reflection, false) = false
                     AND COALESCE(is_follow_up, false) = false
                     AND workflow_type = 'unified'"#,
                    &[],
                )
                .await
                .map_err(|e| format!("PG should_launch: {}", e))?;
            row.get(0)
        };

        Ok(completed_since >= threshold)
    }

    // ========================================================================
    // Golden dataset — build from history (PG-primary)
    // ========================================================================

    /// Query golden entries from pipeline_agent_traces + task_runs.
    pub async fn build_golden_entries_from_history(
        &self,
        agent_type: &str,
        max_entries: i64,
    ) -> Result<Vec<(String, String, i64, bool)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let period_start = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();

        let rows = conn
            .query(
                r#"SELECT pat.task_run_id, tr.task_name, pat.duration_ms, pat.downstream_success
               FROM pipeline_agent_traces pat
               JOIN task_runs tr ON pat.task_run_id = tr.id
               WHERE pat.agent_type = $1
                 AND pat.downstream_success = true
                 AND pat.created_at > $2
               ORDER BY pat.created_at DESC
               LIMIT $3"#,
                &[&agent_type, &period_start, &max_entries],
            )
            .await
            .map_err(|e| format!("PG build_golden_entries: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let task_run_id: String = r.get(0);
                let task_name: String = r.get(1);
                let duration_ms: i64 = r.get(2);
                let success: bool = r.get::<_, Option<bool>>(3).unwrap_or(false);
                (task_run_id, task_name, duration_ms, success)
            })
            .collect())
    }

    // ========================================================================
    // Prompt extraction — PG-primary queries
    // ========================================================================

    /// Extract prompt samples from recent runs (PG version).
    pub async fn extract_prompt_samples(
        &self,
        limit: i64,
    ) -> Result<Vec<(String, String, String, f64, String, i64, f64)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT
                   ptu.task_run_id,
                   ptu.phase,
                   COALESCE(tr.workflow_name, ptu.phase) AS agent_type,
                   COALESCE(lo.composite_agentic_score, 0.0) AS outcome_score,
                   COALESCE(lo.status, tr.status) AS outcome_status,
                   COALESCE(ptu.iteration, 0) AS iteration,
                   COALESCE(ptu.cost_cents, 0)::float8 / 100.0 AS cost_usd
               FROM phase_token_usage ptu
               INNER JOIN task_runs tr ON tr.id = ptu.task_run_id
               LEFT JOIN learning_outcomes lo ON lo.task_id = tr.id
               WHERE tr.status IN ('complete', 'failed')
                 AND COALESCE(tr.is_meta_optimizer, false) = false
                 AND COALESCE(tr.is_fixer, false) = false
                 AND COALESCE(tr.is_reflection, false) = false
                 AND COALESCE(tr.is_follow_up, false) = false
                 AND ptu.phase IN ('generation', 'verification', 'agentic')
               ORDER BY outcome_score ASC
               LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG extract_prompt_samples: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, f64>(3),
                    r.get::<_, String>(4),
                    r.get::<_, i64>(5),
                    r.get::<_, f64>(6),
                )
            })
            .collect())
    }

    /// Collect failure and success evidence samples for a specific prompt group (PG version).
    pub async fn collect_evidence_samples(
        &self,
        phase: &str,
        agent_type: &str,
        max_failures: i64,
        max_successes: i64,
    ) -> Result<
        (
            Vec<(String, String, String, f64, String, i64, f64)>,
            Vec<(String, String, String, f64, String, i64, f64)>,
        ),
        String,
    > {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let agent_filter = if agent_type.is_empty() {
            String::new()
        } else {
            " AND COALESCE(tr.workflow_name, ptu.phase) = $3".to_string()
        };

        // Failures (score < 0.5)
        let fail_sql = format!(
            r#"SELECT
                   ptu.task_run_id,
                   ptu.phase,
                   COALESCE(tr.workflow_name, ptu.phase),
                   COALESCE(lo.composite_agentic_score, 0.0),
                   COALESCE(lo.status, tr.status),
                   COALESCE(ptu.iteration, 0),
                   COALESCE(ptu.cost_cents, 0)::float8 / 100.0
               FROM phase_token_usage ptu
               INNER JOIN task_runs tr ON tr.id = ptu.task_run_id
               LEFT JOIN learning_outcomes lo ON lo.task_id = tr.id
               WHERE ptu.phase = $1
                 AND tr.status IN ('complete', 'failed')
                 AND COALESCE(tr.is_meta_optimizer, false) = false
                 AND COALESCE(tr.is_fixer, false) = false
                 AND COALESCE(lo.composite_agentic_score, 0.0) < 0.5
                 {}
               ORDER BY lo.created_at DESC
               LIMIT $2"#,
            agent_filter
        );

        let failures: Vec<(String, String, String, f64, String, i64, f64)> =
            if agent_type.is_empty() {
                let rows = conn
                    .query(&fail_sql, &[&phase, &max_failures])
                    .await
                    .map_err(|e| format!("PG failures query: {}", e))?;
                rows.iter()
                    .map(|r| {
                        (
                            r.get(0),
                            r.get(1),
                            r.get(2),
                            r.get(3),
                            r.get(4),
                            r.get(5),
                            r.get(6),
                        )
                    })
                    .collect()
            } else {
                let rows = conn
                    .query(&fail_sql, &[&phase, &max_failures, &agent_type])
                    .await
                    .map_err(|e| format!("PG failures query: {}", e))?;
                rows.iter()
                    .map(|r| {
                        (
                            r.get(0),
                            r.get(1),
                            r.get(2),
                            r.get(3),
                            r.get(4),
                            r.get(5),
                            r.get(6),
                        )
                    })
                    .collect()
            };

        // Successes (score > 0.8)
        let success_sql = format!(
            r#"SELECT
                   ptu.task_run_id,
                   ptu.phase,
                   COALESCE(tr.workflow_name, ptu.phase),
                   COALESCE(lo.composite_agentic_score, 0.0),
                   COALESCE(lo.status, tr.status),
                   COALESCE(ptu.iteration, 0),
                   COALESCE(ptu.cost_cents, 0)::float8 / 100.0
               FROM phase_token_usage ptu
               INNER JOIN task_runs tr ON tr.id = ptu.task_run_id
               LEFT JOIN learning_outcomes lo ON lo.task_id = tr.id
               WHERE ptu.phase = $1
                 AND tr.status IN ('complete', 'failed')
                 AND COALESCE(tr.is_meta_optimizer, false) = false
                 AND COALESCE(tr.is_fixer, false) = false
                 AND COALESCE(lo.composite_agentic_score, 0.0) > 0.8
                 {}
               ORDER BY lo.created_at DESC
               LIMIT $2"#,
            agent_filter
        );

        let successes: Vec<(String, String, String, f64, String, i64, f64)> =
            if agent_type.is_empty() {
                let rows = conn
                    .query(&success_sql, &[&phase, &max_successes])
                    .await
                    .map_err(|e| format!("PG successes query: {}", e))?;
                rows.iter()
                    .map(|r| {
                        (
                            r.get(0),
                            r.get(1),
                            r.get(2),
                            r.get(3),
                            r.get(4),
                            r.get(5),
                            r.get(6),
                        )
                    })
                    .collect()
            } else {
                let rows = conn
                    .query(&success_sql, &[&phase, &max_successes, &agent_type])
                    .await
                    .map_err(|e| format!("PG successes query: {}", e))?;
                rows.iter()
                    .map(|r| {
                        (
                            r.get(0),
                            r.get(1),
                            r.get(2),
                            r.get(3),
                            r.get(4),
                            r.get(5),
                            r.get(6),
                        )
                    })
                    .collect()
            };

        Ok((failures, successes))
    }

    // ========================================================================
    // Comparison bridge — most recent active workflow
    // ========================================================================

    /// Get the most recent active (non-deleted) workflow ID.
    pub async fn get_most_recent_workflow_id(&self) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn.query_opt(
            "SELECT id FROM unified_workflows WHERE is_deleted = false ORDER BY updated_at DESC LIMIT 1",
            &[],
        ).await.map_err(|e| format!("PG get_most_recent_workflow_id: {}", e))?;
        Ok(row.map(|r| r.get(0)))
    }

    // ========================================================================
    // Failure analysis — PG-primary complex analytics
    // ========================================================================

    /// Get comprehensive failure analysis over a time period (PG version).
    pub async fn get_failure_analysis(
        &self,
        since: &str,
        category: &crate::meta_optimizer::types::WorkflowCategory,
    ) -> Result<crate::meta_optimizer::failure_analysis::FailureAnalysis, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let filter = category.pg_sql_filter("tr");
        let task_runs_filter = category.pg_sql_filter("task_runs");

        // Run totals
        let total_runs: i64 = conn
            .query_one(
                &format!(
                    "SELECT COUNT(*) FROM task_runs WHERE created_at > $1{}",
                    task_runs_filter.replace("task_runs.", "")
                ),
                &[&since],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        let failed_runs: i64 = conn.query_one(
            &format!("SELECT COUNT(*) FROM task_runs WHERE status IN ('failed', 'stopped') AND created_at > $1{}", task_runs_filter.replace("task_runs.", "")),
            &[&since],
        ).await.map(|r| r.get(0)).unwrap_or(0);

        let failure_rate = if total_runs > 0 {
            100.0 * failed_runs as f64 / total_runs as f64
        } else {
            0.0
        };

        // Abort reasons from learning_outcomes
        let mut abort_reasons: Vec<crate::meta_optimizer::failure_analysis::AbortReason> =
            Vec::new();
        if let Ok(rows) = conn
            .query(
                &format!(
                    r#"SELECT
                     CASE
                         WHEN lo.status = 'partial' THEN 'stopped_by_user'
                         WHEN lo.error_type IS NOT NULL THEN lo.error_type
                         ELSE 'unknown_failure'
                     END as reason,
                     COUNT(*) as count
                 FROM learning_outcomes lo
                 JOIN task_runs tr ON lo.task_id = tr.id
                 WHERE lo.status != 'success' AND lo.created_at > $1{}
                 GROUP BY reason
                 ORDER BY count DESC"#,
                    filter
                ),
                &[&since],
            )
            .await
        {
            for r in &rows {
                abort_reasons.push(crate::meta_optimizer::failure_analysis::AbortReason {
                    reason: r.get(0),
                    count: r.get(1),
                    percentage: 0.0,
                });
            }
        }

        // Abort reasons from task_runs
        if let Ok(rows) = conn.query(
            &format!(
                r#"SELECT
                     CASE
                         WHEN error_message LIKE '%max iterations%' OR error_message LIKE '%max_iterations%' OR error_message LIKE '%iteration budget%' OR error_message LIKE '%iterations exhausted%' THEN 'max_iterations_reached'
                         WHEN error_message LIKE '%Max sessions%' OR error_message LIKE '%sessions exhausted%' THEN 'max_sessions_reached'
                         WHEN error_message LIKE '%setup failed%' THEN 'setup_failure'
                         WHEN error_message LIKE '%Unfixable errors%' THEN 'unfixable_errors'
                         WHEN error_message LIKE '%stopped%' OR status = 'stopped' THEN 'stopped_by_user'
                         WHEN error_message LIKE '%Critical failure%' OR error_message LIKE '%critical%' THEN 'critical_failure'
                         WHEN error_message LIKE '%no iterations ran%' THEN 'zero_iterations'
                         WHEN error_message LIKE '%interrupted%' OR error_message LIKE '%restart%' THEN 'interrupted'
                         WHEN error_message IS NOT NULL THEN 'runtime_error'
                         WHEN goal_achieved = 0 THEN 'goal_not_achieved'
                         ELSE 'unknown'
                     END as reason,
                     COUNT(*) as count
                 FROM task_runs
                 WHERE status IN ('failed', 'stopped') AND created_at > $1{}
                 GROUP BY reason
                 ORDER BY count DESC"#,
                task_runs_filter
            ),
            &[&since],
        ).await {
            for r in &rows {
                let reason: String = r.get(0);
                let count: i64 = r.get(1);
                if let Some(existing) = abort_reasons.iter_mut().find(|a| a.reason == reason) {
                    existing.count += count;
                } else {
                    abort_reasons.push(crate::meta_optimizer::failure_analysis::AbortReason {
                        reason, count, percentage: 0.0,
                    });
                }
            }
        }
        let total_abort: i64 = abort_reasons.iter().map(|r| r.count).sum();
        if total_abort > 0 {
            for r in &mut abort_reasons {
                r.percentage = 100.0 * r.count as f64 / total_abort as f64;
            }
        }
        abort_reasons.sort_by(|a, b| b.count.cmp(&a.count));

        // Verification failures
        let mut verification_failures = Vec::new();
        if let Ok(rows) = conn.query(
            &format!(
                r#"SELECT vpr.iteration,
                     SUM(vpr.total_steps) as total_checks,
                     SUM(vpr.failed_steps) as failed_checks,
                     ROUND(100.0 * SUM(vpr.failed_steps) / NULLIF(SUM(vpr.total_steps), 0), 1) as failure_rate,
                     SUM(CASE WHEN vpr.critical_failure THEN 1 ELSE 0 END) as critical_failure_count
                 FROM workflow_verification_phase_results vpr
                 JOIN task_runs tr ON vpr.task_run_id = tr.id
                 WHERE vpr.created_at > $1{}
                 GROUP BY vpr.iteration ORDER BY vpr.iteration"#,
                filter
            ),
            &[&since],
        ).await {
            for r in &rows {
                verification_failures.push(crate::meta_optimizer::failure_analysis::VerificationFailurePattern {
                    iteration: r.get::<_, i64>(0),
                    total_checks: r.get::<_, i64>(1),
                    failed_checks: r.get::<_, i64>(2),
                    failure_rate: r.get::<_, Option<f64>>(3).unwrap_or(0.0),
                    critical_failure_count: r.get::<_, i64>(4),
                });
            }
        }

        // Finding distribution
        let mut finding_distribution = Vec::new();
        if let Ok(rows) = conn
            .query(
                &format!(
                    r#"SELECT trf.category, COUNT(*) as count
                 FROM task_run_findings trf
                 JOIN task_runs tr ON trf.task_run_id = tr.id
                 WHERE trf.detected_at > $1{}
                 GROUP BY trf.category ORDER BY count DESC"#,
                    filter
                ),
                &[&since],
            )
            .await
        {
            for r in &rows {
                finding_distribution.push(crate::meta_optimizer::failure_analysis::CategoryCount {
                    category: r.get(0),
                    count: r.get(1),
                });
            }
        }

        // Severity distribution
        let mut severity_distribution = Vec::new();
        if let Ok(rows) = conn
            .query(
                &format!(
                    r#"SELECT trf.severity, COUNT(*) as count
                 FROM task_run_findings trf
                 JOIN task_runs tr ON trf.task_run_id = tr.id
                 WHERE trf.detected_at > $1{}
                 GROUP BY trf.severity
                 ORDER BY CASE trf.severity
                     WHEN 'critical' THEN 1 WHEN 'high' THEN 2 WHEN 'medium' THEN 3
                     WHEN 'low' THEN 4 WHEN 'info' THEN 5 END"#,
                    filter
                ),
                &[&since],
            )
            .await
        {
            for r in &rows {
                severity_distribution.push(
                    crate::meta_optimizer::failure_analysis::CategoryCount {
                        category: r.get(0),
                        count: r.get(1),
                    },
                );
            }
        }

        // Fix effectiveness
        let mut reflection_fix_effectiveness = Vec::new();
        if let Ok(rows) = conn.query(
            r#"SELECT fix_type, source_agent,
                 COUNT(*) as total,
                 SUM(CASE WHEN effectiveness IS NULL OR effectiveness = 'effective' THEN 1 ELSE 0 END) as effective,
                 SUM(CASE WHEN effectiveness = 'ineffective' THEN 1 ELSE 0 END) as ineffective,
                 SUM(CASE WHEN effectiveness = 'caused_regression' THEN 1 ELSE 0 END) as caused_regression
             FROM reflection_fixes
             WHERE created_at > $1
             GROUP BY fix_type, source_agent ORDER BY total DESC"#,
            &[&since],
        ).await {
            for r in &rows {
                let total: i64 = r.get(2);
                let effective: i64 = r.get(3);
                reflection_fix_effectiveness.push(crate::meta_optimizer::failure_analysis::FixEffectivenessRecord {
                    fix_type: r.get(0),
                    source_agent: r.get(1),
                    total,
                    effective,
                    ineffective: r.get(4),
                    caused_regression: r.get(5),
                    effectiveness_rate: if total > 0 { 100.0 * effective as f64 / total as f64 } else { 0.0 },
                });
            }
        }

        // Generation quality
        let mut generation_quality =
            crate::meta_optimizer::failure_analysis::GenerationQualityMetrics {
                total_feedback: 0,
                edits: 0,
                deletes: 0,
                avg_rating: None,
                most_edited_fields: Vec::new(),
                delete_reasons: Vec::new(),
            };
        if let Ok(r) = conn
            .query_one(
                r#"SELECT COUNT(*), SUM(CASE WHEN feedback_type = 'edit' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN feedback_type = 'delete' THEN 1 ELSE 0 END),
                 AVG(CASE WHEN feedback_type = 'rating' THEN rating END)
             FROM workflow_generation_feedback WHERE created_at > $1"#,
                &[&since],
            )
            .await
        {
            generation_quality.total_feedback = r.get(0);
            generation_quality.edits = r.get(1);
            generation_quality.deletes = r.get(2);
            generation_quality.avg_rating = r.get(3);
        }
        if let Ok(rows) = conn
            .query(
                r#"SELECT edited_field, COUNT(*) FROM workflow_generation_feedback
             WHERE feedback_type = 'edit' AND edited_field IS NOT NULL AND created_at > $1
             GROUP BY edited_field ORDER BY COUNT(*) DESC LIMIT 10"#,
                &[&since],
            )
            .await
        {
            generation_quality.most_edited_fields = rows
                .iter()
                .map(|r| crate::meta_optimizer::failure_analysis::CategoryCount {
                    category: r.get(0),
                    count: r.get(1),
                })
                .collect();
        }
        if let Ok(rows) = conn.query(
            r#"SELECT COALESCE(delete_reason, 'unspecified'), COUNT(*) FROM workflow_generation_feedback
             WHERE feedback_type = 'delete' AND created_at > $1
             GROUP BY COALESCE(delete_reason, 'unspecified') ORDER BY COUNT(*) DESC"#,
            &[&since],
        ).await {
            generation_quality.delete_reasons = rows.iter().map(|r| crate::meta_optimizer::failure_analysis::CategoryCount {
                category: r.get(0), count: r.get(1),
            }).collect();
        }

        // Pipeline agent failures
        let mut pipeline_agent_failures = Vec::new();
        if let Ok(rows) = conn
            .query(
                &format!(
                    r#"SELECT pat.agent_type,
                     COUNT(*) as total_runs,
                     SUM(CASE WHEN pat.downstream_success = false THEN 1 ELSE 0 END) as failures,
                     AVG(pat.duration_ms) as avg_duration_ms,
                     AVG(pat.cost_usd) as avg_cost_usd
                 FROM pipeline_agent_traces pat
                 JOIN task_runs tr ON pat.task_run_id = tr.id
                 WHERE pat.created_at > $1 AND pat.downstream_success IS NOT NULL{}
                 GROUP BY pat.agent_type ORDER BY pat.agent_type"#,
                    filter
                ),
                &[&since],
            )
            .await
        {
            for r in &rows {
                let total_runs: i64 = r.get(1);
                let failures: i64 = r.get(2);
                pipeline_agent_failures.push(
                    crate::meta_optimizer::failure_analysis::PipelineAgentFailureRecord {
                        agent_type: r.get(0),
                        total_runs,
                        failures,
                        failure_rate: if total_runs > 0 {
                            100.0 * failures as f64 / total_runs as f64
                        } else {
                            0.0
                        },
                        avg_duration_ms: r.get::<_, Option<f64>>(3).unwrap_or(0.0),
                        avg_cost_usd: r.get::<_, Option<f64>>(4).unwrap_or(0.0),
                    },
                );
            }
        }

        // Recurring issues
        let mut recurring_issues = Vec::new();
        if let Ok(rows) = conn
            .query(
                &format!(
                    r#"SELECT trf.signature_hash, trf.title, trf.category, trf.severity,
                     COUNT(*) as occurrence_count, MAX(trf.detected_at) as last_seen
                 FROM task_run_findings trf
                 JOIN task_runs tr ON trf.task_run_id = tr.id
                 WHERE trf.detected_at > $1 AND trf.signature_hash IS NOT NULL{}
                 GROUP BY trf.signature_hash, trf.title, trf.category, trf.severity
                 HAVING COUNT(*) >= 2 ORDER BY occurrence_count DESC LIMIT 20"#,
                    filter
                ),
                &[&since],
            )
            .await
        {
            for r in &rows {
                recurring_issues.push(crate::meta_optimizer::failure_analysis::RecurringIssue {
                    signature_hash: r.get(0),
                    title: r.get(1),
                    category: r.get(2),
                    severity: r.get(3),
                    occurrence_count: r.get(4),
                    last_seen: r.get(5),
                });
            }
        }

        Ok(crate::meta_optimizer::failure_analysis::FailureAnalysis {
            period_days: 0, // caller sets this
            total_runs,
            failed_runs,
            failure_rate,
            abort_reasons,
            verification_failures,
            finding_distribution,
            severity_distribution,
            reflection_fix_effectiveness,
            generation_quality,
            pipeline_agent_failures,
            recurring_issues,
            agentic_metric_summary: Vec::new(), // caller fills this
        })
    }
}
