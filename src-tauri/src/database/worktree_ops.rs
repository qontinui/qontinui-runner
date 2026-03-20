//! Worktree CRUD operations.
//!
//! Contains all CheckpointDb methods related to git worktree management.

use rusqlite::OptionalExtension;

use super::CheckpointDb;

impl CheckpointDb {
    // ========================================================================
    // Worktree CRUD operations
    // ========================================================================

    /// Insert a worktree record.
    pub fn insert_worktree(&self, record: &crate::worktree::WorktreeRecord) -> Result<(), String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        conn.execute(
            r#"INSERT INTO worktrees (id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            rusqlite::params![
                record.id,
                record.worktree_path,
                record.branch_name,
                record.source_branch,
                record.source_commit,
                record.repo_path,
                record.task_run_id,
                record.workflow_name,
                record.status.to_string(),
                record.created_at,
                record.updated_at,
            ],
        )
        .map_err(|e| format!("Failed to insert worktree: {}", e))?;
        Ok(())
    }

    /// Update the status of a worktree.
    pub fn update_worktree_status(
        &self,
        id: &str,
        status: &crate::worktree::WorktreeStatus,
    ) -> Result<(), String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE worktrees SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![status.to_string(), id],
        )
        .map_err(|e| format!("Failed to update worktree status: {}", e))?;
        Ok(())
    }

    /// List worktrees, optionally filtered by status.
    pub fn list_worktrees(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<crate::worktree::WorktreeRecord>, String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;

        let (query, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = status
        {
            (
                "SELECT id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at FROM worktrees WHERE status = ?1 ORDER BY created_at DESC",
                vec![Box::new(s.to_string())],
            )
        } else {
            (
                "SELECT id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at FROM worktrees ORDER BY created_at DESC",
                vec![],
            )
        };

        let mut stmt = conn.prepare(query).map_err(|e| e.to_string())?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(crate::worktree::WorktreeRecord {
                    id: row.get(0)?,
                    worktree_path: row.get(1)?,
                    branch_name: row.get(2)?,
                    source_branch: row.get(3)?,
                    source_commit: row.get(4)?,
                    repo_path: row.get(5)?,
                    task_run_id: row.get(6)?,
                    workflow_name: row.get(7)?,
                    status: crate::worktree::WorktreeStatus::from_str(&row.get::<_, String>(8)?),
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })
            .map_err(|e| e.to_string())?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| e.to_string())?);
        }
        Ok(results)
    }

    /// Get a single worktree by ID.
    pub fn get_worktree(
        &self,
        id: &str,
    ) -> Result<Option<crate::worktree::WorktreeRecord>, String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, worktree_path, branch_name, source_branch, source_commit, repo_path, task_run_id, workflow_name, status, created_at, updated_at FROM worktrees WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;

        let result = stmt
            .query_row(rusqlite::params![id], |row| {
                Ok(crate::worktree::WorktreeRecord {
                    id: row.get(0)?,
                    worktree_path: row.get(1)?,
                    branch_name: row.get(2)?,
                    source_branch: row.get(3)?,
                    source_commit: row.get(4)?,
                    repo_path: row.get(5)?,
                    task_run_id: row.get(6)?,
                    workflow_name: row.get(7)?,
                    status: crate::worktree::WorktreeStatus::from_str(&row.get::<_, String>(8)?),
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            })
            .optional()
            .map_err(|e| e.to_string())?;

        Ok(result)
    }

    /// Delete a worktree record.
    pub fn delete_worktree(&self, id: &str) -> Result<(), String> {
        let conn = self.pool.get().map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM worktrees WHERE id = ?1", rusqlite::params![id])
            .map_err(|e| format!("Failed to delete worktree: {}", e))?;
        Ok(())
    }
}
