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
}
