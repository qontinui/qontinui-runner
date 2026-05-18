//! Self-heal helper for `coord.agent_sessions` — the Claude Code
//! session lookup table introduced by Plan
//! `D:/qontinui-root/plans/coord-agent-session-id-tracking.md` Phase 2
//! (Side B).
//!
//! Plan reference: see Side B "lookup table" section. The runner does
//! NOT write this table directly today — `POST /coord/...` mutating
//! endpoints in qontinui-coord own the writes via the
//! `agent_sessions::upsert_agent_session` helper. The ensure_* helper
//! is provided so a runner-driven local-coord harness (or a fresh dev
//! database whose alembic chain hasn't reached the agent_sessions
//! revision yet) can still allocate the table before coord first
//! writes to it.
//!
//! Schema must stay byte-equivalent to the alembic migration. Plan
//! Phase 6 will tighten `id` columns to NOT NULL on the audit tables
//! after Side C2 (Claude Code env-var surface) lands; this lookup
//! table itself has `id PRIMARY KEY` from day one.

use super::PgDb;

impl PgDb {
    /// Self-heal `coord.agent_sessions`. Idempotent — `CREATE TABLE
    /// IF NOT EXISTS` plus `CREATE INDEX IF NOT EXISTS`. Logs but does
    /// not own auth.users / coord.machines provisioning (those land via
    /// other self-heal helpers / alembic).
    ///
    /// The FK to `coord.machines(machine_id)` is omitted from the
    /// self-heal — different alembic chains seed `coord.machines` at
    /// different revisions, and an out-of-order self-heal would fail
    /// trying to reference a not-yet-present table. The alembic
    /// migration in qontinui-web/backend/alembic/versions/ adds the
    /// FK constraint when both tables are guaranteed to exist.
    pub async fn ensure_agent_sessions_table(&self) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.batch_execute(
            r#"
            CREATE SCHEMA IF NOT EXISTS coord;

            CREATE TABLE IF NOT EXISTS coord.agent_sessions (
                id           UUID PRIMARY KEY,
                user_id      UUID,
                machine_id   UUID,
                first_seen   TIMESTAMPTZ NOT NULL DEFAULT now(),
                last_seen    TIMESTAMPTZ NOT NULL DEFAULT now(),
                label        TEXT,
                closed_at    TIMESTAMPTZ
            );

            CREATE INDEX IF NOT EXISTS idx_agent_sessions_user
                ON coord.agent_sessions (user_id)
                WHERE user_id IS NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_agent_sessions_machine
                ON coord.agent_sessions (machine_id)
                WHERE machine_id IS NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_agent_sessions_last_seen
                ON coord.agent_sessions (last_seen DESC);
            "#,
        )
        .await
        .map_err(|e| format!("Failed to ensure coord.agent_sessions: {}", e))
    }
}
