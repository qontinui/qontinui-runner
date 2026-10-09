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
use crate::orchestration_loop::conductor::OrchestrationRunConfig;
use crate::orchestration_loop::ledger::{Run, Subtask, SubtaskState};
use chrono::{DateTime, Utc};
use tokio_postgres::Row;
use tracing::{info, warn};
use uuid::Uuid;

/// The `orchestration.runs` column list every run read selects, in the order
/// [`PgDb::run_from_row`] decodes it. One constant so a new column cannot be
/// added to one query and missed in another.
const RUN_COLUMNS: &str = "run_id, goal, recipe, phases, status, status_reason, \
                           created_at, updated_at, config, owner_instance";

/// Why [`PgDb::create_run`] refused to create or re-enter a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRunError {
    /// The `run_id` exists and has left `running`. A terminal verdict is
    /// durable; re-entering would relaunch a finished run over it.
    Terminal { run_id: Uuid, status: String },
    /// The `run_id` exists, is `running`, and belongs to another runner
    /// instance sharing this PG cluster.
    ForeignOwner { run_id: Uuid, owner: String },
    /// The database call itself failed.
    Db(String),
}

impl std::fmt::Display for CreateRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateRunError::Terminal { run_id, status } => write!(
                f,
                "run_terminal: orchestration run {run_id} is already {status} and cannot be \
                 re-entered"
            ),
            CreateRunError::ForeignOwner { run_id, owner } => write!(
                f,
                "run_foreign_owner: orchestration run {run_id} is running under runner \
                 instance {owner:?}, not this one"
            ),
            CreateRunError::Db(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for CreateRunError {}

impl From<CreateRunError> for String {
    fn from(e: CreateRunError) -> Self {
        e.to_string()
    }
}

/// How the boot sweep settles a `Working` row whose worker died with the
/// previous process ([`PgDb::settle_lost_worker`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostWorkerSettlement {
    /// The report landed before the process died; only the idle signal was
    /// lost. `Working → Completed`.
    Complete,
    /// No report: `Working → Submitted`, `task_run_id` cleared,
    /// `restart_resets += 1`, so the reconciler dispatches it afresh.
    Resubmit,
    /// The row has already been reset the maximum number of times.
    /// `Working → Failed`.
    Fail,
}

impl PgDb {
    // ========================================================================
    // orchestration.runs / orchestration.subtasks (Phase 1 ledger)
    // ========================================================================

    /// Create an orchestration run, or re-enter the one that already has this
    /// `run_id`, and return the persisted [`Run`].
    ///
    /// Idempotent on `run_id`: `INSERT … ON CONFLICT (run_id) DO NOTHING`, then
    /// the row is read back, so a re-entry returns the stored row rather than
    /// a 23505. The stored row wins — its goal, phases, `config` and owner are
    /// what a re-entered run is driven with, never the arguments of the call
    /// that re-entered it. A re-entry is refused, typed, when the row:
    ///
    /// - has left `running` ([`CreateRunError::Terminal`]) — a finished run's
    ///   verdict is durable and a relaunch would overwrite it;
    /// - is owned by another runner instance ([`CreateRunError::ForeignOwner`])
    ///   — two instances share one embedded cluster, and two reconcilers over
    ///   one DAG would double-dispatch it.
    ///
    /// A `running` row with NO owner (written before `owner_instance` existed)
    /// is CLAIMED for `owner_instance` by an explicit re-entry: an operator
    /// naming the run is the attribution the row lacked. The boot sweep never
    /// takes that path — it skips unowned rows before calling here.
    ///
    /// New rows are always created `running`; `created_at` / `updated_at` are
    /// stamped by the DB defaults.
    pub async fn create_run(
        &self,
        run_id: Uuid,
        goal: &str,
        recipe: Option<&str>,
        phases: &[String],
        config: &OrchestrationRunConfig,
        owner_instance: &str,
    ) -> Result<Run, CreateRunError> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| CreateRunError::Db(format!("PG pool error: {}", e)))?;

        let phases_owned: Vec<String> = phases.to_vec();
        let config_json = serde_json::to_value(config)
            .map_err(|e| CreateRunError::Db(format!("create_run serialize config: {}", e)))?;
        let inserted = conn
            .query_opt(
                &format!(
                    r#"
                    INSERT INTO orchestration.runs
                        (run_id, goal, recipe, phases, status, config, owner_instance)
                    VALUES ($1, $2, $3, $4, 'running', $5, $6)
                    ON CONFLICT (run_id) DO NOTHING
                    RETURNING {RUN_COLUMNS}
                    "#
                ),
                &[
                    &run_id,
                    &goal,
                    &recipe,
                    &phases_owned,
                    &config_json,
                    &owner_instance,
                ],
            )
            .await
            .map_err(|e| CreateRunError::Db(crate::database::pg::pg_err("create_run", &e)))?;
        if let Some(row) = inserted {
            return Ok(Self::run_from_row(&row));
        }

        // The run already exists: re-entry. Claim an unowned `running` row in
        // the same statement that reads it back, so two concurrent re-entries
        // cannot both believe they claimed it.
        let row = conn
            .query_opt(
                &format!(
                    r#"
                    UPDATE orchestration.runs
                    SET owner_instance = CASE WHEN owner_instance IS NULL AND status = 'running'
                                              THEN $2 ELSE owner_instance END
                    WHERE run_id = $1
                    RETURNING {RUN_COLUMNS}
                    "#
                ),
                &[&run_id, &owner_instance],
            )
            .await
            .map_err(|e| {
                CreateRunError::Db(crate::database::pg::pg_err("create_run re-entry", &e))
            })?
            .ok_or_else(|| {
                // Deleted between the conflicting INSERT and this read.
                CreateRunError::Db(format!("create_run: run {run_id} vanished during re-entry"))
            })?;
        let run = Self::run_from_row(&row);
        if run.status != "running" {
            return Err(CreateRunError::Terminal {
                run_id,
                status: run.status,
            });
        }
        match run.owner_instance.as_deref() {
            Some(owner) if owner != owner_instance => Err(CreateRunError::ForeignOwner {
                run_id,
                owner: owner.to_string(),
            }),
            _ => {
                info!(
                    "create_run: run {run_id} already exists and is running — re-entering \
                     it as owner {owner_instance:?}"
                );
                Ok(run)
            }
        }
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
                &format!("SELECT {RUN_COLUMNS} FROM orchestration.runs WHERE run_id = $1"),
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
                &format!("SELECT {RUN_COLUMNS} FROM orchestration.runs ORDER BY created_at DESC"),
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_runs", &e))?;

        Ok(rows.iter().map(Self::run_from_row).collect())
    }

    /// Every run whose durable status is `running`, oldest first — whatever
    /// its owner. The boot sweep's input: it decides per row whether this
    /// instance may relaunch it, so the owner filter is deliberately NOT pushed
    /// into SQL (a foreign or unowned row is logged, not silently invisible).
    pub async fn list_running_runs(&self) -> Result<Vec<Run>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM orchestration.runs \
                     WHERE status = 'running' ORDER BY created_at ASC"
                ),
                &[],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("list_running_runs", &e))?;

        Ok(rows.iter().map(Self::run_from_row).collect())
    }

    /// Boot sweep: settle one `Working` row whose worker died with the previous
    /// runner process. Returns whether the row moved.
    ///
    /// Guarded on the row STILL being `Working` under the SAME `task_run_id`
    /// the sweep judged dead, so it can never clobber a row something else
    /// moved in between. `Resubmit` clears `task_run_id` and bumps
    /// `restart_resets` in SQL, relative to the stored count.
    pub async fn settle_lost_worker(
        &self,
        run_id: Uuid,
        task_id: &str,
        lost_task_run_id: Option<Uuid>,
        settlement: LostWorkerSettlement,
    ) -> Result<bool, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let set = match settlement {
            LostWorkerSettlement::Complete => "state = 'completed'",
            LostWorkerSettlement::Resubmit => {
                "state = 'submitted', task_run_id = NULL, restart_resets = restart_resets + 1"
            }
            LostWorkerSettlement::Fail => "state = 'failed'",
        };
        let n = conn
            .execute(
                &format!(
                    "UPDATE orchestration.subtasks SET {set}, updated_at = now() \
                     WHERE run_id = $1 AND task_id = $2 AND state = 'working' \
                       AND task_run_id IS NOT DISTINCT FROM $3"
                ),
                &[&run_id, &task_id, &lost_task_run_id],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("settle_lost_worker", &e))?;
        Ok(n > 0)
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
                 produced_by, gate_id, gate_status)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                    $15, $16)
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
                       created_at, updated_at, restart_resets
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
        let run_id: Uuid = row.get(0);
        // A stored config that no longer decodes must not make the run
        // unlistable; it reads `None` and the re-entering caller falls back to
        // the config it was handed. `#[serde(default)]` already absorbs a
        // missing field, so this arm is a changed field TYPE.
        let config = row.get::<_, Option<serde_json::Value>>(8).and_then(|v| {
            serde_json::from_value::<OrchestrationRunConfig>(v)
                .map_err(|e| {
                    warn!("run_from_row: run {run_id} has an undecodable config ({e}); ignoring it")
                })
                .ok()
        });
        Run {
            run_id,
            goal: row.get(1),
            recipe: row.get(2),
            phases: row.get(3),
            status: row.get(4),
            status_reason: row.get(5),
            created_at: row.get::<_, DateTime<Utc>>(6),
            updated_at: row.get::<_, DateTime<Utc>>(7),
            config,
            owner_instance: row.get(9),
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
            restart_resets: row.get(18),
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
            restart_resets: 0,
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
                &OrchestrationRunConfig::default(),
                "primary",
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

    /// The stored run config serializes snake_case (like the `Run` it rides
    /// on), omits the per-tick `fanout_bound` cache, and decodes a row written
    /// by an older build that lacks a field by defaulting it — no PG needed.
    #[test]
    fn run_config_serde_round_trip() {
        let cfg = OrchestrationRunConfig {
            concurrency_cap: 9,
            tick_interval_secs: 2,
            stall_after_secs: 600,
            fanout_bound: Some(4),
            ..OrchestrationRunConfig::default()
        };
        let v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(v["concurrency_cap"], 9);
        assert!(
            v.get("fanout_bound").is_none(),
            "the runtime cache is never stored"
        );
        let back: OrchestrationRunConfig = serde_json::from_value(v).unwrap();
        assert_eq!(
            back,
            OrchestrationRunConfig {
                fanout_bound: None,
                ..cfg
            }
        );
        let partial: OrchestrationRunConfig =
            serde_json::from_value(serde_json::json!({"concurrency_cap": 5})).unwrap();
        assert_eq!(
            partial,
            OrchestrationRunConfig {
                concurrency_cap: 5,
                ..OrchestrationRunConfig::default()
            }
        );
    }

    /// `create_run` is idempotent on `run_id`: re-entry returns the STORED row
    /// (goal, config, owner) instead of a 23505, a terminal row refuses, a
    /// foreign owner refuses, and an unowned `running` row is claimed.
    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn create_run_reentry_and_config_round_trip() {
        let pg = PgDb::new_for_test().await;
        let run_id = Uuid::new_v4();
        let me = format!("reentry-test-{}", Uuid::new_v4());
        let phases = vec!["implement".to_string()];
        let cfg = OrchestrationRunConfig {
            concurrency_cap: 6,
            report_timeout_secs: 45,
            fanout_bound: Some(2),
            ..OrchestrationRunConfig::default()
        };

        let first = pg
            .create_run(run_id, "original goal", None, &phases, &cfg, &me)
            .await
            .expect("create");
        assert_eq!(first.status, "running");
        assert_eq!(first.owner_instance.as_deref(), Some(me.as_str()));
        // Config round-trip through the JSONB column (fanout_bound is a
        // runtime cache and is not stored).
        let stored = pg.get_run(run_id).await.unwrap().unwrap();
        assert_eq!(
            stored.config,
            Some(OrchestrationRunConfig {
                fanout_bound: None,
                ..cfg.clone()
            })
        );

        // Re-entry: no 23505, and the stored row wins over the new arguments.
        let again = pg
            .create_run(
                run_id,
                "a different goal",
                None,
                &phases,
                &OrchestrationRunConfig::default(),
                &me,
            )
            .await
            .expect("re-entry returns the row");
        assert_eq!(again.goal, "original goal");
        assert_eq!(again.config.as_ref().map(|c| c.concurrency_cap), Some(6));

        // Another instance may not re-enter it.
        let foreign = pg
            .create_run(run_id, "x", None, &phases, &cfg, "someone-else")
            .await
            .expect_err("foreign owner refused");
        assert!(matches!(foreign, CreateRunError::ForeignOwner { ref owner, .. } if *owner == me));

        // An unowned (pre-column) running row is claimed by explicit re-entry.
        let conn = pg.pool().get().await.expect("conn");
        conn.execute(
            "UPDATE orchestration.runs SET owner_instance = NULL WHERE run_id = $1",
            &[&run_id],
        )
        .await
        .expect("null owner");
        let claimed = pg
            .create_run(run_id, "x", None, &phases, &cfg, "claimer")
            .await
            .expect("claim");
        assert_eq!(claimed.owner_instance.as_deref(), Some("claimer"));

        // A terminal row refuses re-entry, typed, and keeps its verdict.
        pg.set_run_status(run_id, "stalled", Some("stall"))
            .await
            .expect("stall");
        let terminal = pg
            .create_run(run_id, "x", None, &phases, &cfg, "claimer")
            .await
            .expect_err("terminal refused");
        assert_eq!(
            terminal,
            CreateRunError::Terminal {
                run_id,
                status: "stalled".to_string()
            }
        );
        let after = pg.get_run(run_id).await.unwrap().unwrap();
        assert_eq!(after.status, "stalled");
        assert_eq!(after.status_reason.as_deref(), Some("stall"));

        let _ = conn
            .execute(
                "DELETE FROM orchestration.runs WHERE run_id = $1",
                &[&run_id],
            )
            .await;
    }
}
