//! PostgreSQL CRUD for the `project.coordinator_decisions` table.
//!
//! See §4 of `qontinui-dev-notes/plans/productivity-stack.md`. Since Phase 4
//! of `2026-09-12-consolidate-local-orchestration-onto-conductor` deleted
//! the Productivity scheduler and its dashboard, the only writer is the
//! `crate::deconflict` loop (`advise-with-text` advisories) and the only
//! reader/resolver is `commands::deconflict::resolve_escalation` behind the
//! in-session advisory banner.
//!
//! UUIDs round-trip as TEXT because tokio-postgres in this crate does not
//! enable `with-uuid-1` — same convention as `pg::tasks`.
//!
//! ## Schema authority — the runner authors this table
//!
//! Re-homed from `coord.*` to `project.*` by P3 of plan
//! `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration`. The
//! `coord.*` schema is authored SOLELY by qontinui-web's alembic, which
//! never runs on an end-user machine — and on such a machine the runner's
//! bundled per-machine PostgreSQL (`postgresql_embedded`) IS the production
//! database. So the old `coord.`-qualified SQL here either errored against a
//! table that was never provisioned or wrote to a private table no fleet
//! member could read. This table is machine-local operational state the
//! runner reads back itself, so the runner is now its author: the shape is
//! defined by the `CREATE TABLE IF NOT EXISTS` self-heal in
//! `database/pg/mod.rs` (`MACHINE_LOCAL_TABLES_DDL`), not by any alembic
//! revision.

use super::PgDb;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row from `coordinator_decisions`. The TS contract in
/// productivity-stack §8 lists this as `CoordinatorDecision` with
/// `camelCase` fields; the `serde(rename_all)` attribute lets Tauri
/// commands return this type directly to the React frontend.
///
/// `observation_hash` was added by the shadow-decisions migration
/// (`sd01_coord_coordinator_shadow_decisions`). Empty string for legacy
/// rows and any callers that don't pass a hash; the deleted Rust scheduler
/// stamped real SHA-256 hex digests so its diff endpoint could join shadow
/// vs live by observation snapshot.
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
    /// SHA-256 hex of the observation snapshot the decision was based on.
    /// Empty string for legacy rows or callers that don't supply one.
    #[serde(default)]
    pub observation_hash: String,
}

/// Input shape for `insert_coordinator_decision`. The DB allocates the UUID;
/// `created_at` defaults to NOW(). `resolved`/`resolution`/`resolved_at`
/// are not settable on insert — they're updated later via
/// [`PgDb::resolve_coordinator_decision`].
///
/// `observation_hash` defaults to empty when callers don't observe state
/// (HTTP-driven actions). The Rust scheduler always stamps a real hash.
#[derive(Debug, Clone)]
pub struct InsertCoordinatorDecisionInput<'a> {
    pub session_id: &'a str,
    pub iteration: i64,
    pub rule: &'a str,
    pub action: &'a str,
    pub target_id: Option<&'a str>,
    pub reasoning: &'a str,
    pub auto_acted: bool,
    /// SHA-256 hex of the observation snapshot. Pass `""` from callers
    /// that don't observe (the deconflicter's advisory rows).
    pub observation_hash: &'a str,
}

const SELECT_COLS: &str = r#"
    id::text, session_id, iteration, rule, action, target_id,
    reasoning, auto_acted, resolved, resolution,
    resolved_at::text, created_at::text,
    COALESCE(observation_hash, '')
"#;

#[expect(
    clippy::disallowed_methods,
    reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
)]
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
        observation_hash: r.get(12),
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
                    INSERT INTO project.coordinator_decisions
                        (session_id, iteration, rule, action, target_id,
                         reasoning, auto_acted, observation_hash)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
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
                    &input.observation_hash,
                ],
            )
            .await
            .map_err(|e| {
                crate::database::pg::pg_err("Failed to insert coordinator decision", &e)
            })?;

        Ok(row_to_decision(&row))
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

        let decision_uuid =
            Uuid::parse_str(decision_id).map_err(|e| format!("invalid decision_id uuid: {}", e))?;
        let n = conn
            .execute(
                r#"
                UPDATE project.coordinator_decisions
                SET resolved = TRUE,
                    resolution = $2,
                    resolved_at = NOW()
                WHERE id = $1::uuid
                  AND resolved = FALSE
                "#,
                &[&decision_uuid, &resolution],
            )
            .await
            .map_err(|e| {
                crate::database::pg::pg_err("Failed to resolve coordinator decision", &e)
            })?;

        Ok(n > 0)
    }
}
