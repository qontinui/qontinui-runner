//! PostgreSQL workflow AI session operations.

use super::PgDb;
use crate::database::types::AiSessionSummary;
use tracing::info;

/// One attempt on a `workflow_ai_sessions` row: the row and the
/// `session_started_at` (epoch microseconds) its create stamped. A resume
/// re-uses the row and the CLI id but gets a new `started_at_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowAiSessionAttempt {
    pub row_id: i64,
    pub started_at_us: i64,
}

impl PgDb {
    /// Get recent AI sessions (chat-type task runs) for the sidebar.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_ai_sessions(
        &self,
        limit: u32,
        runner_port: Option<u16>,
    ) -> Result<Vec<AiSessionSummary>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let limit_i64 = limit as i64;
        let port_i64: Option<i64> = runner_port.map(|p| p as i64);

        let rows = conn
            .query(
                r#"
                SELECT id, task_name, status,
                       COALESCE(updated_at::TEXT, created_at::TEXT) as updated_at,
                       created_at::TEXT
                FROM task_runs
                WHERE workflow_type = 'chat'
                  AND ($1::BIGINT IS NULL OR runner_port IS NULL OR runner_port = $1)
                ORDER BY COALESCE(updated_at, created_at) DESC
                LIMIT $2
                "#,
                &[
                    &port_i64 as &(dyn tokio_postgres::types::ToSql + Sync),
                    &limit_i64,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_ai_sessions", &e))?;

        Ok(rows
            .iter()
            .map(|r| AiSessionSummary {
                id: r.get(0),
                task_name: r.get(1),
                status: r.get(2),
                updated_at: r.get(3),
                created_at: r.get(4),
            })
            .collect())
    }

    /// Get running AI sessions (chat-type task runs that are still running).
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_running_ai_sessions(
        &self,
        runner_port: Option<u16>,
    ) -> Result<Vec<crate::database::types::TaskRun>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port_i64: Option<i64> = runner_port.map(|p| p as i64);

        let rows = conn
            .query(
                r#"
                SELECT id, task_name, prompt, status, sessions_count, max_sessions,
                       error_message, auto_continue, execution_steps_json, log_sources_json,
                       COALESCE(summary, ai_summary) as summary, ai_summary,
                       goal_achieved, remaining_work, summary_generated_at::TEXT,
                       workspace_id, triggered_by,
                       created_at::TEXT, updated_at::TEXT, completed_at::TEXT,
                       task_type, config_id, workflow_name
                FROM task_runs
                WHERE status = 'running' AND workflow_type = 'chat'
                  AND ($1::BIGINT IS NULL OR runner_port IS NULL OR runner_port = $1)
                ORDER BY COALESCE(updated_at, created_at) DESC
                "#,
                &[&port_i64 as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_running_ai_sessions", &e))?;

        Ok(rows
            .iter()
            .map(|r| crate::database::types::TaskRun {
                id: r.get(0),
                task_name: r.get(1),
                prompt: r.get(2),
                status: r.get(3),
                sessions_count: r.get::<_, i32>(4) as u32,
                max_sessions: r.get::<_, Option<i32>>(5).map(|v| v as u32),
                output_log: String::new(),
                error_message: r.get(6),
                auto_continue: r.get::<_, bool>(7),
                execution_steps_json: r.get(8),
                log_sources_json: r.get(9),
                summary: r.get(10),
                ai_summary: r.get(11),
                goal_achieved: r.get(12),
                remaining_work: r.get(13),
                summary_generated_at: r.get(14),
                workspace_id: r.get(15),
                triggered_by: r.get(16),
                created_at: r.get::<_, Option<String>>(17).unwrap_or_default(),
                updated_at: r.get::<_, Option<String>>(18).unwrap_or_default(),
                completed_at: r.get(19),
                task_type: r
                    .get::<_, Option<String>>(20)
                    .unwrap_or_else(|| "task".to_string()),
                config_id: r.get(21),
                workflow_name: r.get(22),
                workflow_id: None,
                transition_history_json: None,
                workflow_type: Some("chat".to_string()),
                parent_task_run_id: None,
                root_task_run_id: None,
                depth: 0,
                bridge_id: None,
                result_data: None,
                is_reflection: false,
                reflection_source_task_run_id: None,
                is_follow_up: false,
                follow_up_source_task_run_id: None,
                is_fixer: false,
                fixer_source_task_run_id: None,
                is_meta_optimizer: false,
                is_review: false,
                blocks_parent: false,
            })
            .collect())
    }

    /// Stricter variant of `get_running_ai_sessions` used by the startup
    /// chat-resume path: requires `runner_port` to match exactly (no NULL
    /// fallthrough) so multiple runners cannot double-claim the same
    /// orphaned chat session.
    ///
    /// Returns lightweight `(id, task_name, auto_continue)` tuples — the
    /// caller fetches the full row (including `output_log`) via
    /// `get_task_run` only for sessions it actually intends to resume.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_resumable_chat_sessions_for_runner(
        &self,
        runner_port: u16,
    ) -> Result<Vec<(String, String, bool)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let port = runner_port as i32;

        let rows = conn
            .query(
                r#"SELECT id, task_name, COALESCE(auto_continue, true) as auto_continue
                   FROM task_runs
                   WHERE status = 'running'
                     AND workflow_type = 'chat'
                     AND runner_port = $1
                   ORDER BY COALESCE(updated_at, created_at) DESC"#,
                &[&port],
            )
            .await
            .map_err(|e| {
                crate::database::pg::pg_err("PG get_resumable_chat_sessions_for_runner", &e)
            })?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let id: String = r.get("id");
                let task_name: String = r.get("task_name");
                let auto_continue: bool = r.get("auto_continue");
                (id, task_name, auto_continue)
            })
            .collect())
    }

    /// Create (or upsert) a workflow AI session record -- one ATTEMPT on its
    /// key. Every call (a first run, a resume under the same CLI id, the
    /// resume-failed fallback) stamps a fresh `session_started_at`, so the
    /// returned [`WorkflowAiSessionAttempt`] names this attempt and no other.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn create_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        claude_cli_session_id: &str,
    ) -> Result<WorkflowAiSessionAttempt, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Use ON CONFLICT on the unique index (task_run_id, iteration, phase, COALESCE(stage_index, -1)).
        // PG doesn't allow COALESCE in ON CONFLICT target directly, so we do a manual check-then-upsert.
        let coalesced_stage = stage_index.unwrap_or(-1);

        let existing = conn
            .query_opt(
                r#"SELECT id FROM workflow_ai_sessions
                   WHERE task_run_id = $1 AND iteration = $2 AND phase = $3
                     AND COALESCE(stage_index, -1) = $4"#,
                &[&task_run_id, &iteration, &phase, &coalesced_stage],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG create_workflow_ai_session query", &e))?;

        let attempt = if let Some(row) = existing {
            let id: i64 = row.get(0);
            let row = conn
                .query_one(
                    r#"UPDATE workflow_ai_sessions
                       SET claude_cli_session_id = $1, session_started_at = clock_timestamp(),
                           session_completed_at = NULL, output_length = 0, status = 'running'
                       WHERE id = $2
                       RETURNING (EXTRACT(EPOCH FROM session_started_at) * 1000000)::bigint"#,
                    &[&claude_cli_session_id, &id],
                )
                .await
                .map_err(|e| {
                    crate::database::pg::pg_err("PG create_workflow_ai_session update", &e)
                })?;
            WorkflowAiSessionAttempt {
                row_id: id,
                started_at_us: row.get(0),
            }
        } else {
            let row = conn
                .query_one(
                    r#"INSERT INTO workflow_ai_sessions
                       (task_run_id, iteration, phase, stage_index, claude_cli_session_id, session_started_at, status)
                       VALUES ($1, $2, $3, $4, $5, clock_timestamp(), 'running')
                       RETURNING id, (EXTRACT(EPOCH FROM session_started_at) * 1000000)::bigint"#,
                    &[&task_run_id, &iteration, &phase, &stage_index, &claude_cli_session_id],
                )
                .await
                .map_err(|e| crate::database::pg::pg_err("PG create_workflow_ai_session insert", &e))?;
            WorkflowAiSessionAttempt {
                row_id: row.get(0),
                started_at_us: row.get(1),
            }
        };

        info!(
            "Created workflow AI session: task={}, iter={}, phase={}, cli_session={}",
            task_run_id, iteration, phase, claude_cli_session_id
        );
        Ok(attempt)
    }

    /// Mark a workflow AI session with its terminal status.
    ///
    /// The only caller (the agentic phase) writes `completed` or `failed` —
    /// see `workflow_ai_session_terminal_status` in
    /// `unified_workflow_executor/phases/agentic.rs`. Nothing writes
    /// `interrupted` today: a row left `running` at the start of a later
    /// attempt is how an interrupted session is recognised.
    pub async fn complete_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
        status: &str,
        output_length: i64,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let coalesced_stage = stage_index.unwrap_or(-1);
        let output_len_i32 = output_length as i32;

        conn.execute(
            r#"UPDATE workflow_ai_sessions
               SET status = $1, session_completed_at = NOW(), output_length = $2
               WHERE task_run_id = $3 AND iteration = $4 AND phase = $5
                 AND COALESCE(stage_index, -1) = $6"#,
            &[
                &status,
                &output_len_i32,
                &task_run_id,
                &iteration,
                &phase,
                &coalesced_stage,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("PG complete_workflow_ai_session", &e))?;

        Ok(())
    }

    /// Write `status` ONLY while the row still holds `attempt` -- the row id
    /// and the `session_started_at` that [`Self::create_workflow_ai_session`]
    /// returned for it -- AND is still `running`. The abandoned-phase write:
    /// it never overwrites a terminal status already committed (an abort that
    /// lands after the terminal write), nor any LATER attempt on the same
    /// row, finished or still running -- a resume under the same CLI id and
    /// the fallback's re-create under a fresh one included, since every create
    /// stamps a fresh `session_started_at`. Returns the number of rows changed.
    pub async fn set_workflow_ai_session_status_if_running_attempt(
        &self,
        attempt: WorkflowAiSessionAttempt,
        status: &str,
    ) -> Result<u64, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        conn.execute(
            r#"UPDATE workflow_ai_sessions
               SET status = $1, session_completed_at = NOW()
               WHERE id = $2
                 AND (EXTRACT(EPOCH FROM session_started_at) * 1000000)::bigint = $3
                 AND status = 'running'"#,
            &[&status, &attempt.row_id, &attempt.started_at_us],
        )
        .await
        .map_err(|e| {
            crate::database::pg::pg_err("PG set_workflow_ai_session_status_if_running_attempt", &e)
        })
    }

    /// Get the most recent AI session for a task run, keyed exactly as
    /// `create_workflow_ai_session` / `complete_workflow_ai_session` key it:
    /// `(task_run_id, iteration, phase, COALESCE(stage_index, -1))`, so a row
    /// written by another stage can never be returned.
    /// Returns (claude_cli_session_id, status) if found.
    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    pub async fn get_workflow_ai_session(
        &self,
        task_run_id: &str,
        iteration: i32,
        phase: &str,
        stage_index: Option<i32>,
    ) -> Result<Option<(String, String)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let coalesced_stage = stage_index.unwrap_or(-1);

        let row = conn
            .query_opt(
                r#"SELECT claude_cli_session_id, status
                   FROM workflow_ai_sessions
                   WHERE task_run_id = $1 AND iteration = $2 AND phase = $3
                     AND COALESCE(stage_index, -1) = $4
                   ORDER BY id DESC
                   LIMIT 1"#,
                &[&task_run_id, &iteration, &phase, &coalesced_stage],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("PG get_workflow_ai_session", &e))?;

        Ok(row.map(|r| (r.get(0), r.get(1))))
    }
}

#[cfg(test)]
mod workflow_ai_session_tests {
    //! `workflow_ai_sessions` keying, against a FRESH database from
    //! `PgDb::new_for_test()` (the bundled PostgreSQL harness).

    use super::*;

    const TASK: &str = "task-run-ai-session-keying";
    const PHASE: &str = "agentic";

    /// `workflow_ai_sessions.task_run_id` is FK'd to `task_runs(id)`.
    async fn fresh_db_with_task_run() -> std::sync::Arc<PgDb> {
        let pg = PgDb::new_for_test().await;
        let conn = pg.pool().get().await.expect("PG pool");
        conn.execute(
            "INSERT INTO task_runs (id, task_name) VALUES ($1, $2)",
            &[&TASK, &"workflow_ai_sessions keying test"],
        )
        .await
        .expect("insert task_run fixture");
        pg
    }

    /// Two stages of one (task run, iteration, phase): the lookup for stage 0
    /// returns stage 0's row even though stage 1's row is newer. The old
    /// `ORDER BY id DESC` query with no stage predicate returned stage 1's.
    #[tokio::test]
    async fn get_workflow_ai_session_is_keyed_on_stage() {
        let pg = fresh_db_with_task_run().await;
        pg.create_workflow_ai_session(TASK, 1, PHASE, Some(0), "cli-stage-0")
            .await
            .expect("create stage 0");
        pg.create_workflow_ai_session(TASK, 1, PHASE, Some(1), "cli-stage-1")
            .await
            .expect("create stage 1");

        let got = pg
            .get_workflow_ai_session(TASK, 1, PHASE, Some(0))
            .await
            .expect("get stage 0");
        assert_eq!(
            got,
            Some(("cli-stage-0".to_string(), "running".to_string()))
        );

        let got = pg
            .get_workflow_ai_session(TASK, 1, PHASE, Some(1))
            .await
            .expect("get stage 1");
        assert_eq!(
            got,
            Some(("cli-stage-1".to_string(), "running".to_string()))
        );
    }

    /// A stageless row (`None`, stored NULL, keyed as -1) is not shadowed by a
    /// newer staged row, and `None` and `Some(-1)` name the same key, exactly
    /// as `create_workflow_ai_session` / `complete_workflow_ai_session` use it.
    #[tokio::test]
    async fn get_workflow_ai_session_stageless_key_is_minus_one() {
        let pg = fresh_db_with_task_run().await;
        pg.create_workflow_ai_session(TASK, 2, PHASE, None, "cli-stageless")
            .await
            .expect("create stageless");
        pg.create_workflow_ai_session(TASK, 2, PHASE, Some(0), "cli-stage-0")
            .await
            .expect("create stage 0");

        let expected = Some(("cli-stageless".to_string(), "running".to_string()));
        let got = pg
            .get_workflow_ai_session(TASK, 2, PHASE, None)
            .await
            .expect("get None");
        assert_eq!(got, expected);
        let got = pg
            .get_workflow_ai_session(TASK, 2, PHASE, Some(-1))
            .await
            .expect("get Some(-1)");
        assert_eq!(got, expected);
    }

    /// The abandoned-phase write changes only the `running` row of its OWN
    /// attempt: a completed row, a later attempt under another CLI id, and a
    /// RESUMED attempt under the same CLI id are all left alone.
    #[tokio::test]
    async fn status_if_running_attempt_only_touches_its_own_running_attempt() {
        let pg = fresh_db_with_task_run().await;
        let done = pg
            .create_workflow_ai_session(TASK, 4, PHASE, Some(0), "cli-done")
            .await
            .expect("create done");
        pg.complete_workflow_ai_session(TASK, 4, PHASE, Some(0), "completed", 7)
            .await
            .expect("complete");
        let changed = pg
            .set_workflow_ai_session_status_if_running_attempt(done, "failed")
            .await
            .expect("write done");
        assert_eq!(changed, 0, "a completed row must not be overwritten");
        assert_eq!(
            pg.get_workflow_ai_session(TASK, 4, PHASE, Some(0))
                .await
                .expect("get done"),
            Some(("cli-done".to_string(), "completed".to_string()))
        );

        // A later attempt under ANOTHER CLI id.
        let stale = pg
            .create_workflow_ai_session(TASK, 4, PHASE, Some(1), "cli-stale")
            .await
            .expect("create stale");
        pg.create_workflow_ai_session(TASK, 4, PHASE, Some(1), "cli-live")
            .await
            .expect("create live");
        let changed = pg
            .set_workflow_ai_session_status_if_running_attempt(stale, "failed")
            .await
            .expect("write stale");
        assert_eq!(
            changed, 0,
            "another CLI id's running row must not be failed"
        );
        assert_eq!(
            pg.get_workflow_ai_session(TASK, 4, PHASE, Some(1))
                .await
                .expect("get live"),
            Some(("cli-live".to_string(), "running".to_string()))
        );

        // A RESUMED attempt under the SAME CLI id: same row, same id, new
        // session_started_at.
        let first = pg
            .create_workflow_ai_session(TASK, 4, PHASE, Some(2), "cli-same")
            .await
            .expect("create first");
        let resumed = pg
            .create_workflow_ai_session(TASK, 4, PHASE, Some(2), "cli-same")
            .await
            .expect("create resumed");
        assert_eq!(first.row_id, resumed.row_id);
        assert_ne!(first.started_at_us, resumed.started_at_us);
        let changed = pg
            .set_workflow_ai_session_status_if_running_attempt(first, "failed")
            .await
            .expect("write prior attempt");
        assert_eq!(changed, 0, "the prior attempt must not fail a resumed one");
        assert_eq!(
            pg.get_workflow_ai_session(TASK, 4, PHASE, Some(2))
                .await
                .expect("get resumed"),
            Some(("cli-same".to_string(), "running".to_string()))
        );

        // Its own running attempt changes.
        let changed = pg
            .set_workflow_ai_session_status_if_running_attempt(resumed, "failed")
            .await
            .expect("write own attempt");
        assert_eq!(changed, 1);
        assert_eq!(
            pg.get_workflow_ai_session(TASK, 4, PHASE, Some(2))
                .await
                .expect("get own"),
            Some(("cli-same".to_string(), "failed".to_string()))
        );
    }

    /// A terminal status written by `complete_workflow_ai_session` is what the
    /// next phase-start lookup reads back.
    #[tokio::test]
    async fn complete_then_get_reads_back_failed() {
        let pg = fresh_db_with_task_run().await;
        pg.create_workflow_ai_session(TASK, 3, PHASE, Some(2), "cli-budget-stop")
            .await
            .expect("create");
        pg.complete_workflow_ai_session(TASK, 3, PHASE, Some(2), "failed", 0)
            .await
            .expect("complete");

        let got = pg
            .get_workflow_ai_session(TASK, 3, PHASE, Some(2))
            .await
            .expect("get");
        assert_eq!(
            got,
            Some(("cli-budget-stop".to_string(), "failed".to_string()))
        );
    }
}
