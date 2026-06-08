//! PostgreSQL worktree operations.

use super::PgDb;

impl PgDb {
    /// List all worktrees, optionally filtered by status.
    pub async fn list_worktrees(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<crate::worktree::WorktreeRecord>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = if let Some(s) = status {
            conn.query(
                r#"SELECT id, worktree_path, branch_name, source_branch, source_commit,
                          repo_path, task_run_id, workflow_name, status, created_at, updated_at
                   FROM coord.worktrees WHERE status = $1 ORDER BY created_at DESC"#,
                &[&s],
            )
            .await
            .map_err(|e| format!("PG list_worktrees: {}", e))?
        } else {
            conn.query(
                r#"SELECT id, worktree_path, branch_name, source_branch, source_commit,
                          repo_path, task_run_id, workflow_name, status, created_at, updated_at
                   FROM coord.worktrees ORDER BY created_at DESC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG list_worktrees: {}", e))?
        };

        Ok(rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(9);
                let updated: chrono::DateTime<chrono::Utc> = r.get(10);
                let status_str: String = r.get(8);
                crate::worktree::WorktreeRecord {
                    id: r.get(0),
                    worktree_path: r.get(1),
                    branch_name: r.get(2),
                    source_branch: r.get(3),
                    source_commit: r.get(4),
                    repo_path: r.get(5),
                    task_run_id: r.get(6),
                    workflow_name: r.get(7),
                    status: crate::worktree::WorktreeStatus::from_str(&status_str),
                    created_at: created.to_rfc3339(),
                    updated_at: updated.to_rfc3339(),
                }
            })
            .collect())
    }

    /// Fetch the worktree rows belonging to a single `task_run_id`.
    ///
    /// Used by the chat-resume path (Phase 3 of
    /// `2026-06-06-runner-dev-loop-and-restart-resilience`) to learn which
    /// repo(s)/worktree(s) a resumed AI session was editing, so it can
    /// re-acquire the coord `kind=worktree` claim and restore the
    /// `refs/wip/<agent_session_id>` ref on disk. Returns active rows
    /// most-recent first (a session is normally promoted into at most one
    /// worktree, but we never assume cardinality).
    pub async fn get_worktrees_for_task_run(
        &self,
        task_run_id: &str,
    ) -> Result<Vec<crate::worktree::WorktreeRecord>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, worktree_path, branch_name, source_branch, source_commit,
                          repo_path, task_run_id, workflow_name, status, created_at, updated_at
                   FROM coord.worktrees
                   WHERE task_run_id = $1 AND status = 'active'
                   ORDER BY created_at DESC"#,
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_worktrees_for_task_run: {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let created: chrono::DateTime<chrono::Utc> = r.get(9);
                let updated: chrono::DateTime<chrono::Utc> = r.get(10);
                let status_str: String = r.get(8);
                crate::worktree::WorktreeRecord {
                    id: r.get(0),
                    worktree_path: r.get(1),
                    branch_name: r.get(2),
                    source_branch: r.get(3),
                    source_commit: r.get(4),
                    repo_path: r.get(5),
                    task_run_id: r.get(6),
                    workflow_name: r.get(7),
                    status: crate::worktree::WorktreeStatus::from_str(&status_str),
                    created_at: created.to_rfc3339(),
                    updated_at: updated.to_rfc3339(),
                }
            })
            .collect())
    }

    /// Insert a worktree record.
    pub async fn insert_worktree(
        &self,
        record: &crate::worktree::WorktreeRecord,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let status_str = record.status.to_string();

        conn.execute(
            r#"INSERT INTO coord.worktrees
               (id, worktree_path, branch_name, source_branch, source_commit, repo_path,
                task_run_id, workflow_name, status, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::timestamptz, $11::timestamptz)"#,
            &[
                &record.id,
                &record.worktree_path,
                &record.branch_name,
                &record.source_branch,
                &record.source_commit,
                &record.repo_path,
                &record.task_run_id,
                &record.workflow_name,
                &status_str,
                &record.created_at,
                &record.updated_at,
            ],
        )
        .await
        .map_err(|e| format!("PG insert_worktree: {}", e))?;

        Ok(())
    }
}
