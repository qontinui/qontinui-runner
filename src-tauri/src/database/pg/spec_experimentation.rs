//! PostgreSQL spec experimentation operations.
//!
//! Provides CRUD for spec_compliance_results, spec_accuracy_results,
//! and spec_versions tables.

use super::PgDb;
use tracing::{debug, info};

/// Lightweight compliance result row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PgSpecComplianceRow {
    pub id: String,
    pub task_run_id: String,
    pub spec_id: Option<String>,
    pub iteration: i64,
    pub overall_score: f64,
    pub raw_pass_rate: f64,
    pub critical_passed: i64,
    pub critical_total: i64,
    pub warning_passed: i64,
    pub warning_total: i64,
    pub info_passed: i64,
    pub info_total: i64,
    pub assertions_passed: i64,
    pub assertions_total: i64,
    pub group_scores_json: String,
    pub assertion_details_json: String,
    pub created_at: String,
}

fn row_to_compliance(row: &tokio_postgres::Row) -> PgSpecComplianceRow {
    PgSpecComplianceRow {
        id: row.get("id"),
        task_run_id: row.get("task_run_id"),
        spec_id: row.get("spec_id"),
        iteration: row.get("iteration"),
        overall_score: row.get("overall_score"),
        raw_pass_rate: row.get("raw_pass_rate"),
        critical_passed: row.get("critical_passed"),
        critical_total: row.get("critical_total"),
        warning_passed: row.get("warning_passed"),
        warning_total: row.get("warning_total"),
        info_passed: row.get("info_passed"),
        info_total: row.get("info_total"),
        assertions_passed: row.get("assertions_passed"),
        assertions_total: row.get("assertions_total"),
        group_scores_json: row.get("group_scores_json"),
        assertion_details_json: row.get("assertion_details_json"),
        created_at: row.get("created_at"),
    }
}

/// Lightweight spec version row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PgSpecVersionRow {
    pub id: String,
    pub spec_id: String,
    pub version_number: i64,
    pub content_hash: String,
    pub spec_json: String,
    pub change_summary: Option<String>,
    pub change_type: String,
    pub parent_version_id: Option<String>,
    pub assertion_count: i64,
    pub group_count: i64,
    pub created_at: String,
}

fn row_to_version(row: &tokio_postgres::Row) -> PgSpecVersionRow {
    PgSpecVersionRow {
        id: row.get("id"),
        spec_id: row.get("spec_id"),
        version_number: row.get("version_number"),
        content_hash: row.get("content_hash"),
        spec_json: row.get("spec_json"),
        change_summary: row.get("change_summary"),
        change_type: row.get("change_type"),
        parent_version_id: row.get("parent_version_id"),
        assertion_count: row.get("assertion_count"),
        group_count: row.get("group_count"),
        created_at: row.get("created_at"),
    }
}

/// Lightweight accuracy result row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PgSpecAccuracyRow {
    pub id: String,
    pub spec_id: String,
    pub analysis_type: String,
    pub score: f64,
    pub detail_json: String,
    pub created_at: String,
}

impl PgDb {
    // ========================================================================
    // Compliance Results
    // ========================================================================

    /// Get verification phase result_json and spec_id for latest iteration.
    pub async fn get_latest_verification_result(
        &self,
        task_run_id: &str,
    ) -> Result<(i64, String, Option<String>), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let (iteration, result_json): (i64, String) = conn
            .query_one(
                r#"SELECT iteration, result_json
                   FROM workflow_verification_phase_results
                   WHERE task_run_id = $1
                   ORDER BY iteration DESC
                   LIMIT 1"#,
                &[&task_run_id],
            )
            .await
            .map(|row| (row.get(0), row.get(1)))
            .map_err(|e| {
                format!(
                    "No verification results for task_run_id {}: {}",
                    task_run_id, e
                )
            })?;

        let spec_id: Option<String> = conn
            .query_opt(
                r#"SELECT uw.id
                   FROM task_runs tr
                   JOIN unified_workflows uw ON tr.workflow_id = uw.id
                   WHERE tr.id = $1 AND uw.category = 'spec-generated'"#,
                &[&task_run_id],
            )
            .await
            .ok()
            .flatten()
            .map(|row| row.get(0));

        Ok((iteration, result_json, spec_id))
    }

    /// Insert or replace a spec compliance result.
    pub async fn upsert_spec_compliance_result(
        &self,
        id: &str,
        task_run_id: &str,
        spec_id: Option<&str>,
        iteration: i64,
        overall_score: f64,
        raw_pass_rate: f64,
        critical_passed: i64,
        critical_total: i64,
        warning_passed: i64,
        warning_total: i64,
        info_passed: i64,
        info_total: i64,
        assertions_passed: i64,
        assertions_total: i64,
        group_scores_json: &str,
        assertion_details_json: &str,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.execute(
            r#"INSERT INTO spec_compliance_results (
                id, task_run_id, spec_id, iteration,
                overall_score, raw_pass_rate,
                critical_passed, critical_total,
                warning_passed, warning_total,
                info_passed, info_total,
                assertions_passed, assertions_total,
                group_scores_json, assertion_details_json,
                created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
            ON CONFLICT (id) DO UPDATE SET
                overall_score = EXCLUDED.overall_score,
                raw_pass_rate = EXCLUDED.raw_pass_rate,
                critical_passed = EXCLUDED.critical_passed,
                critical_total = EXCLUDED.critical_total,
                warning_passed = EXCLUDED.warning_passed,
                warning_total = EXCLUDED.warning_total,
                info_passed = EXCLUDED.info_passed,
                info_total = EXCLUDED.info_total,
                assertions_passed = EXCLUDED.assertions_passed,
                assertions_total = EXCLUDED.assertions_total,
                group_scores_json = EXCLUDED.group_scores_json,
                assertion_details_json = EXCLUDED.assertion_details_json"#,
            &[
                &id,
                &task_run_id,
                &spec_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &iteration,
                &overall_score,
                &raw_pass_rate,
                &critical_passed,
                &critical_total,
                &warning_passed,
                &warning_total,
                &info_passed,
                &info_total,
                &assertions_passed,
                &assertions_total,
                &group_scores_json,
                &assertion_details_json,
                &created_at,
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert spec_compliance_results: {e}"))?;

        Ok(())
    }

    /// Get compliance result for a specific task run.
    pub async fn get_compliance_for_run(
        &self,
        task_run_id: &str,
    ) -> Result<Option<PgSpecComplianceRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_opt(
                r#"SELECT id, task_run_id, spec_id, iteration,
                          overall_score, raw_pass_rate,
                          critical_passed, critical_total,
                          warning_passed, warning_total,
                          info_passed, info_total,
                          assertions_passed, assertions_total,
                          group_scores_json, assertion_details_json,
                          created_at
                   FROM spec_compliance_results
                   WHERE task_run_id = $1
                   ORDER BY created_at DESC LIMIT 1"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query spec_compliance_results: {e}"))?;

        Ok(row.map(|r| row_to_compliance(&r)))
    }

    /// Get compliance history, optionally filtered by spec_id.
    pub async fn get_compliance_history(
        &self,
        spec_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<PgSpecComplianceRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = if let Some(sid) = spec_id {
            conn.query(
                r#"SELECT id, task_run_id, spec_id, iteration,
                          overall_score, raw_pass_rate,
                          critical_passed, critical_total,
                          warning_passed, warning_total,
                          info_passed, info_total,
                          assertions_passed, assertions_total,
                          group_scores_json, assertion_details_json,
                          created_at
                   FROM spec_compliance_results
                   WHERE spec_id = $1
                   ORDER BY created_at DESC
                   LIMIT $2"#,
                &[&sid, &limit],
            )
            .await
        } else {
            conn.query(
                r#"SELECT id, task_run_id, spec_id, iteration,
                          overall_score, raw_pass_rate,
                          critical_passed, critical_total,
                          warning_passed, warning_total,
                          info_passed, info_total,
                          assertions_passed, assertions_total,
                          group_scores_json, assertion_details_json,
                          created_at
                   FROM spec_compliance_results
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
        }
        .map_err(|e| format!("Failed to query compliance history: {e}"))?;

        Ok(rows.iter().map(row_to_compliance).collect())
    }

    /// Get compliance summary: latest score + run count per spec_id.
    pub async fn get_compliance_summary(&self) -> Result<Vec<(Option<String>, f64, i64)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT spec_id,
                          (SELECT overall_score FROM spec_compliance_results scr2
                           WHERE scr2.spec_id IS NOT DISTINCT FROM scr.spec_id
                           ORDER BY created_at DESC LIMIT 1) as latest_score,
                          COUNT(*) as run_count
                   FROM spec_compliance_results scr
                   GROUP BY spec_id
                   ORDER BY latest_score ASC"#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query compliance summary: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                let spec_id: Option<String> = r.get(0);
                let latest_score: f64 = r.get(1);
                let run_count: i64 = r.get(2);
                (spec_id, latest_score, run_count)
            })
            .collect())
    }

    /// Compute trend from last 5 scores for a spec.
    pub async fn get_compliance_trend(&self, spec_id: Option<&str>) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = if let Some(sid) = spec_id {
            conn.query(
                "SELECT overall_score FROM spec_compliance_results WHERE spec_id = $1 ORDER BY created_at DESC LIMIT 5",
                &[&sid],
            ).await
        } else {
            conn.query(
                "SELECT overall_score FROM spec_compliance_results WHERE spec_id IS NULL ORDER BY created_at DESC LIMIT 5",
                &[],
            ).await
        }.map_err(|e| format!("Failed to query trend: {e}"))?;

        let scores: Vec<f64> = rows.iter().map(|r| r.get(0)).collect();

        if scores.len() < 2 {
            return Ok("stable".to_string());
        }

        let newest = scores[0];
        let oldest = scores[scores.len() - 1];
        let delta = newest - oldest;

        Ok(if delta > 0.03 {
            "improving".to_string()
        } else if delta < -0.03 {
            "declining".to_string()
        } else {
            "stable".to_string()
        })
    }

    /// Get average compliance score since a given timestamp.
    pub async fn get_avg_compliance_since(&self, since: &str) -> Result<Option<f64>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_opt(
                "SELECT AVG(overall_score) FROM spec_compliance_results WHERE created_at > $1",
                &[&since],
            )
            .await
            .map_err(|e| format!("Failed to query avg compliance: {e}"))?;

        Ok(row.and_then(|r| r.get::<_, Option<f64>>(0)))
    }

    /// Detect broken assertions between most recent runs for a spec.
    pub async fn get_compliance_runs_for_broken_detection(
        &self,
        spec_id: &str,
        limit: i64,
    ) -> Result<Vec<(String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT assertion_details_json, created_at
                   FROM spec_compliance_results
                   WHERE spec_id = $1
                   ORDER BY created_at DESC
                   LIMIT $2"#,
                &[&spec_id, &limit],
            )
            .await
            .map_err(|e| format!("Failed to query compliance runs: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    /// Get distinct spec_ids with their latest scores.
    pub async fn get_spec_ids_with_latest_scores(&self) -> Result<Vec<(String, f64)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT DISTINCT spec_id,
                          (SELECT overall_score FROM spec_compliance_results scr2
                           WHERE scr2.spec_id = scr.spec_id
                           ORDER BY created_at DESC LIMIT 1) as latest_score
                   FROM spec_compliance_results scr
                   WHERE spec_id IS NOT NULL"#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query spec_ids: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, f64>(1)))
            .collect())
    }

    /// Get stale specs from accuracy results.
    pub async fn get_stale_specs_from_accuracy(&self) -> Result<Vec<(String, i64)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT spec_id, detail_json
                   FROM spec_accuracy_results
                   WHERE analysis_type = 'freshness'
                   ORDER BY created_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query freshness results: {e}"))?;

        let mut seen = std::collections::HashSet::new();
        let mut stale = Vec::new();
        for row in &rows {
            let sid: String = row.get(0);
            let detail_json: String = row.get(1);
            if !seen.insert(sid.clone()) {
                continue;
            }
            if let Ok(detail) = serde_json::from_str::<serde_json::Value>(&detail_json) {
                let is_stale = detail
                    .get("is_stale")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let staleness_days = detail
                    .get("staleness_days")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if is_stale && staleness_days > 0 {
                    stale.push((sid, staleness_days));
                }
            }
        }
        Ok(stale)
    }

    /// Get spec_ids that have never been compliance-checked.
    pub async fn get_never_run_spec_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT DISTINCT sv.spec_id
                   FROM spec_versions sv
                   WHERE NOT EXISTS (
                       SELECT 1 FROM spec_compliance_results scr
                       WHERE scr.spec_id = sv.spec_id
                   )"#,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query never-run specs: {e}"))?;

        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// Check if a task run is spec-generated.
    pub async fn is_spec_generated_workflow(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let category: Option<String> = conn
            .query_opt(
                r#"SELECT uw.category
                   FROM task_runs tr
                   JOIN unified_workflows uw ON tr.workflow_id = uw.id
                   WHERE tr.id = $1"#,
                &[&task_run_id],
            )
            .await
            .ok()
            .flatten()
            .map(|row| row.get(0));

        Ok(category.as_deref() == Some("spec-generated"))
    }

    // ========================================================================
    // Spec Versions
    // ========================================================================

    /// Insert a new spec version.
    pub async fn insert_spec_version(
        &self,
        id: &str,
        spec_id: &str,
        version_number: i64,
        content_hash: &str,
        spec_json: &str,
        change_summary: Option<&str>,
        change_type: &str,
        parent_version_id: Option<&str>,
        assertion_count: i64,
        group_count: i64,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.execute(
            r#"INSERT INTO spec_versions (
                id, spec_id, version_number, content_hash, spec_json,
                change_summary, change_type, parent_version_id,
                assertion_count, group_count, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
            &[
                &id,
                &spec_id,
                &version_number,
                &content_hash,
                &spec_json,
                &change_summary as &(dyn tokio_postgres::types::ToSql + Sync),
                &change_type,
                &parent_version_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &assertion_count,
                &group_count,
                &created_at,
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert spec version: {e}"))?;

        Ok(())
    }

    /// Get the latest version of a spec.
    pub async fn get_latest_spec_version(
        &self,
        spec_id: &str,
    ) -> Result<Option<PgSpecVersionRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_opt(
                r#"SELECT id, spec_id, version_number, content_hash, spec_json,
                          change_summary, change_type, parent_version_id,
                          assertion_count, group_count, created_at
                   FROM spec_versions
                   WHERE spec_id = $1
                   ORDER BY version_number DESC
                   LIMIT 1"#,
                &[&spec_id],
            )
            .await
            .map_err(|e| format!("Failed to query latest spec version: {e}"))?;

        Ok(row.map(|r| row_to_version(&r)))
    }

    /// Get version history for a spec.
    pub async fn get_spec_version_history(
        &self,
        spec_id: &str,
        limit: i64,
    ) -> Result<Vec<PgSpecVersionRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT id, spec_id, version_number, content_hash, spec_json,
                          change_summary, change_type, parent_version_id,
                          assertion_count, group_count, created_at
                   FROM spec_versions
                   WHERE spec_id = $1
                   ORDER BY version_number DESC
                   LIMIT $2"#,
                &[&spec_id, &limit],
            )
            .await
            .map_err(|e| format!("Failed to query version history: {e}"))?;

        Ok(rows.iter().map(row_to_version).collect())
    }

    /// Get spec_json for two versions (for diffing).
    pub async fn get_spec_json_for_versions(
        &self,
        spec_id: &str,
        from_version: i64,
        to_version: i64,
    ) -> Result<(String, String), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let old: String = conn
            .query_one(
                "SELECT spec_json FROM spec_versions WHERE spec_id = $1 AND version_number = $2",
                &[&spec_id, &from_version],
            )
            .await
            .map(|r| r.get(0))
            .map_err(|e| format!("Version {} not found: {}", from_version, e))?;

        let new: String = conn
            .query_one(
                "SELECT spec_json FROM spec_versions WHERE spec_id = $1 AND version_number = $2",
                &[&spec_id, &to_version],
            )
            .await
            .map(|r| r.get(0))
            .map_err(|e| format!("Version {} not found: {}", to_version, e))?;

        Ok((old, new))
    }

    // ========================================================================
    // Accuracy Results
    // ========================================================================

    /// Store a spec accuracy result.
    pub async fn insert_spec_accuracy_result(
        &self,
        id: &str,
        spec_id: &str,
        analysis_type: &str,
        score: f64,
        detail_json: &str,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.execute(
            r#"INSERT INTO spec_accuracy_results (id, spec_id, analysis_type, score, detail_json, created_at)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
            &[&id, &spec_id, &analysis_type, &score, &detail_json, &created_at],
        )
        .await
        .map_err(|e| format!("Failed to insert spec_accuracy_result: {e}"))?;

        Ok(())
    }

    /// Get accuracy results with optional filters.
    pub async fn get_accuracy_results(
        &self,
        spec_id: Option<&str>,
        analysis_type: Option<&str>,
    ) -> Result<Vec<PgSpecAccuracyRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = match (spec_id, analysis_type) {
            (Some(sid), Some(at)) => {
                conn.query(
                    "SELECT id, spec_id, analysis_type, score, detail_json, created_at FROM spec_accuracy_results WHERE spec_id = $1 AND analysis_type = $2 ORDER BY created_at DESC LIMIT 100",
                    &[&sid, &at],
                ).await
            }
            (Some(sid), None) => {
                conn.query(
                    "SELECT id, spec_id, analysis_type, score, detail_json, created_at FROM spec_accuracy_results WHERE spec_id = $1 ORDER BY created_at DESC LIMIT 100",
                    &[&sid],
                ).await
            }
            (None, Some(at)) => {
                conn.query(
                    "SELECT id, spec_id, analysis_type, score, detail_json, created_at FROM spec_accuracy_results WHERE analysis_type = $1 ORDER BY created_at DESC LIMIT 100",
                    &[&at],
                ).await
            }
            (None, None) => {
                conn.query(
                    "SELECT id, spec_id, analysis_type, score, detail_json, created_at FROM spec_accuracy_results ORDER BY created_at DESC LIMIT 100",
                    &[],
                ).await
            }
        }.map_err(|e| format!("Failed to query accuracy results: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| PgSpecAccuracyRow {
                id: r.get(0),
                spec_id: r.get(1),
                analysis_type: r.get(2),
                score: r.get(3),
                detail_json: r.get(4),
                created_at: r.get(5),
            })
            .collect())
    }

    // ========================================================================
    // Autoresearch Campaigns & Experiments
    // ========================================================================

    /// Get campaign summaries, optionally filtered by name.
    pub async fn get_autoresearch_campaigns(
        &self,
        filter: Option<&str>,
    ) -> Result<Vec<(String, String, String, i64, i64, String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = if let Some(f) = filter {
            let pattern = format!("%{}%", f);
            conn.query(
                "SELECT id, name, status, experiment_count, accepted_count, config_json, created_at \
                 FROM autoresearch_campaigns WHERE name LIKE $1 ORDER BY created_at DESC",
                &[&pattern],
            ).await
        } else {
            conn.query(
                "SELECT id, name, status, experiment_count, accepted_count, config_json, created_at \
                 FROM autoresearch_campaigns ORDER BY created_at DESC",
                &[],
            ).await
        }.map_err(|e| format!("Failed to query campaigns: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, i64>(3),
                    r.get::<_, i64>(4),
                    r.get::<_, String>(5),
                    r.get::<_, String>(6),
                )
            })
            .collect())
    }

    /// Get experiments for a campaign.
    pub async fn get_autoresearch_experiments(
        &self,
        campaign_id: &str,
    ) -> Result<
        Vec<(
            i32,
            String,
            String,
            String,
            bool,
            Option<String>,
            Option<f64>,
        )>,
        String,
    > {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn.query(
            "SELECT experiment_number, config_json, trials_json, aggregate_json, accepted, reason, p_value \
             FROM autoresearch_experiments WHERE campaign_id = $1 ORDER BY experiment_number ASC",
            &[&campaign_id],
        ).await
        .map_err(|e| format!("Failed to query experiments: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, i32>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, String>(3),
                    r.get::<_, bool>(4),
                    r.get::<_, Option<String>>(5),
                    r.get::<_, Option<f64>>(6),
                )
            })
            .collect())
    }

    /// Get campaign config JSON by id.
    pub async fn get_campaign_config(&self, campaign_id: &str) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.query_one(
            "SELECT config_json FROM autoresearch_campaigns WHERE id = $1",
            &[&campaign_id],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("Campaign not found: {e}"))
    }

    /// Get campaign summary by id.
    pub async fn get_campaign_summary_by_id(
        &self,
        campaign_id: &str,
    ) -> Result<(String, String, String, i64, i64, String, String), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_one(
                "SELECT id, name, status, experiment_count, accepted_count, config_json, created_at \
                 FROM autoresearch_campaigns WHERE id = $1",
                &[&campaign_id],
            )
            .await
            .map_err(|e| format!("Campaign not found: {e}"))?;

        Ok((
            row.get(0),
            row.get(1),
            row.get(2),
            row.get(3),
            row.get(4),
            row.get(5),
            row.get(6),
        ))
    }

    /// Get experiments with aggregate for campaign comparison.
    pub async fn get_experiments_for_comparison(
        &self,
        campaign_id: &str,
    ) -> Result<Vec<(i32, String, bool)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                "SELECT experiment_number, aggregate_json, accepted FROM autoresearch_experiments \
                 WHERE campaign_id = $1 ORDER BY experiment_number ASC",
                &[&campaign_id],
            )
            .await
            .map_err(|e| format!("Failed to query experiments: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, i32>(0),
                    r.get::<_, String>(1),
                    r.get::<_, bool>(2),
                )
            })
            .collect())
    }

    /// Load all Q-table entries.
    pub async fn load_q_table(&self) -> Result<Vec<(String, String, f64, u32)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                "SELECT state_key, action, q_value, visit_count FROM q_routing_table ORDER BY state_key, action",
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query Q-table: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, f64>(2),
                    r.get::<_, i32>(3) as u32,
                )
            })
            .collect())
    }

    /// Load all Q-routing overrides.
    pub async fn load_q_overrides(&self) -> Result<Vec<(String, String)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                "SELECT state_key, forced_action FROM q_routing_overrides ORDER BY state_key",
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query overrides: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }

    // ========================================================================
    // Model Profiles
    // ========================================================================

    /// Build model profile from phase_token_usage + learning_outcomes.
    pub async fn build_model_profile_data(
        &self,
        model_id: &str,
        period_start: &str,
    ) -> Result<Option<(i64, i64, f64, f64, f64)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_opt(
                r#"SELECT
                       COUNT(DISTINCT ptu.task_run_id) as run_count,
                       COUNT(DISTINCT CASE WHEN lo.status = 'success' THEN ptu.task_run_id END) as success_count,
                       COALESCE(AVG(lo.iterations), 0) as avg_iterations,
                       COALESCE(AVG(lo.duration_secs * 1000), 0) as avg_duration_ms
                   FROM phase_token_usage ptu
                   JOIN learning_outcomes lo ON ptu.task_run_id = lo.task_id
                   WHERE ptu.model_used = $1
                     AND ptu.created_at > $2"#,
                &[&model_id, &period_start],
            )
            .await
            .map_err(|e| format!("Failed to query model data: {e}"))?;

        let row = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        let run_count: i64 = row.get(0);
        if run_count == 0 {
            return Ok(None);
        }
        let success_count: i64 = row.get(1);
        let avg_iterations: f64 = row.get(2);
        let avg_duration_ms: f64 = row.get(3);

        let avg_cost: f64 = conn
            .query_one(
                r#"SELECT COALESCE(AVG(run_cost), 0) FROM (
                       SELECT ptu.task_run_id, SUM(ptu.cost_cents) / 100.0 as run_cost
                       FROM phase_token_usage ptu
                       WHERE ptu.model_used = $1 AND ptu.created_at > $2
                       GROUP BY ptu.task_run_id
                   ) sub"#,
                &[&model_id, &period_start],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0.0);

        Ok(Some((
            run_count,
            success_count,
            avg_iterations,
            avg_duration_ms,
            avg_cost,
        )))
    }

    /// Get distinct models used in the period.
    pub async fn get_distinct_models(&self, period_start: &str) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT DISTINCT model_used FROM phase_token_usage
                   WHERE model_used IS NOT NULL AND created_at > $1"#,
                &[&period_start],
            )
            .await
            .map_err(|e| format!("Failed to query distinct models: {e}"))?;

        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// Save a model profile.
    pub async fn save_model_profile(
        &self,
        id: &str,
        model_id: &str,
        profile_json: &str,
        trial_count: i64,
        last_updated: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.execute(
            r#"INSERT INTO model_profiles (id, model_id, profile_json, trial_count, last_updated)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT(model_id) DO UPDATE SET
                   profile_json = EXCLUDED.profile_json,
                   trial_count = EXCLUDED.trial_count,
                   last_updated = EXCLUDED.last_updated"#,
            &[&id, &model_id, &profile_json, &trial_count, &last_updated],
        )
        .await
        .map_err(|e| format!("Failed to save model profile: {e}"))?;

        info!(
            "Saved model profile for {} ({} trials)",
            model_id, trial_count
        );
        Ok(())
    }

    /// Load all saved model profiles.
    pub async fn list_model_profiles(&self) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                "SELECT profile_json FROM model_profiles ORDER BY trial_count DESC",
                &[],
            )
            .await
            .map_err(|e| format!("Failed to query model profiles: {e}"))?;

        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    // ========================================================================
    // Reflection trigger guard queries
    // ========================================================================

    /// Check if a same-workflow reflection is running (for guard check).
    pub async fn has_running_reflection_for_workflow(
        &self,
        source_task_run_id: &str,
        workflow_name_prefix: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let sql = if let Some(prefix) = workflow_name_prefix {
            format!(
                r#"SELECT COUNT(*) > 0 FROM task_runs r
                   WHERE r.status = 'running'
                     AND r.is_reflection = true
                     AND r.workflow_type = 'unified'
                     AND r.workflow_name LIKE '{}%'
                     AND r.reflection_source_task_run_id IN (
                         SELECT s.id FROM task_runs s
                         WHERE s.workflow_name = (
                             SELECT workflow_name FROM task_runs WHERE id = $1
                         )
                     )"#,
                prefix.replace('\'', "''")
            )
        } else {
            r#"SELECT COUNT(*) > 0 FROM task_runs r
               WHERE r.status = 'running'
                 AND r.is_reflection = true
                 AND r.workflow_type = 'unified'
                 AND r.reflection_source_task_run_id IN (
                     SELECT s.id FROM task_runs s
                     WHERE s.workflow_name = (
                         SELECT workflow_name FROM task_runs WHERE id = $1
                     )
                 )"#
            .to_string()
        };

        let has_running: bool = conn
            .query_one(&sql, &[&source_task_run_id])
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        Ok(has_running)
    }

    /// Get task_run flags (is_reflection, is_fixer, is_follow_up).
    pub async fn get_task_run_flags(
        &self,
        task_run_id: &str,
    ) -> Result<(bool, bool, bool), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_one(
                r#"SELECT COALESCE(is_reflection, false),
                          COALESCE(is_fixer, false),
                          COALESCE(is_follow_up, false)
                   FROM task_runs WHERE id = $1"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to get task_run flags: {e}"))?;

        Ok((row.get(0), row.get(1), row.get(2)))
    }

    /// Get a dev_mode setting bool value.
    pub async fn get_dev_mode_setting_bool(
        &self,
        field: &str,
        default: bool,
    ) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let val: Option<String> = conn
            .query_opt(
                &format!(
                    "SELECT value::jsonb->>'{}' FROM settings WHERE key = 'dev_mode'",
                    field.replace('\'', "''")
                ),
                &[],
            )
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get(0));

        Ok(match val.as_deref() {
            Some("true") | Some("1") => true,
            Some("false") | Some("0") => false,
            _ => default,
        })
    }

    /// Check if a task run has sufficient output for reflection.
    pub async fn has_sufficient_output(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let has_output: bool = conn
            .query_one(
                r#"SELECT COALESCE(
                    (SELECT SUM(LENGTH(content)) FROM task_run_output_chunks WHERE task_run_id = $1),
                    0
                ) + LENGTH(COALESCE((SELECT output_log FROM task_runs WHERE id = $1), '')) > 100"#,
                &[&task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        Ok(has_output)
    }

    /// Get workflow_name for a task run (with COALESCE to task_name).
    pub async fn get_task_run_workflow_name(&self, task_run_id: &str) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.query_one(
            "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
            &[&task_run_id],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("Failed to get source task run: {e}"))
    }

    /// Check if fixer is running (optionally excluding an id).
    pub async fn has_running_fixer(&self, exclude_id: Option<&str>) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let has_running: bool = if let Some(eid) = exclude_id {
            conn.query_one(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_fixer = true AND workflow_type = 'unified' AND id != $1",
                &[&eid],
            ).await
        } else {
            conn.query_one(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_fixer = true AND workflow_type = 'unified'",
                &[],
            ).await
        }.map(|r| r.get(0)).unwrap_or(false);

        Ok(has_running)
    }

    /// Check if a follow-up workflow is running.
    pub async fn has_running_follow_up(&self) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let has_running: bool = conn
            .query_one(
                "SELECT COUNT(*) > 0 FROM task_runs WHERE status = 'running' AND is_follow_up = true AND workflow_type = 'unified'",
                &[],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        Ok(has_running)
    }

    /// Count running children of a task (excluding fixers, excluding self).
    pub async fn count_running_children(&self, parent_task_run_id: &str) -> Result<i64, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.query_one(
            r#"SELECT COUNT(*) FROM task_runs
               WHERE parent_task_run_id = $1
                 AND status IN ('running', 'pending')
                 AND id != $1
                 AND COALESCE(is_fixer, false) = false"#,
            &[&parent_task_run_id],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("Failed to count running children: {e}"))
    }

    /// Get workflow_name and project_path for a task run (for project reflection).
    pub async fn get_task_run_workflow_and_project(
        &self,
        task_run_id: &str,
    ) -> Result<(String, Option<String>), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let wf_name: String = conn
            .query_one(
                "SELECT COALESCE(workflow_name, task_name) FROM task_runs WHERE id = $1",
                &[&task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .map_err(|e| format!("Failed to get source task run: {e}"))?;

        // Try to get project path from workflow steps (PG uses jsonb)
        let proj_path: Option<String> = conn
            .query_opt(
                r#"SELECT COALESCE(
                    (s.value)::jsonb->>'shell_command_working_directory',
                    (s.value)::jsonb->>'working_directory'
                )
                FROM unified_workflows uw,
                     jsonb_array_elements(uw.setup_steps::jsonb) s(value)
                INNER JOIN task_runs tr ON tr.workflow_name = uw.name
                WHERE tr.id = $1
                  AND COALESCE(
                    (s.value)::jsonb->>'shell_command_working_directory',
                    (s.value)::jsonb->>'working_directory'
                  ) IS NOT NULL
                LIMIT 1"#,
                &[&task_run_id],
            )
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get(0))
            .or({
                // Fallback: check agentic_steps
                // Note: this is best-effort; if it fails, we just return None
                None
            });

        Ok((wf_name, proj_path))
    }

    /// Check if a task has UI Bridge activity (for UI Bridge reflection guard).
    pub async fn has_ui_bridge_activity(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let has: bool = conn
            .query_one(
                r#"SELECT (
                    (SELECT COUNT(*) FROM workflow_step_checkpoints
                     WHERE task_run_id = $1
                       AND (step_type LIKE '%ui_bridge%' OR step_type LIKE '%ui-bridge%'
                            OR step_name LIKE '%UI Bridge%' OR step_name LIKE '%ui_bridge%'
                            OR step_name LIKE '%snapshot%' OR step_name LIKE '%discover%'))
                    +
                    (SELECT COUNT(*) FROM unified_workflows uw
                     INNER JOIN task_runs tr ON tr.workflow_name = uw.name
                     WHERE tr.id = $1
                       AND (uw.setup_steps LIKE '%ui_bridge%' OR uw.setup_steps LIKE '%ui-bridge%'
                            OR uw.verification_steps LIKE '%ui_bridge%' OR uw.verification_steps LIKE '%ui-bridge%'
                            OR uw.agentic_steps LIKE '%ui_bridge%' OR uw.agentic_steps LIKE '%ui-bridge%'
                            OR uw.completion_steps LIKE '%ui_bridge%' OR uw.completion_steps LIKE '%ui-bridge%'))
                ) > 0"#,
                &[&task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        Ok(has)
    }

    /// Check if AI output mentions UI Bridge.
    pub async fn ai_mentions_ui_bridge(&self, task_run_id: &str) -> Result<bool, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let has: bool = conn
            .query_one(
                r#"SELECT COALESCE(
                    (SELECT 1 FROM task_run_output_chunks
                     WHERE task_run_id = $1
                       AND (content LIKE '%ui-bridge%' OR content LIKE '%ui_bridge%'
                            OR content LIKE '%/control/snapshot%' OR content LIKE '%/control/discover%')
                     LIMIT 1),
                    0
                ) > 0"#,
                &[&task_run_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(false);

        Ok(has)
    }

    /// Check convergence for a reflection type (project, ui_bridge).
    pub async fn check_reflection_convergence(
        &self,
        source_task_run_id: &str,
        workflow_name_prefix: &str,
        fix_scope_filter: &str,
        threshold: i64,
    ) -> Result<Vec<u32>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let workflow_name: Option<String> = conn
            .query_opt(
                "SELECT workflow_name FROM task_runs WHERE id = $1",
                &[&source_task_run_id],
            )
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get(0));

        let workflow_name = match workflow_name {
            Some(name) => name,
            None => return Ok(Vec::new()),
        };

        let pattern = format!("{}%", workflow_name_prefix);
        let rows = conn
            .query(
                &format!(
                    r#"SELECT tr.id,
                           (SELECT COUNT(*) FROM reflection_fixes rf
                            WHERE rf.reflection_task_run_id = tr.id
                              AND {}) as fix_count
                    FROM task_runs tr
                    WHERE tr.workflow_name LIKE $1
                      AND tr.is_reflection = true
                      AND tr.status IN ('complete', 'completed')
                      AND tr.reflection_source_task_run_id IN (
                        SELECT id FROM task_runs WHERE workflow_name = $2
                      )
                    ORDER BY tr.completed_at DESC
                    LIMIT $3"#,
                    fix_scope_filter
                ),
                &[&pattern, &workflow_name, &threshold],
            )
            .await
            .map_err(|e| format!("Failed to query convergence: {e}"))?;

        Ok(rows.iter().map(|r| r.get::<_, i64>(1) as u32).collect())
    }

    // ========================================================================
    // Agentic metric scores (for LLM judge / RAG judge)
    // ========================================================================

    /// Insert an agentic metric score.
    pub async fn upsert_agentic_metric_score(
        &self,
        id: &str,
        task_run_id: &str,
        metric_type: &str,
        score: f64,
        confidence: f64,
        rationale: &str,
        is_llm_judged: bool,
        model_used: Option<&str>,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.execute(
            r#"INSERT INTO agentic_metric_scores
                (id, task_run_id, metric_type, score, confidence,
                 rationale, is_llm_judged, model_used, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (id) DO UPDATE SET
                 score = EXCLUDED.score,
                 confidence = EXCLUDED.confidence,
                 rationale = EXCLUDED.rationale,
                 is_llm_judged = EXCLUDED.is_llm_judged,
                 model_used = EXCLUDED.model_used"#,
            &[
                &id,
                &task_run_id,
                &metric_type,
                &score,
                &confidence,
                &rationale,
                &is_llm_judged,
                &model_used as &(dyn tokio_postgres::types::ToSql + Sync),
                &created_at,
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert agentic score: {e}"))?;

        Ok(())
    }

    /// Get all metric scores for a task run (metric_type, score).
    pub async fn get_all_metric_scores(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<(String, f64)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                "SELECT metric_type, score FROM agentic_metric_scores WHERE task_run_id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query scores: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, f64>(1)))
            .collect())
    }

    /// Update composite agentic score in learning_outcomes.
    pub async fn update_composite_agentic_score(
        &self,
        task_id: &str,
        composite: f64,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.execute(
            "UPDATE learning_outcomes SET composite_agentic_score = $1 WHERE task_id = $2",
            &[&composite, &task_id],
        )
        .await
        .map_err(|e| format!("Failed to update composite score: {e}"))?;

        Ok(())
    }

    /// Get task run data for LLM judge input.
    pub async fn get_task_run_for_judge(
        &self,
        task_run_id: &str,
    ) -> Result<
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<bool>,
            String,
        ),
        String,
    > {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_one(
                r#"SELECT prompt, execution_steps_json, summary, goal_achieved, status
                   FROM task_runs WHERE id = $1"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query task_run {}: {}", task_run_id, e))?;

        Ok((row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)))
    }

    /// Get execution trace summary from events.
    pub async fn get_execution_trace_summary(
        &self,
        task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT message FROM task_run_events
                   WHERE task_run_id = $1
                     AND event_type IN ('step_execution', 'step_completed', 'step_failed')
                   ORDER BY created_at ASC
                   LIMIT 50"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query trace: {e}"))?;

        if rows.is_empty() {
            return Ok(None);
        }

        let msgs: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        Ok(Some(msgs.join("\n")))
    }

    /// Get task prompt (for RAG judge).
    pub async fn get_task_prompt(&self, task_run_id: &str) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        conn.query_one(
            "SELECT COALESCE(prompt, '') FROM task_runs WHERE id = $1",
            &[&task_run_id],
        )
        .await
        .map(|r| r.get(0))
        .map_err(|e| format!("Failed to get prompt: {e}"))
    }

    /// Get reflection source task run id.
    pub async fn get_reflection_source_task_run_id(
        &self,
        task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_opt(
                "SELECT reflection_source_task_run_id FROM task_runs WHERE id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query: {e}"))?;

        Ok(row.and_then(|r| r.get(0)))
    }

    /// Look up source workflow name for cross-run analysis.
    pub async fn get_source_workflow_name(
        &self,
        task_run_id: &str,
        default_name: &str,
    ) -> Result<String, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let name: String = conn
            .query_one(
                r#"SELECT COALESCE(
                    (SELECT workflow_name FROM task_runs WHERE id = (
                        SELECT reflection_source_task_run_id FROM task_runs WHERE id = $1
                    )),
                    $2
                )"#,
                &[&task_run_id, &default_name],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or_else(|_| default_name.to_string());

        Ok(name)
    }

    /// Get workflow_name from task_runs.
    pub async fn get_workflow_name_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_opt(
                "SELECT workflow_name FROM task_runs WHERE id = $1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query: {e}"))?;

        Ok(row.and_then(|r| r.get(0)))
    }

    // ========================================================================
    // Finding insertion (for claude_session, phase_helpers, test_executor)
    // ========================================================================

    /// Insert a finding into task_run_findings (PG equivalent of finding_storage::insert_finding).
    pub async fn insert_finding_pg(
        &self,
        id: &str,
        task_run_id: &str,
        session_num: i32,
        title: &str,
        description: &str,
        category: &str,
        severity: &str,
        signature_hash: &str,
        status: &str,
        is_resolved: bool,
        evidence: Option<&str>,
        ai_model: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO task_run_findings (
                id, task_run_id, session_num, title, description,
                category, severity, signature_hash, status,
                is_resolved, evidence, ai_model, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            ON CONFLICT (id) DO NOTHING"#,
            &[
                &id,
                &task_run_id,
                &session_num,
                &title,
                &description,
                &category,
                &severity,
                &signature_hash,
                &status,
                &is_resolved,
                &evidence as &(dyn tokio_postgres::types::ToSql + Sync),
                &ai_model as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert finding: {e}"))?;

        Ok(())
    }

    // ========================================================================
    // Causal events (PG equivalents for reflection/causal.rs)
    // ========================================================================

    /// Insert a causal event into PG.
    pub async fn insert_causal_event(
        &self,
        cause_type: &str,
        cause_ref: &str,
        effect_type: &str,
        effect_ref: &str,
        relationship: &str,
        confidence: &str,
        source: &str,
        workflow_name: Option<&str>,
        task_run_id: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO causal_events
               (id, cause_event_type, cause_event_ref, effect_event_type, effect_event_ref,
                relationship, confidence, source, workflow_name, task_run_id, description, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
               ON CONFLICT DO NOTHING"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &cause_type, &cause_ref, &effect_type, &effect_ref,
                &relationship, &confidence, &source,
                &workflow_name as &(dyn tokio_postgres::types::ToSql + Sync),
                &task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &description as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_causal_event: {e}"))?;

        Ok(())
    }

    /// Get causal events for a workflow.
    pub async fn get_causal_events_for_workflow(
        &self,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::reflection::causal::CausalEvent>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT id, cause_event_type, cause_event_ref, effect_event_type,
                          effect_event_ref, relationship, confidence, source,
                          workflow_name, task_run_id, description, created_at::TEXT
                   FROM causal_events
                   WHERE workflow_name = $1
                   ORDER BY created_at DESC
                   LIMIT $2"#,
                &[&workflow_name, &limit],
            )
            .await
            .map_err(|e| format!("PG get_causal_events: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| crate::reflection::causal::CausalEvent {
                id: r.get(0),
                cause_event_type: r.get(1),
                cause_event_id: r.get(2),
                effect_event_type: r.get(3),
                effect_event_id: r.get(4),
                relationship: r.get(5),
                confidence: r.get(6),
                source: r.get(7),
                workflow_name: r.get(8),
                task_run_id: r.get(9),
                description: r.get(10),
                created_at: r.get(11),
            })
            .collect())
    }

    /// Get impact analysis for a file in a workflow.
    pub async fn get_impact_analysis(
        &self,
        workflow_name: &str,
        file_path: &str,
    ) -> Result<crate::reflection::architecture::ImpactAnalysis, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let rows = conn
            .query(
                r#"SELECT component_path, relationship_type, impact_direction
                   FROM architecture_components
                   WHERE workflow_name = $1
                     AND (component_path = $2 OR related_file = $2)
                   LIMIT 20"#,
                &[&workflow_name, &file_path],
            )
            .await
            .unwrap_or_default();

        let direct_impacts: Vec<crate::reflection::architecture::ImpactEntry> = rows
            .iter()
            .map(|r| crate::reflection::architecture::ImpactEntry {
                component_path: r.get(0),
                relationship_type: r.get(1),
                strength: 1,
            })
            .collect();

        let total = direct_impacts.len() as u32;

        Ok(crate::reflection::architecture::ImpactAnalysis {
            component: file_path.to_string(),
            direct_impacts,
            transitive_impacts: vec![],
            total_impact_radius: total,
            highest_risk_path: vec![],
        })
    }

    // ========================================================================
    // Known issues auto-detection
    // ========================================================================

    /// Check and promote recurring findings to known issues.
    /// Returns IDs of newly created known issues.
    pub async fn check_and_promote_recurring_findings(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<String>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        // Find findings with 3+ occurrences across task runs
        let rows = conn
            .query(
                r#"SELECT trf.signature_hash, trf.title, trf.description, COUNT(*) as cnt
                   FROM task_run_findings trf
                   WHERE trf.signature_hash IN (
                       SELECT signature_hash FROM task_run_findings WHERE task_run_id = $1
                   )
                   AND trf.signature_hash IS NOT NULL
                   AND trf.signature_hash NOT IN (
                       SELECT COALESCE(signature_hash, '') FROM known_issues
                       WHERE signature_hash IS NOT NULL
                   )
                   GROUP BY trf.signature_hash, trf.title, trf.description
                   HAVING COUNT(*) >= 3
                   LIMIT 10"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG check_recurring: {e}"))?;

        let mut new_ids = Vec::new();
        for row in &rows {
            let sig: String = row.get(0);
            let title: String = row.get(1);
            let description: Option<String> = row.get(2);
            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();

            let result = conn
                .execute(
                    r#"INSERT INTO known_issues
                       (id, title, description, signature_hash, status, auto_detected, created_at)
                       VALUES ($1, $2, $3, $4, 'new', true, $5)
                       ON CONFLICT DO NOTHING"#,
                    &[
                        &id as &(dyn tokio_postgres::types::ToSql + Sync),
                        &title,
                        &description as &(dyn tokio_postgres::types::ToSql + Sync),
                        &sig,
                        &now,
                    ],
                )
                .await;

            if let Ok(n) = result {
                if n > 0 {
                    new_ids.push(id);
                }
            }
        }

        Ok(new_ids)
    }

    // ========================================================================
    // Example workflows promotion
    // ========================================================================

    /// Try to promote a workflow to the example library on success.
    pub async fn try_promote_on_success(&self, workflow_id: &str) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        // Check if workflow has enough successful runs
        let success_count: i64 = conn
            .query_one(
                r#"SELECT COUNT(*) FROM task_runs
                   WHERE workflow_name = (SELECT name FROM unified_workflows WHERE id = $1)
                     AND status IN ('complete', 'completed')
                     AND goal_achieved = true"#,
                &[&workflow_id],
            )
            .await
            .map(|r| r.get(0))
            .unwrap_or(0);

        if success_count >= 3 {
            // Check if already in example library
            let existing: bool = conn
                .query_one(
                    "SELECT COUNT(*) > 0 FROM example_workflows WHERE source_workflow_id = $1",
                    &[&workflow_id],
                )
                .await
                .map(|r| r.get(0))
                .unwrap_or(true);

            if !existing {
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();

                let _ = conn
                    .execute(
                        r#"INSERT INTO example_workflows
                           (id, source_workflow_id, success_count, promoted_at)
                           VALUES ($1, $2, $3, $4)
                           ON CONFLICT DO NOTHING"#,
                        &[
                            &id as &(dyn tokio_postgres::types::ToSql + Sync),
                            &workflow_id,
                            &(success_count as i32),
                            &now,
                        ],
                    )
                    .await;
            }
        }

        Ok(())
    }

    // ========================================================================
    // Workflow learning from learning_recorder.rs
    // ========================================================================

    /// Record workflow learning from a completed task outcome.
    pub async fn record_workflow_learning_from_outcome(
        &self,
        outcome: &crate::orchestrator::learning_recorder::WorkflowOutcome,
    ) -> Result<(), String> {
        let id = uuid::Uuid::new_v4().to_string();

        let tools_json = if !outcome.tools_used.is_empty() {
            Some(serde_json::to_string(&outcome.tools_used).unwrap_or_default())
        } else {
            None
        };
        let files_json = if !outcome.files_modified.is_empty() {
            Some(serde_json::to_string(&outcome.files_modified).unwrap_or_default())
        } else {
            None
        };

        self.record_learning_outcome(
            &id,
            &outcome.task_run_id,
            &outcome.status,
            Some(outcome.duration_secs),
            Some(outcome.iterations as i32),
            None, // strategy
            tools_json.as_deref(),
            files_json.as_deref(),
            outcome.error_type.as_deref(),
            outcome.error_message.as_deref(),
            None, // feedback
            outcome.workflow_architecture.as_deref(),
            outcome.step_count,
            outcome.verification_step_count,
            outcome.agentic_step_count,
            outcome.has_ui_bridge,
            outcome.total_tokens.map(|t| t as i64),
            outcome.total_cost_usd,
            None, // technology_tags
            None, // domain_tags
            None, // complexity_tier
        )
        .await?;

        Ok(())
    }

    // ========================================================================
    // LLM Judge input gathering
    // ========================================================================

    /// Gather input data for the LLM judge from PG.
    pub async fn gather_llm_judge_input(
        &self,
        task_run_id: &str,
    ) -> Result<crate::meta_optimizer::agentic_metrics::llm_judge::LlmJudgeInput, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let row = conn
            .query_one(
                r#"SELECT prompt, execution_steps_json, summary, goal_achieved, status
                   FROM task_runs WHERE id = $1"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("Failed to query task_run {}: {e}", task_run_id))?;

        let prompt: Option<String> = row.get(0);
        let execution_steps: Option<String> = row.get(1);
        let summary: Option<String> = row.get(2);
        let goal_achieved: Option<bool> = row.get(3);
        let status: String = row.get(4);

        // Build execution trace summary
        let trace_rows = conn
            .query(
                r#"SELECT message FROM task_run_events
                   WHERE task_run_id = $1
                     AND event_type = 'step_execution'
                   ORDER BY timestamp ASC
                   LIMIT 20"#,
                &[&task_run_id],
            )
            .await
            .unwrap_or_default();

        let execution_trace_summary = if !trace_rows.is_empty() {
            let msgs: Vec<String> = trace_rows.iter().map(|r| r.get::<_, String>(0)).collect();
            Some(msgs.join("\n"))
        } else {
            None
        };

        Ok(
            crate::meta_optimizer::agentic_metrics::llm_judge::LlmJudgeInput {
                task_run_id: task_run_id.to_string(),
                task_prompt: prompt.unwrap_or_default(),
                execution_plan: execution_steps,
                outcome_summary: summary,
                goal_achieved,
                status,
                execution_trace_summary,
            },
        )
    }

    // ========================================================================
    // Error monitor context (PG equivalent of DebugContextCurator)
    // ========================================================================

    /// Build error monitor debug context for AI injection.
    /// Returns formatted string or None if no errors.
    pub async fn build_error_monitor_context(
        &self,
        task_run_id: Option<&str>,
        max_errors: usize,
    ) -> Result<Option<String>, String> {
        let errors = self.get_unresolved_errors(task_run_id, max_errors).await?;

        if errors.is_empty() {
            return Ok(None);
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "## Error Monitor ({} unresolved errors)",
            errors.len()
        ));

        for err in &errors {
            let severity = err["severity"].as_str().unwrap_or("unknown");
            let message = err["message"].as_str().unwrap_or("(no message)");
            let file = err["file_path"].as_str().unwrap_or("");
            let line_num = err["line_number"].as_i64().unwrap_or(0);

            if file.is_empty() {
                lines.push(format!("- [{}] {}", severity.to_uppercase(), message));
            } else {
                lines.push(format!(
                    "- [{}] {} ({}:{})",
                    severity.to_uppercase(),
                    message,
                    file,
                    line_num
                ));
            }
        }

        Ok(Some(lines.join("\n")))
    }

    // ========================================================================
    // Generation rules from reflection fixes
    // ========================================================================

    /// Create generation rules from reflection fixes (PG equivalent).
    /// Returns the number of rules created.
    pub async fn create_rules_from_reflection_fixes(
        &self,
        fixes: &[crate::reflection::types::ReflectionFix],
    ) -> Result<usize, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;
        let mut count = 0;

        for fix in fixes {
            // Only high/medium confidence fixes
            if !matches!(fix.confidence.as_str(), "high" | "medium") {
                continue;
            }
            // Only qualifying fix types
            if !matches!(
                fix.fix_type.as_str(),
                "instruction_clarification"
                    | "workflow_step_rewrite"
                    | "prompt_refinement"
                    | "verification_improvement"
                    | "error_handling"
            ) {
                continue;
            }

            let id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let title = if fix.fix_description.len() > 100 {
                format!("{}...", &fix.fix_description[..97])
            } else {
                fix.fix_description.clone()
            };

            let result = conn
                .execute(
                    r#"INSERT INTO generation_rules
                       (id, rule_type, title, content, priority, is_active,
                        source_fix_id, provenance, created_at)
                       VALUES ($1, $2, $3, $4, 5, true, $5, 'reflection', $6)
                       ON CONFLICT DO NOTHING"#,
                    &[
                        &id as &(dyn tokio_postgres::types::ToSql + Sync),
                        &fix.fix_type,
                        &title,
                        &fix.fix_description,
                        &fix.id as &(dyn tokio_postgres::types::ToSql + Sync),
                        &now,
                    ],
                )
                .await;

            if let Ok(n) = result {
                if n > 0 {
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    // ========================================================================
    // Hybrid search for universal fixes
    // ========================================================================

    /// SQL-based fallback for universal fixes retrieval (no embedding required).
    pub async fn get_universal_fixes_by_text(
        &self,
        query_text: &str,
        limit: i64,
    ) -> Result<Vec<(String, String, Option<String>)>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool: {e}"))?;

        let search = format!("%{}%", &query_text[..query_text.len().min(100)]);

        let rows = conn
            .query(
                r#"SELECT rf.fix_description, rf.fix_type, rf.file_changed
                   FROM reflection_fixes rf
                   WHERE rf.reflection_scope = 'universal'
                     AND rf.status = 'applied'
                     AND rf.effectiveness IN ('effective', 'pending')
                     AND rf.fix_description ILIKE $1
                   ORDER BY rf.created_at DESC
                   LIMIT $2"#,
                &[&search, &limit],
            )
            .await
            .map_err(|e| format!("PG get_universal_fixes: {e}"))?;

        Ok(rows
            .iter()
            .map(|r| {
                let desc: String = r.get(0);
                let fix_type: String = r.get(1);
                let file: Option<String> = r.get(2);
                (desc, fix_type, file)
            })
            .collect())
    }
}
