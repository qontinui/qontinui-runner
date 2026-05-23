//! Local-first session event outbox. Plan §D7 — the runner records every
//! lifecycle event LOCALLY first, then [`crate::session::coord_sync`]
//! drains the outbox into coord. If coord is unreachable, sessions still
//! run; the queue replays on reconnect (idempotent by
//! `(machine_id, session_id, seq)`).
//!
//! ## Storage choice (deviation from plan §Phase 2)
//!
//! The plan calls out a SQLite outbox at
//! `~/.qontinui/runner/session-outbox.sqlite`. The runner has no existing
//! SQLite infrastructure (all durable state lives in coord Postgres via
//! `src-tauri/src/database/pg/`), and adding `rusqlite` for an outbox we
//! will read sequentially anyway introduces a new native dep, a new build
//! flag, and a migration surface. We mirror the existing precedent set by
//! `wrappers/registry.rs`: an append-only JSON-lines file at
//! `<APP_DATA>/qontinui-runner/session-outbox.jsonl` with one record per
//! line and an atomic "compact" pass that drops acked rows in bulk.
//!
//! Semantics — by design:
//! - **Monotonic seq per `(machine_id, session_id)`** — enforced by
//!   [`OutboxWriter::next_seq`], which scans the live records on open and
//!   continues from `max + 1`.
//! - **Idempotent replay** — coord-side uniqueness is `UNIQUE
//!   (session_id, seq)`, so re-pushing the same record is a no-op there.
//! - **Append-only writes** — every [`OutboxWriter::record`] call fsyncs a
//!   single line; the file never gets a partial row.
//! - **Compact on read** — the drainer calls [`OutboxWriter::compact`] after
//!   each ack batch to drop acked records; until compact runs the file
//!   just grows.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::warn;
use uuid::Uuid;

use super::SessionEventKind;

/// A single outbox record. Persisted as one JSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxRecord {
    /// Stable machine identity (matches `coord.devices.id`).
    pub machine_id: Uuid,
    /// Owning session.
    pub session_id: Uuid,
    /// Per-(machine_id, session_id) monotonic counter. Acts as the
    /// idempotency key on the coord side via the
    /// `UNIQUE (session_id, seq)` constraint on `coord.session_events`.
    pub seq: i64,
    /// Wire-form event kind. Matches [`SessionEventKind::as_str`].
    pub event_kind: String,
    /// Free-form payload — passed through to coord verbatim.
    pub payload: JsonValue,
    /// Local timestamp at write time. Coord re-stamps `occurred_at` on
    /// receipt; this field is the runner's view for replay/debug.
    pub recorded_at: DateTime<Utc>,
    /// `Some(timestamp)` once the row has been ACKed by coord (so a future
    /// [`OutboxWriter::compact`] can drop it).
    #[serde(default)]
    pub acked_at: Option<DateTime<Utc>>,
}

/// Append-only outbox backed by a single JSON-lines file.
///
/// We deliberately do NOT cache an open file handle. `ack`/`compact`
/// rewrite the file via temp-file + atomic rename (see [`rewrite_all`]),
/// which orphans any long-lived append handle — on Unix the rename unlinks
/// the old inode, so a cached handle keeps appending to a deleted file and
/// every subsequent `record` write is silently lost. Instead, `record`
/// opens a fresh append handle each call and fsyncs it, while `write_lock`
/// serializes all file operations (append, rewrite, read) so a `record`
/// can never interleave with an in-flight `ack` rename.
#[derive(Debug)]
pub struct OutboxWriter {
    path: PathBuf,
    next_seqs: Mutex<HashMap<(Uuid, Uuid), i64>>,
    /// Serializes every file operation. Holds no handle by design (see the
    /// struct-level note on the rename-orphan hazard).
    write_lock: Mutex<()>,
}

impl OutboxWriter {
    /// Open (or create) the outbox at the given path. On open, scans
    /// existing records to populate the per-`(machine_id, session_id)`
    /// next-seq map.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let next_seqs = scan_next_seqs(&path)?;

        // Ensure the file exists so a brand-new outbox has a readable path;
        // we drop the handle immediately — `record` reopens per write.
        OpenOptions::new().append(true).create(true).open(&path)?;

        Ok(Self {
            path,
            next_seqs: Mutex::new(next_seqs),
            write_lock: Mutex::new(()),
        })
    }

    /// Record a new event for `(machine_id, session_id)`. Allocates the
    /// next monotonic seq, fsyncs the JSON-line, returns the persisted
    /// record (with its allocated seq).
    pub fn record(
        &self,
        machine_id: Uuid,
        session_id: Uuid,
        event_kind: SessionEventKind,
        payload: JsonValue,
    ) -> std::io::Result<OutboxRecord> {
        let seq = {
            let mut map = self.next_seqs.lock().expect("outbox seqs poisoned");
            let entry = map.entry((machine_id, session_id)).or_insert(0);
            *entry += 1;
            *entry
        };

        let rec = OutboxRecord {
            machine_id,
            session_id,
            seq,
            event_kind: event_kind.as_str().to_string(),
            payload,
            recorded_at: Utc::now(),
            acked_at: None,
        };

        let mut line = serde_json::to_string(&rec).map_err(std::io::Error::other)?;
        line.push('\n');

        // Open a fresh append handle under the lock so the write always
        // lands in the current file even after an `ack`/`compact` rename.
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.sync_data()?;

        Ok(rec)
    }

    /// Return all not-yet-acked records, in seq order per session. Used
    /// by [`crate::session::coord_sync`] to drive the push loop.
    pub fn pending(&self) -> std::io::Result<Vec<OutboxRecord>> {
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        let mut pending: Vec<OutboxRecord> = read_all(&self.path)?
            .into_iter()
            .filter(|r| r.acked_at.is_none())
            .collect();
        pending.sort_by_key(|r| (r.session_id, r.seq));
        Ok(pending)
    }

    /// Mark records as acked. Calls [`OutboxWriter::compact`] once the
    /// batch is applied so the file doesn't grow unbounded across many
    /// successful pushes.
    pub fn ack(&self, acks: &[(Uuid, i64)]) -> std::io::Result<()> {
        if acks.is_empty() {
            return Ok(());
        }
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");

        let mut all = read_all(&self.path)?;
        let now = Utc::now();
        let ack_set: std::collections::HashSet<(Uuid, i64)> = acks.iter().copied().collect();
        for rec in &mut all {
            if ack_set.contains(&(rec.session_id, rec.seq)) && rec.acked_at.is_none() {
                rec.acked_at = Some(now);
            }
        }

        rewrite_all(&self.path, &all)
    }

    /// Drop already-acked records. Called by [`OutboxWriter::ack`] on
    /// every batch and available as a manual sweep for cron-style
    /// callers.
    pub fn compact(&self) -> std::io::Result<()> {
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        let all = read_all(&self.path)?;
        let kept: Vec<_> = all.into_iter().filter(|r| r.acked_at.is_none()).collect();
        rewrite_all(&self.path, &kept)
    }
}

fn scan_next_seqs(path: &Path) -> std::io::Result<HashMap<(Uuid, Uuid), i64>> {
    let mut map: HashMap<(Uuid, Uuid), i64> = HashMap::new();
    if !path.exists() {
        return Ok(map);
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for (idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                warn!("outbox: line {} unreadable ({}); stopping scan", idx, e);
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec: OutboxRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    "outbox: line {} unparseable ({}); skipping (replay safe)",
                    idx, e
                );
                continue;
            }
        };
        let entry = map.entry((rec.machine_id, rec.session_id)).or_insert(0);
        if rec.seq > *entry {
            *entry = rec.seq;
        }
    }
    Ok(map)
}

fn read_all(path: &Path) -> std::io::Result<Vec<OutboxRecord>> {
    let mut out = Vec::new();
    if !path.exists() {
        return Ok(out);
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<OutboxRecord>(line) {
            Ok(rec) => out.push(rec),
            Err(e) => warn!("outbox: skipping unparseable record: {}", e),
        }
    }
    Ok(out)
}

fn rewrite_all(path: &Path, records: &[OutboxRecord]) -> std::io::Result<()> {
    // Write to a temp file then rename for atomicity. Mirrors the pattern
    // used elsewhere in the runner (see `wrappers/registry.rs` index
    // rewrites + `fs_atomic.rs`).
    let tmp = path.with_extension("jsonl.tmp");
    {
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        let mut w = BufWriter::new(f);
        for rec in records {
            let mut line = serde_json::to_string(rec).map_err(std::io::Error::other)?;
            line.push('\n');
            w.write_all(line.as_bytes())?;
        }
        w.flush()?;
        w.get_ref().sync_data()?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn record_assigns_monotonic_seq_per_session() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();

        let r1 = outbox
            .record(m, s1, SessionEventKind::Started, json!({}))
            .unwrap();
        let r2 = outbox
            .record(m, s1, SessionEventKind::Heartbeat, json!({}))
            .unwrap();
        let r3 = outbox
            .record(m, s2, SessionEventKind::Started, json!({}))
            .unwrap();
        let r4 = outbox
            .record(m, s1, SessionEventKind::Closed, json!({}))
            .unwrap();

        assert_eq!(r1.seq, 1);
        assert_eq!(r2.seq, 2);
        assert_eq!(r3.seq, 1);
        assert_eq!(r4.seq, 3);
    }

    #[test]
    fn pending_returns_unacked_records_in_seq_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        outbox
            .record(m, s, SessionEventKind::Started, json!({}))
            .unwrap();
        outbox
            .record(m, s, SessionEventKind::Heartbeat, json!({}))
            .unwrap();
        outbox
            .record(m, s, SessionEventKind::Closed, json!({}))
            .unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].seq, 1);
        assert_eq!(pending[1].seq, 2);
        assert_eq!(pending[2].seq, 3);
    }

    #[test]
    fn ack_drops_records_on_compact() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let r1 = outbox
            .record(m, s, SessionEventKind::Started, json!({}))
            .unwrap();
        let r2 = outbox
            .record(m, s, SessionEventKind::Heartbeat, json!({}))
            .unwrap();
        outbox
            .record(m, s, SessionEventKind::Closed, json!({}))
            .unwrap();

        outbox
            .ack(&[(r1.session_id, r1.seq), (r2.session_id, r2.seq)])
            .unwrap();

        // ack() compacts implicitly via rewrite_all; pending() drops the
        // acked rows.
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 3);
    }

    #[test]
    fn record_after_ack_survives_rewrite() {
        // Regression: `ack` rewrites the file via temp-file + atomic rename.
        // A cached append handle would be orphaned by the rename and silently
        // drop every later `record`. Opening a fresh handle per write keeps
        // the post-ack record landing in the live file.
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let r1 = outbox
            .record(m, s, SessionEventKind::Started, json!({}))
            .unwrap();
        outbox.ack(&[(r1.session_id, r1.seq)]).unwrap();

        // This append happens AFTER the ack's rewrite/rename.
        let r2 = outbox
            .record(m, s, SessionEventKind::Heartbeat, json!({}))
            .unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "post-ack record must survive the rewrite");
        assert_eq!(pending[0].seq, r2.seq);
        assert_eq!(pending[0].event_kind, "heartbeat");
    }

    #[test]
    fn reopen_continues_seq_from_max() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        {
            let outbox = OutboxWriter::open(&path).unwrap();
            outbox
                .record(m, s, SessionEventKind::Started, json!({}))
                .unwrap();
            outbox
                .record(m, s, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
        }

        let outbox = OutboxWriter::open(&path).unwrap();
        let r = outbox
            .record(m, s, SessionEventKind::Closed, json!({}))
            .unwrap();
        assert_eq!(r.seq, 3);
    }
}
