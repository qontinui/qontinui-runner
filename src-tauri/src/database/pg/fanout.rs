//! PostgreSQL ledger for the fan-out dispatcher — `project.fanout_runs` and
//! `project.fanout_members` (plan
//! `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
//! Phase 6).
//!
//! ## Schema authority — the runner authors these tables
//!
//! Runner-native operational state the runner reads back itself, so it is
//! provisioned by [`FANOUT_TABLES_DDL`] from `PgDb::verify_and_provision`
//! (`CREATE TABLE IF NOT EXISTS` + `ADD COLUMN IF NOT EXISTS`, the
//! `project.apps` / `orchestration.*` idiom) and by no alembic revision. It is
//! deliberately NOT part of `MACHINE_LOCAL_TABLES_DDL`: that DDL is the
//! inventory of tables re-homed out of `coord.*`, and a test pins it to that
//! list.
//!
//! `owner_instance` is not in the plan's column list and is load-bearing: a
//! temp runner and the primary share one embedded PG cluster, and without it
//! the first instance to boot would reconcile — and admit — the other's runs.
//! The member position column is `idx` (as in `orchestration.subtasks`), since
//! `index` reads as the keyword everywhere it appears in SQL.

use super::PgDb;
use crate::fanout::dispatcher::FanoutStore;
use crate::fanout::model::{ConfigDirPolicy, FanoutMember, FanoutRun, MemberState, RunState};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_postgres::Row;
use uuid::Uuid;

/// Idempotent self-provision DDL for the fan-out ledger.
pub(crate) const FANOUT_TABLES_DDL: &str = "\
CREATE SCHEMA IF NOT EXISTS project; \
CREATE TABLE IF NOT EXISTS project.fanout_runs ( \
    id                UUID PRIMARY KEY, \
    tenant_id         UUID, \
    template_slug     TEXT, \
    template_version  INTEGER, \
    max_concurrent    INTEGER NOT NULL, \
    config_dir_policy JSONB NOT NULL, \
    working_dir       TEXT NOT NULL, \
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(), \
    state             TEXT NOT NULL \
); \
ALTER TABLE project.fanout_runs ADD COLUMN IF NOT EXISTS owner_instance TEXT NOT NULL DEFAULT 'primary'; \
CREATE TABLE IF NOT EXISTS project.fanout_members ( \
    run_id            UUID NOT NULL REFERENCES project.fanout_runs(id) ON DELETE CASCADE, \
    idx               INTEGER NOT NULL, \
    title             TEXT NOT NULL, \
    prompt            TEXT NOT NULL, \
    state             TEXT NOT NULL, \
    terminal_id       TEXT, \
    claude_session_id TEXT, \
    reason            TEXT, \
    admitted_at       TIMESTAMPTZ, \
    released_at       TIMESTAMPTZ, \
    PRIMARY KEY (run_id, idx) \
); \
CREATE INDEX IF NOT EXISTS idx_fanout_runs_owner_state \
    ON project.fanout_runs (owner_instance, state);";

const RUN_COLUMNS: &str = "id, tenant_id, template_slug, template_version, max_concurrent, \
     config_dir_policy, working_dir, created_at, state, owner_instance";

const MEMBER_COLUMNS: &str = "run_id, idx, title, prompt, state, terminal_id, claude_session_id, \
     reason, admitted_at, released_at";

fn col<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a Row,
    idx: usize,
    name: &str,
) -> Result<T, String> {
    row.try_get(idx)
        .map_err(|e| format!("fanout row: column {name}: {e}"))
}

fn as_i32(n: u32, what: &str) -> Result<i32, String> {
    i32::try_from(n).map_err(|_| format!("fanout: {what} {n} does not fit INTEGER"))
}

fn run_from_row(row: &Row) -> Result<FanoutRun, String> {
    let max: i32 = col(row, 4, "max_concurrent")?;
    let policy: serde_json::Value = col(row, 5, "config_dir_policy")?;
    let state: String = col(row, 8, "state")?;
    Ok(FanoutRun {
        id: col(row, 0, "id")?,
        tenant_id: col(row, 1, "tenant_id")?,
        template_slug: col(row, 2, "template_slug")?,
        template_version: col(row, 3, "template_version")?,
        max_concurrent: u32::try_from(max)
            .map_err(|_| format!("fanout row: max_concurrent {max} is negative"))?,
        config_dir_policy: serde_json::from_value::<ConfigDirPolicy>(policy)
            .map_err(|e| format!("fanout row: config_dir_policy: {e}"))?,
        working_dir: col(row, 6, "working_dir")?,
        created_at: col::<DateTime<Utc>>(row, 7, "created_at")?,
        state: RunState::parse(&state)
            .ok_or_else(|| format!("fanout row: unknown run state {state:?}"))?,
        owner_instance: col(row, 9, "owner_instance")?,
    })
}

fn member_from_row(row: &Row) -> Result<(Uuid, FanoutMember), String> {
    let idx: i32 = col(row, 1, "idx")?;
    let state: String = col(row, 4, "state")?;
    Ok((
        col(row, 0, "run_id")?,
        FanoutMember {
            index: u32::try_from(idx).map_err(|_| format!("fanout row: idx {idx} is negative"))?,
            title: col(row, 2, "title")?,
            prompt: col(row, 3, "prompt")?,
            state: MemberState::parse(&state)
                .ok_or_else(|| format!("fanout row: unknown member state {state:?}"))?,
            terminal_id: col(row, 5, "terminal_id")?,
            claude_session_id: col(row, 6, "claude_session_id")?,
            reason: col(row, 7, "reason")?,
            admitted_at: col(row, 8, "admitted_at")?,
            released_at: col(row, 9, "released_at")?,
        },
    ))
}

impl PgDb {
    /// Insert a run and its members in one transaction.
    pub(crate) async fn insert_fanout_run(
        &self,
        run: &FanoutRun,
        members: &[FanoutMember],
    ) -> Result<(), String> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;
        let tx = conn
            .transaction()
            .await
            .map_err(|e| crate::database::pg::pg_err("insert_fanout_run begin", &e))?;
        let policy = serde_json::to_value(&run.config_dir_policy)
            .map_err(|e| format!("insert_fanout_run: config_dir_policy: {e}"))?;
        let max = as_i32(run.max_concurrent, "max_concurrent")?;
        tx.execute(
            &*format!(
                "INSERT INTO project.fanout_runs ({RUN_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
            ),
            &[
                &run.id,
                &run.tenant_id,
                &run.template_slug,
                &run.template_version,
                &max,
                &policy,
                &run.working_dir,
                &run.created_at,
                &run.state.as_str(),
                &run.owner_instance,
            ],
        )
        .await
        .map_err(|e| crate::database::pg::pg_err("insert_fanout_run run", &e))?;
        for m in members {
            let idx = as_i32(m.index, "member index")?;
            tx.execute(
                &*format!(
                    "INSERT INTO project.fanout_members ({MEMBER_COLUMNS}) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
                ),
                &[
                    &run.id,
                    &idx,
                    &m.title,
                    &m.prompt,
                    &m.state.as_str(),
                    &m.terminal_id,
                    &m.claude_session_id,
                    &m.reason,
                    &m.admitted_at,
                    &m.released_at,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("insert_fanout_run member", &e))?;
        }
        tx.commit()
            .await
            .map_err(|e| crate::database::pg::pg_err("insert_fanout_run commit", &e))
    }

    /// Persist `max_concurrent` and `state`.
    pub(crate) async fn update_fanout_run(&self, run: &FanoutRun) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;
        let max = as_i32(run.max_concurrent, "max_concurrent")?;
        let n = conn
            .execute(
                "UPDATE project.fanout_runs SET max_concurrent = $2, state = $3 WHERE id = $1",
                &[&run.id, &max, &run.state.as_str()],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("update_fanout_run", &e))?;
        if n == 1 {
            Ok(())
        } else {
            Err(format!("update_fanout_run: no run {}", run.id))
        }
    }

    /// Persist one member's mutable fields.
    pub(crate) async fn update_fanout_member(
        &self,
        run_id: Uuid,
        m: &FanoutMember,
    ) -> Result<(), String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;
        let idx = as_i32(m.index, "member index")?;
        let n = conn
            .execute(
                "UPDATE project.fanout_members SET state = $3, terminal_id = $4, \
                 claude_session_id = $5, reason = $6, admitted_at = $7, released_at = $8 \
                 WHERE run_id = $1 AND idx = $2",
                &[
                    &run_id,
                    &idx,
                    &m.state.as_str(),
                    &m.terminal_id,
                    &m.claude_session_id,
                    &m.reason,
                    &m.admitted_at,
                    &m.released_at,
                ],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("update_fanout_member", &e))?;
        if n == 1 {
            Ok(())
        } else {
            Err(format!("update_fanout_member: no member {run_id}/{}", m.index))
        }
    }

    /// Every `active` run `owner_instance` owns, with its members in index
    /// order. A row that does not decode fails the whole load: a run read with
    /// a member missing would be admitted short.
    pub(crate) async fn load_active_fanout_runs(
        &self,
        owner_instance: &str,
    ) -> Result<Vec<(FanoutRun, Vec<FanoutMember>)>, String> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| format!("PG pool error: {e}"))?;
        let run_rows = conn
            .query(
                &*format!(
                    "SELECT {RUN_COLUMNS} FROM project.fanout_runs \
                     WHERE owner_instance = $1 AND state = 'active' ORDER BY created_at"
                ),
                &[&owner_instance],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("load_active_fanout_runs runs", &e))?;
        let runs: Vec<FanoutRun> = run_rows
            .iter()
            .map(run_from_row)
            .collect::<Result<_, _>>()?;
        let ids: Vec<Uuid> = runs.iter().map(|r| r.id).collect();
        let member_rows = conn
            .query(
                &*format!(
                    "SELECT {MEMBER_COLUMNS} FROM project.fanout_members \
                     WHERE run_id = ANY($1) ORDER BY run_id, idx"
                ),
                &[&ids],
            )
            .await
            .map_err(|e| crate::database::pg::pg_err("load_active_fanout_runs members", &e))?;
        let mut out: Vec<(FanoutRun, Vec<FanoutMember>)> =
            runs.into_iter().map(|r| (r, Vec::new())).collect();
        for row in &member_rows {
            let (run_id, member) = member_from_row(row)?;
            if let Some((_, members)) = out.iter_mut().find(|(r, _)| r.id == run_id) {
                members.push(member);
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl FanoutStore for PgDb {
    async fn insert_run(&self, run: &FanoutRun, members: &[FanoutMember]) -> Result<(), String> {
        self.insert_fanout_run(run, members).await
    }

    async fn update_run(&self, run: &FanoutRun) -> Result<(), String> {
        self.update_fanout_run(run).await
    }

    async fn update_member(&self, run_id: Uuid, member: &FanoutMember) -> Result<(), String> {
        self.update_fanout_member(run_id, member).await
    }

    async fn load_active_runs(
        &self,
        owner_instance: &str,
    ) -> Result<Vec<(FanoutRun, Vec<FanoutMember>)>, String> {
        self.load_active_fanout_runs(owner_instance).await
    }
}

#[cfg(test)]
mod tests {
    use super::FANOUT_TABLES_DDL;

    /// The self-heal is re-run on every boot and every degraded-mode reconnect,
    /// so every statement must be the idempotent form.
    #[test]
    fn fanout_ddl_is_idempotent() {
        for stmt in FANOUT_TABLES_DDL
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            assert!(
                stmt.starts_with("CREATE SCHEMA IF NOT EXISTS")
                    || stmt.starts_with("CREATE TABLE IF NOT EXISTS")
                    || stmt.starts_with("CREATE INDEX IF NOT EXISTS")
                    || (stmt.starts_with("ALTER TABLE") && stmt.contains("ADD COLUMN IF NOT EXISTS")),
                "non-idempotent statement: {stmt}"
            );
        }
    }

    /// Runner-native, never a re-homed coord table.
    #[test]
    fn fanout_tables_are_not_in_the_rehomed_inventory() {
        for t in ["fanout_runs", "fanout_members"] {
            assert!(!crate::database::pg::REHOMED_MACHINE_LOCAL_TABLES.contains(&t));
            assert!(!crate::database::pg::MACHINE_LOCAL_TABLES_DDL.contains(t));
            assert!(FANOUT_TABLES_DDL.contains(&*format!("project.{t}")));
        }
        assert!(!FANOUT_TABLES_DDL.contains("coord."));
    }
}
