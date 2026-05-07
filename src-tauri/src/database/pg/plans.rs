//! PostgreSQL CRUD operations for the `plans` table (migration v28).
//!
//! See §2 of `qontinui-dev-notes/plans/productivity-stack.md` for the data
//! model. Each row corresponds to a decomposed `.md` plan in
//! `qontinui-dev-notes/plans/`. `version_hash` is the content-addressing
//! handle: when the plan markdown changes, downstream tasks referencing the
//! old hash become stale and must be re-decomposed.
//!
//! Queries cast UUID columns to TEXT in SELECT and from TEXT in INSERT (via
//! `$N::uuid`) because tokio-postgres in this crate does not have the
//! `with-uuid-1` feature enabled. UUID identity is preserved on the wire as
//! the canonical `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` string form.

use super::PgDb;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A row from the `plans` table.
///
/// Uses `camelCase` serde rename so that Tauri commands can return this
/// shape directly to the frontend without an additional mapping layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRow {
    pub id: String,
    pub markdown_path: String,
    pub version_hash: String,
    pub status: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input shape for `insert_plan` — the caller supplies the markdown path,
/// hash, and optional metadata; the DB allocates the UUID via
/// `gen_random_uuid()` and the row is returned.
#[derive(Debug, Clone)]
pub struct InsertPlanInput<'a> {
    pub markdown_path: &'a str,
    pub version_hash: &'a str,
    pub status: &'a str,
    pub title: Option<&'a str>,
    pub summary: Option<&'a str>,
}

impl PgDb {
    /// Insert a new plan row. The DB allocates the UUID.
    pub async fn insert_plan(&self, input: &InsertPlanInput<'_>) -> Result<PlanRow, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                r#"
                INSERT INTO coord.plans (markdown_path, version_hash, status, title, summary)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING id::text, markdown_path, version_hash, status, title, summary,
                          created_at::text, updated_at::text
                "#,
                &[
                    &input.markdown_path,
                    &input.version_hash,
                    &input.status,
                    &input.title,
                    &input.summary,
                ],
            )
            .await
            .map_err(|e| format!("Failed to insert plan: {}", e))?;

        Ok(PlanRow {
            id: row.get(0),
            markdown_path: row.get(1),
            version_hash: row.get(2),
            status: row.get(3),
            title: row.get(4),
            summary: row.get(5),
            created_at: row.get(6),
            updated_at: row.get(7),
        })
    }

    /// Look up a plan by its absolute markdown path. Used by `/decompose-plan`
    /// to detect re-runs of the same plan.
    pub async fn get_plan_by_path(&self, markdown_path: &str) -> Result<Option<PlanRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT id::text, markdown_path, version_hash, status, title, summary,
                       created_at::text, updated_at::text
                FROM coord.plans
                WHERE markdown_path = $1
                "#,
                &[&markdown_path],
            )
            .await
            .map_err(|e| format!("Failed to load plan by path: {}", e))?;

        Ok(row.map(|r| PlanRow {
            id: r.get(0),
            markdown_path: r.get(1),
            version_hash: r.get(2),
            status: r.get(3),
            title: r.get(4),
            summary: r.get(5),
            created_at: r.get(6),
            updated_at: r.get(7),
        }))
    }

    /// Look up a plan by its UUID (string form).
    pub async fn get_plan_by_id(&self, plan_id: &str) -> Result<Option<PlanRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let plan_uuid =
            Uuid::parse_str(plan_id).map_err(|e| format!("invalid plan_id uuid: {}", e))?;
        let row = conn
            .query_opt(
                r#"
                SELECT id::text, markdown_path, version_hash, status, title, summary,
                       created_at::text, updated_at::text
                FROM coord.plans
                WHERE id = $1::uuid
                "#,
                &[&plan_uuid],
            )
            .await
            .map_err(|e| format!("Failed to load plan by id: {}", e))?;

        Ok(row.map(|r| PlanRow {
            id: r.get(0),
            markdown_path: r.get(1),
            version_hash: r.get(2),
            status: r.get(3),
            title: r.get(4),
            summary: r.get(5),
            created_at: r.get(6),
            updated_at: r.get(7),
        }))
    }

    /// List all plans, newest first. Excludes `done` and `abandoned` rows
    /// so the live workboard isn't polluted by historical plans —
    /// `list_plans_filtered(true)` includes them on demand.
    pub async fn list_plans(&self) -> Result<Vec<PlanRow>, String> {
        self.list_plans_filtered(false).await
    }

    /// List plans, newest first. When `include_archived` is `false`
    /// (default), drops rows whose status is `done` or `abandoned`. When
    /// `true`, returns every row regardless of status — used by the plan
    /// board's "Show archived" toggle.
    pub async fn list_plans_filtered(
        &self,
        include_archived: bool,
    ) -> Result<Vec<PlanRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let sql = if include_archived {
            r#"
            SELECT id::text, markdown_path, version_hash, status, title, summary,
                   created_at::text, updated_at::text
            FROM coord.plans
            ORDER BY created_at DESC
            "#
        } else {
            r#"
            SELECT id::text, markdown_path, version_hash, status, title, summary,
                   created_at::text, updated_at::text
            FROM coord.plans
            WHERE status NOT IN ('done', 'abandoned')
            ORDER BY created_at DESC
            "#
        };

        let rows = conn
            .query(sql, &[])
            .await
            .map_err(|e| format!("Failed to list plans: {}", e))?;

        Ok(rows
            .into_iter()
            .map(|r| PlanRow {
                id: r.get(0),
                markdown_path: r.get(1),
                version_hash: r.get(2),
                status: r.get(3),
                title: r.get(4),
                summary: r.get(5),
                created_at: r.get(6),
                updated_at: r.get(7),
            })
            .collect())
    }

    /// Archive a plan: set status → 'abandoned'. Returns true if a row
    /// was updated. Reversible via `unarchive_plan`.
    pub async fn archive_plan(&self, plan_id: &str) -> Result<bool, String> {
        self.update_plan_status(plan_id, "abandoned").await
    }

    /// Restore an archived plan to live status. If the plan has at least
    /// one task row the status flips to `decomposed` (it's already been
    /// through `/decompose-plan`); otherwise it falls back to `vetted`
    /// (the pre-decomposition status). Returns true on update.
    pub async fn unarchive_plan(&self, plan_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let plan_uuid =
            Uuid::parse_str(plan_id).map_err(|e| format!("invalid plan_id uuid: {}", e))?;
        let task_count: i64 = conn
            .query_one(
                "SELECT COUNT(*)::bigint FROM coord.tasks WHERE plan_id = $1::uuid",
                &[&plan_uuid],
            )
            .await
            .map_err(|e| format!("Failed to count tasks for plan: {}", e))?
            .get(0);

        let target = if task_count > 0 {
            "decomposed"
        } else {
            "vetted"
        };
        self.update_plan_status(plan_id, target).await
    }

    /// Update a plan's status. Returns true if a row was updated.
    pub async fn update_plan_status(&self, plan_id: &str, status: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let plan_uuid =
            Uuid::parse_str(plan_id).map_err(|e| format!("invalid plan_id uuid: {}", e))?;
        let n = conn
            .execute(
                r#"
                UPDATE coord.plans
                SET status = $2, updated_at = NOW()
                WHERE id = $1::uuid
                "#,
                &[&plan_uuid, &status],
            )
            .await
            .map_err(|e| format!("Failed to update plan status: {}", e))?;

        Ok(n > 0)
    }

    /// Delete a plan by id. Tasks cascade-delete via FK. Returns true if a
    /// row was removed.
    pub async fn delete_plan(&self, plan_id: &str) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let plan_uuid =
            Uuid::parse_str(plan_id).map_err(|e| format!("invalid plan_id uuid: {}", e))?;
        let n = conn
            .execute(
                r#"
                DELETE FROM coord.plans WHERE id = $1::uuid
                "#,
                &[&plan_uuid],
            )
            .await
            .map_err(|e| format!("Failed to delete plan: {}", e))?;

        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    // Integration tests require a live PG; keeping placeholder for the team
    // to wire behind the sibling integration-test harness when it lands.
    // Phase 1 acceptance criteria do not require executable tests at this
    // module — see `qontinui-dev-notes/plans/productivity-stack.md` §8 Phase 1
    // "Tests".
}
