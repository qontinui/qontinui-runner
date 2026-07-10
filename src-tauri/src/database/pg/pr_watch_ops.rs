//! Database operations for PR watch state tracking.

use chrono::{DateTime, Utc};

use super::PgDb;
use crate::trigger_system::watchers::pr_watcher::PrWatchState;

/// The active-watch poll SELECT. Reads the two runner-self-healed columns
/// (`authoring_session_id`, `first_fully_green_at`); see
/// [`PgDb::get_active_pr_watches`] for the shape-recovery path when the
/// alembic-canonical table was created WITHOUT them after runner boot.
const SELECT_ACTIVE_PR_WATCHES_SQL: &str = r#"SELECT id, task_run_id, pr_number, repo_full_name, head_sha,
                       workflow_id, last_checks_status, last_review_status,
                       auto_resume_count, max_auto_resumes, github_token,
                       auto_resume_enabled, authoring_session_id,
                       first_fully_green_at
                FROM pr_watch_state
                WHERE completed_at IS NULL
                ORDER BY created_at ASC"#;

/// Task-run-keyed seed upsert. The `first_fully_green_at` CASE mirrors the
/// poll path's per-head streak-reset invariant
/// (`pr_watcher::compute_first_fully_green_at`): a seed that ADVANCES the
/// stored head (e.g. a failed re-run of `gh pr create` resolving the open PR
/// after a push) must reset the green streak — otherwise the next poll sees
/// the head "unchanged", carries the OLD head's streak, and fires
/// GreenButUnmerged immediately for a head whose CI only just went green.
const UPSERT_PR_WATCH_TASK_RUN_SQL: &str = r#"INSERT INTO pr_watch_state (id, task_run_id, authoring_session_id, pr_number, repo_full_name, head_sha, workflow_id, max_auto_resumes, github_token, auto_resume_enabled)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                   ON CONFLICT (task_run_id, pr_number) DO UPDATE SET
                       authoring_session_id = COALESCE(EXCLUDED.authoring_session_id, pr_watch_state.authoring_session_id),
                       first_fully_green_at = CASE
                           WHEN pr_watch_state.head_sha IS DISTINCT FROM EXCLUDED.head_sha THEN NULL
                           ELSE pr_watch_state.first_fully_green_at
                       END,
                       head_sha = EXCLUDED.head_sha,
                       workflow_id = EXCLUDED.workflow_id,
                       max_auto_resumes = EXCLUDED.max_auto_resumes,
                       github_token = EXCLUDED.github_token,
                       auto_resume_enabled = EXCLUDED.auto_resume_enabled,
                       updated_at = NOW()
                   RETURNING id"#;

/// Session-keyed seed upsert — same streak-reset invariant as
/// [`UPSERT_PR_WATCH_TASK_RUN_SQL`].
const UPSERT_PR_WATCH_SESSION_SQL: &str = r#"INSERT INTO pr_watch_state (id, task_run_id, authoring_session_id, pr_number, repo_full_name, head_sha, workflow_id, max_auto_resumes, github_token, auto_resume_enabled)
                   VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8, $9)
                   ON CONFLICT (authoring_session_id, pr_number) WHERE task_run_id IS NULL DO UPDATE SET
                       first_fully_green_at = CASE
                           WHEN pr_watch_state.head_sha IS DISTINCT FROM EXCLUDED.head_sha THEN NULL
                           ELSE pr_watch_state.first_fully_green_at
                       END,
                       head_sha = EXCLUDED.head_sha,
                       workflow_id = EXCLUDED.workflow_id,
                       max_auto_resumes = EXCLUDED.max_auto_resumes,
                       github_token = EXCLUDED.github_token,
                       auto_resume_enabled = EXCLUDED.auto_resume_enabled,
                       updated_at = NOW()
                   RETURNING id"#;

impl PgDb {
    /// Get all active (non-completed) PR watches.
    ///
    /// Shape tolerance: if the SELECT fails with SQLSTATE 42703 (undefined
    /// column), the table exists in its alembic-canonical shape WITHOUT the
    /// runner-self-healed columns — the boot-time heal no-oped because the
    /// table did not exist yet, and alembic created it afterwards. Re-run the
    /// same idempotent self-heal block and retry once, so watcher polls (and
    /// the pre-existing CI auto-resume / review-subtask features that ride on
    /// them) recover without a runner restart.
    pub async fn get_active_pr_watches(&self) -> Result<Vec<PrWatchState>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = match conn.query(SELECT_ACTIVE_PR_WATCHES_SQL, &[]).await {
            Ok(rows) => rows,
            Err(e)
                if e.code() == Some(&tokio_postgres::error::SqlState::UNDEFINED_COLUMN) =>
            {
                tracing::warn!(
                    "PG get_active_pr_watches: pr_watch_state is missing the shepherd columns \
                     ({e}) — table created after runner boot; re-running the idempotent shape \
                     self-heal and retrying"
                );
                conn.batch_execute(super::PR_WATCH_STATE_SELF_HEAL_SQL)
                    .await
                    .map_err(|e| {
                        format!("PG get_active_pr_watches: shape self-heal failed: {}", e)
                    })?;
                conn.query(SELECT_ACTIVE_PR_WATCHES_SQL, &[])
                    .await
                    .map_err(|e| format!("PG get_active_pr_watches (after self-heal): {}", e))?
            }
            Err(e) => return Err(format!("PG get_active_pr_watches: {}", e)),
        };

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
                authoring_session_id: r.get(12),
                first_fully_green_at: r.get(13),
            })
            .collect())
    }

    /// Insert or update a PR watch entry.
    ///
    /// Two identity keys (PR-shepherd Phase 1): task-run-authored watches key
    /// on the existing `(task_run_id, pr_number)` unique constraint;
    /// interactive-session watches carry a NULL `task_run_id` and key on the
    /// partial unique index `(authoring_session_id, pr_number) WHERE
    /// task_run_id IS NULL` instead (a NULL `task_run_id` can never conflict
    /// on the constraint). At least one identity is required.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_pr_watch_state(
        &self,
        task_run_id: Option<&str>,
        authoring_session_id: Option<&str>,
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

        let row = match task_run_id {
            Some(task_run_id) => conn
                .query_one(
                    UPSERT_PR_WATCH_TASK_RUN_SQL,
                    &[&id, &task_run_id, &authoring_session_id, &pr_num, &repo_full_name, &head_sha, &workflow_id, &max_resumes, &github_token, &auto_resume_enabled],
                )
                .await
                .map_err(|e| format!("PG upsert_pr_watch_state: {}", e))?,
            None => {
                let Some(authoring_session_id) = authoring_session_id else {
                    return Err(
                        "upsert_pr_watch_state requires task_run_id or authoring_session_id"
                            .to_string(),
                    );
                };
                conn.query_one(
                    UPSERT_PR_WATCH_SESSION_SQL,
                    &[&id, &authoring_session_id, &pr_num, &repo_full_name, &head_sha, &workflow_id, &max_resumes, &github_token, &auto_resume_enabled],
                )
                .await
                .map_err(|e| format!("PG upsert_pr_watch_state (session-keyed): {}", e))?
            }
        };

        Ok(row.get(0))
    }

    /// Update check/review status, head SHA, and the green-streak clock after
    /// polling. `first_fully_green_at` is written unconditionally — the
    /// caller computes set/carry/reset semantics
    /// (`pr_watcher::compute_first_fully_green_at`), so `None` here clears a
    /// broken streak.
    pub async fn update_pr_watch_state(
        &self,
        id: &str,
        checks_status: &str,
        review_status: &str,
        head_sha: &str,
        first_fully_green_at: Option<DateTime<Utc>>,
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
                first_fully_green_at = $4,
                last_polled_at = NOW(),
                updated_at = NOW()
            WHERE id = $5"#,
            &[
                &checks_status,
                &review_status,
                &head_sha,
                &first_fully_green_at,
                &id,
            ],
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
    /// Returns the most recently created watch's token (active or completed — the
    /// token remains valid even after the watch finishes).
    pub async fn get_pr_watch_token_for_task(
        &self,
        task_run_id: &str,
    ) -> Result<Option<String>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                "SELECT github_token FROM pr_watch_state WHERE task_run_id = $1 ORDER BY created_at DESC LIMIT 1",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-head streak-reset invariant: BOTH seed-upsert branches must
    /// clear `first_fully_green_at` when the stored head differs from the
    /// seeded head — matching the poll path
    /// (`pr_watcher::compute_first_fully_green_at`). A seed that silently
    /// advances `head_sha` without this reset makes the next poll carry the
    /// OLD head's green streak onto the new head and fire GreenButUnmerged
    /// for CI that only just went green.
    #[test]
    fn seed_upserts_reset_green_streak_on_head_change() {
        for (name, sql) in [
            ("task-run", UPSERT_PR_WATCH_TASK_RUN_SQL),
            ("session", UPSERT_PR_WATCH_SESSION_SQL),
        ] {
            assert!(
                sql.contains("head_sha = EXCLUDED.head_sha"),
                "{name}: upsert must advance head_sha"
            );
            assert!(
                sql.contains(
                    "WHEN pr_watch_state.head_sha IS DISTINCT FROM EXCLUDED.head_sha THEN NULL"
                ),
                "{name}: upsert must NULL first_fully_green_at on a head change"
            );
            assert!(
                sql.contains("ELSE pr_watch_state.first_fully_green_at"),
                "{name}: upsert must carry the streak when the head is unchanged"
            );
        }
    }

    /// The active-watch SELECT reads the runner-self-healed columns; the
    /// 42703 recovery path in `get_active_pr_watches` re-runs this exact heal
    /// block, which must therefore add every such column the SELECT names.
    #[test]
    fn select_columns_are_covered_by_the_self_heal() {
        for col in ["authoring_session_id", "first_fully_green_at"] {
            assert!(
                SELECT_ACTIVE_PR_WATCHES_SQL.contains(col),
                "SELECT must read {col}"
            );
            assert!(
                crate::database::pg::PR_WATCH_STATE_SELF_HEAL_SQL
                    .contains(&format!("ADD COLUMN IF NOT EXISTS {col}")),
                "self-heal must add {col} (the 42703 recovery re-runs it)"
            );
        }
    }
}
