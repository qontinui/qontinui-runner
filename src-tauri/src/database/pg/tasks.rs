//! PostgreSQL CRUD operations for the `project.tasks` table.
//!
//! Since Phase 4 of `2026-09-12-consolidate-local-orchestration-onto-conductor`
//! deleted the Productivity scheduler and the plan/task board, the runner's
//! only writer here is [`PgDb::create_emergent_task`] — the row an AI session
//! creates for itself so its touched-file and review bookkeeping has a task
//! to hang off. `plan_id` is a bare column: alembic revision
//! `coord_p4_03_drop_plans` DROPPED `coord.tasks.plan_id` (and `coord.plans`
//! outright) when coord moved onto `coord.work_units`, and `project.plans` is
//! no longer created either, so the column carries no FK. coord keeps its own
//! `coord.tasks` for the work-unit-linked rows its merge train writes; the
//! two populations are disjoint (the runner never sets `work_unit_id`, and
//! coord's merge->done UPDATE matches only rows that have one).
//!
//! UUID columns are cast to/from TEXT in queries because the runner does
//! not enable tokio-postgres `with-uuid-1`. UUIDs flow as canonical strings.
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

impl PgDb {
    /// Insert a `session_emergent` row in `project.tasks` for an AI session
    /// that didn't come from a plan decomposition. Idempotent via the
    /// partial unique index `idx_tasks_emergent_per_session` — created by this
    /// repo's own `MACHINE_LOCAL_TABLES_DDL` self-heal (`database/pg/mod.rs`),
    /// not by alembic, exactly as this module's header says;
    /// a second call for the same `assigned_session_id` returns `Ok(None)`.
    ///
    /// `assigned_session_id` is typically the worker's `task_run_id`
    /// (workers) or the Claude session id (launch-menu AI sessions).
    /// `description` is intentionally `Option<&str>` — Phase 1 leaves it
    /// `None`; Phase 2 will enrich.
    ///
    /// Returns `Ok(Some(id))` on a fresh insert, `Ok(None)` on a conflict
    /// (existing emergent row), or `Err(_)` on a PG error. Callers are
    /// expected to treat this as best-effort: never fail an AI-session
    /// spawn just because PG was unreachable.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn create_emergent_task(
        &self,
        assigned_session_id: &str,
        status: &str,
        origin: &str,
        description: Option<&str>,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                INSERT INTO project.tasks (
                    assigned_session_id, status, origin, plan_id,
                    plan_version_hash, phase_name, sequence_in_phase, description
                )
                VALUES ($1, $2, $3, NULL, NULL, NULL, NULL, $4)
                ON CONFLICT (assigned_session_id) WHERE origin = 'session_emergent' DO NOTHING
                RETURNING id::text
                "#,
                &[&assigned_session_id, &status, &origin, &description],
            )
            .await
            // Was a hand-rolled `as_db_error()` match — the same job the shared
            // `pg_err` now does for every query in this module (and it also
            // surfaces table/detail/hint, which the local copy dropped).
            .map_err(|e| crate::database::pg::pg_err("Failed to create emergent task", &e))?;

        Ok(row.map(|r| r.get(0)))
    }
}
