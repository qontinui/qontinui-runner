//! PostgreSQL CRUD for the runner-owned `orchestration` schema —
//! Approach-D Conductor/Engine Phase 1 durable ledger.
//!
//! Persists the run + its growing subtask DAG (`orchestration.runs` /
//! `orchestration.subtasks`) so a later (Phase 3) reconciler is stateless over
//! durable state. Schema + self-heal live in `atlas/schema.hcl` and
//! `database/pg/mod.rs::verify_and_provision`; the Rust-native row types live
//! in `orchestration_loop::ledger`.
//!
//! Search-path note: the runner's PG pool sets `search_path TO project, public`
//! (`database/pg/mod.rs`). `orchestration` is NOT in search_path, so every SQL
//! statement here schema-qualifies `orchestration.runs` / `.subtasks`.
//!
//! Type-binding note: this crate's `tokio-postgres` enables `with-uuid-1`,
//! `with-chrono-0_4`, and `with-serde_json-1`, so `Uuid`, `DateTime<Utc>`, and
//! `serde_json::Value` are bound/decoded natively (no text round-trip needed).
//! `text[]` columns map directly to `Vec<String>`.

use super::PgDb;
use crate::database::pg::completion_reports::CompletionReport;
use crate::orchestration_loop::ledger::{
    LostWorkerDisposition, Run, Subtask, SubtaskState, WorkingRow,
};
use chrono::{DateTime, Utc};
use tokio_postgres::Row;
use uuid::Uuid;

impl PgDb {
    // ========================================================================
    // orchestration.runs / orchestration.subtasks (Phase 1 ledger)
    // ========================================================================

    /// Insert a new orchestration run and return the persisted [`Run`], with no
    /// owner and no stored config. Test fixtures only; the conductor start
    /// goes through [`Self::create_or_get_run`].
    ///
    /// Idempotent like its sibling: an existing `run_id` is returned as stored.
    #[cfg(test)]
    pub async fn create_run(
        &self,
        run_id: Uuid,
        goal: &str,
        recipe: Option<&str>,
        phases: &[String],
        status: &str,
    ) -> Result<Run, String> {
        self.create_or_get_run(run_id, goal, recipe, phases, status, None, None)
            .await
            .map(|(run, _created)| run)
    }

    /// Insert a run, or return the row that already has this `run_id`, with
    /// `true` when this call created it.
    ///
    /// `INSERT … ON CONFLICT (run_id) DO NOTHING` then a read-back, so a
    /// re-entry with an existing id (a restart relaunching its own run, or an
    /// operator re-posting the same id) returns the stored row instead of a
    /// 23505. The STORED row wins every field, the arguments included: the
    /// caller decides from it whether re-entry is allowed
    /// (`loop_engine::reentry_refusal`).
    ///
    /// `created_at` / `updated_at` are stamped by the DB defaults.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_or_get_run(
        &self,
        run_id: Uuid,
        goal: &str,
        recipe: Option<&str>,
        phases: &[String],
        status: &str,
        owner_instance: Option<&str>,
        config: Option<&serde_json::Value>,
    ) -> Result<(Run, bool), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let phases_owned: Vec<String> = phases.to_vec();
        let inserted = conn
            .execute(
                r#"
            INSERT INTO orchestration.runs
                (run_id, goal, recipe, phases, status, owner_instance, config)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (run_id) DO NOTHING
            "#,
                &[
                    &run_id,
                    &goal,
                    &recipe,
                    &phases_owned,
                    &status,
                    &owner_instance,
                    &config,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("create_or_get_run", &e))?;

        let row = conn
            .query_one(
                r#"
                SELECT run_id, goal, recipe, phases, status, status_reason, created_at, updated_at,
                       owner_instance, config
                FROM orchestration.runs
                WHERE run_id = $1
                "#,
                &[&run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("create_or_get_run read-back", &e))?;

        Ok((Self::run_from_row(&row), inserted == 1))
    }

    /// Every `running` run owned by `owner_instance`, oldest first — the boot
    /// sweep's relaunch candidates. Rows owned by another instance, and rows
    /// with a NULL owner (they predate the column), are never returned.
    pub async fn list_running_runs_owned_by(
        &self,
        owner_instance: &str,
    ) -> Result<Vec<Run>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT run_id, goal, recipe, phases, status, status_reason, created_at, updated_at,
                       owner_instance, config
                FROM orchestration.runs
                WHERE status = 'running' AND owner_instance = $1
                ORDER BY created_at ASC
                "#,
                &[&owner_instance],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_running_runs_owned_by", &e))?;

        Ok(rows.iter().map(Self::run_from_row).collect())
    }

    /// The ids of every `running` run with no recorded owner. The boot sweep
    /// logs these and leaves them alone: an unattributable row must not be
    /// driven by whichever instance boots first.
    pub async fn list_unowned_running_run_ids(&self) -> Result<Vec<Uuid>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT run_id
                FROM orchestration.runs
                WHERE status = 'running' AND owner_instance IS NULL
                ORDER BY created_at ASC
                "#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_unowned_running_run_ids", &e))?;

        rows.iter()
            .map(|r| {
                r.try_get::<_, Uuid>(0)
                    .map_err(|e| crate::database::pg::pg_err("list_unowned_running_run_ids", &e))
            })
            .collect()
    }

    /// The run's `working` rows with what the boot sweep needs to decide each
    /// one: the worker id, whether a report already landed, and how many times
    /// the row has been reset before.
    pub async fn list_working_rows_for_sweep(
        &self,
        run_id: Uuid,
    ) -> Result<Vec<WorkingRow>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT task_id, task_run_id, artifact IS NOT NULL, restart_resets
                FROM orchestration.subtasks
                WHERE run_id = $1 AND state = 'working'
                ORDER BY idx ASC, task_id ASC
                "#,
                &[&run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_working_rows_for_sweep", &e))?;

        rows.iter()
            .map(|r| {
                let err = |e: tokio_postgres::Error| {
                    crate::database::pg::pg_err("list_working_rows_for_sweep", &e)
                };
                Ok(WorkingRow {
                    task_id: r.try_get(0).map_err(err)?,
                    task_run_id: r.try_get(1).map_err(err)?,
                    has_artifact: r.try_get(2).map_err(err)?,
                    restart_resets: r.try_get(3).map_err(err)?,
                })
            })
            .collect()
    }

    /// Apply the boot sweep's decision to one lost `working` row. Guarded on
    /// `state = 'working'` so a row something else already moved is left
    /// alone; returns whether the row moved.
    ///
    /// - [`LostWorkerDisposition::Reported`] → no write (`Ok(false)`): the
    ///   relaunched reconciler completes it through its normal path.
    /// - [`LostWorkerDisposition::Resubmit`] → `submitted`, `task_run_id`
    ///   cleared, `restart_resets + 1`.
    /// - [`LostWorkerDisposition::Fail`] → `failed`, with
    ///   [`LostWorkerDisposition::FAIL_REASON`] as its `state_reason`.
    pub async fn apply_lost_worker(
        &self,
        run_id: Uuid,
        task_id: &str,
        disposition: LostWorkerDisposition,
    ) -> Result<bool, String> {
        // `$3` is the row's `state_reason`: the fail reason, or NULL when the
        // row goes back to `submitted` (it is not failed any more).
        let (sql, reason): (&str, Option<&str>) = match disposition {
            LostWorkerDisposition::Reported => return Ok(false),
            LostWorkerDisposition::Resubmit => (
                r#"
                UPDATE orchestration.subtasks
                SET state = 'submitted',
                    task_run_id = NULL,
                    restart_resets = restart_resets + 1,
                    state_reason = $3,
                    updated_at = now()
                WHERE run_id = $1 AND task_id = $2 AND state = 'working'
                "#,
                None,
            ),
            LostWorkerDisposition::Fail => (
                r#"
                UPDATE orchestration.subtasks
                SET state = 'failed', state_reason = $3, updated_at = now()
                WHERE run_id = $1 AND task_id = $2 AND state = 'working'
                "#,
                Some(LostWorkerDisposition::FAIL_REASON),
            ),
        };
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;
        let n = conn
            .execute(sql, &[&run_id, &task_id, &reason])
            .await
            .map_err(|e| crate::database::pg::pg_err("apply_lost_worker", &e))?;
        Ok(n > 0)
    }

    /// Fetch a single run row by id. Returns `Ok(None)` when no row matches.
    pub async fn get_run(&self, run_id: Uuid) -> Result<Option<Run>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let row = conn
            .query_opt(
                r#"
                SELECT run_id, goal, recipe, phases, status, status_reason, created_at, updated_at,
                       owner_instance, config
                FROM orchestration.runs
                WHERE run_id = $1
                "#,
                &[&run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("get_run", &e))?;

        Ok(row.as_ref().map(Self::run_from_row))
    }

    /// List all runs, newest first. Backs the conductor run list API.
    pub async fn list_runs(&self) -> Result<Vec<Run>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT run_id, goal, recipe, phases, status, status_reason, created_at, updated_at,
                       owner_instance, config
                FROM orchestration.runs
                ORDER BY created_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_runs", &e))?;

        Ok(rows.iter().map(Self::run_from_row).collect())
    }

    /// Update a run's `status` (e.g. `running` → `complete` / `failed` /
    /// `stalled` / `stopped`) together with the reason it left `running`
    /// (`status_reason`; `None` clears it — a `complete` run carries no reason).
    /// Returns `Err` if no row matched `run_id`.
    ///
    /// **UNCONDITIONAL — it overwrites whatever the row says, including a
    /// terminal status another writer just wrote.** That is why the conductor
    /// does NOT use it: its exits go through
    /// [`Self::set_run_status_if_running`], which loses to whoever left
    /// `running` first, because the reconciler and `stop_orchestration_run`
    /// race one tick wide and an operator's `stopped` must not be clobbered by
    /// a `stalled` decided before the Stop was pressed.
    ///
    /// Use this one only where the caller is the run's sole writer at that
    /// moment — creating the row, or a path that has already established the
    /// run is not being reconciled.
    pub async fn set_run_status(
        &self,
        run_id: Uuid,
        status: &str,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE orchestration.runs
                SET status = $2,
                    status_reason = $3,
                    updated_at = now()
                WHERE run_id = $1
                "#,
                &[&run_id, &status, &reason],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("set_run_status", &e))?;

        if n == 0 {
            return Err(format!("set_run_status: no run {}", run_id));
        }
        Ok(())
    }

    /// Move a run out of `running` ONLY IF it is still `running`, returning
    /// whether the row moved. The guard is in the SQL (`AND status = 'running'`)
    /// so it is atomic against the reconciler's own terminal write.
    ///
    /// This is what a stop must use. An unconditional `stopped` write overwrote
    /// the terminal diagnosis a run had already reached: a run that exited
    /// `stalled` (or `failed` on a DAG cycle) is still listed, the operator
    /// presses Stop, and the row becomes `stopped` / "stop requested" — erasing
    /// the reason the run ended, which is the whole point of persisting it.
    /// `false` means the run was already terminal and keeps the status it has.
    pub async fn set_run_status_if_running(
        &self,
        run_id: Uuid,
        status: &str,
        reason: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE orchestration.runs
                SET status = $2,
                    status_reason = $3,
                    updated_at = now()
                WHERE run_id = $1 AND status = 'running'
                "#,
                &[&run_id, &status, &reason],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("set_run_status_if_running", &e))?;

        Ok(n > 0)
    }

    /// Insert-or-update a subtask keyed on `(run_id, task_id)`. Used by the
    /// conductor when it (re)declares a DAG node. On conflict every mutable
    /// column is overwritten and `updated_at` is bumped; `created_at` is
    /// preserved.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_subtask(&self, subtask: &Subtask) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let artifact_json = match &subtask.artifact {
            Some(report) => Some(
                serde_json::to_value(report)
                    .map_err(|e| format!("upsert_subtask serialize artifact: {}", e))?,
            ),
            None => None,
        };
        let depends_on: Vec<String> = subtask.depends_on.clone();
        let state = subtask.state.as_str();

        conn.execute(
            r#"
            INSERT INTO orchestration.subtasks
                (task_id, run_id, idx, title, brief, phase, repo, depends_on,
                 expected_output, emits_subtasks, state, task_run_id, artifact,
                 produced_by, gate_id, gate_status, state_reason)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                    $15, $16, $17)
            ON CONFLICT (run_id, task_id) DO UPDATE SET
                idx             = EXCLUDED.idx,
                title           = EXCLUDED.title,
                brief           = EXCLUDED.brief,
                phase           = EXCLUDED.phase,
                repo            = EXCLUDED.repo,
                depends_on      = EXCLUDED.depends_on,
                expected_output = EXCLUDED.expected_output,
                emits_subtasks  = EXCLUDED.emits_subtasks,
                state           = EXCLUDED.state,
                task_run_id     = EXCLUDED.task_run_id,
                artifact        = EXCLUDED.artifact,
                produced_by     = EXCLUDED.produced_by,
                gate_id         = EXCLUDED.gate_id,
                gate_status     = EXCLUDED.gate_status,
                state_reason    = EXCLUDED.state_reason,
                updated_at      = now()
            "#,
            &[
                &subtask.task_id,
                &subtask.run_id,
                &subtask.idx,
                &subtask.title,
                &subtask.brief,
                &subtask.phase,
                &subtask.repo,
                &depends_on,
                &subtask.expected_output,
                &subtask.emits_subtasks,
                &state,
                &subtask.task_run_id,
                &artifact_json,
                &subtask.produced_by,
                &subtask.gate_id,
                &subtask.gate_status,
                &subtask.state_reason,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("upsert_subtask", &e))?;

        Ok(())
    }

    /// List every subtask for a run, ordered by `idx` then `task_id` for a
    /// stable shape.
    pub async fn list_subtasks(&self, run_id: Uuid) -> Result<Vec<Subtask>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                r#"
                SELECT task_id, run_id, idx, title, brief, phase, repo,
                       depends_on, expected_output, emits_subtasks, state,
                       task_run_id, artifact, produced_by, gate_id, gate_status,
                       created_at, updated_at, state_reason
                FROM orchestration.subtasks
                WHERE run_id = $1
                ORDER BY idx ASC, task_id ASC
                "#,
                &[&run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_subtasks", &e))?;

        rows.iter().map(Self::subtask_from_row).collect()
    }

    /// Transition a single subtask's lifecycle state. Returns `Err` if no row
    /// matched `(run_id, task_id)`.
    pub async fn set_subtask_state(
        &self,
        run_id: Uuid,
        task_id: &str,
        state: SubtaskState,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE orchestration.subtasks
                SET state = $3,
                    updated_at = now()
                WHERE run_id = $1 AND task_id = $2
                "#,
                &[&run_id, &task_id, &state.as_str()],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("set_subtask_state", &e))?;

        if n == 0 {
            return Err(format!(
                "set_subtask_state: no subtask {} in run {}",
                task_id, run_id
            ));
        }
        Ok(())
    }

    /// Move a subtask to `failed` and record WHY in `state_reason`. Every
    /// terminal failure the runner decides goes through here, so a failed row
    /// always says what failed it and a failed run can name its rows' causes
    /// (plan `2026-09-23-conductor-e2e-phase1-defects`, Phase 3). Returns `Err`
    /// if no row matched `(run_id, task_id)`.
    pub async fn fail_subtask(
        &self,
        run_id: Uuid,
        task_id: &str,
        reason: &str,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE orchestration.subtasks
                SET state = 'failed',
                    state_reason = $3,
                    updated_at = now()
                WHERE run_id = $1 AND task_id = $2
                "#,
                &[&run_id, &task_id, &reason],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("fail_subtask", &e))?;

        if n == 0 {
            return Err(format!(
                "fail_subtask: no subtask {} in run {}",
                task_id, run_id
            ));
        }
        Ok(())
    }

    /// Phase 6: associate a coord gate with a subtask (and/or update its
    /// last-polled status). Writes BOTH `gate_id` and `gate_status` — pass the
    /// current `gate_id` again when only refreshing the status. This is the
    /// DURABLE gate association a restart re-attaches to (the reconciler re-reads
    /// it on load and resumes polling without re-registering). Returns `Err` if
    /// no row matched `(run_id, task_id)`.
    pub async fn set_subtask_gate(
        &self,
        run_id: Uuid,
        task_id: &str,
        gate_id: Option<&str>,
        gate_status: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE orchestration.subtasks
                SET gate_id = $3,
                    gate_status = $4,
                    updated_at = now()
                WHERE run_id = $1 AND task_id = $2
                "#,
                &[&run_id, &task_id, &gate_id, &gate_status],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("set_subtask_gate", &e))?;

        if n == 0 {
            return Err(format!(
                "set_subtask_gate: no subtask {} in run {}",
                task_id, run_id
            ));
        }
        Ok(())
    }

    /// Persist a worker-written [`CompletionReport`] to a subtask's `artifact`
    /// column. Serializes byte-identically to the canonical report shape.
    /// Returns `Err` if no row matched `(run_id, task_id)`.
    pub async fn write_subtask_artifact(
        &self,
        run_id: Uuid,
        task_id: &str,
        artifact: &CompletionReport,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let artifact_json = serde_json::to_value(artifact)
            .map_err(|e| format!("write_subtask_artifact serialize: {}", e))?;

        let n = conn
            .execute(
                r#"
                UPDATE orchestration.subtasks
                SET artifact = $3::jsonb,
                    updated_at = now()
                WHERE run_id = $1 AND task_id = $2
                "#,
                &[&run_id, &task_id, &artifact_json],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("write_subtask_artifact", &e))?;

        if n == 0 {
            return Err(format!(
                "write_subtask_artifact: no subtask {} in run {}",
                task_id, run_id
            ));
        }
        Ok(())
    }

    /// Insert-if-not-exists a batch of subtasks for idempotent DESIGN /
    /// elaboration splicing (Phase 4). Rows that already exist (same
    /// `(run_id, task_id)`) are left untouched — this is the idempotent splice
    /// primitive, so re-running an elaboration never clobbers in-flight state.
    ///
    /// Returns the number of rows actually inserted.
    pub async fn splice_subtasks(&self, subtasks: &[Subtask]) -> Result<u64, String> {
        if subtasks.is_empty() {
            return Ok(0);
        }

        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let mut inserted = 0u64;
        for subtask in subtasks {
            let artifact_json = match &subtask.artifact {
                Some(report) => Some(
                    serde_json::to_value(report)
                        .map_err(|e| format!("splice_subtasks serialize artifact: {}", e))?,
                ),
                None => None,
            };
            let depends_on: Vec<String> = subtask.depends_on.clone();
            let state = subtask.state.as_str();

            let n = conn
                .execute(
                    r#"
                    INSERT INTO orchestration.subtasks
                        (task_id, run_id, idx, title, brief, phase, repo,
                         depends_on, expected_output, emits_subtasks, state,
                         task_run_id, artifact, produced_by, gate_id, gate_status)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                            $13, $14, $15, $16)
                    ON CONFLICT (run_id, task_id) DO NOTHING
                    "#,
                    &[
                        &subtask.task_id,
                        &subtask.run_id,
                        &subtask.idx,
                        &subtask.title,
                        &subtask.brief,
                        &subtask.phase,
                        &subtask.repo,
                        &depends_on,
                        &subtask.expected_output,
                        &subtask.emits_subtasks,
                        &state,
                        &subtask.task_run_id,
                        &artifact_json,
                        &subtask.produced_by,
                        &subtask.gate_id,
                        &subtask.gate_status,
                    ],
                )
                .await
                .map_err(|e| crate::database::pg::pg_err("splice_subtasks", &e))?;
            inserted += n;
        }

        Ok(inserted)
    }

    // -- row decoders --

    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    fn run_from_row(row: &Row) -> Run {
        Run {
            run_id: row.get(0),
            goal: row.get(1),
            recipe: row.get(2),
            phases: row.get(3),
            status: row.get(4),
            status_reason: row.get(5),
            created_at: row.get::<_, DateTime<Utc>>(6),
            updated_at: row.get::<_, DateTime<Utc>>(7),
            owner_instance: row.get(8),
            config: row.get(9),
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "legacy Row::get — migrate to try_get; dossier row-get-panic-kills-spawned-loop"
    )]
    fn subtask_from_row(row: &Row) -> Result<Subtask, String> {
        let state_text: String = row.get(10);
        let state = SubtaskState::from_str_value(&state_text)
            .ok_or_else(|| format!("subtask_from_row: unknown state {:?}", state_text))?;

        let artifact_json: Option<serde_json::Value> = row.get(12);
        let artifact = match artifact_json {
            Some(v) => Some(
                serde_json::from_value::<CompletionReport>(v)
                    .map_err(|e| format!("subtask_from_row deserialize artifact: {}", e))?,
            ),
            None => None,
        };

        Ok(Subtask {
            task_id: row.get(0),
            run_id: row.get(1),
            idx: row.get(2),
            title: row.get(3),
            brief: row.get(4),
            phase: row.get(5),
            repo: row.get(6),
            depends_on: row.get(7),
            expected_output: row.get(8),
            emits_subtasks: row.get(9),
            state,
            task_run_id: row.get(11),
            artifact,
            produced_by: row.get(13),
            gate_id: row.get(14),
            gate_status: row.get(15),
            created_at: row.get::<_, DateTime<Utc>>(16),
            updated_at: row.get::<_, DateTime<Utc>>(17),
            state_reason: row.get(18),
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pg::completion_reports::{BreakingChange, Deliverable, FollowUp};
    use std::collections::HashMap;

    /// Build a `Subtask` with the given key fields and sensible defaults for
    /// the rest. `artifact`/`task_run_id`/`produced_by` start empty.
    fn mk_subtask(
        run_id: Uuid,
        task_id: &str,
        idx: i32,
        depends_on: Vec<String>,
        emits_subtasks: bool,
    ) -> Subtask {
        Subtask {
            task_id: task_id.to_string(),
            run_id,
            idx,
            title: format!("title-{}", task_id),
            brief: format!("brief for {}", task_id),
            phase: "implement".to_string(),
            repo: Some("qontinui-runner".to_string()),
            depends_on,
            expected_output: "a green PR".to_string(),
            emits_subtasks,
            state: SubtaskState::Submitted,
            task_run_id: None,
            artifact: None,
            produced_by: None,
            gate_id: None,
            gate_status: None,
            state_reason: None,
            // Placeholders — the DB stamps real `now()` values on insert; the
            // in-memory struct never sends these (they're not in any INSERT
            // column list).
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// A `CompletionReport` with at least one deliverable, breaking_change, and
    /// follow_up populated — for the artifact round-trip assertion.
    fn sample_report() -> CompletionReport {
        CompletionReport {
            summary_md: "Phase 1 durable ledger landed.".to_string(),
            deliverables: vec![Deliverable {
                kind: "schema-change".to_string(),
                reference: "orchestration.runs".to_string(),
                description: "Added orchestration schema + two tables".to_string(),
            }],
            breaking_changes: vec![BreakingChange {
                area: "database".to_string(),
                description: "New runner-owned schema".to_string(),
                migration_steps_md: "Self-heals on next runner boot.".to_string(),
            }],
            follow_ups: vec![FollowUp {
                description: "Wire the reconciler in Phase 3".to_string(),
                priority: "important".to_string(),
                blocking_for_dependents: false,
            }],
            artifacts: {
                let mut m = HashMap::new();
                m.insert(
                    "conductor".to_string(),
                    serde_json::json!({"runKind": "design"}),
                );
                m
            },
        }
    }

    /// Round-trip a run + a small subtask DAG and an artifact write.
    ///
    /// `#[ignore]` to match the `database/pg/*` test convention — needs a live
    /// PG fixture (DATABASE_URL) with the `orchestration` schema present (the
    /// runner self-heals it at `PgDb::new`). Run with:
    /// `cargo test -p qontinui-runner orchestration::tests::run_and_subtask_dag_round_trip -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn run_and_subtask_dag_round_trip() {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();

        // 1. Create the run.
        let phases = vec![
            "plan".to_string(),
            "implement".to_string(),
            "test".to_string(),
        ];
        let run = pg
            .create_run(
                run_id,
                "ship the conductor",
                Some("approach-d"),
                &phases,
                "running",
            )
            .await
            .expect("create_run");
        assert_eq!(run.run_id, run_id);
        assert_eq!(run.goal, "ship the conductor");
        assert_eq!(run.recipe.as_deref(), Some("approach-d"));
        assert_eq!(run.phases, phases);
        assert_eq!(run.status, "running");
        assert_eq!(run.status_reason, None, "a fresh run carries no reason");

        // set_run_status writes status AND reason; get_run reads both back.
        pg.set_run_status(run_id, "stalled", Some("Stall detected: x"))
            .await
            .expect("set_run_status stalled");
        let stalled = pg.get_run(run_id).await.expect("get_run").expect("row");
        assert_eq!(stalled.status, "stalled");
        assert_eq!(stalled.status_reason.as_deref(), Some("Stall detected: x"));
        // A STOP must not erase a terminal diagnosis: the conditional write
        // refuses to move a run that already left `running`.
        let moved = pg
            .set_run_status_if_running(run_id, "stopped", Some("stop requested"))
            .await
            .expect("conditional write");
        assert!(!moved, "a stalled run is not moved by a stop");
        let still = pg.get_run(run_id).await.expect("get_run").expect("row");
        assert_eq!(still.status, "stalled", "the terminal status survives Stop");
        assert_eq!(
            still.status_reason.as_deref(),
            Some("Stall detected: x"),
            "and so does the reason the run ended"
        );

        pg.set_run_status(run_id, "running", None)
            .await
            .expect("set_run_status running");
        let back = pg.get_run(run_id).await.expect("get_run").expect("row");
        assert_eq!(back.status, "running");
        assert_eq!(back.status_reason, None, "None clears the reason");

        // The same call on a run that IS running does move it.
        let moved = pg
            .set_run_status_if_running(run_id, "stopped", Some("stop requested"))
            .await
            .expect("conditional write");
        assert!(moved, "a running run is stopped");
        let stopped = pg.get_run(run_id).await.expect("get_run").expect("row");
        assert_eq!(stopped.status, "stopped");
        assert_eq!(stopped.status_reason.as_deref(), Some("stop requested"));
        pg.set_run_status(run_id, "running", None)
            .await
            .expect("back to running for the rest of the test");

        // 2. Upsert a small DAG: A (root), B depends on A, C depends on A+B.
        let a = mk_subtask(run_id, "A", 0, vec![], true);
        let b = mk_subtask(run_id, "B", 1, vec!["A".to_string()], false);
        let c = mk_subtask(
            run_id,
            "C",
            2,
            vec!["A".to_string(), "B".to_string()],
            false,
        );
        pg.upsert_subtask(&a).await.expect("upsert A");
        pg.upsert_subtask(&b).await.expect("upsert B");
        pg.upsert_subtask(&c).await.expect("upsert C");

        // 3. list_subtasks returns them with correct fields incl. depends_on.
        let listed = pg.list_subtasks(run_id).await.expect("list_subtasks");
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].task_id, "A");
        assert!(listed[0].emits_subtasks);
        assert_eq!(listed[0].depends_on, Vec::<String>::new());
        assert_eq!(listed[1].task_id, "B");
        assert_eq!(listed[1].depends_on, vec!["A".to_string()]);
        assert_eq!(listed[2].task_id, "C");
        assert_eq!(listed[2].depends_on, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(listed[2].repo.as_deref(), Some("qontinui-runner"));
        assert_eq!(listed[2].expected_output, "a green PR");
        assert!(matches!(listed[2].state, SubtaskState::Submitted));

        // upsert idempotency: re-upsert B with a new title overwrites it.
        let mut b2 = b.clone();
        b2.title = "B-renamed".to_string();
        b2.state = SubtaskState::Working;
        pg.upsert_subtask(&b2).await.expect("re-upsert B");
        let after = pg
            .list_subtasks(run_id)
            .await
            .expect("list after re-upsert");
        assert_eq!(after.len(), 3, "upsert must not add a row on conflict");
        let b_row = after.iter().find(|s| s.task_id == "B").unwrap();
        assert_eq!(b_row.title, "B-renamed");
        assert!(matches!(b_row.state, SubtaskState::Working));

        // set_subtask_state.
        pg.set_subtask_state(run_id, "A", SubtaskState::Completed)
            .await
            .expect("set_subtask_state A");
        let a_row = pg
            .list_subtasks(run_id)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.task_id == "A")
            .unwrap();
        assert!(matches!(a_row.state, SubtaskState::Completed));

        // 4. Write a CompletionReport to artifact and read it back identically.
        let report = sample_report();
        pg.write_subtask_artifact(run_id, "A", &report)
            .await
            .expect("write_subtask_artifact");
        let a_with_artifact = pg
            .list_subtasks(run_id)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.task_id == "A")
            .unwrap();
        let read_back = a_with_artifact
            .artifact
            .expect("artifact must be present after write");
        // Byte-identical round trip via serde value comparison.
        assert_eq!(
            serde_json::to_value(&read_back).unwrap(),
            serde_json::to_value(&report).unwrap(),
            "artifact must round-trip byte-identically to the original CompletionReport"
        );

        // splice_subtasks: insert-if-not-exists. Splicing A (exists) + D (new)
        // inserts only D and leaves A untouched.
        let mut a_stale = mk_subtask(run_id, "A", 99, vec![], false);
        a_stale.title = "A-should-not-overwrite".to_string();
        a_stale.produced_by = Some("A".to_string());
        let d = {
            let mut d = mk_subtask(run_id, "D", 3, vec!["B".to_string()], false);
            d.produced_by = Some("A".to_string());
            d
        };
        let inserted = pg
            .splice_subtasks(&[a_stale, d])
            .await
            .expect("splice_subtasks");
        assert_eq!(inserted, 1, "only the new row D should insert");
        let final_rows = pg.list_subtasks(run_id).await.unwrap();
        assert_eq!(final_rows.len(), 4);
        let a_final = final_rows.iter().find(|s| s.task_id == "A").unwrap();
        assert_eq!(
            a_final.title, "title-A",
            "splice must NOT overwrite the existing A row"
        );
        let d_final = final_rows.iter().find(|s| s.task_id == "D").unwrap();
        assert_eq!(d_final.produced_by.as_deref(), Some("A"));

        // Cleanup (FK CASCADE removes subtasks when the run is deleted).
        let conn = pg.pool().get().await.expect("conn");
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    /// Delete a test run (FK CASCADE removes its subtasks).
    async fn delete_run(pg: &PgDb, run_id: Uuid) {
        let conn = pg.pool().get().await.expect("conn");
        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }

    /// A unique owner per test, so parallel tests and rows a live runner left
    /// in the fixture never cross.
    fn test_owner() -> String {
        format!("test-owner-{}", Uuid::new_v4())
    }

    /// `subtasks.restart_resets` for one row — not on [`Subtask`], so read raw.
    async fn restart_resets(pg: &PgDb, run_id: Uuid, task_id: &str) -> i32 {
        let conn = pg.pool().get().await.expect("conn");
        let row = conn
            .query_one(
                "SELECT restart_resets FROM orchestration.subtasks \
                 WHERE run_id = $1 AND task_id = $2",
                &[&run_id, &task_id],
            )
            .await
            .expect("restart_resets");
        row.try_get::<_, i32>(0).expect("int")
    }

    /// Phase 2 (`2026-09-23-conductor-e2e-phase1-defects`): re-entry with an
    /// existing `run_id` returns the stored row instead of a 23505, and the
    /// stored row wins every field; the config and owner round-trip.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn create_or_get_run_is_idempotent_and_round_trips_owner_and_config() {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        let owner = test_owner();
        let cfg = serde_json::json!({ "concurrency_cap": 7, "tick_interval_secs": 11 });
        let phases = vec!["implement".to_string()];

        let (first, created) = pg
            .create_or_get_run(
                run_id,
                "goal",
                None,
                &phases,
                "running",
                Some(&owner),
                Some(&cfg),
            )
            .await
            .expect("create");
        assert!(created, "the first call creates the row");
        assert_eq!(first.owner_instance.as_deref(), Some(owner.as_str()));
        assert_eq!(first.config.as_ref(), Some(&cfg));

        let (again, created) = pg
            .create_or_get_run(
                run_id,
                "a different goal",
                None,
                &[],
                "running",
                Some("someone-else"),
                None,
            )
            .await
            .expect("re-entry returns the row, not 23505");
        assert!(!created, "a re-entry does not create");
        assert_eq!(again.goal, "goal", "the stored row wins");
        assert_eq!(again.owner_instance.as_deref(), Some(owner.as_str()));
        assert_eq!(again.config.as_ref(), Some(&cfg));
        assert_eq!(again.created_at, first.created_at);

        delete_run(&pg, run_id).await;
    }

    /// The sweep reads only its own `running` rows: a foreign-owned row, an
    /// unowned row and a terminal own row are all excluded.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn the_sweep_lists_only_its_own_running_runs() {
        let pg = PgDb::new_for_test().await;
        let me = test_owner();
        let other = test_owner();
        let mine = Uuid::new_v4();
        let mine_done = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let unowned = Uuid::new_v4();
        for (id, owner) in [
            (mine, Some(me.as_str())),
            (mine_done, Some(me.as_str())),
            (foreign, Some(other.as_str())),
            (unowned, None),
        ] {
            pg.create_or_get_run(id, "g", None, &[], "running", owner, None)
                .await
                .expect("create");
        }
        pg.set_run_status(mine_done, "complete", None)
            .await
            .expect("complete");

        let listed: Vec<Uuid> = pg
            .list_running_runs_owned_by(&me)
            .await
            .expect("list")
            .into_iter()
            .map(|r| r.run_id)
            .collect();
        assert_eq!(listed, vec![mine]);

        let unowned_ids = pg.list_unowned_running_run_ids().await.expect("unowned");
        assert!(unowned_ids.contains(&unowned));
        assert!(!unowned_ids.contains(&foreign));

        for id in [mine, mine_done, foreign, unowned] {
            delete_run(&pg, id).await;
        }
    }

    /// Phase 3: `fail_subtask` writes `failed` with its reason, `list_subtasks`
    /// reads the reason back, and a lost worker failed by the boot sweep
    /// carries its own reason.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn fail_subtask_records_its_reason() {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_or_get_run(run_id, "g", None, &[], "running", Some(&test_owner()), None)
            .await
            .expect("create");
        pg.upsert_subtask(&mk_subtask(run_id, "A", 0, vec![], false))
            .await
            .expect("upsert");
        pg.fail_subtask(run_id, "A", "dependency Z failed")
            .await
            .expect("fail");
        let a = pg.list_subtasks(run_id).await.expect("list").remove(0);
        assert_eq!(a.state, SubtaskState::Failed);
        assert_eq!(a.state_reason.as_deref(), Some("dependency Z failed"));
        assert!(pg.fail_subtask(run_id, "nope", "x").await.is_err());

        let mut lost = mk_subtask(run_id, "L", 1, vec![], false);
        lost.state = SubtaskState::Working;
        lost.task_run_id = Some(Uuid::new_v4());
        pg.upsert_subtask(&lost).await.expect("upsert");
        assert!(pg
            .apply_lost_worker(run_id, "L", LostWorkerDisposition::Fail)
            .await
            .expect("apply"));
        let rows = pg.list_subtasks(run_id).await.expect("list");
        let l = rows.iter().find(|s| s.task_id == "L").expect("L");
        assert_eq!(
            l.state_reason.as_deref(),
            Some(LostWorkerDisposition::FAIL_REASON)
        );

        delete_run(&pg, run_id).await;
    }

    /// The lost-worker settle: a completed and a submitted row are untouched
    /// (nothing is re-run), a `working` row with a landed report is left for
    /// the reconciler, a
    /// `working` row without one returns to `submitted` with `task_run_id`
    /// cleared and `restart_resets = 1`, and a live worker is left alone.
    /// Two more losses of the same row fail it.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn lost_workers_are_settled_before_relaunch() {
        use crate::orchestration_loop::loop_engine::{settle_lost_workers, LostWorkerCounts};

        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        pg.create_or_get_run(run_id, "g", None, &[], "running", Some(&test_owner()), None)
            .await
            .expect("create");

        let live_worker = Uuid::new_v4();
        let cases = [
            (
                "done",
                SubtaskState::Completed,
                Some(Uuid::new_v4()),
                Some(sample_report()),
            ),
            ("queued", SubtaskState::Submitted, None, None),
            (
                "reported",
                SubtaskState::Working,
                Some(Uuid::new_v4()),
                Some(sample_report()),
            ),
            ("lost", SubtaskState::Working, Some(Uuid::new_v4()), None),
            ("alive", SubtaskState::Working, Some(live_worker), None),
        ];
        for (idx, (task_id, state, trid, artifact)) in cases.into_iter().enumerate() {
            let mut st = mk_subtask(run_id, task_id, idx as i32, vec![], false);
            st.state = state;
            st.task_run_id = trid;
            st.artifact = artifact;
            pg.upsert_subtask(&st).await.expect("upsert");
        }

        let counts = settle_lost_workers(&pg, run_id, |t| t == live_worker)
            .await
            .expect("settle");
        assert_eq!(
            counts,
            LostWorkerCounts {
                reported: 1,
                resubmitted: 1,
                failed: 0,
                live: 1
            }
        );

        let after = pg.list_subtasks(run_id).await.expect("list");
        let row = |id: &str| after.iter().find(|s| s.task_id == id).expect(id).clone();
        assert_eq!(row("done").state, SubtaskState::Completed);
        assert_eq!(row("queued").state, SubtaskState::Submitted);
        // Left for the relaunched reconciler, which completes a `Gone` worker
        // with an artifact through its normal path.
        assert_eq!(row("reported").state, SubtaskState::Working);
        assert_eq!(row("alive").state, SubtaskState::Working);
        let lost = row("lost");
        assert_eq!(lost.state, SubtaskState::Submitted);
        assert_eq!(lost.task_run_id, None, "the dead worker id is cleared");
        assert_eq!(restart_resets(&pg, run_id, "lost").await, 1);

        // Lose the same row twice more: the second reset is allowed, the
        // third loss fails it.
        let mut again = lost;
        for expected in [SubtaskState::Submitted, SubtaskState::Failed] {
            again.state = SubtaskState::Working;
            again.task_run_id = Some(Uuid::new_v4());
            pg.upsert_subtask(&again).await.expect("re-dispatch");
            settle_lost_workers(&pg, run_id, |t| t == live_worker)
                .await
                .expect("settle");
            let now = pg.list_subtasks(run_id).await.expect("list");
            let r = now.iter().find(|s| s.task_id == "lost").expect("lost");
            assert_eq!(r.state, expected);
        }
        assert_eq!(
            restart_resets(&pg, run_id, "lost").await,
            2,
            "a fail does not count as a reset"
        );

        delete_run(&pg, run_id).await;
    }
}
