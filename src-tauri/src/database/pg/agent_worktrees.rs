//! PostgreSQL CRUD for `coord.agent_worktrees` — the per-(agent, repo)
//! durable row that backs the worktree-per-agent spawn model.
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-branch-per-agent-coordination-plan.md`
//! §4.1 + §4.2. The schema is defined authoritatively in alembic revision
//! `coord_phase_1_01_agent_worktrees` (qontinui-web), which is the sole
//! author of the `coord.*` schema; the runner only reads/writes rows.
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
    /// Per Phase 1B (§4.10): file/glob paths the agent declared at
    /// allocation time. Populated by agent input, falling back to LLM
    /// derivation from `intent`; `None` for pre-1B rows.
    pub declared_overlap_paths: Option<Vec<String>>,
    pub created_at: String,
    pub updated_at: String,
}

const SELECT_COLS: &str = r#"
    agent_id::text, machine_id::text, repo, branch, parent_sha,
    worktree_path, status, intent, declared_overlap_paths,
    created_at::text, updated_at::text
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
        declared_overlap_paths: r.get(8),
        created_at: r.get(9),
        updated_at: r.get(10),
    }
}

impl PgDb {
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

    /// List currently-active agents that declared at least one overlap
    /// path. Used by the runner's Productivity dashboard to render the
    /// L2 "Overlapping intents" panel (Phase 1B §4.10).
    ///
    /// "Active" = status in ('allocated', 'active') AND
    /// declared_overlap_paths is non-empty. One row per agent (DISTINCT
    /// ON agent_id) — multi-repo agents share intent + path set across
    /// their N worktree rows, so picking any one row gives the same
    /// answer.
    ///
    /// Ordered newest-first so the dashboard surfaces fresh intents
    /// before stale ones; the panel is capped at `limit` rows.
    pub async fn list_active_agents_with_paths(
        &self,
        limit: i64,
    ) -> Result<Vec<AgentWorktreeRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let rows = conn
            .query(
                &format!(
                    "SELECT DISTINCT ON (agent_id) {SELECT_COLS} \
                     FROM coord.agent_worktrees \
                     WHERE status IN ('allocated', 'active') \
                       AND declared_overlap_paths IS NOT NULL \
                       AND array_length(declared_overlap_paths, 1) > 0 \
                     ORDER BY agent_id, updated_at DESC \
                     LIMIT $1",
                ),
                &[&limit],
            )
            .await
            .map_err(|e| format!("Failed to list_active_agents_with_paths: {}", e))?;
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
