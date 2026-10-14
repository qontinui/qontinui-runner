//! The pane's DURABLE scrollback tail: the newest bytes of its output ring,
//! on disk, anchored to their absolute offset.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 4, decision D7 — *"a feature that exists to survive an unscheduled
//! event cannot be opt-in"*. The holder's [`OutputRing`] survives a runner
//! crash (the holder outlives the runner, D13), but not the holder's own death
//! or a host reboot; the runner's only disk copy today is written by the
//! webview on a GRACEFUL close, so the crash it exists for loses everything.
//! This module is the holder writing its own tail, always, with no switch:
//!
//! - **What** — the last [`SCROLLBACK_CAP_BYTES`] of the ring (less while the
//!   ring holds less), the ABSOLUTE offset of its first byte, and the child pid
//!   whose stream those offsets count. The offset is what keeps a restore
//!   gap-honest: a reader that last saw offset `N` and reads a tail starting
//!   at `S > N` knows exactly that `[N, S)` is lost — the same coordinates
//!   as the ring, `output_lost` and the runner's `splice_replay`.
//! - **Where** — `<pane-dir>/<pane-id>.scrollback` ([`crate::pane::scrollback_path`]),
//!   mode 0600 inside the 0700 pane directory (Unix). Windows: the file
//!   inherits the per-user ACL of the pane directory, which is the same
//!   barrier every other pane file (lock, spec, log) relies on there.
//! - **How** — write-then-rename: the bytes go to `<pane-id>.scrollback.tmp`,
//!   created exclusively (`O_EXCL`, so never through a planted symlink),
//!   `fsync`ed, then renamed over the target. A holder killed at any instant
//!   leaves either the previous complete file or the new complete file —
//!   never a torn one. A stale `.tmp` from such a kill is removed by the next
//!   write. Every file also carries a checksum, so a torn file from some
//!   other cause is REFUSED by the reader, never replayed.
//! - **When** — on a timer ([`SCROLLBACK_INTERVAL`]) with a change mark: a
//!   tick whose ring end offset equals the last persisted one writes nothing.
//!   An idle pane costs no disk I/O; a streaming one costs at most one
//!   capped write per interval (≤ 1 MiB / 5 s). Plus one final write when the
//!   holder ends ([`crate::server::Holder::run`]), so the child's last output
//!   is on disk before the lock is released.
//!
//! The READ side is [`read_scrollback`]: bounded (a file larger than
//! [`MAX_SCROLLBACK_FILE_BYTES`] is refused before it is read), checked (the
//! same no-symlink / regular-file / our-uid open as the lock), and validated
//! (magic, format, declared length, checksum). It is for the runner to
//! restore a pane whose holder is gone; Phase 4 ships it with no production
//! caller — the runner-side consumers are the boot sweep (Phase 3) and resume
//! after holder death (Phase 6).
//!
//! The file is never removed by the holder: outliving the holder is its whole
//! purpose. Reaping it belongs with the lock file's, in the boot sweep.
//!
//! On-disk layout, all integers big-endian:
//!
//! | bytes | field |
//! |---|---|
//! | 8 | magic `QPTYSCRL` |
//! | 4 | format ([`SCROLLBACK_FORMAT`]) |
//! | 4 | child pid |
//! | 8 | written at, Unix ms |
//! | 8 | absolute offset of the first tail byte |
//! | 4 | tail length `n` (≤ [`SCROLLBACK_CAP_BYTES`]) |
//! | n | the tail, raw bytes |
//! | 8 | FNV-1a 64 over every byte above |
//!
//! DATA-PATH module: `source_guard` bans text decoding here.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::pane::{scrollback_path, PaneId};
use crate::pty::Pane;
use crate::ring::OutputRing;

/// Most tail bytes persisted. Half the default ring
/// (`crate::spec::DEFAULT_RING_CAPACITY`): enough screens of context to
/// restore a pane, small enough that a streaming pane's timer write stays
/// cheap.
pub const SCROLLBACK_CAP_BYTES: usize = 1024 * 1024;

/// How often the holder checks whether its tail changed and, if so, writes
/// it. A holder killed without warning loses at most this much output from
/// the durable copy — and the offset anchor says exactly how much.
pub const SCROLLBACK_INTERVAL: Duration = Duration::from_secs(5);

/// The on-disk format version this build writes and reads.
pub const SCROLLBACK_FORMAT: u32 = 1;

const MAGIC: &[u8; 8] = b"QPTYSCRL";
const HEADER_LEN: usize = 8 + 4 + 4 + 8 + 8 + 4;
const TRAILER_LEN: usize = 8;

/// Largest file [`read_scrollback`] will read: a full tail plus its framing.
/// A bound on what a corrupt or foreign file can make a reader allocate.
pub const MAX_SCROLLBACK_FILE_BYTES: u64 = (HEADER_LEN + SCROLLBACK_CAP_BYTES + TRAILER_LEN) as u64;

/// A persisted tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrollback {
    /// The child whose output stream the offsets count.
    pub child_pid: u32,
    pub written_at_unix_ms: u64,
    /// Absolute offset of `bytes[0]` in the child's output stream.
    pub start_offset: u64,
    pub bytes: Vec<u8>,
}

impl Scrollback {
    /// Absolute offset one past the last persisted byte.
    pub fn end_offset(&self) -> u64 {
        self.start_offset + self.bytes.len() as u64
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.bytes.len() + TRAILER_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&SCROLLBACK_FORMAT.to_be_bytes());
        out.extend_from_slice(&self.child_pid.to_be_bytes());
        out.extend_from_slice(&self.written_at_unix_ms.to_be_bytes());
        out.extend_from_slice(&self.start_offset.to_be_bytes());
        // The writer never takes more than the cap, which fits a u32.
        out.extend_from_slice(&(self.bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.bytes);
        let sum = fnv1a64(&out);
        out.extend_from_slice(&sum.to_be_bytes());
        out
    }

    fn decode(buf: &[u8]) -> Result<Scrollback, ScrollbackError> {
        let corrupt = |why: &'static str| Err(ScrollbackError::Corrupt(why));
        if buf.len() < HEADER_LEN + TRAILER_LEN {
            return corrupt("shorter than the header");
        }
        let (body, trailer) = buf.split_at(buf.len() - TRAILER_LEN);
        let (header, tail) = body.split_at(HEADER_LEN);
        if &header[..8] != MAGIC {
            return corrupt("bad magic");
        }
        let u32_at =
            |i: usize| u32::from_be_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
        let u64_at = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&header[i..i + 8]);
            u64::from_be_bytes(b)
        };
        if u32_at(8) != SCROLLBACK_FORMAT {
            return corrupt("unknown format version");
        }
        let child_pid = u32_at(12);
        let written_at_unix_ms = u64_at(16);
        let start_offset = u64_at(24);
        let len = u32_at(32) as usize;
        if len > SCROLLBACK_CAP_BYTES {
            return corrupt("declared tail exceeds the cap");
        }
        if len != tail.len() {
            return corrupt("declared tail length disagrees with the file length");
        }
        if start_offset.checked_add(len as u64).is_none() {
            return corrupt("offset overflows");
        }
        let mut sum = [0u8; 8];
        sum.copy_from_slice(trailer);
        if u64::from_be_bytes(sum) != fnv1a64(body) {
            return corrupt("checksum mismatch");
        }
        Ok(Scrollback {
            child_pid,
            written_at_unix_ms,
            start_offset,
            bytes: tail.to_vec(),
        })
    }
}

/// FNV-1a, 64-bit. Detects a torn or damaged file; it is not a MAC — the
/// file's integrity against other users is the 0600 mode in a 0700 directory.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Why a persisted tail could not be read.
#[derive(Debug)]
pub enum ScrollbackError {
    /// The open or read failed, or the path is not a regular file this user
    /// owns (`PermissionDenied`, or `ELOOP` for a symlink on Unix).
    Io(io::Error),
    /// The file is larger than any tail this crate writes; it was not read.
    TooLarge { len: u64 },
    /// The file is not a whole, valid tail.
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
pub fn read_scrollback(
    pane_dir: &Path,
    pane: &PaneId,
) -> Result<Option<Scrollback>, ScrollbackError> {
    read_scrollback_at(&scrollback_path(pane_dir, pane))
}

fn read_scrollback_at(path: &Path) -> Result<Option<Scrollback>, ScrollbackError> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    let file = match crate::lock::open_checked(&mut opts, path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ScrollbackError::Io(e)),
    };
    let len = file.metadata().map_err(ScrollbackError::Io)?.len();
    if len > MAX_SCROLLBACK_FILE_BYTES {
        return Err(ScrollbackError::TooLarge { len });
    }
    let mut buf = Vec::with_capacity(len as usize);
    // `take` re-bounds the read: the file may have grown since `metadata`.
    file.take(MAX_SCROLLBACK_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(ScrollbackError::Io)?;
    if buf.len() as u64 > MAX_SCROLLBACK_FILE_BYTES {
        return Err(ScrollbackError::TooLarge {
            len: buf.len() as u64,
        });
    }
    Scrollback::decode(&buf).map(Some)
}

/// The holder's writer of its pane's tail. Remembers the end offset it last
/// persisted — the change mark.
#[derive(Debug)]
pub struct ScrollbackWriter {
    path: PathBuf,
    tmp: PathBuf,
    child_pid: u32,
    /// End offset of the last tail that reached disk; `None` before the
    /// first, so a fresh holder writes once even with nothing produced yet
    /// (the file then names THIS child's stream, not a predecessor's).
    persisted_end: Option<u64>,
    /// The last write failed; the next success is logged as a recovery.
    failing: bool,
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
            persisted_end: None,
            failing: false,
        }
    }

    /// The ring's tail, or `None` when its end offset is the one already on
    /// disk (the change mark). Cheap to call under the pane lock: it copies at
    /// most [`SCROLLBACK_CAP_BYTES`] and does no I/O.
    pub fn snapshot_if_changed(&self, ring: &OutputRing) -> Option<Scrollback> {
        let end = ring.end_offset();
        if self.persisted_end == Some(end) {
            return None;
        }
        let from = end.saturating_sub(SCROLLBACK_CAP_BYTES as u64);
        let (start_offset, bytes) = ring.slice_from(from, SCROLLBACK_CAP_BYTES);
        Some(Scrollback {
            child_pid: self.child_pid,
            written_at_unix_ms: now_unix_ms(),
            start_offset,
            bytes,
        })
    }

    /// Write `tail` atomically (see the module docs). On success the change
    /// mark moves to its end; on failure it does not, so the next tick
    /// retries, and no `.tmp` is left behind.
    pub fn write(&mut self, tail: &Scrollback) -> io::Result<()> {
        let result = self.write_atomic(&tail.encode());
        if result.is_ok() {
            self.persisted_end = Some(tail.end_offset());
        } else {
            let _ = std::fs::remove_file(&self.tmp);
        }
        result
    }

    fn write_atomic(&self, bytes: &[u8]) -> io::Result<()> {
        // A `.tmp` left by a holder killed mid-write is garbage: drop it, then
        // create exclusively so the open can never follow a planted link.
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
        f.write_all(bytes)?;
        // The data must be durable BEFORE the rename makes it the tail: a
        // rename that reaches disk ahead of the data is a torn file after a
        // power loss.
        f.sync_data()?;
        drop(f);
        std::fs::rename(&self.tmp, &self.path)?;
        // Make the rename itself durable. Best effort: if the directory
        // cannot be synced, the file on disk is still one whole tail — the
        // previous one or this one.
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One timer tick: persist the pane's tail if it changed. The pane lock is
/// held only to copy the tail; the file I/O happens after it is released.
/// A failure is logged once (and its recovery once), never fatal: the pane is
/// worth more than its durable copy.
pub fn persist_tick(pane: &Pane, writer: &Mutex<ScrollbackWriter>, log: &Path) {
    let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
    let tail = {
        let st = pane.lock_state();
        w.snapshot_if_changed(&st.ring)
    };
    let Some(tail) = tail else {
        return;
    };
    match w.write(&tail) {
        Ok(()) if w.failing => {
            w.failing = false;
            crate::server::log_line(log, "scrollback: writing again");
        }
        Ok(()) => {}
        Err(e) if !w.failing => {
            w.failing = true;
            crate::server::log_line(log, &format!("scrollback: write failed, will retry: {e}"));
        }
        Err(_) => {}
    }
}

/// Start the timer thread: [`persist_tick`] now, then every
/// [`SCROLLBACK_INTERVAL`], for the life of the holder process.
pub fn spawn_persister(
    pane: Arc<Pane>,
    writer: Arc<Mutex<ScrollbackWriter>>,
    log: PathBuf,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("pty-holder-scrollback".into())
        .spawn(move || loop {
            persist_tick(&pane, &writer, &log);
            std::thread::sleep(SCROLLBACK_INTERVAL);
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Persist `ring` through a fresh writer and return it (for its mark).
    fn persist(dir: &Path, ring: &OutputRing) -> ScrollbackWriter {
        let mut w = ScrollbackWriter::new(dir, &pane(), 4242);
        let tail = w
            .snapshot_if_changed(ring)
            .expect("a fresh writer persists");
        w.write(&tail).unwrap();
        w
    }

    #[test]
    fn pty_holder_scrollback_cap_enforced() {
        let dir = tmpdir("cap");
        let mut ring = OutputRing::new(2 * SCROLLBACK_CAP_BYTES);
        let all: Vec<u8> = (0..SCROLLBACK_CAP_BYTES + SCROLLBACK_CAP_BYTES / 2)
            .map(|i| (i % 251) as u8)
            .collect();
        ring.push(&all);
        persist(&dir, &ring);
        let sb = read_scrollback(&dir, &pane()).unwrap().unwrap();
        assert_eq!(sb.bytes.len(), SCROLLBACK_CAP_BYTES);
        assert_eq!(sb.end_offset(), all.len() as u64);
        assert_eq!(sb.bytes[..], all[all.len() - SCROLLBACK_CAP_BYTES..]);
        let on_disk = std::fs::metadata(scrollback_path(&dir, &pane()))
            .unwrap()
            .len();
        assert_eq!(on_disk, MAX_SCROLLBACK_FILE_BYTES);
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
        persist(&dir, &ring);
        let sb = read_scrollback(&dir, &pane()).unwrap().unwrap();
        assert_eq!(sb.child_pid, 4242);
        assert_eq!(sb.start_offset, ring.start_offset());
        assert_eq!(sb.end_offset(), ring.end_offset());
        assert_eq!(
            (sb.start_offset, sb.bytes.clone()),
            ring.slice_from(0, usize::MAX)
        );
        // The empty tail of a fresh pane anchors at 0 and round-trips too.
        let dir2 = tmpdir("anchor0");
        persist(&dir2, &OutputRing::new(4096));
        let sb = read_scrollback(&dir2, &pane()).unwrap().unwrap();
        assert_eq!((sb.start_offset, sb.bytes.len()), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn pty_holder_scrollback_no_write_when_unchanged() {
        let dir = tmpdir("mark");
        let mut ring = OutputRing::new(4096);
        ring.push(b"abc");
        let mut w = persist(&dir, &ring);
        assert!(
            w.snapshot_if_changed(&ring).is_none(),
            "unchanged: no write"
        );
        ring.push(b"def");
        let tail = w.snapshot_if_changed(&ring).expect("changed: a write");
        assert_eq!(tail.bytes, b"abcdef");
        w.write(&tail).unwrap();
        assert!(w.snapshot_if_changed(&ring).is_none());

        // A failed write does NOT move the mark: the next tick retries. A
        // non-empty directory at the target makes the rename fail.
        let target = scrollback_path(&dir, &pane());
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("x"), b"").unwrap();
        ring.push(b"ghi");
        let tail = w.snapshot_if_changed(&ring).unwrap();
        assert!(w.write(&tail).is_err());
        assert!(!w.tmp.exists(), "a failed write leaves no .tmp");
        assert!(w.snapshot_if_changed(&ring).is_some(), "retried next tick");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_scrollback_atomic_rename_survives_interrupted_write() {
        let dir = tmpdir("atomic");
        let mut ring = OutputRing::new(4096);
        ring.push(b"first complete tail");
        let w = persist(&dir, &ring);
        // A holder killed mid-write leaves a partial `.tmp` and never renames:
        // the reader still sees the previous whole tail, never the fragment.
        let partial = Scrollback {
            child_pid: 4242,
            written_at_unix_ms: 1,
            start_offset: 0,
            bytes: b"second tail that never finished".to_vec(),
        }
        .encode();
        std::fs::write(&w.tmp, &partial[..partial.len() / 2]).unwrap();
        let sb = read_scrollback(&dir, &pane()).unwrap().unwrap();
        assert_eq!(sb.bytes, b"first complete tail");

        // The next holder's write replaces the stale `.tmp` and lands whole.
        ring.push(b" + more");
        persist(&dir, &ring);
        let sb = read_scrollback(&dir, &pane()).unwrap().unwrap();
        assert_eq!(sb.bytes, b"first complete tail + more");
        assert!(!w.tmp.exists(), "the stale .tmp was consumed by the rename");
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
        persist(&dir, &ring);
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

        let good = Scrollback {
            child_pid: 7,
            written_at_unix_ms: 9,
            start_offset: 100,
            bytes: b"hello".to_vec(),
        }
        .encode();
        std::fs::write(&target, &good).unwrap();
        assert_eq!(
            read_scrollback(&dir, &pane()).unwrap().unwrap().bytes,
            b"hello"
        );

        let corrupt_cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("truncated", good[..good.len() - 3].to_vec()),
            ("flipped tail byte", {
                let mut b = good.clone();
                b[HEADER_LEN] ^= 0xFF;
                b
            }),
            ("bad magic", {
                let mut b = good.clone();
                b[0] = b'X';
                b
            }),
            ("future format", {
                let mut b = good.clone();
                b[11] = 2;
                b
            }),
            ("declared length over the cap", {
                let mut b = good.clone();
                b[32..36].copy_from_slice(&((SCROLLBACK_CAP_BYTES as u32) + 1).to_be_bytes());
                b
            }),
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
