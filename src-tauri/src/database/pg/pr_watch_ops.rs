//! Database operations for PR watch state tracking.
//!
//! The runner neither reads nor writes `pr_watch_state.github_token`, a
//! plaintext slot that would have stored a user's GitHub token (coord finding
//! e871ac27). The watcher polls with the client its trigger built. The column
//! itself is still in the schema: plan
//! `2026-10-18-drop-the-plaintext-pr-watch-token-column-once-no-deployed-runner-reads-it`
//! drops it in qontinui-web once no deployed runner reads it.

use super::PgDb;
use crate::trigger_system::watchers::pr_watcher::PrWatchState;

impl PgDb {
    /// Get all active (non-completed) PR watches.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_active_pr_watches(&self) -> Result<Vec<PrWatchState>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"SELECT id, task_run_id, pr_number, repo_full_name, head_sha,
                       workflow_id, last_checks_status, last_review_status,
                       auto_resume_count, max_auto_resumes, auto_resume_enabled
                FROM pr_watch_state
                WHERE completed_at IS NULL
                ORDER BY created_at ASC"#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_active_pr_watches", &e))?;

        Ok(rows
            .iter()
            .map(|r| PrWatchState {
                id: r.get(0),
                task_run_id: r.get(1),
                pr_number: r.get::<_, i64>(2) as u64,
                repo_full_name: r.get(3),
                head_sha: r.get(4),
                workflow_id: r.get(5),
                last_checks_status: r.get(6),
                last_review_status: r.get(7),
                auto_resume_count: r.get::<_, i32>(8) as u32,
                max_auto_resumes: r.get::<_, i32>(9) as u32,
                auto_resume_enabled: r.get(10),
            })
            .collect())
    }

    /// Update check/review status and head SHA after polling.
    pub async fn update_pr_watch_state(
        &self,
        id: &str,
        checks_status: &str,
        review_status: &str,
        head_sha: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            r#"UPDATE pr_watch_state SET
                last_checks_status = $1,
                last_review_status = $2,
                head_sha = $3,
                last_polled_at = NOW(),
                updated_at = NOW()
            WHERE id = $4"#,
            &[&checks_status, &review_status, &head_sha, &id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG update_pr_watch_state", &e))?;

        Ok(())
    }

    /// Increment the auto-resume counter.
    pub async fn increment_pr_watch_auto_resume(&self, id: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE pr_watch_state SET auto_resume_count = auto_resume_count + 1, updated_at = NOW() WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG increment_pr_watch_auto_resume", &e))?;

        Ok(())
    }

    /// Mark a PR watch as complete with a reason.
    pub async fn mark_pr_watch_complete(&self, id: &str, reason: &str) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        conn.execute(
            "UPDATE pr_watch_state SET completed_at = NOW(), completion_reason = $1, updated_at = NOW() WHERE id = $2",
            &[&reason, &id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG mark_pr_watch_complete", &e))?;

        Ok(())
    }

    /// Store CI failure context JSON on a task run for the executor to pick up.
    ///
    /// Atomically merges the CI context into the existing result_data JSON using
    /// a single UPDATE statement. If result_data already exists, the ci_failure_context
    /// key is added/overwritten. Otherwise a new JSON object is created.
    pub async fn set_task_run_ci_failure_context(
        &self,
        task_run_id: &str,
        context_json: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Build the merged JSON object with ci_failure_context key
        let wrapper = format!(r#"{{"ci_failure_context":{}}}"#, context_json);

        // Atomic merge: parse existing result_data as jsonb, merge with new context, cast back to text
        conn.execute(
            r#"UPDATE task_runs
               SET result_data = CASE
                   WHEN result_data IS NULL OR result_data = '' THEN $1
                   ELSE (result_data::jsonb || $1::jsonb)::text
               END,
               updated_at = NOW()
               WHERE id = $2"#,
            &[&wrapper, &task_run_id],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG set_task_run_ci_failure_context", &e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// This file must not handle a token again. The PR-watch token column
    /// would have stored a user's GitHub token in plaintext (coord finding
    /// e871ac27). A watch that ever needs its own credential must keep the
    /// secret out of this table.
    #[test]
    fn pr_watch_code_handles_no_token() {
        let source = include_str!("pr_watch_ops.rs");
        let (production, _) = source
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("pr_watch_ops.rs keeps its tests in a trailing `mod tests`");
        let offending: Vec<&str> = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.to_ascii_lowercase().contains("token"))
            .collect();
        assert!(
            offending.is_empty(),
            "pr_watch_ops.rs code mentions a token again. A credential must stay out of \
             pr_watch_state (coord finding e871ac27); if this is not one, narrow this test: \
             {offending:?}"
        );
    }
}
