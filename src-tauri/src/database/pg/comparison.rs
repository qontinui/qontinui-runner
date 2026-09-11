//! PostgreSQL operations for comparison runs.
//!
//! This is the **only** persistence path for `project.comparison_runs`. It used
//! to be one of two: `tiered_info.rs` carried a second, near-identical set
//! (`insert_comparison` / `update_comparison` / `update_comparison_entries` /
//! `get_comparison` / `complete_comparison` / `list_comparisons`) that the Tauri
//! command path used while the HTTP path used this one. Two writers for one
//! table is how the declared axis got recorded on one path and not the other —
//! the same duplicate-grammar defect the plan closes on the *variation* side, a
//! layer down. The duplicates are deleted; both surfaces call these.
//!
//! Every writer here derives the run's axis facts from the very bytes it is
//! storing, via [`crate::comparison::axis_facts_from_entries_json`]. That is
//! what makes `computed_axis` / `axis_drift_class` a *recorded* fact rather than
//! a second declaration: the pair cannot drift from `entries_json`, because
//! nothing can write one without the other.
//!
//! ## Two Rust/Postgres type mismatches fixed in passing
//!
//! Both were latent runtime errors that no compile step could see, and both
//! were found by asking Postgres what it infers rather than by reading the SQL:
//!
//! * `created_at` is `timestamptz`, but the insert bound a Rust `&str` to a
//!   bare `$5`. Postgres resolves that parameter to `timestamptz` (verified via
//!   `pg_prepared_statements`), so the bind failed. `$5::text::timestamptz`
//!   pins the parameter to `text` and casts server-side.
//! * The reads selected `created_at` / `completed_at` straight into Rust
//!   `String`, which cannot decode a `timestamptz`. They now select
//!   `::TEXT` — the same cast the (now-deleted) `tiered_info` copy always had,
//!   which is why the desktop path worked and this one did not.

use super::PgDb;
use crate::comparison::axis_facts_from_entries_json;

impl PgDb {
    /// Insert a new comparison run, recording the axis its arms actually move
    /// alongside the `variation_type` its author declared.
    pub async fn create_comparison_run(
        &self,
        id: &str,
        workflow_id: &str,
        variation_type: &str,
        entries_json: &str,
        created_at: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let facts = axis_facts_from_entries_json(variation_type, entries_json);
        let computed_axis = facts.computed_axis_json();
        let drift_class = facts.drift_class.as_wire_str();
        conn.execute(
            r#"INSERT INTO comparison_runs
                   (id, workflow_id, variation_type, status, entries_json, created_at,
                    computed_axis, axis_drift_class)
               VALUES ($1, $2, $3, 'running', $4, $5::text::timestamptz, $6, $7)"#,
            &[
                &id,
                &workflow_id,
                &variation_type,
                &entries_json,
                &created_at,
                &computed_axis,
                &drift_class,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG create_comparison_run", &e))?;
        Ok(())
    }

    /// Update comparison run entries and status, re-deriving the axis facts.
    ///
    /// `variation_type` is taken as an argument rather than re-read from the row
    /// because the caller always has it, and because the pair must be written in
    /// the same statement as the `entries_json` it describes.
    pub async fn update_comparison_run_entries(
        &self,
        id: &str,
        variation_type: &str,
        entries_json: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let facts = axis_facts_from_entries_json(variation_type, entries_json);
        let computed_axis = facts.computed_axis_json();
        let drift_class = facts.drift_class.as_wire_str();
        conn.execute(
            r#"UPDATE comparison_runs
                  SET entries_json = $1, status = $2,
                      computed_axis = $3, axis_drift_class = $4
                WHERE id = $5"#,
            &[&entries_json, &status, &computed_axis, &drift_class, &id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_comparison_run_entries", &e))?;
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
                          report, created_at::TEXT, completed_at::TEXT,
                          computed_axis, axis_drift_class
                   FROM comparison_runs WHERE id = $1"#,
                &[&id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_comparison_run", &e))?;

        Ok(row.map(|r| ComparisonRunRow::from_row(&r)))
    }

    /// Complete a comparison run, re-deriving the axis facts from the final
    /// entries.
    pub async fn complete_comparison_run(
        &self,
        id: &str,
        variation_type: &str,
        entries_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let facts = axis_facts_from_entries_json(variation_type, entries_json);
        let computed_axis = facts.computed_axis_json();
        let drift_class = facts.drift_class.as_wire_str();
        conn.execute(
            r#"UPDATE comparison_runs
                  SET status = 'completed', completed_at = NOW(), entries_json = $1,
                      computed_axis = $2, axis_drift_class = $3
                WHERE id = $4"#,
            &[&entries_json, &computed_axis, &drift_class, &id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG complete_comparison_run", &e))?;
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
                          report, created_at::TEXT, completed_at::TEXT,
                          computed_axis, axis_drift_class
                   FROM comparison_runs
                   ORDER BY created_at DESC
                   LIMIT $1"#,
                &[&limit],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG list_comparison_runs", &e))?;

        Ok(rows.iter().map(ComparisonRunRow::from_row).collect())
    }
}

/// Row from comparison_runs table.
#[derive(Debug, Clone)]
pub struct ComparisonRunRow {
    pub id: String,
    pub workflow_id: String,
    pub variation_type: String,
    pub status: String,
    pub entries_json: String,
    pub report: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    /// The key paths observed to actually differ across this run's arms.
    ///
    /// `None` is the column's SQL `NULL`: the axis was **never computed** —
    /// a row written before `cmpaxis_01_comparison_computed_axis`, or one whose
    /// arms could not be read. `Some(<empty array>)` is the opposite claim,
    /// that it *was* computed and nothing differed. Readers must keep those
    /// apart; `axis_drift_class` says which case a row is in.
    pub computed_axis: Option<serde_json::Value>,
    /// The declared-vs-actual classification wire token — see
    /// [`crate::comparison::AxisDriftClass`]. `NOT NULL DEFAULT 'unknown'`, so a
    /// pre-existing row reads as the coverage-gap class, never as agreement.
    pub axis_drift_class: String,
}

impl ComparisonRunRow {
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    fn from_row(r: &tokio_postgres::Row) -> Self {
        ComparisonRunRow {
            id: r.get(0),
            workflow_id: r.get(1),
            variation_type: r.get(2),
            status: r.get(3),
            entries_json: r.get(4),
            report: r.get(5),
            created_at: r.get(6),
            completed_at: r.get(7),
            computed_axis: r.get(8),
            axis_drift_class: r.get(9),
        }
    }
}
