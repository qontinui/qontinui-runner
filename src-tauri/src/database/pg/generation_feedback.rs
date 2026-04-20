//! PostgreSQL workflow generation feedback operations.
//!
//! Provides save/query for the `workflow_generation_feedback` table using raw SQL.

use super::PgDb;
use tracing::info;

impl PgDb {
    // ========================================================================
    // Workflow Generation Feedback
    // ========================================================================

    /// Record feedback on a generated workflow.
    pub async fn record_generation_feedback(
        &self,
        workflow_id: &str,
        task_run_id: Option<&str>,
        feedback_type: &str,
        edited_field: Option<&str>,
        old_value: Option<&str>,
        new_value: Option<&str>,
        delete_reason: Option<&str>,
        rating: Option<i32>,
        rating_comment: Option<&str>,
        workflow_category: Option<&str>,
        workflow_description: Option<&str>,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            r#"INSERT INTO workflow_generation_feedback
                (id, workflow_id, task_run_id, feedback_type, edited_field, old_value, new_value,
                 delete_reason, rating, rating_comment, workflow_category, workflow_description, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
            &[
                &id as &(dyn tokio_postgres::types::ToSql + Sync),
                &workflow_id,
                &task_run_id as &(dyn tokio_postgres::types::ToSql + Sync),
                &feedback_type,
                &edited_field as &(dyn tokio_postgres::types::ToSql + Sync),
                &old_value as &(dyn tokio_postgres::types::ToSql + Sync),
                &new_value as &(dyn tokio_postgres::types::ToSql + Sync),
                &delete_reason as &(dyn tokio_postgres::types::ToSql + Sync),
                &rating as &(dyn tokio_postgres::types::ToSql + Sync),
                &rating_comment as &(dyn tokio_postgres::types::ToSql + Sync),
                &workflow_category as &(dyn tokio_postgres::types::ToSql + Sync),
                &workflow_description as &(dyn tokio_postgres::types::ToSql + Sync),
                &now,
            ],
        )
        .await
        .map_err(|e| format!("PG record_generation_feedback: {}", e))?;

        info!(
            "Recorded PG {} feedback for workflow {} (id={})",
            feedback_type, workflow_id, id
        );
        Ok(id)
    }

    /// Get all feedback for a specific workflow.
    pub async fn get_feedback_for_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, workflow_id, task_run_id, feedback_type, edited_field,
                          old_value, new_value, delete_reason, rating, rating_comment,
                          workflow_category, workflow_description, created_at::TEXT
                   FROM workflow_generation_feedback
                   WHERE workflow_id = $1
                   ORDER BY created_at DESC"#,
                &[&workflow_id],
            )
            .await
            .map_err(|e| format!("PG get_feedback_for_workflow: {}", e))?;

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

    /// Composite edit-analysis view for the Generator Evaluation page.
    ///
    /// Returns a JSON object with edited_fields, type_distribution,
    /// rating_distribution, and recent_feedback arrays built from
    /// `workflow_generation_feedback`.
    pub async fn get_edit_analysis(&self) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let fields_rows = conn
            .query(
                r#"SELECT edited_field, COUNT(*)::BIGINT
                   FROM workflow_generation_feedback
                   WHERE feedback_type = 'edit' AND edited_field IS NOT NULL
                   GROUP BY edited_field
                   ORDER BY COUNT(*) DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_edit_analysis (fields): {}", e))?;
        let edited_fields: Vec<serde_json::Value> = fields_rows
            .iter()
            .map(|r| serde_json::json!([r.get::<_, String>(0), r.get::<_, i64>(1)]))
            .collect();

        let type_rows = conn
            .query(
                r#"SELECT feedback_type, COUNT(*)::BIGINT
                   FROM workflow_generation_feedback
                   GROUP BY feedback_type
                   ORDER BY COUNT(*) DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_edit_analysis (types): {}", e))?;
        let type_distribution: Vec<serde_json::Value> = type_rows
            .iter()
            .map(|r| serde_json::json!([r.get::<_, String>(0), r.get::<_, i64>(1)]))
            .collect();

        let rating_rows = conn
            .query(
                r#"SELECT rating, COUNT(*)::BIGINT
                   FROM workflow_generation_feedback
                   WHERE rating IS NOT NULL
                   GROUP BY rating
                   ORDER BY rating"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_edit_analysis (ratings): {}", e))?;
        let rating_distribution: Vec<serde_json::Value> = rating_rows
            .iter()
            .map(|r| serde_json::json!([r.get::<_, i32>(0), r.get::<_, i64>(1)]))
            .collect();

        let recent_rows = conn
            .query(
                r#"SELECT id, workflow_id, feedback_type, edited_field, old_value, new_value,
                          created_at::TEXT, workflow_description
                   FROM workflow_generation_feedback
                   ORDER BY created_at DESC
                   LIMIT 20"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_edit_analysis (recent): {}", e))?;
        let recent_feedback: Vec<serde_json::Value> = recent_rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<_, String>(0),
                    "workflow_id": r.get::<_, String>(1),
                    "feedback_type": r.get::<_, String>(2),
                    "edited_field": r.get::<_, Option<String>>(3),
                    "old_value": r.get::<_, Option<String>>(4),
                    "new_value": r.get::<_, Option<String>>(5),
                    "created_at": r.get::<_, String>(6),
                    "workflow_name": r.get::<_, Option<String>>(7),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "edited_fields": edited_fields,
            "type_distribution": type_distribution,
            "rating_distribution": rating_distribution,
            "recent_feedback": recent_feedback,
        }))
    }

    /// Get aggregated feedback summary for a workflow category.
    pub async fn get_feedback_summary(
        &self,
        workflow_category: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let (sql, rows) = if let Some(cat) = workflow_category {
            let r = conn
                .query(
                    r#"SELECT feedback_type, COUNT(*) as cnt,
                              AVG(CASE WHEN rating IS NOT NULL THEN rating END)::double precision as avg_rating
                       FROM workflow_generation_feedback
                       WHERE workflow_category = $1
                       GROUP BY feedback_type"#,
                    &[&cat],
                )
                .await
                .map_err(|e| format!("PG get_feedback_summary: {}", e))?;
            ("category", r)
        } else {
            let r = conn
                .query(
                    r#"SELECT feedback_type, COUNT(*) as cnt,
                              AVG(CASE WHEN rating IS NOT NULL THEN rating END)::double precision as avg_rating
                       FROM workflow_generation_feedback
                       GROUP BY feedback_type"#,
                    &[],
                )
                .await
                .map_err(|e| format!("PG get_feedback_summary: {}", e))?;
            ("all", r)
        };

        let mut summary = serde_json::Map::new();
        for row in &rows {
            let ft: String = row.get(0);
            let count: i64 = row.get(1);
            // AVG over integer returns numeric in PG; cast ::double precision in SQL,
            // but still use try_get to avoid panic on unexpected type.
            let avg_rating: Option<f64> = row.try_get(2).unwrap_or_else(|e| {
                tracing::warn!(
                    "get_feedback_summary: col 2 (avg_rating) decode error: {}",
                    e
                );
                None
            });
            summary.insert(
                ft,
                serde_json::json!({ "count": count, "avg_rating": avg_rating }),
            );
        }

        Ok(serde_json::json!({
            "scope": sql,
            "breakdown": summary,
        }))
    }
}
