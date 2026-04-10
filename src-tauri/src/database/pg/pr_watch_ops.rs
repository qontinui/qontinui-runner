//! Database operations for PR watch state tracking.

use super::PgDb;
use crate::trigger_system::watchers::pr_watcher::PrWatchState;

impl PgDb {
    /// Get all active (non-completed) PR watches.
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
                       auto_resume_count, max_auto_resumes, github_token,
                       auto_resume_enabled
                FROM pr_watch_state
                WHERE completed_at IS NULL
                ORDER BY created_at ASC"#,
                &[],
            )
            .await
            .map_err(|e| format!("PG get_active_pr_watches: {}", e))?;

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
                github_token: r.get(10),
                auto_resume_enabled: r.get(11),
            })
            .collect())
    }

    /// Insert or update a PR watch entry.
    pub async fn upsert_pr_watch_state(
        &self,
        task_run_id: &str,
        pr_number: u64,
        repo_full_name: &str,
        head_sha: &str,
        workflow_id: &str,
        max_auto_resumes: u32,
        github_token: &str,
        auto_resume_enabled: bool,
    ) -> Result<String, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let pr_num = pr_number as i64;
        let max_resumes = max_auto_resumes as i32;

        let row = conn
            .query_one(
                r#"INSERT INTO pr_watch_state (id, task_run_id, pr_number, repo_full_name, head_sha, workflow_id, max_auto_resumes, github_token, auto_resume_enabled)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (task_run_id, pr_number) DO UPDATE SET
                   head_sha = EXCLUDED.head_sha,
                   workflow_id = EXCLUDED.workflow_id,
                   max_auto_resumes = EXCLUDED.max_auto_resumes,
                   github_token = EXCLUDED.github_token,
                   auto_resume_enabled = EXCLUDED.auto_resume_enabled,
                   updated_at = NOW()
               RETURNING id"#,
                &[&id, &task_run_id, &pr_num, &repo_full_name, &head_sha, &workflow_id, &max_resumes, &github_token, &auto_resume_enabled],
            )
            .await
            .map_err(|e| format!("PG upsert_pr_watch_state: {}", e))?;

        Ok(row.get(0))
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
        .map_err(|e| format!("PG update_pr_watch_state: {}", e))?;

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
        .map_err(|e| format!("PG increment_pr_watch_auto_resume: {}", e))?;

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
        .map_err(|e| format!("PG mark_pr_watch_complete: {}", e))?;

        Ok(())
    }

    /// Get the GitHub token from the PR watch associated with a task run.
    pub async fn get_pr_watch_token_for_task(&self, task_run_id: &str) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT github_token FROM pr_watch_state WHERE task_run_id = $1 AND completed_at IS NULL LIMIT 1",
                &[&task_run_id],
            )
            .await
            .map_err(|e| format!("PG get_pr_watch_token_for_task: {}", e))?;

        Ok(row.map(|r| r.get(0)))
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
        .map_err(|e| format!("PG set_task_run_ci_failure_context: {}", e))?;

        Ok(())
    }
}
