//! The pane's DURABLE scrollback tail: the newest bytes of its output ring,
//! on disk, anchored to their absolute offset.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 4, decision D7 — *"a feature that exists to survive an unscheduled
//! event cannot be opt-in"*. The holder's [`OutputRing`] survives a runner
//! crash (the holder outlives the runner, D13), but not the holder's own death
//! or a host reboot; the runner's only disk copy today is written by the
//! webview on a GRACEFUL close, so the crash it exists for loses everything.
//! This module is the holder writing its own tail, always, with no switch.
//!
//! ## What, and where
//!
//! The last [`SCROLLBACK_CAP_BYTES`] of the pane's output (less while it has
//! produced less), the ABSOLUTE offset of the first of those bytes, and the
//! child pid whose stream the offsets count, in
//! `<pane-dir>/<pane-id>.scrollback` ([`crate::pane::scrollback_path`]). The
//! offset keeps a restore gap-honest: a reader that last saw offset `N` and
//! reads a tail starting at `S > N` knows exactly that `[N, S)` is lost — the
//! coordinates of the ring, `output_lost` and the runner's `splice_replay`.
//!
//! Mode 0600 inside the 0700 pane directory (Unix). Windows: the file inherits
//! the per-user ACL of the pane directory, the same barrier the lock, spec and
//! log files rely on there.
//!
//! ## How — an append-only log, compacted by atomic rename
//!
//! Rewriting the whole capped tail whenever it changed would make a busy TUI
//! (a spinner redrawing a status line) cost a full 1 MiB write per pane per
//! tick. So the file is a header followed by RECORDS, each one a contiguous
//! run of output with its absolute start offset and its own checksum:
//!
//! - **Append** — a tick that finds new output appends one record holding
//!   only the bytes produced since the last record. The cost of a tick is the
//!   pane's own new output, plus 20 bytes.
//! - **Rewrite (compaction)** — when an append would push the file past
//!   [`MAX_SCROLLBACK_FILE_BYTES`] (two full tails), when the new output alone
//!   exceeds the cap, when the ring has already rolled past the last persisted
//!   byte, on a holder's first write, and after any failed write, the file is
//!   REPLACED by a fresh one holding a single record: the current capped tail.
//!   A rewrite goes to `<pane-id>.scrollback.tmp` (created exclusively, so
//!   never through a planted symlink), is `fsync`ed, then renamed over the
//!   target, and the directory is `fsync`ed — it is atomic. A stale `.tmp`
//!   (a holder killed mid-rewrite) is ignored by readers and removed by the
//!   next rewrite.
//! - **Crash-safe recovery rule: the longest valid prefix.** An append is not
//!   atomic, and an un-`fsync`ed one need not survive a power loss in any
//!   particular shape — a filesystem can make the new SIZE durable before the
//!   data, leaving zeros (or garbage) after the last synced record. So a
//!   reader walks the records in order and stops at the first one that is
//!   incomplete, declares more than the cap, or fails its checksum; it
//!   returns every record before it — each of which passed its own checksum
//!   and starts exactly where its predecessor ended — and reports what it
//!   dropped in [`Scrollback::discarded_bytes`] (never silently). Two things
//!   are refused outright ([`ScrollbackError::Corrupt`]), because no crash
//!   produces them: a damaged header, and a record that passes its checksum
//!   but does not start where the previous one ended. After a failed append
//!   or a failed `fsync` the writer never appends to that file again: its next
//!   write is a rewrite.
//!
//! ## When — and what it costs
//!
//! On a timer ([`SCROLLBACK_INTERVAL`], 5 s) with a change mark: a tick whose
//! ring end offset equals the last persisted one writes nothing, so an idle
//! pane costs no I/O. Plus one final write when the holder ends
//! ([`persist_final`], called by [`crate::server::Holder::run`] before the
//! pane lock is released), so the child's last output is on disk.
//!
//! - **Bytes.** For a pane producing `R` bytes/s, appends write `R` (plus 20
//!   bytes per tick) and compaction rewrites at most one capped tail per ~cap
//!   bytes appended: about `2R`. A spinner-busy TUI at ~2 KiB/s therefore
//!   writes ~4 KiB/s; 25 such panes ~100 KiB/s (~8.6 GB/day), against ~5 MiB/s
//!   (~430 GB/day) for rewrite-every-tick.
//! - **Flood bound.** Rewrites a flood forces (new output beyond what an
//!   append may carry) are rate-limited to one per [`REWRITE_MIN_INTERVAL`]
//!   (30 s); a deferred tick writes nothing and the next one catches up, so a
//!   flooding pane's durable copy is at most ~35 s stale. Between two rewrites
//!   at most one file's worth of appends (≤ 2 MiB + framing) can land, so the
//!   ABSOLUTE worst case is ≤ ~3 MiB per 30 s ≈ 100 KiB/s per pane, ~2.5 MiB/s
//!   for 25 panes all flooding at once (and those panes are then themselves
//!   producing far more than that). Only the holder's first write and its
//!   final write are never deferred; the recovery rewrite after a failure is
//!   rate-limited the same way, so a disk that keeps failing costs at most one
//!   capped attempt per 30 s.
//! - **fsync.** A rewrite always syncs (it must, before its rename). Appends
//!   are synced at most once per [`FSYNC_INTERVAL`] (30 s), and always by the
//!   final write: ≤ 25 / 30 s ≈ 0.8 fsync/s for 25 busy panes. An unsynced
//!   append survives a holder crash (it is in the page cache); only a power
//!   loss can drop it, and then the recovery rule above returns the tail as
//!   of the last sync.
//! - **Staleness.** A holder killed without warning loses at most the last
//!   [`SCROLLBACK_INTERVAL`] (~5 s) of output from the durable copy (~35 s for
//!   a flooding pane, see the flood bound). A POWER LOSS can lose up to ~35 s
//!   of output: up to [`FSYNC_INTERVAL`] (30 s) of unsynced appends plus the
//!   tick interval. In every case the offset anchor says exactly how much.
//!
//! ## Lifecycle
//!
//! The writer is CLOSED by the final write, under its own mutex, before the
//! holder releases its pane lock. A timer tick that runs after that does
//! nothing — it can never rename over, append to, or delete the `.tmp` of a
//! SUCCESSOR holder that has since taken the same pane id. The timer thread
//! ends when it sees the writer closed; if it ends any other way (a panic) it
//! says so once in the holder log.
//!
//! The file is never removed by the holder: outliving the holder is its whole
//! purpose. Reaping it belongs with the lock file's, in the boot sweep.
//!
//! ## Reading
//!
//! [`read_scrollback`]: bounded (a file larger than
//! [`MAX_SCROLLBACK_FILE_BYTES`] is refused before it is read), checked (the
//! same no-symlink / regular-file / our-uid open as the lock), validated, and
//! returns at most [`SCROLLBACK_CAP_BYTES`]. It is for the runner to restore a
//! pane whose holder is gone; Phase 4 ships it with no production caller —
//! the runner-side consumers are the boot sweep (Phase 3) and resume after
//! holder death (Phase 6).
//!
//! ## Layout (integers big-endian)
//!
//! Header, 24 bytes: magic `QPTYSCRL` (8), format [`SCROLLBACK_FORMAT`] (4),
//! child pid (4), FNV-1a 64 of those 16 bytes (8).
//!
//! Record, 20 + `n` bytes: absolute start offset (8), length `n` ≤
//! [`SCROLLBACK_CAP_BYTES`] (4), the bytes (`n`), FNV-1a 64 of the offset,
//! length and bytes (8).
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::pane::{scrollback_path, PaneId};
use crate::pty::Pane;
use crate::ring::OutputRing;

/// Most tail bytes a reader is given, and most bytes one record carries. Half
/// the default ring (`crate::spec::DEFAULT_RING_CAPACITY`): enough screens of
/// context to restore a pane.
pub const SCROLLBACK_CAP_BYTES: usize = 1024 * 1024;

/// How often the holder checks whether its tail changed and, if so, persists
/// it. A holder killed without warning loses at most this much output from
/// the durable copy (more only while a flood is being rate-limited) — and the
/// offset anchor says exactly how much.
pub const SCROLLBACK_INTERVAL: Duration = Duration::from_secs(5);

/// Least time between two rewrites the writer could defer (see the module
/// docs: the flood bound).
pub const REWRITE_MIN_INTERVAL: Duration = Duration::from_secs(30);

/// Longest an appended record stays un-`fsync`ed while the holder runs.
pub const FSYNC_INTERVAL: Duration = Duration::from_secs(30);

/// The on-disk format version this build writes and reads.
pub const SCROLLBACK_FORMAT: u32 = 2;

const MAGIC: &[u8; 8] = b"QPTYSCRL";
const HEADER_LEN: usize = 8 + 4 + 4 + 8;
const RECORD_OVERHEAD: usize = 8 + 4 + 8;

/// Largest file the writer lets grow and [`read_scrollback`] will read: the
/// header plus two full records. A bound on what a corrupt or foreign file
/// can make a reader allocate.
pub const MAX_SCROLLBACK_FILE_BYTES: u64 =
    (HEADER_LEN + 2 * (RECORD_OVERHEAD + SCROLLBACK_CAP_BYTES)) as u64;

/// A persisted tail, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrollback {
    /// The child whose output stream the offsets count.
    pub child_pid: u32,
    /// Absolute offset of `bytes[0]` in the child's output stream.
    pub start_offset: u64,
    /// At most [`SCROLLBACK_CAP_BYTES`].
    pub bytes: Vec<u8>,
    /// Bytes after the longest valid prefix that the reader dropped: a torn
    /// or damaged record and everything after it (see the module docs). Zero
    /// for a file that read back whole.
    pub discarded_bytes: u64,
}

impl Scrollback {
    /// Whether the reader dropped anything ([`Scrollback::discarded_bytes`]).
    pub fn is_torn(&self) -> bool {
        self.discarded_bytes > 0
    }

    /// Absolute offset one past the last persisted byte.
    pub fn end_offset(&self) -> u64 {
        self.start_offset + self.bytes.len() as u64
    }
}

/// FNV-1a, 64-bit. Detects a torn or damaged file; it is not a MAC — the
/// file's integrity against other users is the 0600 mode in a 0700 directory.
fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in *part {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn encode_header(child_pid: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&SCROLLBACK_FORMAT.to_be_bytes());
    out.extend_from_slice(&child_pid.to_be_bytes());
    let sum = fnv1a64(&[&out]);
    out.extend_from_slice(&sum.to_be_bytes());
    out
}

/// Append one record for `bytes` starting at absolute offset `start`.
fn encode_record(out: &mut Vec<u8>, start: u64, bytes: &[u8]) {
    let start_be = start.to_be_bytes();
    // Every caller passes at most the cap, which fits a u32.
    let len_be = (bytes.len() as u32).to_be_bytes();
    out.extend_from_slice(&start_be);
    out.extend_from_slice(&len_be);
    out.extend_from_slice(bytes);
    out.extend_from_slice(&fnv1a64(&[&start_be, &len_be, bytes]).to_be_bytes());
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_be_bytes(a)
}

/// Parse a whole file (see the module docs for the recovery rule).
fn decode(buf: &[u8]) -> Result<Scrollback, ScrollbackError> {
    let corrupt = |why: &'static str| Err(ScrollbackError::Corrupt(why));
    if buf.len() < HEADER_LEN {
        return corrupt("shorter than the header");
    }
    let header = &buf[..HEADER_LEN];
    if &header[..8] != MAGIC {
        return corrupt("bad magic");
    }
    if be_u32(&header[8..]) != SCROLLBACK_FORMAT {
        return corrupt("unknown format version");
    }
    if be_u64(&header[16..]) != fnv1a64(&[&header[..16]]) {
        return corrupt("header checksum mismatch");
    }
    let child_pid = be_u32(&header[12..]);

    // The longest valid prefix: stop at the first record that is incomplete,
    // over the cap, or fails its checksum — whatever its position, since an
    // unsynced append can leave a durable size with zeros behind it.
    let mut records: Vec<(u64, &[u8])> = Vec::new();
    let mut pos = HEADER_LEN;
    while pos < buf.len() {
        let rest = &buf[pos..];
        if rest.len() < RECORD_OVERHEAD {
            break;
        }
        let start = be_u64(rest);
        let len = be_u32(&rest[8..]) as usize;
        if len > SCROLLBACK_CAP_BYTES || RECORD_OVERHEAD + len > rest.len() {
            break;
        }
        let total = RECORD_OVERHEAD + len;
        let bytes = &rest[12..12 + len];
        let sum = be_u64(&rest[12 + len..]);
        if sum != fnv1a64(&[&rest[..8], &rest[8..12], bytes]) {
            break;
        }
        // A record that passes its checksum is one the writer wrote whole;
        // out of order, it is not a crash artefact.
        if let Some(&(prev_start, prev)) = records.last() {
            if prev_start + prev.len() as u64 != start {
                return corrupt("records are not contiguous");
            }
        }
        if start.checked_add(len as u64).is_none() {
            return corrupt("offset overflows");
        }
        records.push((start, bytes));
        pos += total;
    }
    let Some(&(last_start, last)) = records.last() else {
        // Every file starts as a synced rewrite holding one record, so a
        // file without a whole one is not a crash artefact.
        return corrupt("no complete record");
    };
    let end = last_start + last.len() as u64;
    let start_offset = end
        - end
            .saturating_sub(records[0].0)
            .min(SCROLLBACK_CAP_BYTES as u64);
    let mut bytes = Vec::with_capacity((end - start_offset) as usize);
    for (s, b) in records {
        let rec_end = s + b.len() as u64;
        if rec_end <= start_offset {
            continue;
        }
        let skip = start_offset.saturating_sub(s) as usize;
        bytes.extend_from_slice(&b[skip..]);
    }
    Ok(Scrollback {
        child_pid,
        start_offset,
        bytes,
        discarded_bytes: (buf.len() - pos) as u64,
    })
}

/// Why a persisted tail could not be read.
#[derive(Debug)]
pub enum ScrollbackError {
    /// The open or read failed, or the path is not a regular file this user
    /// owns (`PermissionDenied`, or `ELOOP` for a symlink on Unix).
    Io(io::Error),
    /// The file is larger than any file this crate writes; it was not read.
    TooLarge { len: u64 },
    /// The file has no valid tail: a damaged header, no whole first record,
    /// or two checksum-valid records out of order. (A torn or damaged record
    /// is NOT this: the reader returns the valid prefix before it and reports
    /// the rest in [`Scrollback::discarded_bytes`].)
    Corrupt(&'static str),
}

impl std::fmt::Display for ScrollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScrollbackError::Io(e) => write!(f, "scrollback: {e}"),
            ScrollbackError::TooLarge { len } => write!(
                f,
                "scrollback: {len} bytes exceeds the {MAX_SCROLLBACK_FILE_BYTES}-byte bound; not read"
            ),
            ScrollbackError::Corrupt(why) => write!(f, "scrollback: corrupt ({why})"),
        }
    }
}

impl std::error::Error for ScrollbackError {}

/// Read a pane's persisted tail. `Ok(None)`: the pane never persisted one.
/// Every other failure is an error, never an empty tail: a reader must not
/// mistake "unreadable" for "the pane printed nothing".
///
/// **Read it BEFORE spawning a new holder under the same pane id.** A fresh
/// holder's first write — within milliseconds of its start — replaces the
/// file with its own child's stream, starting at offset 0, and the
/// predecessor's tail is gone. So a consumer restoring a dead holder's pane
/// (the Phase 3 boot sweep, Phase 6 resume) reads first, then respawns; the
/// `child_pid` in the result says which stream the offsets belong to.
pub fn read_scrollback(
    pane_dir: &Path,
    pane: &PaneId,
) -> Result<Option<Scrollback>, ScrollbackError> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    let file = match crate::lock::open_checked(&mut opts, &scrollback_path(pane_dir, pane)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ScrollbackError::Io(e)),
    };
    let len = file.metadata().map_err(ScrollbackError::Io)?.len();
    if len > MAX_SCROLLBACK_FILE_BYTES {
        return Err(ScrollbackError::TooLarge { len });
    }
    let mut buf = Vec::with_capacity(len as usize);
    // `take` re-bounds the read: a live holder may append after `metadata`.
    file.take(MAX_SCROLLBACK_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(ScrollbackError::Io)?;
    if buf.len() as u64 > MAX_SCROLLBACK_FILE_BYTES {
        return Err(ScrollbackError::TooLarge {
            len: buf.len() as u64,
        });
    }
    decode(&buf).map(Some)
}

/// What one tick will write.
#[derive(Debug, PartialEq, Eq)]
pub enum Pending {
    /// Append the output in `[start, start + bytes.len())` to the current
    /// file, as one record.
    Append { start: u64, bytes: Vec<u8> },
    /// Replace the file with one record holding the capped tail.
    Rewrite { start: u64, bytes: Vec<u8> },
}

/// The holder's writer of its pane's tail.
#[derive(Debug)]
pub struct ScrollbackWriter {
    path: PathBuf,
    tmp: PathBuf,
    child_pid: u32,
    /// The current file, open for appending; `None` before the first write
    /// and after any failed one — either way the next write is a rewrite.
    file: Option<File>,
    /// Bytes in the current file.
    file_len: u64,
    /// End offset of the output on disk — the change mark.
    persisted_end: u64,
    /// When the last rewrite was ATTEMPTED (successful or not).
    last_rewrite: Option<Instant>,
    /// Appends written since the last `fsync`, and when that sync was.
    unsynced: bool,
    last_sync: Instant,
    rewrite_min_interval: Duration,
    fsync_interval: Duration,
    /// Set by the final write; nothing touches the file after it.
    closed: bool,
    /// The last write failed; the next success is logged as a recovery.
    failing: bool,
}

/// Why the writer wrote nothing.
fn closed_error() -> io::Error {
    io::Error::other("scrollback writer is closed; the pane id may belong to a successor")
}

impl ScrollbackWriter {
    pub fn new(pane_dir: &Path, pane: &PaneId, child_pid: u32) -> Self {
        let path = scrollback_path(pane_dir, pane);
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        ScrollbackWriter {
            path,
            tmp: PathBuf::from(tmp),
            child_pid,
            file: None,
            file_len: 0,
            persisted_end: 0,
            last_rewrite: None,
            unsynced: false,
            last_sync: Instant::now(),
            rewrite_min_interval: REWRITE_MIN_INTERVAL,
            fsync_interval: FSYNC_INTERVAL,
            closed: false,
            failing: false,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// What to write for `ring` now, or `None`: closed, unchanged (the
    /// change mark), or a rewrite deferred by the flood bound. `force` (the
    /// final write) never defers. No I/O; it copies only the new bytes for an
    /// append, at most [`SCROLLBACK_CAP_BYTES`] for a rewrite — cheap under
    /// the pane lock.
    pub fn pending(&self, ring: &OutputRing, force: bool) -> Option<Pending> {
        if self.closed {
            return None;
        }
        let end = ring.end_offset();
        if self.file.is_some() {
            if self.persisted_end == end {
                return None;
            }
            let new = end - self.persisted_end;
            let fits = self.persisted_end >= ring.start_offset()
                && new <= SCROLLBACK_CAP_BYTES as u64
                && self.file_len + RECORD_OVERHEAD as u64 + new <= MAX_SCROLLBACK_FILE_BYTES;
            if fits {
                let (start, bytes) = ring.slice_from(self.persisted_end, new as usize);
                return Some(Pending::Append { start, bytes });
            }
        }
        let deferred = self
            .last_rewrite
            .is_some_and(|t| t.elapsed() < self.rewrite_min_interval);
        if deferred && !force {
            return None;
        }
        let from = end.saturating_sub(SCROLLBACK_CAP_BYTES as u64);
        let (start, bytes) = ring.slice_from(from, SCROLLBACK_CAP_BYTES);
        Some(Pending::Rewrite { start, bytes })
    }

    /// Write `p`. On success the change mark moves; on failure the file is
    /// dropped, so the next write is a rewrite (never an append after a
    /// possibly torn one) and no `.tmp` is left behind. A closed writer
    /// refuses without touching anything.
    pub fn apply(&mut self, p: Pending) -> io::Result<()> {
        if self.closed {
            return Err(closed_error());
        }
        let result = match &p {
            Pending::Append { start, bytes } => self.append(*start, bytes),
            Pending::Rewrite { start, bytes } => {
                self.last_rewrite = Some(Instant::now());
                self.rewrite(*start, bytes)
            }
        };
        if result.is_err() {
            self.file = None;
            let _ = std::fs::remove_file(&self.tmp);
        }
        result
    }

    fn append(&mut self, start: u64, bytes: &[u8]) -> io::Result<()> {
        let Some(f) = self.file.as_mut() else {
            return Err(io::Error::other("no current file to append to"));
        };
        let mut rec = Vec::with_capacity(RECORD_OVERHEAD + bytes.len());
        encode_record(&mut rec, start, bytes);
        f.write_all(&rec)?;
        self.file_len += rec.len() as u64;
        self.persisted_end = start + bytes.len() as u64;
        self.unsynced = true;
        Ok(())
    }

    fn rewrite(&mut self, start: u64, bytes: &[u8]) -> io::Result<()> {
        let mut out = encode_header(self.child_pid);
        encode_record(&mut out, start, bytes);
        // A `.tmp` left by a holder killed mid-rewrite is garbage: drop it,
        // then create exclusively so the open can never follow a planted link.
        match std::fs::remove_file(&self.tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&self.tmp)?;
        f.write_all(&out)?;
        // The data must be durable BEFORE the rename makes it the tail: a
        // rename that reaches disk ahead of the data is a torn file after a
        // power loss.
        f.sync_data()?;
        drop(f);
        // Let go of the old file's append handle BEFORE the rename: Windows
        // refuses to replace a file that is open without delete sharing, and
        // a failed rename must not leave appends going to the old file.
        self.file = None;
        std::fs::rename(&self.tmp, &self.path)?;
        // Make the rename itself durable. Best effort: if the directory
        // cannot be synced, the file on disk is still one whole tail — the
        // previous one or this one.
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
        }
        // Later appends go to the file just installed, opened with the same
        // checks as every other pane file.
        let mut opts = OpenOptions::new();
        opts.append(true);
        self.file = Some(crate::lock::open_checked(&mut opts, &self.path)?);
        self.file_len = out.len() as u64;
        self.persisted_end = start + bytes.len() as u64;
        self.unsynced = false;
        self.last_sync = Instant::now();
        Ok(())
    }

    /// `fsync` pending appends once [`FSYNC_INTERVAL`] has passed since the
    /// last sync (`force`: now).
    pub fn sync_if_due(&mut self, force: bool) -> io::Result<()> {
        if !self.unsynced || (!force && self.last_sync.elapsed() < self.fsync_interval) {
            return Ok(());
        }
        let Some(f) = self.file.as_ref() else {
            return Ok(());
        };
        if let Err(e) = f.sync_data() {
            // The kernel may have dropped the unsynced pages: appending after
            // them could leave a hole mid-file. Drop the handle, so the next
            // write is a rewrite of the whole tail.
            self.file = None;
            return Err(e);
        }
        self.unsynced = false;
        self.last_sync = Instant::now();
        Ok(())
    }

    /// The final write: persist whatever `ring` has that the file does not
    /// (never deferred), sync it, and CLOSE — after this the writer touches
    /// nothing, whatever a late timer tick asks.
    pub fn finish(&mut self, ring: &OutputRing) -> io::Result<()> {
        let result = match self.pending(ring, true) {
            Some(p) => self.apply(p),
            None => Ok(()),
        }
        .and_then(|()| self.sync_if_due(true));
        self.closed = true;
        self.file = None;
        result
    }

    fn note(&mut self, log: &Path, result: io::Result<()>) {
        match result {
            Ok(()) if self.failing => {
                self.failing = false;
                crate::server::log_line(log, "scrollback: writing again");
            }
            Ok(()) => {}
            Err(e) if !self.failing => {
                self.failing = true;
                crate::server::log_line(log, &format!("scrollback: write failed, will retry: {e}"));
            }
            Err(_) => {}
        }
    }
}

/// One timer tick: persist the pane's new output, if any, and sync when due.
/// The pane lock is held only to copy bytes; the file I/O happens after it is
/// released. Returns `false` once the writer is closed (the timer then
/// stops). A failure is logged once (and its recovery once), never fatal: the
/// pane is worth more than its durable copy.
pub fn persist_tick(pane: &Pane, writer: &Mutex<ScrollbackWriter>, log: &Path) -> bool {
    let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
    if w.is_closed() {
        return false;
    }
    let pending = {
        let st = pane.lock_state();
        w.pending(&st.ring, false)
    };
    let result = match pending {
        Some(p) => w.apply(p),
        None => Ok(()),
    }
    .and_then(|()| w.sync_if_due(false));
    w.note(log, result);
    true
}

/// The holder's last write ([`ScrollbackWriter::finish`]), under the writer
/// mutex: a timer tick in flight completes first, and every later one finds
/// the writer closed. Called before the holder releases its pane lock.
pub fn persist_final(pane: &Pane, writer: &Mutex<ScrollbackWriter>, log: &Path) {
    let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
    let result = {
        let st = pane.lock_state();
        w.finish(&st.ring)
    };
    if let Err(e) = result {
        crate::server::log_line(log, &format!("scrollback: final write failed: {e}"));
    }
}

/// Logs, once, a timer thread that ends by panicking — the tail is then
/// written only by [`persist_final`], and that must not be silent.
struct PanicNotice(PathBuf);

impl Drop for PanicNotice {
    fn drop(&mut self) {
        if std::thread::panicking() {
            crate::server::log_line(
                &self.0,
                "scrollback: timer thread panicked; the tail is now written only when the holder ends",
            );
        }
    }
}

/// Start the timer thread: [`persist_tick`] now, then every
/// [`SCROLLBACK_INTERVAL`], until the writer is closed.
pub fn spawn_persister(
    pane: Arc<Pane>,
    writer: Arc<Mutex<ScrollbackWriter>>,
    log: PathBuf,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("pty-holder-scrollback".into())
        .spawn(move || {
            let _notice = PanicNotice(log.clone());
            while persist_tick(&pane, &writer, &log) {
                std::thread::sleep(SCROLLBACK_INTERVAL);
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pty-holder-sb-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::pane::prepare_private_dir(&d).unwrap();
        d
    }

    fn pane() -> PaneId {
        PaneId::new("p1").unwrap()
    }

    fn writer(dir: &Path) -> ScrollbackWriter {
        ScrollbackWriter::new(dir, &pane(), 4242)
    }

    /// One timer tick against `ring`; what it decided.
    fn tick(w: &mut ScrollbackWriter, ring: &OutputRing) -> Option<&'static str> {
        let p = w.pending(ring, false)?;
        let kind = match p {
            Pending::Append { .. } => "append",
            Pending::Rewrite { .. } => "rewrite",
        };
        w.apply(p).unwrap();
        Some(kind)
    }

    fn read(dir: &Path) -> Scrollback {
        read_scrollback(dir, &pane()).unwrap().unwrap()
    }

    fn file_len(dir: &Path) -> u64 {
        std::fs::metadata(scrollback_path(dir, &pane()))
            .unwrap()
            .len()
    }

    #[test]
    fn pty_holder_scrollback_cap_enforced() {
        let dir = tmpdir("cap");
        let mut ring = OutputRing::new(4 * SCROLLBACK_CAP_BYTES);
        let all: Vec<u8> = (0..3 * SCROLLBACK_CAP_BYTES)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut w = writer(&dir);
        // A first write of more than the cap keeps only the cap.
        ring.push(&all[..SCROLLBACK_CAP_BYTES + SCROLLBACK_CAP_BYTES / 2]);
        assert_eq!(tick(&mut w, &ring), Some("rewrite"));
        let sb = read(&dir);
        assert_eq!(sb.bytes.len(), SCROLLBACK_CAP_BYTES);
        assert_eq!(sb.end_offset(), ring.end_offset());
        // Appends accumulate past the cap: the reader still gets only the
        // cap, and the file never exceeds its bound.
        w.rewrite_min_interval = Duration::ZERO;
        let mut pushed = SCROLLBACK_CAP_BYTES + SCROLLBACK_CAP_BYTES / 2;
        for chunk in all[pushed..].chunks(200 * 1024) {
            ring.push(chunk);
            pushed += chunk.len();
            tick(&mut w, &ring);
            assert!(file_len(&dir) <= MAX_SCROLLBACK_FILE_BYTES);
            let sb = read(&dir);
            assert_eq!(sb.bytes.len(), SCROLLBACK_CAP_BYTES);
            assert_eq!(sb.end_offset(), pushed as u64);
            assert_eq!(sb.bytes[..], all[pushed - SCROLLBACK_CAP_BYTES..pushed]);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_offset_anchor_round_trips() {
        let dir = tmpdir("anchor");
        // A small ring that has evicted: its tail starts past offset 0.
        let mut ring = OutputRing::new(4096);
        for i in 0..1000u32 {
            ring.push(format!("line {i:04} of output\r\n").as_bytes());
        }
        assert!(ring.start_offset() > 0);
        let mut w = writer(&dir);
        tick(&mut w, &ring);
        let sb = read(&dir);
        assert_eq!(sb.child_pid, 4242);
        assert_eq!(
            (sb.start_offset, sb.bytes.clone()),
            ring.slice_from(0, usize::MAX)
        );
        // Appended records keep the same coordinates.
        ring.push(b"one more line\r\n");
        assert_eq!(tick(&mut w, &ring), Some("append"));
        let sb = read(&dir);
        assert_eq!(sb.end_offset(), ring.end_offset());
        assert!(sb.bytes.ends_with(b"one more line\r\n"));
        // The empty tail of a fresh pane anchors at 0 and round-trips too.
        let dir2 = tmpdir("anchor0");
        tick(&mut writer(&dir2), &OutputRing::new(4096));
        let sb = read(&dir2);
        assert_eq!((sb.start_offset, sb.bytes.len()), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn pty_holder_scrollback_no_write_when_unchanged() {
        let dir = tmpdir("mark");
        let mut ring = OutputRing::new(4096);
        ring.push(b"abc");
        let mut w = writer(&dir);
        assert_eq!(tick(&mut w, &ring), Some("rewrite"));
        let len = file_len(&dir);
        assert_eq!(tick(&mut w, &ring), None, "unchanged: no write");
        assert_eq!(file_len(&dir), len);
        ring.push(b"def");
        assert_eq!(tick(&mut w, &ring), Some("append"));
        assert_eq!(read(&dir).bytes, b"abcdef");
        assert_eq!(tick(&mut w, &ring), None);

        // A failed write does NOT move the mark, leaves no .tmp, and the
        // retry is a REWRITE. A non-empty directory at the target makes the
        // rewrite's rename fail.
        let target = scrollback_path(&dir, &pane());
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("x"), b"").unwrap();
        // Take the rewrite path, as after a failed append.
        w.file = None;
        w.rewrite_min_interval = Duration::ZERO;
        ring.push(b"ghi");
        let p = w.pending(&ring, false).unwrap();
        assert!(matches!(p, Pending::Rewrite { .. }));
        assert!(w.apply(p).is_err());
        assert!(!w.tmp.exists(), "a failed write leaves no .tmp");
        // The retry is rate-limited like any deferrable rewrite...
        w.rewrite_min_interval = REWRITE_MIN_INTERVAL;
        assert!(w.pending(&ring, false).is_none());
        // ...and comes once the interval has passed.
        w.rewrite_min_interval = Duration::ZERO;
        assert!(matches!(
            w.pending(&ring, false),
            Some(Pending::Rewrite { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_busy_tui_appends_only_its_new_bytes() {
        // A spinner: ~100 bytes per tick. Each tick writes one small record,
        // never the 1 MiB tail — the file grows by exactly the new bytes plus
        // the record framing.
        let dir = tmpdir("spinner");
        let mut ring = OutputRing::new(2 * SCROLLBACK_CAP_BYTES);
        ring.push(&vec![b'x'; SCROLLBACK_CAP_BYTES]);
        let mut w = writer(&dir);
        assert_eq!(tick(&mut w, &ring), Some("rewrite"));
        let base = file_len(&dir);
        for i in 0..50u64 {
            ring.push(&[b'|'; 100]);
            assert_eq!(tick(&mut w, &ring), Some("append"));
            assert_eq!(
                file_len(&dir),
                base + (i + 1) * (100 + RECORD_OVERHEAD as u64)
            );
        }
        let sb = read(&dir);
        assert_eq!(sb.bytes.len(), SCROLLBACK_CAP_BYTES);
        assert_eq!(sb.end_offset(), ring.end_offset());
        assert!(sb.bytes.ends_with(&[b'|'; 100]));

        // Once appends would pass the file bound, a compaction rewrite
        // brings it back to one capped record.
        w.rewrite_min_interval = Duration::ZERO;
        let mut kinds = Vec::new();
        while kinds.last() != Some(&"rewrite") {
            ring.push(&[b'-'; 64 * 1024]);
            kinds.push(tick(&mut w, &ring).unwrap());
            assert!(file_len(&dir) <= MAX_SCROLLBACK_FILE_BYTES);
        }
        assert_eq!(
            file_len(&dir),
            (HEADER_LEN + RECORD_OVERHEAD + SCROLLBACK_CAP_BYTES) as u64
        );
        assert_eq!(read(&dir).end_offset(), ring.end_offset());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_flood_rewrites_are_rate_limited() {
        let dir = tmpdir("flood");
        let mut ring = OutputRing::new(4 * SCROLLBACK_CAP_BYTES);
        let mut w = writer(&dir);
        ring.push(b"start");
        assert_eq!(
            tick(&mut w, &ring),
            Some("rewrite"),
            "the first write never waits"
        );
        // More than an append may carry: a rewrite is needed, and it is
        // DEFERRED inside the interval — the tick writes nothing.
        ring.push(&vec![b'f'; SCROLLBACK_CAP_BYTES + 1]);
        let len = file_len(&dir);
        assert_eq!(tick(&mut w, &ring), None);
        assert_eq!(file_len(&dir), len);
        // The final write is never deferred.
        w.finish(&ring).unwrap();
        let sb = read(&dir);
        assert_eq!(sb.end_offset(), ring.end_offset());
        assert_eq!(sb.bytes.len(), SCROLLBACK_CAP_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_closed_writer_never_touches_a_successors_files() {
        let dir = tmpdir("closed");
        let mut ring = OutputRing::new(4096);
        ring.push(b"predecessor");
        let mut w = writer(&dir);
        tick(&mut w, &ring);
        w.finish(&ring).unwrap();
        assert!(w.is_closed());
        // A successor holder now owns the pane id: its tail and its
        // in-flight `.tmp`.
        let target = scrollback_path(&dir, &pane());
        let mut succ = ScrollbackWriter::new(&dir, &pane(), 9999);
        let mut succ_ring = OutputRing::new(4096);
        succ_ring.push(b"successor");
        tick(&mut succ, &succ_ring);
        let succ_file = std::fs::read(&target).unwrap();
        std::fs::write(&w.tmp, b"successor in-flight tmp").unwrap();
        // A late tick of the closed writer, with new output: nothing.
        ring.push(b" late output");
        assert!(w.pending(&ring, false).is_none());
        assert!(w.pending(&ring, true).is_none());
        let forced = Pending::Rewrite {
            start: 0,
            bytes: b"x".to_vec(),
        };
        assert!(w.apply(forced).is_err());
        assert!(w.finish(&ring).is_ok());
        assert_eq!(std::fs::read(&target).unwrap(), succ_file);
        assert_eq!(std::fs::read(&w.tmp).unwrap(), b"successor in-flight tmp");
        assert_eq!(read(&dir).child_pid, 9999);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_stale_tmp_is_ignored_and_cleaned_by_the_next_rewrite() {
        let dir = tmpdir("tmp");
        let mut ring = OutputRing::new(4096);
        ring.push(b"first complete tail");
        tick(&mut writer(&dir), &ring);
        // What a holder killed mid-rewrite leaves: a partial `.tmp`, never
        // renamed. Readers do not look at it.
        let w = writer(&dir);
        std::fs::write(&w.tmp, b"QPTYSCRL half a file").unwrap();
        assert_eq!(read(&dir).bytes, b"first complete tail");
        // The next holder's first write (a rewrite) replaces it and lands.
        ring.push(b" + more");
        tick(&mut writer(&dir), &ring);
        assert_eq!(read(&dir).bytes, b"first complete tail + more");
        assert!(!w.tmp.exists(), "the stale .tmp was consumed by the rename");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file of three records: "synced part", " appended", " more".
    fn three_records(dir: &Path) -> Vec<u8> {
        let mut ring = OutputRing::new(4096);
        ring.push(b"synced part");
        let mut w = writer(dir);
        tick(&mut w, &ring);
        ring.push(b" appended");
        assert_eq!(tick(&mut w, &ring), Some("append"));
        ring.push(b" more");
        assert_eq!(tick(&mut w, &ring), Some("append"));
        std::fs::read(scrollback_path(dir, &pane())).unwrap()
    }

    #[test]
    fn pty_holder_scrollback_reads_the_longest_valid_prefix_and_says_so() {
        let dir = tmpdir("prefix");
        let whole = three_records(&dir);
        let target = scrollback_path(&dir, &pane());
        let read_back = |bytes: &[u8]| {
            std::fs::write(&target, bytes).unwrap();
            read(&dir)
        };
        let sb = read_back(&whole);
        assert_eq!(sb.bytes, b"synced part appended more");
        assert!(!sb.is_torn());

        // Power loss after an unsynced append: the size became durable, the
        // data did not — zeros after the last whole record.
        let mut zeros = whole.clone();
        zeros.extend(std::iter::repeat_n(0u8, 200));
        let sb = read_back(&zeros);
        assert_eq!(sb.bytes, b"synced part appended more");
        assert_eq!(sb.discarded_bytes, 200);
        assert_eq!(sb.end_offset(), 25);

        // A crash mid-append: the last record cut short, or whole in length
        // with damaged bytes. The tail as of the previous record.
        let last = RECORD_OVERHEAD + 5;
        for torn in [
            whole[..whole.len() - 3].to_vec(),
            whole[..whole.len() - last + 4].to_vec(),
            {
                let mut b = whole.clone();
                let n = b.len();
                b[n - 10] ^= 0xFF;
                b
            },
        ] {
            let sb = read_back(&torn);
            assert_eq!(sb.bytes, b"synced part appended");
            assert_eq!(
                sb.discarded_bytes,
                (torn.len() - (whole.len() - last)) as u64
            );
        }

        // Damage in a MIDDLE record — its checksum, or its length field made
        // large (over the cap, or merely past end-of-file) — stops the walk
        // there too, under the same rule: the prefix before it, and every
        // byte from it on counted as discarded, never silently.
        let second = HEADER_LEN + RECORD_OVERHEAD + 11;
        let after_first = (whole.len() - second) as u64;
        let mut bad_sum = whole.clone();
        bad_sum[second + 12] ^= 0xFF;
        let mut over_cap = whole.clone();
        over_cap[second + 8..second + 12]
            .copy_from_slice(&((SCROLLBACK_CAP_BYTES as u32) + 1).to_be_bytes());
        let mut past_eof = whole.clone();
        past_eof[second + 8..second + 12].copy_from_slice(&1000u32.to_be_bytes());
        for damaged in [bad_sum, over_cap, past_eof] {
            let sb = read_back(&damaged);
            assert_eq!(sb.bytes, b"synced part");
            assert_eq!(sb.discarded_bytes, after_first);
            assert!(sb.is_torn());
        }

        // The FIRST record is always a synced rewrite: without it there is no
        // valid prefix at all.
        let mut first = whole.clone();
        first[HEADER_LEN + 12] ^= 0xFF;
        std::fs::write(&target, &first).unwrap();
        assert!(matches!(
            read_scrollback(&dir, &pane()),
            Err(ScrollbackError::Corrupt(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_scrollback_failed_fsync_forces_a_rewrite() {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tmpdir("fsync");
        let mut ring = OutputRing::new(4096);
        ring.push(b"abc");
        let mut w = writer(&dir);
        tick(&mut w, &ring);
        ring.push(b"def");
        assert_eq!(tick(&mut w, &ring), Some("append"));
        // An O_PATH descriptor refuses fsync (EBADF): a stand-in for a disk
        // that fails it.
        let mut opts = OpenOptions::new();
        opts.read(true).custom_flags(libc::O_PATH);
        w.file = Some(opts.open(scrollback_path(&dir, &pane())).unwrap());
        assert!(w.sync_if_due(true).is_err());
        assert!(w.file.is_none(), "a failed fsync drops the append handle");
        w.rewrite_min_interval = Duration::ZERO;
        ring.push(b"ghi");
        assert_eq!(tick(&mut w, &ring), Some("rewrite"));
        assert_eq!(read(&dir).bytes, b"abcdefghi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn pty_holder_scrollback_file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir("mode");
        // Even over a pre-existing, wider file: the rename installs a new one.
        let target = scrollback_path(&dir, &pane());
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut ring = OutputRing::new(4096);
        ring.push(b"x");
        let mut w = writer(&dir);
        tick(&mut w, &ring);
        ring.push(b"y");
        assert_eq!(tick(&mut w, &ring), Some("append"));
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_corrupt_or_oversized_file_rejected() {
        let dir = tmpdir("corrupt");
        let target = scrollback_path(&dir, &pane());
        assert!(
            read_scrollback(&dir, &pane()).unwrap().is_none(),
            "absent is None"
        );

        let mut good = encode_header(7);
        encode_record(&mut good, 100, b"hello");
        encode_record(&mut good, 105, b" world");
        std::fs::write(&target, &good).unwrap();
        let sb = read(&dir);
        assert_eq!((sb.child_pid, sb.start_offset), (7, 100));
        assert_eq!(sb.bytes, b"hello world");

        let gap = {
            let mut b = encode_header(7);
            encode_record(&mut b, 100, b"hello");
            encode_record(&mut b, 200, b"gap");
            b
        };
        let over_cap = {
            let mut b = good.clone();
            b[HEADER_LEN + 8..HEADER_LEN + 12]
                .copy_from_slice(&((SCROLLBACK_CAP_BYTES as u32) + 1).to_be_bytes());
            b.extend(std::iter::repeat_n(0u8, SCROLLBACK_CAP_BYTES + 64));
            b
        };
        let corrupt_cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("header only", encode_header(7)),
            ("bad magic", {
                let mut b = good.clone();
                b[0] = b'X';
                b
            }),
            ("future format", {
                let mut b = good.clone();
                b[11] = 9;
                b
            }),
            ("header checksum", {
                let mut b = good.clone();
                b[13] ^= 1;
                b
            }),
            ("non-contiguous records", gap),
            ("a record over the cap", over_cap),
        ];
        for (name, bytes) in corrupt_cases {
            std::fs::write(&target, &bytes).unwrap();
            match read_scrollback(&dir, &pane()) {
                Err(ScrollbackError::Corrupt(_)) => {}
                other => panic!("{name}: expected Corrupt, got {other:?}"),
            }
        }

        // Oversized: refused on its length, before it is read.
        let big = vec![0u8; MAX_SCROLLBACK_FILE_BYTES as usize + 1];
        std::fs::write(&target, &big).unwrap();
        assert!(matches!(
            read_scrollback(&dir, &pane()),
            Err(ScrollbackError::TooLarge { .. })
        ));

        // Not a regular file / a symlink: refused, never followed.
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        assert!(matches!(
            read_scrollback(&dir, &pane()),
            Err(ScrollbackError::Io(_))
        ));
        #[cfg(unix)]
        {
            std::fs::remove_dir(&target).unwrap();
            let elsewhere = dir.join("elsewhere");
            std::fs::write(&elsewhere, &good).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
            assert!(matches!(
                read_scrollback(&dir, &pane()),
                Err(ScrollbackError::Io(_))
            ));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
