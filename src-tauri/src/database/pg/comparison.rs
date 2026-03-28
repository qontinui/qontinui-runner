//! PostgreSQL operations for comparison runs.

use super::PgDb;

impl PgDb {
    /// Insert a new comparison run.
    pub async fn create_comparison_run(
        &self,
        id: &str,
        workflow_id: &str,
        variation_type: &str,
        entries_json: &str,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO comparison_runs (id, workflow_id, variation_type, status, entries_json, created_at)
               VALUES ($1, $2, $3, 'running', $4, $5)"#,
            &[&id, &workflow_id, &variation_type, &entries_json, &created_at],
        )
        .await
        .map_err(|e| format!("PG create_comparison_run: {}", e))?;
        Ok(())
    }

    /// Update comparison run entries and status.
    pub async fn update_comparison_run_entries(
        &self,
        id: &str,
        entries_json: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE comparison_runs SET entries_json = $1, status = $2 WHERE id = $3",
            &[&entries_json, &status, &id],
        )
        .await
        .map_err(|e| format!("PG update_comparison_run_entries: {}", e))?;
        Ok(())
    }

    /// Get a comparison run by ID.
    pub async fn get_comparison_run(
        &self,
        id: &str,
    ) -> Result<Option<ComparisonRunRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, workflow_id, variation_type, status, entries_json,
                          report, created_at, completed_at
                   FROM comparison_runs WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_comparison_run: {}", e))?;

        Ok(row.map(|r| ComparisonRunRow {
            id: r.get(0),
            workflow_id: r.get(1),
            variation_type: r.get(2),
            status: r.get(3),
            entries_json: r.get(4),
            report: r.get(5),
            created_at: r.get(6),
            completed_at: r.get(7),
        }))
    }

    /// Complete a comparison run.
    pub async fn complete_comparison_run(
        &self,
        id: &str,
        entries_json: &str,
    ) -> Result<(), String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE comparison_runs SET status = 'completed', completed_at = $1, entries_json = $2 WHERE id = $3",
            &[&now, &entries_json, &id],
        )
        .await
        .map_err(|e| format!("PG complete_comparison_run: {}", e))?;
        Ok(())
    }

    /// List recent comparison runs.
    pub async fn list_comparison_runs(&self, limit: i64) -> Result<Vec<ComparisonRunRow>, String> {
        let conn = self.pool.get().await.map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, workflow_id, variation_type, status, entries_json,
                          report, created_at, completed_at
                   FROM comparison_runs
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG list_comparison_runs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| ComparisonRunRow {
                id: r.get(0),
                workflow_id: r.get(1),
                variation_type: r.get(2),
                status: r.get(3),
                entries_json: r.get(4),
                report: r.get(5),
                created_at: r.get(6),
                completed_at: r.get(7),
            })
            .collect())
    }
}

/// Row from comparison_runs table.
pub struct ComparisonRunRow {
    pub id: String,
    pub workflow_id: String,
    pub variation_type: String,
    pub status: String,
    pub entries_json: String,
    pub report: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}
