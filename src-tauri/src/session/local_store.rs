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
//!   each ack batch to drop acked records.
//! - **Self-bounding at the writer** — compaction alone is not a bound: it
//!   only runs *after an ack batch*, so an outbox whose drainer never acks
//!   (coord unreachable, cloud-sync disabled, the drain loop never started)
//!   grows without limit. Observed in the field at 355 MB on a box with a
//!   documented disk-full history. [`OutboxWriter::record`] therefore enforces
//!   [`DEFAULT_MAX_BYTES`] itself, dropping acked rows first and then the
//!   OLDEST-unacked rows, counted in [`OutboxWriter::dropped_unacked`] and
//!   warned — never silently. A durability buffer whose only bound is
//!   "someone else drains me" has no bound at all.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tracing::warn;
use uuid::Uuid;

use super::SessionEventKind;

/// Hard cap on the on-disk outbox, enforced by [`OutboxWriter::record`] itself
/// so the file self-bounds when the drainer never acks.
///
/// Sized as a durability buffer, not an archive: the drain loop pushes every
/// few seconds when coord is reachable, so exceeding this means the outbox has
/// been undrained for a long time and the oldest rows are the least likely to
/// still matter. 64 MiB is ~3 orders of magnitude of headroom over a healthy
/// steady state while staying far below the disk-pressure threshold that has
/// bitten this box before (`os error 112`).
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// After a cap-triggered trim, retain at most 4/5 of the cap so a bounded
/// outbox does not re-trim on every single subsequent `record` call (each trim
/// is a full read + rewrite).
fn trim_target(max_bytes: u64) -> u64 {
    max_bytes / 5 * 4
}

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
    /// Hard byte cap enforced by `record`. See [`DEFAULT_MAX_BYTES`].
    max_bytes: u64,
    /// Count of never-acked records this writer has dropped to stay under
    /// `max_bytes`. Non-zero means real event loss — surfaced via
    /// [`OutboxWriter::dropped_unacked`] and a `warn!` per trim.
    dropped_unacked: AtomicU64,
}

impl OutboxWriter {
    /// Open (or create) the outbox at the given path, bounded by
    /// [`DEFAULT_MAX_BYTES`]. On open, scans existing records to populate the
    /// per-`(machine_id, session_id)` next-seq map.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::open_with_max_bytes(path, DEFAULT_MAX_BYTES)
    }

    /// [`OutboxWriter::open`] with an explicit byte cap. Exists so tests can
    /// drive the trim path without writing 64 MiB.
    pub fn open_with_max_bytes(path: impl AsRef<Path>, max_bytes: u64) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let next_seqs = scan_next_seqs(&path)?;

        // Ensure the file exists so a brand-new outbox has a readable path;
        // we drop the handle immediately — `record` reopens per write.
        OpenOptions::new().append(true).create(true).open(&path)?;

        let writer = Self {
            path,
            next_seqs: Mutex::new(next_seqs),
            write_lock: Mutex::new(()),
            max_bytes,
            dropped_unacked: AtomicU64::new(0),
        };
        // An outbox that grew past the cap under an OLDER build (or while this
        // one was not running) must come back under it at open, not only on the
        // next write — a runner that boots and never records would otherwise
        // keep the oversize file forever.
        {
            let _guard = writer.write_lock.lock().expect("outbox lock poisoned");
            writer.enforce_cap_locked()?;
        }
        Ok(writer)
    }

    /// How many never-acked records this writer has dropped to stay under the
    /// cap. `0` on a healthy (drained) outbox.
    pub fn dropped_unacked(&self) -> u64 {
        self.dropped_unacked.load(Ordering::Relaxed)
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
        drop(file);

        // Bound the file HERE rather than relying on the drainer's post-ack
        // `compact` — see the module note. A trim failure must not fail the
        // write: the record is already durable, and losing the event to keep
        // the file small would invert the trade-off.
        if let Err(e) = self.enforce_cap_locked() {
            warn!(
                "outbox: cap enforcement failed ({}); file may be oversize",
                e
            );
        }

        Ok(rec)
    }

    /// Bring the file back under `max_bytes`, dropping ACKED records first
    /// (already delivered — free) and then the OLDEST unacked records
    /// (chronological append order makes the front the oldest).
    ///
    /// Caller MUST hold `write_lock`. The newest record is never dropped, so a
    /// single record larger than the trim target leaves the file marginally
    /// over cap rather than emptying it.
    fn enforce_cap_locked(&self) -> std::io::Result<()> {
        let size = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            // No file yet / unreadable — nothing to bound.
            Err(_) => return Ok(()),
        };
        if size <= self.max_bytes {
            return Ok(());
        }

        let all = read_all(&self.path)?;
        let total_before = all.len();
        // Phase 1 — drop acked rows (seq-floor preserving). On a drained outbox
        // this alone gets us back under the cap with ZERO event loss.
        let kept = drop_acked_keeping_seq_floor(all);
        let acked_dropped = total_before - kept.len();

        // Phase 2 — still over? Drop the OLDEST UNACKED rows until under the
        // target. Acked rows surviving phase 1 are seq-floor tombstones and are
        // skipped here: dropping one would reset the next-seq scan on reopen.
        let sizes: Vec<u64> = kept.iter().map(serialized_len).collect();
        let mut live: u64 = sizes.iter().sum();
        let target = trim_target(self.max_bytes);
        // Never drop the newest event — a single record bigger than the target
        // leaves the file marginally over cap rather than emptying it.
        let newest_unacked = kept.iter().rposition(|r| r.acked_at.is_none());
        let mut drop_flags = vec![false; kept.len()];
        let mut dropped_unacked = 0usize;
        for (i, rec) in kept.iter().enumerate() {
            if live <= target {
                break;
            }
            if rec.acked_at.is_some() || Some(i) == newest_unacked {
                continue;
            }
            drop_flags[i] = true;
            live -= sizes[i];
            dropped_unacked += 1;
        }

        if dropped_unacked > 0 {
            let total = self
                .dropped_unacked
                .fetch_add(dropped_unacked as u64, Ordering::Relaxed)
                + dropped_unacked as u64;
            warn!(
                "outbox: {} at {} bytes exceeds the {} byte cap and its backlog is UNACKED \
                 — DROPPED {} oldest unacked records (these events are lost; {} dropped total). \
                 Check coord reachability / the coord_sync drain loop.",
                self.path.display(),
                size,
                self.max_bytes,
                dropped_unacked,
                total
            );
        } else if acked_dropped > 0 {
            warn!(
                "outbox: {} at {} bytes exceeded the {} byte cap; compacted {} acked records",
                self.path.display(),
                size,
                self.max_bytes,
                acked_dropped
            );
        }

        if acked_dropped > 0 || dropped_unacked > 0 {
            let final_kept: Vec<OutboxRecord> = kept
                .into_iter()
                .zip(drop_flags)
                .filter(|(_, dropped)| !dropped)
                .map(|(r, _)| r)
                .collect();
            rewrite_all(&self.path, &final_kept)?;
        }
        Ok(())
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

    /// Mark records as acked and drop them in the same rewrite, so the file
    /// does not grow unbounded across many successful pushes.
    ///
    /// **This used to only STAMP `acked_at` and keep the row.** Its doc claimed
    /// it compacted, the module contract said the drainer would call
    /// [`OutboxWriter::compact`] after each ack batch — and nothing ever did
    /// (`compact` had zero callers). So every delivered event stayed on disk
    /// forever: the primary's outbox was measured at 355 MB / 1,137,939 records
    /// of which 1,137,912 were already acked. Dropping on ack is what the
    /// contract always promised; [`drop_acked_keeping_seq_floor`] keeps it safe
    /// against the next-seq rewind.
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

        rewrite_all(&self.path, &drop_acked_keeping_seq_floor(all))
    }

    /// Drop already-acked records. [`OutboxWriter::ack`] now compacts inline,
    /// so this remains as a manual sweep for cron-style callers.
    pub fn compact(&self) -> std::io::Result<()> {
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        let all = read_all(&self.path)?;
        rewrite_all(&self.path, &drop_acked_keeping_seq_floor(all))
    }
}

/// Serialized on-disk size of one record, including its newline.
fn serialized_len(rec: &OutboxRecord) -> u64 {
    match serde_json::to_string(rec) {
        Ok(s) => s.len() as u64 + 1,
        Err(_) => 0,
    }
}

/// Drop acked records while preserving each `(machine_id, session_id)`'s seq
/// high-water mark.
///
/// **Why the high-water matters.** [`scan_next_seqs`] rebuilds the next-seq map
/// on open by taking `max(seq)` of the records still IN the file. Naively
/// dropping every acked row therefore rewinds the counter — an outbox drained
/// to empty restarts at seq 1, and coord's `UNIQUE (session_id, seq)`
/// idempotency then silently swallows the replayed seqs as duplicates. The
/// events would be lost with no error anywhere.
///
/// So: keep every UNACKED record, plus every record whose seq is the maximum
/// for its key. The latter is a no-op whenever that max row is itself unacked
/// (the common case) — a tombstone is retained only for a key whose newest
/// record has been acked, costing at most one row per session.
fn drop_acked_keeping_seq_floor(all: Vec<OutboxRecord>) -> Vec<OutboxRecord> {
    let mut max_seq: HashMap<(Uuid, Uuid), i64> = HashMap::new();
    for rec in &all {
        let entry = max_seq.entry((rec.machine_id, rec.session_id)).or_insert(0);
        if rec.seq > *entry {
            *entry = rec.seq;
        }
    }
    all.into_iter()
        .filter(|rec| {
            rec.acked_at.is_none()
                || max_seq
                    .get(&(rec.machine_id, rec.session_id))
                    .is_some_and(|m| *m == rec.seq)
        })
        .collect()
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

    /// The live root cause of the 355 MB primary outbox: `ack` stamped
    /// `acked_at` but KEPT the row, and `compact` — the thing the module
    /// contract said would drop it — had zero callers. Every delivered event
    /// stayed on disk forever.
    #[test]
    fn ack_drops_delivered_records_from_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let mut acks = Vec::new();
        for _ in 0..50 {
            let r = outbox
                .record(m, s, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
            acks.push((r.session_id, r.seq));
        }
        let before = std::fs::metadata(&path).unwrap().len();
        outbox.ack(&acks).unwrap();
        let after = std::fs::metadata(&path).unwrap().len();

        assert!(
            after < before / 10,
            "acked records must leave the file, not just be stamped ({before} → {after})"
        );
        assert!(outbox.pending().unwrap().is_empty());
    }

    /// The hazard the seq-floor tombstone exists for. `scan_next_seqs` rebuilds
    /// the next-seq map from `max(seq)` of the records still in the file, so a
    /// naive "drop every acked row" rewinds the counter to 1 on reopen — and
    /// coord's `UNIQUE (session_id, seq)` then silently swallows the replayed
    /// events as duplicates. Silent data loss with no error anywhere.
    #[test]
    fn dropping_acked_records_never_rewinds_the_seq_counter() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        {
            let outbox = OutboxWriter::open(&path).unwrap();
            let mut acks = Vec::new();
            for _ in 0..10 {
                let r = outbox
                    .record(m, s, SessionEventKind::Heartbeat, json!({}))
                    .unwrap();
                acks.push((r.session_id, r.seq));
            }
            // Drain the whole session — every row is now acked and droppable.
            outbox.ack(&acks).unwrap();
            assert!(outbox.pending().unwrap().is_empty());
        }

        // Reopen (a runner restart) and record the SAME session again.
        let outbox = OutboxWriter::open(&path).unwrap();
        let r = outbox
            .record(m, s, SessionEventKind::Closed, json!({}))
            .unwrap();
        assert_eq!(
            r.seq, 11,
            "seq must continue past the drained backlog, never restart at 1"
        );
    }

    /// The real item-4 bug: `compact` only runs after an ack batch, so an
    /// outbox whose drainer NEVER acks grew without bound (355 MB in the
    /// field). The writer must bound itself with zero acks ever issued.
    #[test]
    fn outbox_stays_under_cap_with_zero_acks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let cap = 16 * 1024;
        let outbox = OutboxWriter::open_with_max_bytes(&path, cap).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        // Well past the cap's worth of records, never acking anything.
        for _ in 0..400 {
            outbox
                .record(
                    m,
                    s,
                    SessionEventKind::Heartbeat,
                    json!({ "pad": "x".repeat(200) }),
                )
                .unwrap();
        }

        let size = std::fs::metadata(&path).unwrap().len();
        assert!(
            size <= cap,
            "outbox must self-bound without any ack: {size} > {cap}"
        );
        assert!(
            outbox.dropped_unacked() > 0,
            "the drop path must be counted, not silent"
        );
        // Bounding must not corrupt the queue: what survives is the NEWEST
        // tail, still in seq order, and seq allocation stays monotonic.
        let pending = outbox.pending().unwrap();
        assert!(!pending.is_empty());
        assert_eq!(pending.last().unwrap().seq, 400);
        assert!(pending.windows(2).all(|w| w[0].seq < w[1].seq));
    }

    /// A trim must exhaust ACKED rows before it destroys a single unacked
    /// event. This is the primary's live shape: 355 MB / 1,137,939 records of
    /// which 1,137,912 were already acked and only 27 still pending. Reclaiming
    /// it must cost ZERO events.
    ///
    /// The oversize acked backlog is written directly rather than through
    /// `record`/`ack`, because neither can produce one any more — that is the
    /// whole point of the fix. This fabricates what the PRE-FIX writer left on
    /// disk.
    #[test]
    fn cap_drops_acked_before_unacked() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();
        let now = Utc::now();

        let mk = |seq: i64, acked: bool| OutboxRecord {
            machine_id: m,
            session_id: s,
            seq,
            event_kind: SessionEventKind::Heartbeat.as_str().to_string(),
            payload: json!({ "pad": "x".repeat(200) }),
            recorded_at: now,
            acked_at: acked.then_some(now),
        };

        // A legacy file: a huge delivered backlog plus a few real pending rows.
        let mut legacy: Vec<OutboxRecord> = (1..=200).map(|i| mk(i, true)).collect();
        legacy.extend((201..=205).map(|i| mk(i, false)));
        rewrite_all(&path, &legacy).unwrap();

        let cap = 8 * 1024;
        assert!(std::fs::metadata(&path).unwrap().len() > cap);

        let outbox = OutboxWriter::open_with_max_bytes(&path, cap).unwrap();

        assert!(std::fs::metadata(&path).unwrap().len() <= cap);
        assert_eq!(
            outbox.dropped_unacked(),
            0,
            "acked rows must absorb the trim before any unacked event is lost"
        );
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 5, "every undelivered event must survive");
        assert_eq!(pending.first().unwrap().seq, 201);
        assert_eq!(pending.last().unwrap().seq, 205);
        // And the reclaim must not rewind the counter.
        assert_eq!(
            outbox
                .record(m, s, SessionEventKind::Closed, json!({}))
                .unwrap()
                .seq,
            206
        );
    }

    /// An outbox that grew oversize under an older (unbounded) build must come
    /// back under the cap at open — the live 355 MB file's upgrade path.
    #[test]
    fn open_trims_a_preexisting_oversize_outbox() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        // Simulate the legacy unbounded writer.
        {
            let outbox = OutboxWriter::open_with_max_bytes(&path, u64::MAX).unwrap();
            for _ in 0..400 {
                outbox
                    .record(
                        m,
                        s,
                        SessionEventKind::Heartbeat,
                        json!({ "pad": "x".repeat(200) }),
                    )
                    .unwrap();
            }
        }
        let cap = 16 * 1024;
        assert!(std::fs::metadata(&path).unwrap().len() > cap);

        let outbox = OutboxWriter::open_with_max_bytes(&path, cap).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= cap);
        // Seq allocation still continues from the pre-trim max, so coord's
        // `UNIQUE (session_id, seq)` idempotency is not violated by a replay.
        let r = outbox
            .record(m, s, SessionEventKind::Closed, json!({}))
            .unwrap();
        assert_eq!(r.seq, 401);
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
