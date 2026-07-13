//! Database operations for the runner-local per-session PR projection
//! (`project.session_prs`).
//!
//! Hand-written tokio-postgres (NOT Clorinde), mirroring the connection idiom
//! in [`super::pr_watch_ops`] (`let conn = self.pool.get().await …;
//! conn.query(...)`). The table is a single-consumer, runner-local projection:
//! the attribution reconciler ([`crate::session_pr_reconciler`]) writes it and
//! the Terminal zone-header dropdown command
//! ([`crate::commands::session_prs`]) reads it — no cross-component reader, so
//! there is no companion web migration (see the self-heal block in
//! [`super`]`::verify_and_provision`).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::PgDb;

/// One row of the per-session PR projection. Columns map 1:1 to
/// `project.session_prs`.
#[derive(Debug, Clone)]
pub struct SessionPrRow {
    pub claude_session_id: Uuid,
    /// `"owner/name"`.
    pub repo: String,
    pub pr_number: i64,
    pub head_branch: Option<String>,
    /// GitHub-ish lifecycle: `"open"` | `"closed"` | `"merged"`.
    pub pr_state: Option<String>,
    pub merged: bool,
    pub merged_at: Option<DateTime<Utc>>,
}

impl PgDb {
    /// Insert or refresh one attributed PR for a session. Idempotent on the
    /// `(claude_session_id, repo, pr_number)` primary key — a re-observed PR
    /// updates its branch/state/merge fields and bumps `updated_at`.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_session_pr(
        &self,
        claude_session_id: Uuid,
        repo: &str,
        pr_number: i64,
        head_branch: Option<&str>,
        pr_state: Option<&str>,
        merged: bool,
        merged_at: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "INSERT INTO project.session_prs \
                 (claude_session_id, repo, pr_number, head_branch, pr_state, merged, merged_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (claude_session_id, repo, pr_number) DO UPDATE SET \
                 head_branch = EXCLUDED.head_branch, \
                 pr_state    = EXCLUDED.pr_state, \
                 merged      = EXCLUDED.merged, \
                 merged_at   = EXCLUDED.merged_at, \
                 updated_at  = now()",
            &[
                &claude_session_id,
                &repo,
                &pr_number,
                &head_branch,
                &pr_state,
                &merged,
                &merged_at,
            ],
        )
        .await
        .map_err(|e| format!("PG upsert_session_pr: {}", e))?;

        Ok(())
    }

    /// Every PR attributed to `claude_session_id`, unmerged-first then newest
    /// PR number first — the order the Terminal dropdown renders verbatim.
    pub async fn list_session_prs(
        &self,
        claude_session_id: Uuid,
    ) -> Result<Vec<SessionPrRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                "SELECT claude_session_id, repo, pr_number, head_branch, pr_state, merged, merged_at \
                 FROM project.session_prs \
                 WHERE claude_session_id = $1 \
                 ORDER BY merged ASC, pr_number DESC",
                &[&claude_session_id],
            )
            .await
            .map_err(|e| format!("PG list_session_prs: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| SessionPrRow {
                claude_session_id: r.get(0),
                repo: r.get(1),
                pr_number: r.get(2),
                head_branch: r.get(3),
                pr_state: r.get(4),
                merged: r.get(5),
                merged_at: r.get(6),
            })
            .collect())
    }

    /// Refresh only the lifecycle columns (`pr_state`, `merged`, `merged_at`)
    /// of an already-attributed PR — the Phase-3 status-tracking write. A
    /// no-op (zero rows affected, no error) if the row is absent, so the
    /// reconciler can call it unconditionally after a GitHub status fetch.
    pub async fn update_session_pr_status(
        &self,
        claude_session_id: Uuid,
        repo: &str,
        pr_number: i64,
        pr_state: Option<&str>,
        merged: bool,
        merged_at: Option<DateTime<Utc>>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE project.session_prs SET \
                 pr_state   = $4, \
                 merged     = $5, \
                 merged_at  = $6, \
                 updated_at = now() \
             WHERE claude_session_id = $1 AND repo = $2 AND pr_number = $3",
            &[
                &claude_session_id,
                &repo,
                &pr_number,
                &pr_state,
                &merged,
                &merged_at,
            ],
        )
        .await
        .map_err(|e| format!("PG update_session_pr_status: {}", e))?;

        Ok(())
    }
}
