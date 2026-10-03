//! PostgreSQL store for the operator's review of a session's code changes —
//! `project.session_review_hunks` (which hunks the operator has read) and
//! `project.session_review_notes` (the notes they wrote on hunks, and where
//! each note is in its lifecycle).
//!
//! Plan `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
//! Phase 3. The lifecycle rules live in `mcp::session_review` (the
//! [`ReviewStore`] trait implemented here is the only way they reach these
//! tables); this module only reads and writes rows.
//!
//! ## Schema authority — runner-native, NOT re-homed
//!
//! These tables are runner-authored operational state the runner reads back
//! itself, so — like `orchestration.*` and `project.apps` — they are
//! provisioned by the `CREATE TABLE IF NOT EXISTS` self-heal in
//! [`PgDb::verify_and_provision`] ([`SESSION_REVIEW_DDL`]) and have no alembic
//! revision. They are deliberately NOT in `MACHINE_LOCAL_TABLES_DDL`: that DDL
//! is the inventory of the tables re-homed out of `coord.*`
//! (`REHOMED_MACHINE_LOCAL_TABLES`), which these never were.
//!
//! Hunk keys are opaque: they are computed client-side
//! (`src/components/terminal/sessionReview.ts` `hunkKeysForFile`) and stored
//! verbatim; nothing here recomputes or interprets one.
//!
//! ## Retention
//!
//! Pruned by [`PgDb::prune_session_review_older_than`] on the same daily tick
//! that prunes `project.session_file_snapshots`
//! (`session_file_snapshots::start_session_snapshot_pruner`), with the same
//! retention — a review describes a diff whose pre-edit snapshots expire on
//! that schedule anyway.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use tracing::info;

use super::PgDb;
use crate::mcp::session_review::{
    HunkRef, NoteState, NoteTarget, ReadHunk, ReviewNote, ReviewStore,
};

/// How long review rows are kept: the snapshot retention, for the reason in
/// the module doc.
pub const REVIEW_RETENTION_DAYS: i32 = super::session_file_snapshots::SNAPSHOT_RETENTION_DAYS;

/// The self-heal DDL [`PgDb::verify_and_provision`] runs. One constant so the
/// provisioning site stays a single call and the column list is pinned by a
/// test rather than by inspection.
///
/// `target_kind` / `target_id` record where a note was SENT (`terminal` +
/// terminal id, or `task_run` + task-run id) — the key a `terminal-exit`
/// settles still-unconfirmed notes by, since the review's own `session_id` (a
/// `claude` session id or a task-run id) is not a terminal id. The partial
/// indexes cover the two background reads that run on every observed prompt
/// marker and every terminal exit; both only ever look at `submitted` rows.
pub const SESSION_REVIEW_DDL: &str = "\
CREATE SCHEMA IF NOT EXISTS project; \
CREATE TABLE IF NOT EXISTS project.session_review_hunks ( \
    session_id TEXT NOT NULL, \
    hunk_key   TEXT NOT NULL, \
    file_path  TEXT NOT NULL, \
    read_at    TIMESTAMPTZ NOT NULL DEFAULT now(), \
    PRIMARY KEY (session_id, hunk_key) \
); \
CREATE TABLE IF NOT EXISTS project.session_review_notes ( \
    id           TEXT PRIMARY KEY, \
    session_id   TEXT NOT NULL, \
    file_path    TEXT NOT NULL, \
    hunk_key     TEXT NOT NULL, \
    hunk_header  TEXT NOT NULL, \
    excerpt      TEXT NOT NULL, \
    body         TEXT NOT NULL, \
    state        TEXT NOT NULL CHECK (state IN \
                 ('pending', 'attached', 'submitted', 'confirmed', 'discarded', 'unknown')), \
    marker       TEXT, \
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(), \
    submitted_at TIMESTAMPTZ, \
    confirmed_at TIMESTAMPTZ, \
    target_kind  TEXT, \
    target_id    TEXT \
); \
CREATE INDEX IF NOT EXISTS idx_session_review_notes_session \
    ON project.session_review_notes (session_id, created_at); \
CREATE INDEX IF NOT EXISTS idx_session_review_notes_submitted_marker \
    ON project.session_review_notes (marker) WHERE state = 'submitted'; \
CREATE INDEX IF NOT EXISTS idx_session_review_notes_submitted_target \
    ON project.session_review_notes (target_kind, target_id) WHERE state = 'submitted';";

const NOTE_COLUMNS: &str = "id, session_id, file_path, hunk_key, hunk_header, excerpt, body, \
     state, marker, created_at, submitted_at, confirmed_at, target_kind, target_id";

fn iso(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_iso(field: &str, raw: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| format!("session review: {field} {raw:?} is not RFC 3339: {e}"))
}

fn parse_opt_iso(field: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, String> {
    raw.map(|r| parse_iso(field, r)).transpose()
}

fn col<'a, T: tokio_postgres::types::FromSql<'a>>(
    r: &'a tokio_postgres::Row,
    name: &str,
) -> Result<T, String> {
    r.try_get(name)
        .map_err(|e| format!("session review: column {name}: {e}"))
}

fn note_from_row(r: &tokio_postgres::Row) -> Result<ReviewNote, String> {
    let state_raw: String = col(r, "state")?;
    let state = NoteState::parse(&state_raw)
        .ok_or_else(|| format!("session review: stored state {state_raw:?} is not a note state"))?;
    let target_kind: Option<String> = col(r, "target_kind")?;
    let target_id: Option<String> = col(r, "target_id")?;
    let target = match (target_kind.as_deref(), target_id) {
        (Some(kind), Some(id)) => Some(NoteTarget::from_parts(kind, id).ok_or_else(|| {
            format!("session review: stored target kind {kind:?} is not a target")
        })?),
        _ => None,
    };
    Ok(ReviewNote {
        id: col(r, "id")?,
        session_id: col(r, "session_id")?,
        file_path: col(r, "file_path")?,
        hunk_key: col(r, "hunk_key")?,
        hunk_header: col(r, "hunk_header")?,
        excerpt: col(r, "excerpt")?,
        body: col(r, "body")?,
        state,
        marker: col(r, "marker")?,
        created_at: iso(col(r, "created_at")?),
        submitted_at: col::<Option<DateTime<Utc>>>(r, "submitted_at")?.map(iso),
        confirmed_at: col::<Option<DateTime<Utc>>>(r, "confirmed_at")?.map(iso),
        target,
    })
}

impl PgDb {
    async fn review_conn(&self) -> Result<deadpool_postgres::Object, String> {
        self.pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))
    }

    async fn query_notes(
        &self,
        where_clause: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Vec<ReviewNote>, String> {
        let conn = self.review_conn().await?;
        let rows = conn
            .query(
                &format!(
                    "SELECT {NOTE_COLUMNS} FROM project.session_review_notes \
                     WHERE {where_clause} ORDER BY created_at ASC, id ASC"
                ),
                params,
            )
            .await
            .map_err(|e| super::pg_err("Failed to read session_review_notes", &e))?;
        rows.iter().map(note_from_row).collect()
    }

    /// Delete review rows older than `days`, returning `(hunk rows, note
    /// rows)` removed. Notes age from `created_at`, read marks from `read_at`.
    pub async fn prune_session_review_older_than(
        &self,
        days: i32,
    ) -> Result<(usize, usize), String> {
        if days <= 0 {
            return Err("prune_session_review_older_than: days must be positive".to_string());
        }
        let conn = self.review_conn().await?;
        let days = days.to_string();
        let hunks = conn
            .execute(
                "DELETE FROM project.session_review_hunks \
                 WHERE read_at < NOW() - ($1 || ' days')::interval",
                &[&days],
            )
            .await
            .map_err(|e| super::pg_err("Failed to prune session_review_hunks", &e))?
            as usize;
        let notes = conn
            .execute(
                "DELETE FROM project.session_review_notes \
                 WHERE created_at < NOW() - ($1 || ' days')::interval",
                &[&days],
            )
            .await
            .map_err(|e| super::pg_err("Failed to prune session_review_notes", &e))?
            as usize;
        if hunks > 0 || notes > 0 {
            info!(
                "prune_session_review_older_than({} days): deleted {} read marks, {} notes",
                days, hunks, notes
            );
        }
        Ok((hunks, notes))
    }
}

#[async_trait]
impl ReviewStore for PgDb {
    async fn read_hunks(&self, session_id: &str) -> Result<Vec<ReadHunk>, String> {
        let conn = self.review_conn().await?;
        let rows = conn
            .query(
                "SELECT hunk_key, file_path, read_at FROM project.session_review_hunks \
                 WHERE session_id = $1 ORDER BY read_at ASC, hunk_key ASC",
                &[&session_id],
            )
            .await
            .map_err(|e| super::pg_err("Failed to read session_review_hunks", &e))?;
        rows.iter()
            .map(|r| {
                Ok(ReadHunk {
                    hunk_key: col(r, "hunk_key")?,
                    file_path: col(r, "file_path")?,
                    read_at: iso(col(r, "read_at")?),
                })
            })
            .collect()
    }

    async fn set_hunks_read(
        &self,
        session_id: &str,
        hunks: &[HunkRef],
        read: bool,
    ) -> Result<usize, String> {
        let keys: Vec<&str> = hunks.iter().map(|h| h.hunk_key.as_str()).collect();
        let conn = self.review_conn().await?;
        let changed = if read {
            let paths: Vec<&str> = hunks.iter().map(|h| h.file_path.as_str()).collect();
            // ON CONFLICT DO NOTHING: re-marking a read hunk keeps its first
            // `read_at` and counts as no change — the route is idempotent.
            conn.execute(
                "INSERT INTO project.session_review_hunks (session_id, hunk_key, file_path) \
                 SELECT $1, k, p FROM UNNEST($2::text[], $3::text[]) AS t(k, p) \
                 ON CONFLICT (session_id, hunk_key) DO NOTHING",
                &[&session_id, &keys, &paths],
            )
            .await
            .map_err(|e| super::pg_err("Failed to mark review hunks read", &e))?
        } else {
            conn.execute(
                "DELETE FROM project.session_review_hunks \
                 WHERE session_id = $1 AND hunk_key = ANY($2::text[])",
                &[&session_id, &keys],
            )
            .await
            .map_err(|e| super::pg_err("Failed to mark review hunks unread", &e))?
        };
        Ok(changed as usize)
    }

    async fn list_notes(&self, session_id: &str) -> Result<Vec<ReviewNote>, String> {
        self.query_notes("session_id = $1", &[&session_id]).await
    }

    async fn get_note(&self, note_id: &str) -> Result<Option<ReviewNote>, String> {
        Ok(self
            .query_notes("id = $1", &[&note_id])
            .await?
            .into_iter()
            .next())
    }

    async fn insert_note(&self, note: &ReviewNote) -> Result<(), String> {
        let created_at = parse_iso("createdAt", &note.created_at)?;
        let conn = self.review_conn().await?;
        conn.execute(
            "INSERT INTO project.session_review_notes \
                 (id, session_id, file_path, hunk_key, hunk_header, excerpt, body, state, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            &[
                &note.id,
                &note.session_id,
                &note.file_path,
                &note.hunk_key,
                &note.hunk_header,
                &note.excerpt,
                &note.body,
                &note.state.as_str(),
                &created_at,
            ],
        )
        .await
        .map_err(|e| super::pg_err("Failed to insert session_review_note", &e))?;
        Ok(())
    }

    async fn replace_note_if(
        &self,
        note: &ReviewNote,
        expected: NoteState,
    ) -> Result<bool, String> {
        let submitted_at = parse_opt_iso("submittedAt", note.submitted_at.as_deref())?;
        let confirmed_at = parse_opt_iso("confirmedAt", note.confirmed_at.as_deref())?;
        let target_kind = note.target.as_ref().map(NoteTarget::kind);
        let target_id = note.target.as_ref().map(NoteTarget::id);
        let conn = self.review_conn().await?;
        let updated = conn
            .execute(
                "UPDATE project.session_review_notes \
                 SET body = $3, state = $4, marker = $5, submitted_at = $6, \
                     confirmed_at = $7, target_kind = $8, target_id = $9 \
                 WHERE id = $1 AND state = $2",
                &[
                    &note.id,
                    &expected.as_str(),
                    &note.body,
                    &note.state.as_str(),
                    &note.marker,
                    &submitted_at,
                    &confirmed_at,
                    &target_kind,
                    &target_id,
                ],
            )
            .await
            .map_err(|e| super::pg_err("Failed to update session_review_note", &e))?;
        Ok(updated == 1)
    }

    async fn submitted_notes_with_marker(&self, marker: &str) -> Result<Vec<ReviewNote>, String> {
        self.query_notes("state = 'submitted' AND marker = $1", &[&marker])
            .await
    }

    async fn submitted_notes_for_target(
        &self,
        target: &NoteTarget,
    ) -> Result<Vec<ReviewNote>, String> {
        self.query_notes(
            "state = 'submitted' AND target_kind = $1 AND target_id = $2",
            &[&target.kind(), &target.id()],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DDL provisions both tables with the plan's columns, keyed the way
    /// the store's upserts assume, and allows exactly the six note states.
    #[test]
    fn ddl_provisions_both_tables_with_their_keys_and_state_check() {
        assert!(SESSION_REVIEW_DDL
            .contains("CREATE TABLE IF NOT EXISTS project.session_review_hunks ("));
        assert!(SESSION_REVIEW_DDL
            .contains("CREATE TABLE IF NOT EXISTS project.session_review_notes ("));
        assert!(SESSION_REVIEW_DDL.contains("PRIMARY KEY (session_id, hunk_key)"));
        for state in NoteState::ALL {
            assert!(
                SESSION_REVIEW_DDL.contains(&format!("'{}'", state.as_str())),
                "state CHECK is missing {state:?}"
            );
        }
        for column in NOTE_COLUMNS.split(',').map(str::trim) {
            assert!(
                SESSION_REVIEW_DDL.contains(&format!(" {column} ")),
                "the DDL does not create column {column}, which every note read selects"
            );
        }
    }

    /// Not a re-homed table: the inventory test of `MACHINE_LOCAL_TABLES_DDL`
    /// would fail (or be falsified) if these moved there.
    #[test]
    fn review_tables_are_not_in_the_rehomed_inventory() {
        for table in super::super::REHOMED_MACHINE_LOCAL_TABLES {
            assert!(!table.starts_with("session_review"), "{table}");
        }
    }

    #[test]
    fn timestamps_round_trip_through_the_iso_form() {
        let raw = "2026-10-03T12:34:56.789Z";
        let parsed = parse_iso("createdAt", raw).expect("parse");
        assert_eq!(iso(parsed), raw);
        assert!(parse_iso("createdAt", "yesterday").is_err());
    }
}
