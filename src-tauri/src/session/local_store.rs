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
//!   [`OutboxWriter::record`], which scans the live records on open and
//!   continues from `max + 1`.
//! - **Idempotent replay** — coord-side uniqueness is `UNIQUE
//!   (session_id, seq)`, so re-pushing the same record is a no-op there.
//! - **Append-only writes** — every [`OutboxWriter::record`] call appends a
//!   whole line under the write lock; the file never gets a partial row.
//! - **Compact on drain** — acks are folded into the drain cursor and the
//!   file is compacted once the delivered bytes are worth reclaiming.
//! - **Self-bounding at the writer** — compaction alone is not a bound: it
//!   only runs *after an ack batch*, so an outbox whose drainer never acks
//!   (coord unreachable, cloud-sync disabled, the drain loop never started)
//!   grows without limit. Observed in the field at 355 MB on a box with a
//!   documented disk-full history. [`OutboxWriter::record`] therefore enforces
//!   [`DEFAULT_MAX_BYTES`] itself, dropping acked rows first and then the
//!   OLDEST-unacked rows, counted in [`OutboxWriter::dropped_unacked`] and
//!   warned — never silently. A durability buffer whose only bound is
//!   "someone else drains me" has no bound at all.
//!
//! ## Group commit (plan `2026-07-28-runner-many-sessions-performance` §7a/B6)
//!
//! The write path used to be one `open` + `write_all` + **`sync_data`** per
//! event, under a process-global lock. With many sessions that is one fsync
//! per session per heartbeat tick, all serialized.
//!
//! **What changed:** the fsync is now shared. A writer appends its line to the
//! file under the write lock (page cache only) and then blocks until a *group
//! commit* fsyncs it. The first writer to arrive becomes the leader and issues
//! ONE `sync_data` covering every line appended so far; when its commit
//! already covers more than one append it first holds the
//! [`GROUP_COMMIT_WINDOW`] open so still more writers can join.
//!
//! **What "acknowledged" means — unchanged.** `record`/`record_batch` still
//! return only after the record is on stable storage. A crash immediately
//! after `Ok(..)` cannot lose the event, and nothing durable lives only in
//! writer memory — there is no unflushed buffer to lose. What the group commit
//! changes is the *cost*: N concurrent writers pay one fsync between them
//! instead of N, and [`OutboxWriter::record_batch`] collapses a whole
//! heartbeat sweep (one row per session) into a single append + single fsync.
//! A lone writer is never delayed, so bulk emitters must use `record_batch`
//! rather than a `record` loop to get the batching.
//!
//! ## Drain cursor
//!
//! [`OutboxWriter::pending`] used to parse the WHOLE file every tick and
//! [`OutboxWriter::ack`] rewrote the whole file on every ack batch. Both are
//! now incremental: a durable byte cursor (`<outbox>.cursor`) marks how far the
//! drainer has delivered, so a tick reads only the bytes appended since, and a
//! compaction runs only once the delivered bytes are worth reclaiming.
//!
//! The cursor is a **performance hint, never a source of truth**. Losing it
//! costs a re-scan and an idempotent replay of already-delivered rows, nothing
//! more. Three independent guards stop it ever *skipping* an undelivered row:
//!
//! 1. it is reset to zero and persisted **before** any rewrite renames a new
//!    file into place;
//! 2. an anchor over the first line (its byte length plus its
//!    `(session_id, seq)`) resets the cursor whenever the head of the file
//!    changes — which every compaction and every `acked_at` stamp does;
//! 3. `offset > file_len` (truncation) resets the cursor.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

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

/// Longest a durable write waits so its fsync can be shared with every other
/// writer in the same window.
///
/// The wait is **conditional on real concurrency**, measured by the count of
/// writers currently inside `record_batch` rather than by scheduling luck: a
/// commit leader holds the window open only while another writer is in flight.
/// A lone writer has nobody to share with, so throttling it would buy nothing
/// and cost up to 25 ms — it commits immediately, exactly as before this phase.
/// With many sessions recording at once the leader holds the window, every
/// writer that arrives meanwhile joins the same commit, and the fsync rate is
/// capped at 1/25 ms — the regime this phase exists for.
const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(25);

/// Don't rewrite the file to reclaim delivered rows until at least this many
/// bytes are reclaimable. Below it, the rewrite costs more than the space it
/// buys, and every rewrite invalidates the drain cursor.
const COMPACT_MIN_DEAD_BYTES: u64 = 4096;

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

/// One event to append. [`OutboxWriter::record_batch`] takes a slice of these
/// so a whole sweep (e.g. one heartbeat row per live session) lands in a
/// single append and a single fsync.
#[derive(Debug, Clone)]
pub struct OutboxEvent {
    pub machine_id: Uuid,
    pub session_id: Uuid,
    pub kind: SessionEventKind,
    pub payload: JsonValue,
}

impl OutboxEvent {
    pub fn new(
        machine_id: Uuid,
        session_id: Uuid,
        kind: SessionEventKind,
        payload: JsonValue,
    ) -> Self {
        Self {
            machine_id,
            session_id,
            kind,
            payload,
        }
    }
}

/// Idempotency key of one record — what [`OutboxWriter::ack`] takes.
pub type AckKey = (Uuid, i64);

/// Identity of the outbox file a cursor offset was measured against.
///
/// Any rewrite of the file changes its first line (compaction drops leading
/// rows; stamping `acked_at` changes their length), so comparing the first
/// line is enough to detect that an offset no longer means anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CursorAnchor {
    /// Byte length of the first line, including its newline.
    first_line_len: u64,
    /// `(session_id, seq)` of the first record. `seq = -1` marks a first line
    /// that did not parse.
    first_session: Uuid,
    first_seq: i64,
}

/// The persisted half of the drain cursor.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorFile {
    offset: u64,
    #[serde(default)]
    anchor: Option<CursorAnchor>,
    #[serde(default)]
    acked_ahead: Vec<AckKey>,
}

/// One record located in the file by a scan.
struct ScannedRecord {
    /// Byte offset just past this record's newline.
    end: u64,
    /// Bytes this record accounts for, including any unparseable bytes that
    /// immediately preceded it (so offsets stay exact).
    len: u64,
    rec: OutboxRecord,
}

/// Drain progress. `offset` is the byte boundary below which every record is
/// delivered (acked by coord) or intentionally dropped; `acked_ahead` holds
/// the out-of-order acks that could not extend that prefix yet.
#[derive(Debug, Default)]
struct DrainCursor {
    offset: u64,
    anchor: Option<CursorAnchor>,
    acked_ahead: HashSet<AckKey>,
    /// `(end, len, key)` for each record scanned at or after `offset`, in file
    /// order. Rebuilt by every scan; drives the prefix advance on ack without
    /// re-reading the file.
    frontier: Vec<(u64, u64, AckKey)>,
}

impl DrainCursor {
    fn reset(&mut self) {
        self.offset = 0;
        self.anchor = None;
        self.acked_ahead.clear();
        self.frontier.clear();
    }

    /// Bytes in the file that are already delivered and therefore reclaimable.
    fn dead_bytes(&self) -> u64 {
        self.offset
            + self
                .frontier
                .iter()
                .filter(|(_, _, key)| self.acked_ahead.contains(key))
                .map(|(_, len, _)| *len)
                .sum::<u64>()
    }

    /// Walk the frontier from the front, moving `offset` past every record the
    /// drainer has already acked.
    fn advance_prefix(&mut self) {
        let mut consumed = 0usize;
        for (end, _, key) in &self.frontier {
            if !self.acked_ahead.remove(key) {
                break;
            }
            self.offset = *end;
            consumed += 1;
        }
        self.frontier.drain(..consumed);
    }

    fn to_file(&self) -> CursorFile {
        let mut acked_ahead: Vec<AckKey> = self.acked_ahead.iter().copied().collect();
        acked_ahead.sort();
        CursorFile {
            offset: self.offset,
            anchor: self.anchor.clone(),
            acked_ahead,
        }
    }
}

/// Group-commit bookkeeping. `written` counts append batches that reached the
/// file; `durable` counts the ones an fsync has covered.
#[derive(Debug)]
struct CommitState {
    written: u64,
    durable: u64,
    flusher_active: bool,
    last_flush: Instant,
}

/// Append-only outbox backed by a single JSON-lines file.
///
/// We deliberately do NOT cache an open file handle. `ack`/`compact`
/// rewrite the file via temp-file + atomic rename (see [`rewrite_all`]),
/// which orphans any long-lived append handle — on Unix the rename unlinks
/// the old inode, so a cached handle keeps appending to a deleted file and
/// every subsequent `record` write is silently lost. Instead, `record`
/// opens a fresh append handle each call, while `write_lock` serializes all
/// file operations (append, rewrite, read) so a `record` can never interleave
/// with an in-flight `ack` rename.
#[derive(Debug)]
pub struct OutboxWriter {
    path: PathBuf,
    cursor_path: PathBuf,
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
    /// Group-commit state. Lock order is always `write_lock` → `commit`.
    commit: Mutex<CommitState>,
    commit_cv: Condvar,
    /// fsyncs issued by the group committer. Observability + tests.
    syncs: AtomicU64,
    /// Writers currently inside `record_batch`. The commit leader reads this
    /// to decide whether holding the group-commit window can batch anything —
    /// a direct measure of concurrency rather than a guess from scheduling.
    in_flight: AtomicU64,
    /// Drain progress. Lock order is always `write_lock` → `cursor`.
    cursor: Mutex<DrainCursor>,
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

        let cursor_path = cursor_path_for(&path);
        let cursor = load_cursor(&cursor_path);

        let writer = Self {
            path,
            cursor_path,
            next_seqs: Mutex::new(next_seqs),
            write_lock: Mutex::new(()),
            max_bytes,
            dropped_unacked: AtomicU64::new(0),
            commit: Mutex::new(CommitState {
                written: 0,
                durable: 0,
                flusher_active: false,
                // Everything already on disk was fsynced by the build that
                // wrote it, so the first writer commits with no wait.
                last_flush: Instant::now()
                    .checked_sub(GROUP_COMMIT_WINDOW)
                    .unwrap_or_else(Instant::now),
            }),
            commit_cv: Condvar::new(),
            syncs: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            cursor: Mutex::new(cursor),
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

    /// fsyncs the group committer has issued since open. One per commit
    /// window, however many records that window carried.
    pub fn syncs(&self) -> u64 {
        self.syncs.load(Ordering::Relaxed)
    }

    /// Record a new event for `(machine_id, session_id)`. Allocates the
    /// next monotonic seq, appends the JSON line, and returns only once the
    /// line is durable (see the module note on group commit).
    pub fn record(
        &self,
        machine_id: Uuid,
        session_id: Uuid,
        event_kind: SessionEventKind,
        payload: JsonValue,
    ) -> std::io::Result<OutboxRecord> {
        let mut out = self.record_batch(vec![OutboxEvent::new(
            machine_id, session_id, event_kind, payload,
        )])?;
        Ok(out.remove(0))
    }

    /// Append a whole batch of events in ONE file append covered by ONE fsync.
    ///
    /// Semantically identical to calling [`OutboxWriter::record`] per event —
    /// same per-`(machine_id, session_id)` seq allocation, same durability on
    /// return — but it costs one commit instead of N, and it is atomic: either
    /// every line is appended or none is.
    pub fn record_batch(&self, events: Vec<OutboxEvent>) -> std::io::Result<Vec<OutboxRecord>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        // Counted for the whole call — including the durability wait — so the
        // commit leader can see that another writer is there to batch with.
        let _in_flight = InFlightGuard::enter(&self.in_flight);

        let records: Vec<OutboxRecord> = {
            let mut map = self.next_seqs.lock().expect("outbox seqs poisoned");
            events
                .into_iter()
                .map(|ev| {
                    let entry = map.entry((ev.machine_id, ev.session_id)).or_insert(0);
                    *entry += 1;
                    OutboxRecord {
                        machine_id: ev.machine_id,
                        session_id: ev.session_id,
                        seq: *entry,
                        event_kind: ev.kind.as_str().to_string(),
                        payload: ev.payload,
                        recorded_at: Utc::now(),
                        acked_at: None,
                    }
                })
                .collect()
        };

        let mut buf = Vec::new();
        for rec in &records {
            serde_json::to_writer(&mut buf, rec).map_err(std::io::Error::other)?;
            buf.push(b'\n');
        }

        let ticket = {
            // Open a fresh append handle under the lock so the write always
            // lands in the current file even after an `ack`/`compact` rename.
            let _guard = self.write_lock.lock().expect("outbox lock poisoned");
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.path)?;
            file.write_all(&buf)?;
            drop(file);

            let ticket = {
                let mut st = self.commit.lock().expect("outbox commit poisoned");
                st.written += 1;
                st.written
            };

            // Bound the file HERE rather than relying on the drainer's post-ack
            // compaction — see the module note. A trim failure must not fail the
            // write: the record is already queued, and losing the event to keep
            // the file small would invert the trade-off.
            if let Err(e) = self.enforce_cap_locked() {
                warn!(
                    "outbox: cap enforcement failed ({}); file may be oversize",
                    e
                );
            }
            ticket
        };

        // Durability barrier: shared with every other writer in this window.
        self.await_durable(ticket)?;
        Ok(records)
    }

    // -- group commit --------------------------------------------------------

    /// Block until every append up to `ticket` has been fsynced.
    ///
    /// The first waiter becomes the commit leader. If its commit would already
    /// cover more than one append it holds the [`GROUP_COMMIT_WINDOW`] open so
    /// still more writers can join; otherwise it commits straight away. Either
    /// way it then issues ONE fsync covering every append made so far.
    /// Everyone else waits on the condvar and is satisfied by that fsync.
    fn await_durable(&self, ticket: u64) -> std::io::Result<()> {
        loop {
            let deadline = {
                let mut st = self.commit.lock().expect("outbox commit poisoned");
                if st.durable >= ticket {
                    return Ok(());
                }
                if st.flusher_active {
                    // A leader is committing (or waiting out its window); it
                    // will cover us. The timeout is a liveness net only.
                    let _ = self
                        .commit_cv
                        .wait_timeout(st, GROUP_COMMIT_WINDOW * 4)
                        .expect("outbox commit poisoned");
                    continue;
                }
                st.flusher_active = true;
                // Only worth waiting when there is something to batch with:
                // another writer already appended, or is inside `record_batch`
                // right now and will append shortly.
                let contended =
                    st.written > st.durable + 1 || self.in_flight.load(Ordering::Relaxed) > 1;
                if contended {
                    Some(st.last_flush + GROUP_COMMIT_WINDOW)
                } else {
                    None
                }
            };

            if let Some(deadline) = deadline {
                let now = Instant::now();
                if deadline > now {
                    std::thread::sleep(deadline - now);
                }
            }
            let result = self.flush_now();
            {
                let mut st = self.commit.lock().expect("outbox commit poisoned");
                st.flusher_active = false;
            }
            self.commit_cv.notify_all();
            result?;
        }
    }

    /// One group commit: fsync the file, then publish the new durable mark.
    fn flush_now(&self) -> std::io::Result<()> {
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        self.sync_locked()
    }

    /// Caller MUST hold `write_lock`.
    fn sync_locked(&self) -> std::io::Result<()> {
        let (target, durable) = {
            let st = self.commit.lock().expect("outbox commit poisoned");
            (st.written, st.durable)
        };
        if target > durable {
            // A fresh handle is fine: fsync flushes the FILE's dirty pages,
            // not just the ones this descriptor wrote.
            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.path)?;
            file.sync_data()?;
            self.syncs.fetch_add(1, Ordering::Relaxed);
        }
        self.mark_durable_locked(target);
        Ok(())
    }

    /// Caller MUST hold `write_lock`. Publishes `target` as durable and wakes
    /// every waiter.
    fn mark_durable_locked(&self, target: u64) {
        {
            let mut st = self.commit.lock().expect("outbox commit poisoned");
            if target > st.durable {
                st.durable = target;
            }
            st.last_flush = Instant::now();
        }
        self.commit_cv.notify_all();
    }

    // -- cap enforcement -----------------------------------------------------

    /// Bring the file back under `max_bytes`, dropping ACKED records first
    /// (already delivered — free) and then the OLDEST unacked records
    /// (chronological append order makes the front the oldest).
    ///
    /// Caller MUST hold `write_lock`. The newest record is never dropped, so a
    /// single record larger than the trim target leaves the file marginally
    /// over cap rather than emptying it.
    fn enforce_cap_locked(&self) -> std::io::Result<()> {
        let mut size = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            // No file yet / unreadable — nothing to bound.
            Err(_) => return Ok(()),
        };
        if size <= self.max_bytes {
            return Ok(());
        }

        // Delivered rows the drain cursor knows about are not stamped on disk
        // yet. Materialize them first so the trim below counts them as free
        // space rather than reporting phantom event loss.
        let has_cursor_acks = {
            let cur = self.cursor.lock().expect("outbox cursor poisoned");
            cur.dead_bytes() > 0
        };
        if has_cursor_acks {
            self.compact_acked_locked()?;
            size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            if size <= self.max_bytes {
                return Ok(());
            }
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
            self.rewrite_locked(&final_kept)?;
        }
        Ok(())
    }

    // -- drain ---------------------------------------------------------------

    /// Return all not-yet-delivered records, in seq order per session. Used
    /// by [`crate::session::coord_sync`] to drive the push loop.
    ///
    /// Incremental: only the bytes appended since the drain cursor are read.
    /// When nothing has been appended since the last call this costs a single
    /// `metadata` stat.
    pub fn pending(&self) -> std::io::Result<Vec<OutboxRecord>> {
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        self.scan_locked()
    }

    /// Caller MUST hold `write_lock`. Validates the cursor against the file,
    /// absorbs any already-acked prefix, rebuilds the frontier, and returns
    /// the undelivered records at or after the cursor.
    fn scan_locked(&self) -> std::io::Result<Vec<OutboxRecord>> {
        let mut cur = self.cursor.lock().expect("outbox cursor poisoned");

        let len = match std::fs::metadata(&self.path) {
            Ok(m) => m.len(),
            Err(_) => {
                cur.reset();
                return Ok(Vec::new());
            }
        };

        // Guard 3 — the file shrank under us (truncation / external rewrite).
        if cur.offset > len {
            cur.reset();
        }
        // Guard 2 — the head of the file changed (compaction, rotation, an
        // `acked_at` stamp), so the offset no longer addresses what it did.
        if cur.offset > 0 {
            let anchor = read_anchor(&self.path)?;
            if anchor.is_none() || anchor != cur.anchor {
                cur.reset();
            }
        }

        if cur.offset == len && cur.anchor.is_some() {
            // Skip-if-unchanged: everything up to EOF is delivered.
            cur.frontier.clear();
            return Ok(Vec::new());
        }

        let scanned = read_from(&self.path, cur.offset)?;

        // Absorb any leading records already acked out of band, so the cursor
        // advances even when `ack` could not extend the prefix at the time.
        let mut start = 0usize;
        while start < scanned.len() {
            let key = (scanned[start].rec.session_id, scanned[start].rec.seq);
            let delivered = cur.acked_ahead.contains(&key) || scanned[start].rec.acked_at.is_some();
            if !delivered {
                break;
            }
            cur.acked_ahead.remove(&key);
            cur.offset = scanned[start].end;
            start += 1;
        }

        cur.frontier = scanned[start..]
            .iter()
            .map(|s| (s.end, s.len, (s.rec.session_id, s.rec.seq)))
            .collect();

        if cur.anchor.is_none() {
            cur.anchor = read_anchor(&self.path)?;
        }

        let mut pending: Vec<OutboxRecord> = scanned[start..]
            .iter()
            .filter(|s| {
                s.rec.acked_at.is_none()
                    && !cur.acked_ahead.contains(&(s.rec.session_id, s.rec.seq))
            })
            .map(|s| s.rec.clone())
            .collect();
        pending.sort_by_key(|r| (r.session_id, r.seq));
        Ok(pending)
    }

    /// Mark records as delivered.
    ///
    /// Acks fold into the drain cursor (an O(1) sidecar write) instead of
    /// rewriting the whole outbox on every batch; the file is compacted only
    /// once [`COMPACT_MIN_DEAD_BYTES`] of delivered rows have accumulated and
    /// they are worth at least half the file.
    ///
    /// **This used to only STAMP `acked_at` and keep the row.** Its doc claimed
    /// it compacted, the module contract said the drainer would call
    /// [`OutboxWriter::compact`] after each ack batch — and nothing ever did
    /// (`compact` had zero callers). So every delivered event stayed on disk
    /// forever: the primary's outbox was measured at 355 MB / 1,137,939 records
    /// of which 1,137,912 were already acked. Dropping on ack is what the
    /// contract always promised; [`drop_acked_keeping_seq_floor`] keeps it safe
    /// against the next-seq rewind.
    pub fn ack(&self, acks: &[AckKey]) -> std::io::Result<()> {
        if acks.is_empty() {
            return Ok(());
        }
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");

        let needs_scan = {
            let mut cur = self.cursor.lock().expect("outbox cursor poisoned");
            for key in acks {
                cur.acked_ahead.insert(*key);
            }
            cur.frontier.is_empty()
        };
        // A caller that acks without having drained through `pending()` has no
        // frontier to advance over; rebuild it once so the prefix can move.
        if needs_scan {
            self.scan_locked()?;
        }
        {
            let mut cur = self.cursor.lock().expect("outbox cursor poisoned");
            cur.advance_prefix();
            self.persist_cursor_locked(&cur);
        }

        self.maybe_compact_locked()
    }

    /// Drop already-acked records. [`OutboxWriter::ack`] compacts on its own
    /// schedule, so this remains as a manual sweep for cron-style callers.
    pub fn compact(&self) -> std::io::Result<()> {
        let _guard = self.write_lock.lock().expect("outbox lock poisoned");
        self.compact_acked_locked()
    }

    /// Caller MUST hold `write_lock`.
    fn maybe_compact_locked(&self) -> std::io::Result<()> {
        let dead = {
            let cur = self.cursor.lock().expect("outbox cursor poisoned");
            cur.dead_bytes()
        };
        if dead < COMPACT_MIN_DEAD_BYTES {
            return Ok(());
        }
        let len = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if dead.saturating_mul(2) < len {
            return Ok(());
        }
        self.compact_acked_locked()
    }

    /// Caller MUST hold `write_lock`. Materializes the cursor's delivery
    /// knowledge into the file: stamp `acked_at` on every delivered record,
    /// drop the acked rows (seq-floor preserving) and rewrite.
    fn compact_acked_locked(&self) -> std::io::Result<()> {
        let scanned = read_from(&self.path, 0)?;
        let now = Utc::now();
        let records: Vec<OutboxRecord> = {
            let cur = self.cursor.lock().expect("outbox cursor poisoned");
            scanned
                .into_iter()
                .map(|s| {
                    let mut rec = s.rec;
                    let delivered =
                        s.end <= cur.offset || cur.acked_ahead.contains(&(rec.session_id, rec.seq));
                    if delivered && rec.acked_at.is_none() {
                        rec.acked_at = Some(now);
                    }
                    rec
                })
                .collect()
        };
        self.rewrite_locked(&drop_acked_keeping_seq_floor(records))
    }

    /// Caller MUST hold `write_lock`. Replaces the file's contents atomically.
    ///
    /// The cursor is reset and persisted BEFORE the rename: a crash in the
    /// window can then only leave a cursor that re-reads (safe, idempotent),
    /// never one that skips (unsafe).
    fn rewrite_locked(&self, records: &[OutboxRecord]) -> std::io::Result<()> {
        {
            let mut cur = self.cursor.lock().expect("outbox cursor poisoned");
            cur.reset();
            self.persist_cursor_locked(&cur);
        }
        rewrite_all(&self.path, records)?;
        // `rewrite_all` fsyncs the temp file before renaming it into place, so
        // every surviving record — including any append this group-commit
        // window had not yet synced — is now durable.
        let target = {
            let st = self.commit.lock().expect("outbox commit poisoned");
            st.written
        };
        self.mark_durable_locked(target);
        Ok(())
    }

    /// Best-effort cursor persistence. Deliberately NOT fsynced: the cursor is
    /// a hint, and an fsync per ack would undo the group commit. A lost or
    /// stale cursor costs a re-scan plus an idempotent replay, never a skip.
    fn persist_cursor_locked(&self, cur: &DrainCursor) {
        let file = cur.to_file();
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        let tmp = self.cursor_path.with_extension("cursor.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            if let Err(e) = std::fs::rename(&tmp, &self.cursor_path) {
                warn!("outbox: drain cursor persist failed ({e}); will re-scan");
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

/// Counts a writer for as long as it is inside `record_batch`, so the commit
/// leader can tell real concurrency from a lone writer.
struct InFlightGuard<'a>(&'a AtomicU64);

impl<'a> InFlightGuard<'a> {
    fn enter(counter: &'a AtomicU64) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Sidecar path holding the drain cursor for `path`.
fn cursor_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".cursor");
    path.with_file_name(name)
}

fn load_cursor(cursor_path: &Path) -> DrainCursor {
    let Ok(bytes) = std::fs::read(cursor_path) else {
        return DrainCursor::default();
    };
    let Ok(file) = serde_json::from_slice::<CursorFile>(&bytes) else {
        warn!("outbox: drain cursor unreadable; re-scanning from the start");
        return DrainCursor::default();
    };
    DrainCursor {
        offset: file.offset,
        anchor: file.anchor,
        acked_ahead: file.acked_ahead.into_iter().collect(),
        frontier: Vec::new(),
    }
}

/// Identity of the file's first line — the cursor's rotation guard.
fn read_anchor(path: &Path) -> std::io::Result<Option<CursorAnchor>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    let n = reader.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    let parsed = std::str::from_utf8(&buf)
        .ok()
        .and_then(|s| serde_json::from_str::<OutboxRecord>(s.trim()).ok());
    Ok(Some(match parsed {
        Some(rec) => CursorAnchor {
            first_line_len: n as u64,
            first_session: rec.session_id,
            first_seq: rec.seq,
        },
        None => CursorAnchor {
            first_line_len: n as u64,
            first_session: Uuid::nil(),
            first_seq: -1,
        },
    }))
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

/// Parse every record from `start` to EOF, carrying exact byte offsets.
///
/// Unparseable lines are skipped but their bytes are attributed to the next
/// record, so offsets stay exact and a corrupt line can never desynchronize
/// the drain cursor.
fn read_from(path: &Path, start: u64) -> std::io::Result<Vec<ScannedRecord>> {
    let mut out = Vec::new();
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(out),
    };
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut reader = BufReader::new(file);
    let mut pos = start;
    let mut carried = 0u64;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        pos += n as u64;
        let parsed = std::str::from_utf8(&buf).ok().and_then(|s| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                match serde_json::from_str::<OutboxRecord>(s) {
                    Ok(rec) => Some(rec),
                    Err(e) => {
                        warn!("outbox: skipping unparseable record: {}", e);
                        None
                    }
                }
            }
        });
        match parsed {
            Some(rec) => {
                out.push(ScannedRecord {
                    end: pos,
                    len: n as u64 + carried,
                    rec,
                });
                carried = 0;
            }
            None => carried += n as u64,
        }
    }
    Ok(out)
}

fn read_all(path: &Path) -> std::io::Result<Vec<OutboxRecord>> {
    Ok(read_from(path, 0)?.into_iter().map(|s| s.rec).collect())
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
    use std::sync::Arc;
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

        // The acked prefix leaves the drain cursor's view immediately, whether
        // or not the file has been compacted yet.
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
            outbox.compact().unwrap();
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

    // -- group commit --------------------------------------------------------

    /// The durability contract after group commit: a record whose `record()`
    /// call returned `Ok` is on stable storage. Simulated crash boundary — the
    /// writer is `mem::forget`ed so NO destructor, flush or clean shutdown can
    /// run, then the file is reopened cold. Anything held only in writer memory
    /// would vanish here.
    #[test]
    fn acknowledged_records_survive_a_crash_with_no_clean_shutdown() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let acked: Vec<i64> = {
            let outbox = OutboxWriter::open(&path).unwrap();
            let mut seqs = Vec::new();
            for i in 0..25 {
                let r = outbox
                    .record(m, s, SessionEventKind::Heartbeat, json!({ "i": i }))
                    .unwrap();
                seqs.push(r.seq);
            }
            // Crash: no Drop, no flush, no compaction — the process is gone the
            // instant the last `record` returned.
            std::mem::forget(outbox);
            seqs
        };

        let reopened = OutboxWriter::open(&path).unwrap();
        let pending = reopened.pending().unwrap();
        let recovered: Vec<i64> = pending.iter().map(|r| r.seq).collect();
        assert_eq!(
            recovered, acked,
            "every acknowledged record must survive a crash with no clean shutdown"
        );
    }

    /// The same crash boundary for a batch: `record_batch` is one append under
    /// one fsync, and it is all-or-nothing.
    #[test]
    fn record_batch_is_durable_and_atomic_across_a_crash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let sessions: Vec<Uuid> = (0..40).map(|_| Uuid::new_v4()).collect();

        {
            let outbox = OutboxWriter::open(&path).unwrap();
            let events: Vec<OutboxEvent> = sessions
                .iter()
                .map(|s| OutboxEvent::new(m, *s, SessionEventKind::Heartbeat, json!({})))
                .collect();
            let written = outbox.record_batch(events).unwrap();
            assert_eq!(written.len(), 40);
            assert!(
                outbox.syncs() <= 1,
                "a 40-session heartbeat sweep must cost at most ONE fsync, got {}",
                outbox.syncs()
            );
            std::mem::forget(outbox);
        }

        let reopened = OutboxWriter::open(&path).unwrap();
        assert_eq!(reopened.pending().unwrap().len(), 40);
    }

    /// Concurrent writers share commits instead of each paying an fsync.
    #[test]
    fn concurrent_writers_share_group_commits() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = Arc::new(OutboxWriter::open(&path).unwrap());
        let m = Uuid::new_v4();

        let mut handles = Vec::new();
        for _ in 0..8 {
            let outbox = outbox.clone();
            let s = Uuid::new_v4();
            handles.push(std::thread::spawn(move || {
                for i in 0..10 {
                    outbox
                        .record(m, s, SessionEventKind::Heartbeat, json!({ "i": i }))
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(outbox.pending().unwrap().len(), 80, "no lost events");
        assert!(
            outbox.syncs() < 80,
            "80 concurrent records must not cost 80 fsyncs, got {}",
            outbox.syncs()
        );
    }

    // -- drain cursor --------------------------------------------------------

    /// The cursor is durable: after a restart the drainer resumes past the
    /// records it already delivered instead of replaying them.
    #[test]
    fn drain_cursor_survives_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let acks = {
            let outbox = OutboxWriter::open(&path).unwrap();
            let mut acks = Vec::new();
            for _ in 0..5 {
                let r = outbox
                    .record(m, s, SessionEventKind::Heartbeat, json!({}))
                    .unwrap();
                acks.push((r.session_id, r.seq));
            }
            outbox.ack(&acks).unwrap();
            // Below the compaction floor, so the delivered rows are still on
            // disk — only the cursor knows they are done.
            assert!(std::fs::metadata(&path).unwrap().len() > 0);
            assert!(outbox.pending().unwrap().is_empty());
            acks
        };

        let reopened = OutboxWriter::open(&path).unwrap();
        assert!(
            reopened.pending().unwrap().is_empty(),
            "a restart must not replay the {} delivered records",
            acks.len()
        );
        // And the file still carries them, proving it was the CURSOR that
        // survived rather than a compaction having hidden the evidence.
        assert_eq!(read_all(&path).unwrap().len(), 5);
    }

    /// Truncation resets the cursor rather than skipping the surviving tail.
    #[test]
    fn drain_cursor_resets_on_truncation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let mut acks = Vec::new();
        for _ in 0..5 {
            let r = outbox
                .record(m, s, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
            acks.push((r.session_id, r.seq));
        }
        outbox.ack(&acks).unwrap();
        assert!(outbox.pending().unwrap().is_empty());

        // Something outside the writer truncates the file to its first record.
        let all = read_all(&path).unwrap();
        rewrite_all(&path, &all[..1]).unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a truncated outbox must be re-read from the start, not skipped"
        );
        assert_eq!(pending[0].seq, 1);
    }

    /// Rotation — the file is replaced by different content of a similar size.
    /// The offset alone would happily point into the middle of the new file;
    /// the anchor is what catches it.
    #[test]
    fn drain_cursor_resets_on_rotation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let mut acks = Vec::new();
        for _ in 0..6 {
            let r = outbox
                .record(m, s, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
            acks.push((r.session_id, r.seq));
        }
        outbox.ack(&acks).unwrap();
        assert!(outbox.pending().unwrap().is_empty());
        let offset_before = std::fs::metadata(&path).unwrap().len();

        // Rotate: a brand-new file of DIFFERENT records, deliberately no
        // shorter than the old one so the shrink guard cannot fire and only
        // the anchor can catch it.
        let s2 = Uuid::new_v4();
        let now = Utc::now();
        let rotated: Vec<OutboxRecord> = (1..=6)
            .map(|seq| OutboxRecord {
                machine_id: m,
                session_id: s2,
                seq,
                event_kind: SessionEventKind::Heartbeat.as_str().to_string(),
                payload: json!({ "pad": "xxxxxxxxxxxxxxxx" }),
                recorded_at: now,
                acked_at: None,
            })
            .collect();
        rewrite_all(&path, &rotated).unwrap();
        assert!(
            std::fs::metadata(&path).unwrap().len() >= offset_before,
            "the rotated file must not be shorter, or the shrink guard — not \
             the anchor — would be what catches this"
        );

        let pending = outbox.pending().unwrap();
        assert_eq!(
            pending.len(),
            6,
            "a rotated outbox must be re-read from the start, not skipped"
        );
        assert_eq!(pending[0].session_id, s2);
    }

    /// A tick that appends nothing must not re-read the file, and a tick that
    /// appends one record must surface exactly that record.
    #[test]
    fn drain_is_incremental_after_the_first_pass() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let mut acks = Vec::new();
        for _ in 0..20 {
            let r = outbox
                .record(m, s, SessionEventKind::Heartbeat, json!({}))
                .unwrap();
            acks.push((r.session_id, r.seq));
        }
        assert_eq!(outbox.pending().unwrap().len(), 20);
        outbox.ack(&acks).unwrap();

        // Caught up — nothing new to read.
        assert!(outbox.pending().unwrap().is_empty());

        // One more append surfaces alone, not behind a 20-record re-parse.
        let r = outbox
            .record(m, s, SessionEventKind::Closed, json!({}))
            .unwrap();
        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, r.seq);
    }

    /// Out-of-order acks (the drain loop skips a stuck helper-task row and
    /// keeps going) must not advance the cursor past the stuck record, and
    /// must not re-offer the rows already delivered behind it.
    #[test]
    fn out_of_order_acks_hold_the_prefix_without_replaying() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("outbox.jsonl");
        let outbox = OutboxWriter::open(&path).unwrap();
        let m = Uuid::new_v4();
        let s = Uuid::new_v4();

        let mut recs = Vec::new();
        for _ in 0..4 {
            recs.push(
                outbox
                    .record(m, s, SessionEventKind::Heartbeat, json!({}))
                    .unwrap(),
            );
        }
        // seq 1 is stuck; 2..4 delivered.
        outbox
            .ack(
                &recs[1..]
                    .iter()
                    .map(|r| (r.session_id, r.seq))
                    .collect::<Vec<_>>(),
            )
            .unwrap();

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1, "only the stuck record stays pending");
        assert_eq!(pending[0].seq, 1);

        // And that survives a restart — the ack set is persisted alongside the
        // offset, so the delivered rows are not replayed.
        drop(pending);
        let reopened = OutboxWriter::open(&path).unwrap();
        let pending = reopened.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].seq, 1);
    }
}
