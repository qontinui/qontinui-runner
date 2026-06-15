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
use crate::orchestration_loop::ledger::{Run, Subtask, SubtaskState};
use chrono::{DateTime, Utc};
use tokio_postgres::Row;
use uuid::Uuid;

impl PgDb {
    // ========================================================================
    // orchestration.runs / orchestration.subtasks (Phase 1 ledger)
    // ========================================================================

    /// Insert a new orchestration run and return the persisted [`Run`].
    ///
    /// `created_at` / `updated_at` are stamped by the DB defaults.
    pub async fn create_run(
        &self,
        run_id: Uuid,
        goal: &str,
        recipe: Option<&str>,
        phases: &[String],
        status: &str,
    ) -> Result<Run, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let phases_owned: Vec<String> = phases.to_vec();
        let row = conn
            .query_one(
                r#"
                INSERT INTO orchestration.runs
                    (run_id, goal, recipe, phases, status)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING run_id, goal, recipe, phases, status, created_at, updated_at
                "#,
                &[&run_id, &goal, &recipe, &phases_owned, &status],
            )
            .await
            .map_err(|e| format!("create_run: {}", e))?;

        Ok(Self::run_from_row(&row))
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
                SELECT run_id, goal, recipe, phases, status, created_at, updated_at
                FROM orchestration.runs
                WHERE run_id = $1
                "#,
                &[&run_id],
            )
            .await
            .map_err(|e| format!("get_run: {}", e))?;

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
                SELECT run_id, goal, recipe, phases, status, created_at, updated_at
                FROM orchestration.runs
                ORDER BY created_at DESC
                "#,
                &[],
            )
            .await
            .map_err(|e| format!("list_runs: {}", e))?;

        Ok(rows.iter().map(Self::run_from_row).collect())
    }

    /// Update a run's `status` (e.g. `running` → `complete`/`failed`/`stopped`).
    /// Returns `Err` if no row matched `run_id`.
    pub async fn set_run_status(&self, run_id: Uuid, status: &str) -> Result<(), String> {
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
                    updated_at = now()
                WHERE run_id = $1
                "#,
                &[&run_id, &status],
            )
            .await
            .map_err(|e| format!("set_run_status: {}", e))?;

        if n == 0 {
            return Err(format!("set_run_status: no run {}", run_id));
        }
        Ok(())
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
        .map_err(|e| format!("upsert_subtask: {}", e))?;

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
                       created_at, updated_at
                FROM orchestration.subtasks
                WHERE run_id = $1
                ORDER BY idx ASC, task_id ASC
                "#,
                &[&run_id],
            )
            .await
            .map_err(|e| format!("list_subtasks: {}", e))?;

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
            .map_err(|e| format!("set_subtask_state: {}", e))?;

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
            .map_err(|e| format!("set_subtask_gate: {}", e))?;

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
            .map_err(|e| format!("write_subtask_artifact: {}", e))?;

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
                .map_err(|e| format!("splice_subtasks: {}", e))?;
            inserted += n;
        }

        Ok(inserted)
    }

    // -- row decoders --

    fn run_from_row(row: &Row) -> Run {
        Run {
            run_id: row.get(0),
            goal: row.get(1),
            recipe: row.get(2),
            phases: row.get(3),
            status: row.get(4),
            created_at: row.get::<_, DateTime<Utc>>(5),
            updated_at: row.get::<_, DateTime<Utc>>(6),
        }
    }

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
        let pg = PgDb::new_blocking_for_test();
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
}
