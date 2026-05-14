//! PostgreSQL CRUD for `coord.agent_worktrees` — the per-(agent, repo)
//! durable row that backs the worktree-per-agent spawn model.
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-branch-per-agent-coordination-plan.md`
//! §4.1 + §4.2. The schema is defined authoritatively in alembic revision
//! `coord_phase_1_01_agent_worktrees`; this module mirrors it via the
//! self-heal pattern documented in memory
//! [[proj_pg_dual_schema_runner_public]] — the runner can write rows
//! without waiting for qontinui-web's alembic upgrade to run.
//!
//! Column shapes match §4.2 exactly. Downstream phases (merge proposals,
//! merge scheduler, observability heatmap) read this table, so the names
//! and types here are a contract.

use super::PgDb;
use serde::{Deserialize, Serialize};

/// Status enum mapped to TEXT in PG with a CHECK constraint. We hold the
/// string form in Rust because it makes JSON encoding / WS event payloads
/// transparent. Allowed transitions: `Allocated` → `Active` → `Merging`
/// → `Merged` / `Abandoned`. The runner only ever writes `Allocated` and
/// `Active`; coord's merge scheduler advances to the terminal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorktreeStatus {
    Allocated,
    Active,
    Merging,
    Merged,
    Abandoned,
}

impl AgentWorktreeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentWorktreeStatus::Allocated => "allocated",
            AgentWorktreeStatus::Active => "active",
            AgentWorktreeStatus::Merging => "merging",
            AgentWorktreeStatus::Merged => "merged",
            AgentWorktreeStatus::Abandoned => "abandoned",
        }
    }
}

/// One row from `coord.agent_worktrees`. UUIDs surface as TEXT because
/// `tokio-postgres` in this crate doesn't always enable `with-uuid-1`
/// (same convention as `coordinator_shadow_decisions.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorktreeRow {
    pub agent_id: String,
    pub machine_id: Option<String>,
    pub repo: String,
    pub branch: String,
    pub parent_sha: String,
    pub worktree_path: String,
    pub status: String,
    pub intent: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const SELECT_COLS: &str = r#"
    agent_id::text, machine_id::text, repo, branch, parent_sha,
    worktree_path, status, intent, created_at::text, updated_at::text
"#;

fn row_to_aw(r: &tokio_postgres::Row) -> AgentWorktreeRow {
    AgentWorktreeRow {
        agent_id: r.get(0),
        machine_id: r.get(1),
        repo: r.get(2),
        branch: r.get(3),
        parent_sha: r.get(4),
        worktree_path: r.get(5),
        status: r.get(6),
        intent: r.get(7),
        created_at: r.get(8),
        updated_at: r.get(9),
    }
}

impl PgDb {
    /// Self-heal `coord.agent_worktrees` on PG instances where the
    /// alembic migration hasn't been applied yet. Idempotent — uses
    /// `CREATE TABLE IF NOT EXISTS` plus a DO-block that conditionally
    /// adds the CHECK constraint. Mirrors the
    /// `ensure_shadow_decisions_table` / `ensure_coord_tasks_emergent_columns`
    /// self-heal helpers already in this crate.
    ///
    /// Schema must stay byte-equivalent to the alembic migration
    /// `coord_phase_1_01_agent_worktrees`. Both shapes are the §4.2
    /// contract; downstream phases will fail in interesting ways if
    /// the two drift.
    pub async fn ensure_agent_worktrees_table(&self) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.batch_execute(
            r#"
            CREATE SCHEMA IF NOT EXISTS coord;

            CREATE TABLE IF NOT EXISTS coord.agent_worktrees (
                agent_id       UUID NOT NULL,
                machine_id     UUID REFERENCES coord.machines(machine_id)
                                   ON DELETE SET NULL,
                repo           TEXT NOT NULL,
                branch         TEXT NOT NULL,
                parent_sha     TEXT NOT NULL,
                worktree_path  TEXT NOT NULL,
                status         TEXT NOT NULL DEFAULT 'allocated',
                intent         TEXT,
                created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
                updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
                PRIMARY KEY (agent_id, repo),
                CONSTRAINT agent_worktrees_branch_uq UNIQUE (branch)
            );

            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1 FROM pg_constraint
                    WHERE conname = 'agent_worktrees_status_chk'
                ) THEN
                    ALTER TABLE coord.agent_worktrees
                        ADD CONSTRAINT agent_worktrees_status_chk
                        CHECK (status IN ('allocated','active','merging','merged','abandoned'));
                END IF;
            END$$;

            CREATE INDEX IF NOT EXISTS idx_agent_worktrees_status
                ON coord.agent_worktrees (status, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_agent_worktrees_machine_status
                ON coord.agent_worktrees (machine_id, status)
                WHERE machine_id IS NOT NULL;
            "#,
        )
        .await
        .map_err(|e| format!("Failed to ensure coord.agent_worktrees: {}", e))
    }

    /// Fetch all worktree rows for a given agent. Ordered by repo for
    /// deterministic iteration when the spawn path materializes the
    /// worktrees one-by-one.
    pub async fn list_agent_worktrees(
        &self,
        agent_id: &str,
    ) -> Result<Vec<AgentWorktreeRow>, String> {
        let agent_uuid = uuid::Uuid::parse_str(agent_id)
            .map_err(|e| format!("agent_id is not a valid UUID: {}", e))?;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                &format!(
                    "SELECT {} FROM coord.agent_worktrees \
                     WHERE agent_id = $1 ORDER BY repo",
                    SELECT_COLS
                ),
                &[&agent_uuid],
            )
            .await
            .map_err(|e| format!("Failed to list agent_worktrees: {}", e))?;
        Ok(rows.iter().map(row_to_aw).collect())
    }

    /// Transition a single worktree row's status. Used by the runner
    /// when it materializes the worktree (`allocated` → `active`) and
    /// when the merge scheduler later advances it. Returns the post-
    /// update row, or an error if no row matched.
    pub async fn update_agent_worktree_status(
        &self,
        agent_id: &str,
        repo: &str,
        new_status: AgentWorktreeStatus,
    ) -> Result<AgentWorktreeRow, String> {
        let agent_uuid = uuid::Uuid::parse_str(agent_id)
            .map_err(|e| format!("agent_id is not a valid UUID: {}", e))?;
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let row = conn
            .query_one(
                &format!(
                    "UPDATE coord.agent_worktrees \
                     SET status = $3, updated_at = now() \
                     WHERE agent_id = $1 AND repo = $2 \
                     RETURNING {}",
                    SELECT_COLS
                ),
                &[&agent_uuid, &repo, &new_status.as_str()],
            )
            .await
            .map_err(|e| format!("Failed to update agent_worktree status: {}", e))?;
        Ok(row_to_aw(&row))
    }
}
