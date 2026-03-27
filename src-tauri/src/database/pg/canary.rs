//! PostgreSQL wrapper for canary rollout operations.
//!
//! TODO: Switch to Clorinde-generated queries once `clorinde` is run against the
//! canary_rollouts and prompt_template_canaries tables. For now uses raw
//! tokio-postgres queries.

use tracing::info;

use super::PgDb;
use crate::meta_optimizer::canary::{CanaryRollout, PromptTemplateCanary, PromptVersionMetrics};

impl PgDb {
    /// Start a canary rollout for a recommendation.
    pub async fn start_canary(
        &self,
        recommendation_id: &str,
        percentage: f64,
    ) -> Result<String, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("canary-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();
        let percentage_i = (percentage.clamp(1.0, 100.0)) as i64;

        conn.execute(
            r#"INSERT INTO canary_rollouts
               (id, recommendation_id, percentage, status, start_date,
                baseline_run_count, canary_run_count,
                baseline_metrics_json, canary_metrics_json, created_at)
               VALUES ($1, $2, $3, 'active', $4::timestamptz, 0, 0, '{}', '{}', $4::timestamptz)"#,
            &[
                &id,
                &recommendation_id,
                &percentage_i,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("Failed to create canary rollout: {}", e))?;

        // Update recommendation status to "canary"
        conn.execute(
            "UPDATE meta_optimizer_recommendations SET status = 'canary' WHERE id = $1 AND status = 'pending'",
            &[&recommendation_id],
        )
        .await
        .map_err(|e| format!("Failed to update recommendation status: {}", e))?;

        info!(
            "Started canary rollout {} for recommendation {} at {}%",
            id, recommendation_id, percentage_i
        );
        Ok(id)
    }

    /// Get all active canary rollouts.
    pub async fn get_active_canaries(&self) -> Result<Vec<CanaryRollout>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, recommendation_id, percentage, status,
                          start_date::text, end_date::text,
                          baseline_run_count, canary_run_count,
                          baseline_metrics_json, canary_metrics_json, created_at::text
                   FROM canary_rollouts
                   WHERE status = 'active'
                   ORDER BY created_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query active canaries: {}", e))?;

        let results = rows
            .iter()
            .map(|row| CanaryRollout {
                id: row.get(0),
                recommendation_id: row.get(1),
                percentage: row.get(2),
                status: row.get(3),
                start_date: row.get(4),
                end_date: row.get(5),
                baseline_run_count: row.get(6),
                canary_run_count: row.get(7),
                baseline_metrics_json: row.get(8),
                canary_metrics_json: row.get(9),
                created_at: row.get(10),
            })
            .collect();

        Ok(results)
    }

    /// Get the baseline and canary metrics JSON for a canary rollout.
    pub async fn get_canary_metrics(
        &self,
        canary_id: &str,
    ) -> Result<(String, String), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                "SELECT baseline_metrics_json, canary_metrics_json FROM canary_rollouts WHERE id = $1",
                &[&canary_id],
            )
            .await
            .map_err(|e| format!("Canary not found: {}", e))?;

        Ok((row.get(0), row.get(1)))
    }

    /// Update the baseline and canary metrics JSON for a canary rollout.
    pub async fn update_canary_metrics(
        &self,
        canary_id: &str,
        baseline_json: &str,
        canary_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE canary_rollouts SET baseline_metrics_json = $1, canary_metrics_json = $2 WHERE id = $3",
            &[&baseline_json, &canary_json, &canary_id],
        )
        .await
        .map_err(|e| format!("Failed to update canary metrics: {}", e))?;

        Ok(())
    }

    /// Record a completed run as either baseline or canary.
    pub async fn record_canary_run(
        &self,
        canary_id: &str,
        run_type: &str,
        metrics_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let is_canary = run_type == "canary";
        let count_field = if is_canary {
            "canary_run_count"
        } else {
            "baseline_run_count"
        };
        let metrics_field = if is_canary {
            "canary_metrics_json"
        } else {
            "baseline_metrics_json"
        };

        conn.execute(
            &format!(
                "UPDATE canary_rollouts SET {} = {} + 1, {} = $1 WHERE id = $2",
                count_field, count_field, metrics_field
            ),
            &[&metrics_json, &canary_id],
        )
        .await
        .map_err(|e| format!("Failed to record canary run: {}", e))?;

        // Best-effort: insert a per-run record
        let record_id = format!("crr-{}", uuid::Uuid::new_v4());
        let created_at = chrono::Utc::now().to_rfc3339();

        let _ = conn
            .execute(
                r#"INSERT INTO canary_run_records
                   (id, canary_id, is_canary, task_run_id, success, cost_usd, duration_ms, created_at)
                   VALUES ($1, $2, $3, NULL, NULL, NULL, NULL, $4::timestamptz)"#,
                &[
                    &record_id,
                    &canary_id,
                    &is_canary,
                    &created_at,
                ],
            )
            .await;

        Ok(())
    }

    /// Promote a canary rollout (mark as promoted).
    pub async fn promote_canary(&self, canary_id: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE canary_rollouts SET status = 'promoted', end_date = $1::timestamptz WHERE id = $2",
            &[&now, &canary_id],
        )
        .await
        .map_err(|e| format!("Failed to promote canary: {}", e))?;

        info!("Promoted canary rollout {}", canary_id);
        Ok(())
    }

    /// Roll back a canary rollout (mark as rolled_back).
    pub async fn rollback_canary(&self, canary_id: &str) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE canary_rollouts SET status = 'rolled_back', end_date = $1::timestamptz WHERE id = $2",
            &[&now, &canary_id],
        )
        .await
        .map_err(|e| format!("Failed to rollback canary: {}", e))?;

        info!("Rolled back canary rollout {}", canary_id);
        Ok(())
    }

    // ── Prompt template canary operations ────────────────────────────────

    /// Create a new prompt template canary.
    pub async fn create_template_canary(
        &self,
        template_id: &str,
        baseline: i32,
        candidate: i32,
        traffic_pct: f64,
    ) -> Result<String, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = format!("ptc-{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();
        let traffic_pct = traffic_pct.clamp(0.0, 1.0);
        let empty_metrics = serde_json::to_string(&PromptVersionMetrics::default())
            .unwrap_or_else(|_| "{}".to_string());

        conn.execute(
            r#"INSERT INTO prompt_template_canaries
               (id, template_id, baseline_version, candidate_version,
                traffic_percentage, status,
                baseline_metrics_json, candidate_metrics_json,
                created_at)
               VALUES ($1, $2, $3, $4, $5, 'active', $6, $6, $7::timestamptz)"#,
            &[
                &id,
                &template_id,
                &baseline,
                &candidate,
                &traffic_pct,
                &empty_metrics,
                &now,
            ],
        )
        .await
        .map_err(|e| format!("Failed to create prompt template canary: {}", e))?;

        info!(
            "Created prompt template canary {} for template {} (v{} vs v{}, {}% candidate traffic)",
            id,
            template_id,
            baseline,
            candidate,
            (traffic_pct * 100.0) as i64,
        );

        Ok(id)
    }

    /// Load a prompt template canary by id.
    pub async fn get_template_canary(
        &self,
        id: &str,
    ) -> Result<Option<PromptTemplateCanary>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, template_id, baseline_version, candidate_version,
                          traffic_percentage, status,
                          baseline_metrics_json, candidate_metrics_json,
                          created_at::text, ended_at::text
                   FROM prompt_template_canaries WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("Failed to get prompt template canary: {}", e))?;

        Ok(row.map(|r| {
            let baseline_json: String = r.get(6);
            let candidate_json: String = r.get(7);
            PromptTemplateCanary {
                id: r.get(0),
                template_id: r.get(1),
                baseline_version: r.get(2),
                candidate_version: r.get(3),
                traffic_percentage: r.get(4),
                status: r.get(5),
                baseline_metrics: serde_json::from_str(&baseline_json).unwrap_or_default(),
                candidate_metrics: serde_json::from_str(&candidate_json).unwrap_or_default(),
                created_at: r.get(8),
                ended_at: r.get(9),
            }
        }))
    }

    /// Get the active prompt template canary for a given template_id.
    pub async fn get_active_template_canary(
        &self,
        template_id: &str,
    ) -> Result<Option<PromptTemplateCanary>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"SELECT id, template_id, baseline_version, candidate_version,
                          traffic_percentage, status,
                          baseline_metrics_json, candidate_metrics_json,
                          created_at::text, ended_at::text
                   FROM prompt_template_canaries
                   WHERE template_id = $1 AND status = 'active'
                   LIMIT 1"#,
                &[&template_id],
            )
            .await
            .map_err(|e| format!("Failed to get active template canary: {}", e))?;

        Ok(row.map(|r| {
            let baseline_json: String = r.get(6);
            let candidate_json: String = r.get(7);
            PromptTemplateCanary {
                id: r.get(0),
                template_id: r.get(1),
                baseline_version: r.get(2),
                candidate_version: r.get(3),
                traffic_percentage: r.get(4),
                status: r.get(5),
                baseline_metrics: serde_json::from_str(&baseline_json).unwrap_or_default(),
                candidate_metrics: serde_json::from_str(&candidate_json).unwrap_or_default(),
                created_at: r.get(8),
                ended_at: r.get(9),
            }
        }))
    }

    /// Update the metrics JSON for a prompt template canary.
    pub async fn update_template_canary_metrics(
        &self,
        id: &str,
        baseline_json: &str,
        candidate_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE prompt_template_canaries SET baseline_metrics_json = $1, candidate_metrics_json = $2 WHERE id = $3",
            &[&baseline_json, &candidate_json, &id],
        )
        .await
        .map_err(|e| format!("Failed to update template canary metrics: {}", e))?;

        Ok(())
    }
}
