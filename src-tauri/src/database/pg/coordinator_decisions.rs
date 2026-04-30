//! PostgreSQL CRUD for the `coordinator_decisions` table (migration v29).
//!
//! See §4 of `qontinui-dev-notes/plans/productivity-stack.md`. Every decision
//! the Coordinator makes — cheap-rule fires, escalations, and even no-ops —
//! is logged here. The dashboard's Decision Log renders these
//! chronologically; the Escalations panel filters for unresolved rows whose
//! action is destructive (`escalate`, `kill-session`,
//! `force-promote-to-worktree`).
//!
//! Inserts come from `mcp::coordinator::POST /coordinator/act`. Reads come
//! from the dashboard via Tauri commands in `commands::productivity`.
//!
//! UUIDs round-trip as TEXT because tokio-postgres in this crate does not
//! enable `with-uuid-1` — same convention as `pg::plans` / `pg::tasks`.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// One row from `coordinator_decisions`. The TS contract in
/// productivity-stack §8 lists this as `CoordinatorDecision` with
/// `camelCase` fields; the `serde(rename_all)` attribute lets Tauri
/// commands return this type directly to the React frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorDecisionRow {
    pub id: String,
    pub session_id: String,
    pub iteration: i64,
    pub rule: String,
    pub action: String,
    pub target_id: Option<String>,
    pub reasoning: String,
    pub auto_acted: bool,
    pub resolved: bool,
    pub resolution: Option<String>,
    pub resolved_at: Option<String>,
    pub created_at: String,
}

/// Input shape for `insert_coordinator_decision`. The DB allocates the UUID;
/// `created_at` defaults to NOW(). `resolved`/`resolution`/`resolved_at`
/// are not settable on insert — they're updated later via
/// [`PgDb::resolve_coordinator_decision`].
#[derive(Debug, Clone)]
pub struct InsertCoordinatorDecisionInput<'a> {
    pub session_id: &'a str,
    pub iteration: i64,
    pub rule: &'a str,
    pub action: &'a str,
    pub target_id: Option<&'a str>,
    pub reasoning: &'a str,
    pub auto_acted: bool,
}

const SELECT_COLS: &str = r#"
    id::text, session_id, iteration, rule, action, target_id,
    reasoning, auto_acted, resolved, resolution,
    resolved_at::text, created_at::text
"#;

fn row_to_decision(r: &tokio_postgres::Row) -> CoordinatorDecisionRow {
    CoordinatorDecisionRow {
        id: r.get(0),
        session_id: r.get(1),
        iteration: r.get(2),
        rule: r.get(3),
        action: r.get(4),
        target_id: r.get(5),
        reasoning: r.get(6),
        auto_acted: r.get(7),
        resolved: r.get(8),
        resolution: r.get(9),
        resolved_at: r.get(10),
        created_at: r.get(11),
    }
}

impl PgDb {
    /// Insert a single decision row. The DB allocates the UUID and stamps
    /// `created_at`. Returns the freshly-inserted row.
    pub async fn insert_coordinator_decision(
        &self,
        input: &InsertCoordinatorDecisionInput<'_>,
    ) -> Result<CoordinatorDecisionRow, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_one(
                &format!(
                    r#"
                    INSERT INTO coord.coordinator_decisions
                        (session_id, iteration, rule, action, target_id,
                         reasoning, auto_acted)
                    VALUES ($1, $2, $3, $4, $5, $6, $7)
                    RETURNING {}
                    "#,
                    SELECT_COLS
                ),
                &[
                    &input.session_id,
                    &input.iteration,
                    &input.rule,
                    &input.action,
                    &input.target_id,
                    &input.reasoning,
                    &input.auto_acted,
                ],
            )
            .await
            .map_err(|e| format!("Failed to insert coordinator decision: {}", e))?;

        Ok(row_to_decision(&row))
    }

    /// Look up a single decision by id.
    pub async fn get_coordinator_decision(
        &self,
        decision_id: &str,
    ) -> Result<Option<CoordinatorDecisionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                &format!(
                    "SELECT {} FROM coord.coordinator_decisions WHERE id = $1::uuid",
                    SELECT_COLS
                ),
                &[&decision_id],
            )
            .await
            .map_err(|e| format!("Failed to load coordinator decision: {}", e))?;

        Ok(row.as_ref().map(row_to_decision))
    }

    /// List the most-recent N decisions, newest-first. Optional rule and
    /// action filters mirror the Decision Log filter UI.
    pub async fn list_recent_coordinator_decisions(
        &self,
        limit: i64,
        rule_filter: Option<&str>,
        action_filter: Option<&str>,
    ) -> Result<Vec<CoordinatorDecisionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Filters compose so the dashboard can pivot freely without a
        // matrix of stored procedures. NULL on either filter param means
        // "no filter for that column".
        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM coord.coordinator_decisions
                    WHERE ($2::text IS NULL OR rule = $2)
                      AND ($3::text IS NULL OR action = $3)
                    ORDER BY created_at DESC
                    LIMIT $1
                    "#,
                    SELECT_COLS
                ),
                &[&limit, &rule_filter, &action_filter],
            )
            .await
            .map_err(|e| format!("Failed to list coordinator decisions: {}", e))?;

        Ok(rows.iter().map(row_to_decision).collect())
    }

    /// List decisions for a single Coordinator session, newest-first.
    pub async fn list_coordinator_decisions_for_session(
        &self,
        session_id: &str,
        limit: i64,
    ) -> Result<Vec<CoordinatorDecisionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM coord.coordinator_decisions
                    WHERE session_id = $1
                    ORDER BY created_at DESC
                    LIMIT $2
                    "#,
                    SELECT_COLS
                ),
                &[&session_id, &limit],
            )
            .await
            .map_err(|e| format!("Failed to list session decisions: {}", e))?;

        Ok(rows.iter().map(row_to_decision).collect())
    }

    /// List **open escalations**: rows that recommended a destructive action
    /// the user hasn't yet resolved. Matches the Escalations panel in
    /// productivity-stack §6.
    pub async fn list_open_escalations(&self) -> Result<Vec<CoordinatorDecisionRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM coord.coordinator_decisions
                    WHERE resolved = FALSE
                      AND auto_acted = FALSE
                      AND action IN ('escalate', 'kill-session', 'force-promote-to-worktree')
                    ORDER BY created_at DESC
                    "#,
                    SELECT_COLS
                ),
                &[],
            )
            .await
            .map_err(|e| format!("Failed to list open escalations: {}", e))?;

        Ok(rows.iter().map(row_to_decision).collect())
    }

    /// Mark an escalation as resolved with a free-form `resolution` note.
    /// Returns `true` if the row was updated (i.e. it existed and wasn't
    /// already resolved).
    pub async fn resolve_coordinator_decision(
        &self,
        decision_id: &str,
        resolution: &str,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE coord.coordinator_decisions
                SET resolved = TRUE,
                    resolution = $2,
                    resolved_at = NOW()
                WHERE id = $1::uuid
                  AND resolved = FALSE
                "#,
                &[&decision_id, &resolution],
            )
            .await
            .map_err(|e| format!("Failed to resolve coordinator decision: {}", e))?;

        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    // Integration tests require a live PG; tracked alongside the sibling
    // `pg::plans` / `pg::tasks` placeholders. No executable tests at this
    // module per productivity-stack §8 Phase 2.
}
