//! PostgreSQL operations for `project.comparison_runs`.
//!
//! **This is the only persistence layer for the table.** A second, near-verbatim
//! copy used to live in `database::pg::tiered_info` and served the Tauri command
//! path while these served the HTTP path; the two disagreed about timestamps in
//! ways that made one of them fail at runtime (see below). They are unified here
//! for the same reason `comparison::parse_variation` unified the three
//! `variation_type` grammars: one table, one path.
//!
//! # Timestamps are bound as `DateTime<Utc>`, never as RFC3339 strings
//!
//! `created_at` and `completed_at` are `timestamptz`. `tokio_postgres`'s `ToSql`
//! has no `&str`/`String` -> `timestamptz` impl, and it resolves a bound
//! parameter's type from the statement, so passing `Utc::now().to_rfc3339()`
//! fails with `error serializing parameter N` — an explicit `::timestamptz` cast
//! does not help. The same defect was diagnosed and fixed for
//! `coord.process_sessions` in `342469a53`; these functions still carried it,
//! which is why every `POST /comparison/start` failed. The read side had the
//! mirror defect: `row.get::<_, String>()` on a `timestamptz` column panics.
//!
//! Reads therefore go through `DateTime<Utc>` and are projected back to RFC3339
//! at the boundary, so the wire shape stays the ISO-8601 the API always
//! promised.

use chrono::{DateTime, Utc};

use super::PgDb;

impl PgDb {
    /// Insert a new comparison run.
    ///
    /// `computed_axis` / `axis_drift_class` are the *observed* half of the
    /// declared-vs-actual pair whose declared half is `variation_type`. They are
    /// written at creation because a run's arms — and so the axes they move —
    /// are fixed the moment the arms are built; nothing downstream rewrites an
    /// arm's `overrides`.
    ///
    /// `computed_axis: None` writes SQL NULL, which means **the axis was never
    /// computed** (a row from a build predating the column, or arms that could
    /// not be parsed). An empty array is what "nothing differed" looks like.
    /// Readers must keep those two apart.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_comparison_run(
        &self,
        id: &str,
        workflow_id: &str,
        variation_type: &str,
        entries_json: &str,
        created_at: DateTime<Utc>,
        computed_axis: Option<&serde_json::Value>,
        axis_drift_class: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"INSERT INTO comparison_runs
                   (id, workflow_id, variation_type, status, entries_json, created_at,
                    computed_axis, axis_drift_class)
               VALUES ($1, $2, $3, 'running', $4, $5, $6::jsonb, $7)"#,
            &[
                &id,
                &workflow_id,
                &variation_type,
                &entries_json,
                &created_at,
                &computed_axis,
                &axis_drift_class,
            ],
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            "UPDATE comparison_runs SET entries_json = $1, status = $2 WHERE id = $3",
            &[&entries_json, &status, &id],
        )
        .await
        .map_err(|e| format!("PG update_comparison_run_entries: {}", e))?;
        Ok(())
    }

    /// Get a comparison run by ID.
    pub async fn get_comparison_run(&self, id: &str) -> Result<Option<ComparisonRunRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_opt(
                r#"SELECT id, workflow_id, variation_type, status, entries_json,
                          report, created_at, completed_at,
                          computed_axis, axis_drift_class
                   FROM comparison_runs WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| format!("PG get_comparison_run: {}", e))?;

        Ok(row.as_ref().map(ComparisonRunRow::from_row))
    }

    /// Complete a comparison run.
    pub async fn complete_comparison_run(
        &self,
        id: &str,
        entries_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let now = Utc::now();
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
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                r#"SELECT id, workflow_id, variation_type, status, entries_json,
                          report, created_at, completed_at,
                          computed_axis, axis_drift_class
                   FROM comparison_runs
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| format!("PG list_comparison_runs: {}", e))?;

        Ok(rows.iter().map(ComparisonRunRow::from_row).collect())
    }
}

/// Row from `project.comparison_runs`.
pub struct ComparisonRunRow {
    pub id: String,
    pub workflow_id: String,
    pub variation_type: String,
    pub status: String,
    pub entries_json: String,
    pub report: Option<String>,
    /// RFC3339.
    pub created_at: String,
    /// RFC3339.
    pub completed_at: Option<String>,
    /// The key paths that actually differed across this run's arms.
    ///
    /// `None` is **UNKNOWN** — never computed — not "nothing differed";
    /// `Some([])` is the latter.
    pub computed_axis: Option<serde_json::Value>,
    /// The declared-vs-actual classification wire token. `unknown` for rows
    /// written before the axis was computed; never silently read as `none`.
    pub axis_drift_class: String,
}

impl ComparisonRunRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        let created_at: DateTime<Utc> = r.get(6);
        let completed_at: Option<DateTime<Utc>> = r.get(7);
        ComparisonRunRow {
            id: r.get(0),
            workflow_id: r.get(1),
            variation_type: r.get(2),
            status: r.get(3),
            entries_json: r.get(4),
            report: r.get(5),
            created_at: created_at.to_rfc3339(),
            completed_at: completed_at.map(|t| t.to_rfc3339()),
            computed_axis: r.get(8),
            axis_drift_class: r.get(9),
        }
    }

    /// The stored classification, parsed. An unrecognized token reads as
    /// [`crate::comparison::AxisDriftClass::Unknown`] rather than as agreement.
    pub fn axis_drift(&self) -> crate::comparison::AxisDriftClass {
        crate::comparison::AxisDriftClass::from_wire_str(&self.axis_drift_class)
    }
}
