//! The per-pane advisory lock — the answer to "is a holder alive for this pane?"
//! that a socket probe cannot give (plan D3, per holder under D13).
//!
//! - A lock that can be ACQUIRED means no live holder: whatever wrote the file
//!   is dead, because the OS releases the lock when the process dies, however
//!   it dies.
//! - A lock that is HELD means a process is alive holding it. Whether it is a
//!   HEALTHY holder is a different question, answered only by a handshake
//!   (`client::probe`). A held lock with no answered handshake is UNKNOWN.
//!
//! The lock is taken BEFORE the endpoint is created, and the holder records its
//! pid, the protocol versions it speaks and a start time IN the lock file — a
//! lock says "taken" but not by whom.
//!
//! Mechanism:
//! - Unix: `flock(LOCK_EX | LOCK_NB)`. Per open file description, so it is not
//!   inherited across `exec` (std opens with `O_CLOEXEC`) and a PTY child the
//!   holder spawns in Phase 2 cannot keep a dead holder's lock alive.
//! - Windows: `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)`
//!   on ONE byte far beyond the end of the file. Windows byte-range locks are
//!   mandatory, so locking the record's own bytes would stop every prober from
//!   reading who holds it; a range past EOF is legal and blocks nothing a
//!   reader touches.
//!
//! **A held lock is not always a live holder** (coord finding `9bbe1079`, owned
//! by Phase 3). An `flock` belongs to an open file DESCRIPTION, and a process
//! forked between a lock file's open and its `exec` shares that description —
//! and so the lock — until the exec closes it; a prober (`client::probe`)
//! also holds a dead pane's lock for an instant. A holder starting at such a
//! moment used to exit `lock_held` against nobody. A starting holder therefore
//! takes its lock with [`PaneLock::try_acquire_unless_live`]: on `Held` it
//! retries with a short bounded backoff ([`TRANSIENT_HOLD_BACKOFF_MS`]) — but
//! ONLY while the lock's record names no live holder
//! ([`record_names_live_holder`]). A holder whose record names it live is
//! never retried against. One that holds the lock but has not yet WRITTEN its
//! record (it is starting) is retried against for those ~300 ms — harmlessly:
//! a held lock is never taken, so the retries can only end in `Held`.
//!
//! The lock file is never unlinked by a holder. Unlinking a lock file lets a
//! second holder lock a fresh file under the same name while a third still
//! holds the old one — two "exclusive" holders. Dead lock files are removed by
//! the boot sweep (`crate::sweep`, Phase 3) through [`PaneLock::remove`], and
//! that is made safe on both sides:
//! - Unix: every acquisition re-checks, after locking, that the path still
//!   names the file it locked (device and inode); a lock taken on a file that
//!   was unlinked meanwhile is dropped and retried (or, for a prober,
//!   reported `NotFound`). The sweep unlinks while it HOLDS the lock.
//! - Windows: lock files are opened without `FILE_SHARE_DELETE`, so a delete
//!   fails with a sharing violation while any process has one open. The sweep
//!   closes its own handle, then deletes: it succeeds only when nobody else had
//!   the file open, i.e. nobody can be holding (or about to take) that lock.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a holder writes into its lock file once it holds the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub holder_pid: u32,
    /// The pane's child pid. Set by every Phase 2 holder (it spawns the child
    /// before it writes the record); `null` only from a Phase 1 holder.
    pub child_pid: Option<u32>,
    /// Every protocol version the holder speaks.
    pub versions: Vec<u32>,
    pub started_at_unix_ms: u64,
    pub holder_build: String,
    /// The HOLDER's own start token (`crate::proc_identity`), so "is the
    /// recorded holder alive?" survives pid reuse. Absent from records of
    /// earlier holders, which then count as live whenever their pid exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder_start: Option<String>,
    /// How the pane ended, written by its holder under the lock before it
    /// releases it: [`ENDED_BY_EXIT`] when the child exited on its own. A
    /// boot sweep then signals NOTHING for this pane — the child's leftovers
    /// are its user's (plan D8 as decided 2026-10-14). `None` while the pane
    /// runs, after a kill, and when the holder crashed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// The child's start token (`crate::proc_identity`), taken right after it
    /// was spawned: what lets a boot sweep tell the recorded child from a
    /// process that later got the same pid (Phase 3, D8). Absent from records
    /// written by Phase 2 holders, which still parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_start: Option<String>,
    /// Linux scope route: the holder's own transient unit
    /// (`qontinui-pty-holder-….scope`), so a sweep can stop what a dead
    /// holder left in it. `None` on every other route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_unit: Option<String>,
}

/// An acquired pane lock. Dropping it releases the lock (by closing the file).
#[derive(Debug)]
pub struct PaneLock {
    file: File,
    path: PathBuf,
}

/// [`LockRecord::ended`] for a pane whose child exited on its own.
pub const ENDED_BY_EXIT: &str = "exited";

/// The pauses (ms) [`PaneLock::try_acquire_unless_live`] waits out a lock held
/// by no live holder: about 300 ms in all.
pub const TRANSIENT_HOLD_BACKOFF_MS: [u64; 4] = [20, 40, 80, 160];

/// Whether `record` names a holder that is alive right now: its pid is the
/// process its start token was taken from, or — for a record with no token —
/// its pid exists at all (the conservative answer). No record names nobody.
pub fn record_names_live_holder(record: Option<&LockRecord>) -> bool {
    let Some(r) = record else {
        return false;
    };
    match &r.holder_start {
        Some(token) => crate::proc_identity::is_same_process(r.holder_pid, Some(token)),
        None => pid_exists(r.holder_pid),
    }
}

/// Whether any process has pid `pid`.
fn pid_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = crate::spawn::signalable_pid(pid) else {
            return false;
        };
        // SAFETY: signal 0 delivers nothing; it only checks for existence.
        let delivered = unsafe { libc::kill(pid, 0) } == 0;
        delivered || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        crate::proc_identity::start_token(pid).is_some()
    }
}

/// How many times [`PaneLock::try_acquire`] re-opens a lock file that was
/// unlinked under it.
const UNLINK_RACE_RETRIES: usize = 4;

/// The pause before re-opening a lock file a sweep is removing (Windows).
const UNLINK_RACE_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// [`PaneLock::try_lock_file`]'s answer.
enum Locked {
    Current(PaneLock),
    Held,
    /// Locked, but the path no longer names the locked file.
    Unlinked,
}

/// The outcome of a non-blocking lock attempt.
#[derive(Debug)]
pub enum TryLock {
    Acquired(PaneLock),
    /// Another open file description holds it — a live process.
    Held,
}

impl PaneLock {
    /// Try to take the lock at `path` without blocking, creating the file
    /// (mode 0600 on Unix) if absent.
    pub fn try_acquire(path: &Path) -> io::Result<TryLock> {
        // A lock won on a file the sweep unlinked meanwhile is worthless (see
        // the module docs): retry on the file now at the path. Bounded — each
        // retry needs a concurrent unlink, which only a sweep of a dead pane
        // performs.
        for _ in 0..UNLINK_RACE_RETRIES {
            let mut opts = OpenOptions::new();
            opts.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let file = match open_checked(&mut opts, path) {
                Ok(f) => f,
                // Windows: a sweep is deleting the dead pane's file right now;
                // the next open creates a fresh one.
                Err(e) if sys::removal_in_progress(&e) => {
                    std::thread::sleep(UNLINK_RACE_PAUSE);
                    continue;
                }
                Err(e) => return Err(e),
            };
            match Self::try_lock_file(file, path)? {
                Locked::Current(lock) => return Ok(TryLock::Acquired(lock)),
                Locked::Held => return Ok(TryLock::Held),
                Locked::Unlinked => continue,
            }
        }
        Err(io::Error::other(format!(
            "{} was unlinked under every lock attempt",
            path.display()
        )))
    }

    /// [`PaneLock::try_acquire`] for a STARTING holder: a lock held while its
    /// record names no live holder is a transient hold (module docs), waited
    /// out over [`TRANSIENT_HOLD_BACKOFF_MS`]. A lock whose record names a
    /// live holder is `Held` at once, with no retry.
    pub fn try_acquire_unless_live(path: &Path) -> io::Result<TryLock> {
        let mut pauses = TRANSIENT_HOLD_BACKOFF_MS.iter();
        loop {
            match Self::try_acquire(path)? {
                TryLock::Held if !record_names_live_holder(read_record(path).as_ref()) => {
                    match pauses.next() {
                        Some(ms) => std::thread::sleep(std::time::Duration::from_millis(*ms)),
                        None => return Ok(TryLock::Held),
                    }
                }
                other => return Ok(other),
            }
        }
    }

    /// Like [`PaneLock::try_acquire`], but never creates the file: a missing
    /// lock file is `NotFound` — including one unlinked between this open and
    /// its lock. What a prober uses, so probing cannot litter.
    pub fn try_acquire_existing(path: &Path) -> io::Result<TryLock> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        let file = open_checked(&mut opts, path)?;
        match Self::try_lock_file(file, path)? {
            Locked::Current(lock) => Ok(TryLock::Acquired(lock)),
            Locked::Held => Ok(TryLock::Held),
            Locked::Unlinked => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} was removed while it was being locked", path.display()),
            )),
        }
    }

    fn try_lock_file(file: File, path: &Path) -> io::Result<Locked> {
        if !sys::try_lock_exclusive(&file)? {
            return Ok(Locked::Held);
        }
        if !sys::still_names(&file, path)? {
            return Ok(Locked::Unlinked);
        }
        Ok(Locked::Current(PaneLock {
            file,
            path: path.to_path_buf(),
        }))
    }

    /// Remove this (dead pane's) lock file and release the lock — the boot
    /// sweep's last act on a pane. Safe against a holder starting for the same
    /// pane at the same instant; see the module docs. `Ok(false)` on Windows
    /// when another process had the file open, so it was left in place.
    pub fn remove(self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            // Unlinked while held: a racer that opened the old file fails
            // `still_names` once it gets the lock, and retries on a new file.
            match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            drop(self.file);
            Ok(true)
        }
        #[cfg(windows)]
        {
            // Our own handle denies delete sharing too, so close it first. The
            // delete then fails while any other process has the file open.
            let PaneLock { file, path } = self;
            drop(file);
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(e) if e.raw_os_error() == Some(sys::ERROR_SHARING_VIOLATION) => Ok(false),
                Err(e) => Err(e),
            }
        }
    }

    /// Replace the file's contents with `record`, while holding the lock.
    ///
    /// NOT atomic: a reader can observe the file empty or half-written between
    /// `set_len(0)` and the final write, and reads that as "holder
    /// unidentified". So until an atomic rewrite exists, a holder writes its
    /// record exactly once, BEFORE it binds its endpoint — no client can be
    /// talking to it yet, and a prober of a lock-held/endpoint-less pane
    /// already answers Unknown.
    pub fn write_record(&mut self, record: &LockRecord) -> io::Result<()> {
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&bytes)?;
        self.file.sync_data()
    }

    /// Replace the record of a pane that is ALREADY being served — a holder
    /// upgrade's successor naming itself (plan Phase 8) — without ever
    /// exposing an empty file.
    ///
    /// [`PaneLock::write_record`] truncates first, so a concurrent reader can
    /// see nothing and report the pane's holder as unidentified. Here the new
    /// record is padded with trailing spaces (JSON whitespace, which
    /// [`read_record`] accepts) to at least the old record's length and written
    /// over it from offset 0 in one write, with no truncation: a reader sees
    /// the old record or the new one. A read torn across that single small
    /// write could see a prefix of one and the rest of the other, which fails
    /// to parse and reads as unidentified — UNKNOWN, never a wrong holder,
    /// short of a torn mix that happens to be valid JSON. The file never
    /// shrinks, so a later shorter record keeps the padding.
    ///
    /// The lock FILE is never replaced (no write-to-temp-and-rename): the
    /// `flock` belongs to this inode, and a rename would hand every later
    /// locker a different, unlocked file.
    pub fn rewrite_record(&mut self, record: &LockRecord) -> io::Result<()> {
        rewrite_record_in(&mut self.file, record)
    }

    /// Take over the lock a holder upgrade's predecessor passed down as an
    /// inherited fd (plan Phase 8, D12a). The fd shares the predecessor's
    /// open file DESCRIPTION, and an `flock` belongs to the description, so
    /// the lock is held continuously across the swap: a prober never finds
    /// the pane's lock free (which it would read as a dead holder) while the
    /// predecessor lets go. Verified, not assumed: `path` must still name the
    /// inherited file, and a non-blocking `LOCK_EX` on it must succeed — on a
    /// description that already holds the lock that is a no-op.
    #[cfg(unix)]
    pub fn adopt_inherited(file: File, path: &Path) -> io::Result<PaneLock> {
        if !sys::try_lock_exclusive(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the inherited lock fd does not hold the pane lock",
            ));
        }
        if !sys::still_names(&file, path)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} no longer names the inherited lock file", path.display()),
            ));
        }
        Ok(PaneLock {
            file,
            path: path.to_path_buf(),
        })
    }

    /// The lock file's fd, for a holder upgrade to pass to its successor
    /// (plan Phase 8). Never unlock or close it through this.
    #[cfg(unix)]
    pub fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.file.as_raw_fd()
    }

    /// Empty the record, while holding the lock: a reader then sees "holder
    /// unidentified", and a sweep signals nothing for the pane. The fallback
    /// when a record could not be REWRITTEN.
    pub fn clear_record(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.sync_data()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// [`PaneLock::rewrite_record`] on any open handle of a held lock file — a
/// `dup` of the holder's own lock fd, for a predecessor that takes its pane
/// back after its successor died (plan Phase 8).
pub fn rewrite_record_in(file: &mut File, record: &LockRecord) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    let old_len = file.metadata()?.len();
    if (bytes.len() as u64) < old_len {
        bytes.resize(old_len as usize, b' ');
    }
    if bytes.len() as u64 > MAX_RECORD_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lock record would exceed MAX_RECORD_LEN",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes)?;
    file.sync_data()
}

/// Read the record from a lock file WITHOUT taking the lock. `None` when the
/// file is missing, empty (a holder between lock and write) or unparseable —
/// callers treat that as "holder unidentified", never as "no holder".
///
/// Opened with the same checks as the lock itself (no symlink, a regular file
/// owned by this user) and read up to [`MAX_RECORD_LEN`]; anything larger is
/// not a record this crate wrote and reads as `None`.
pub fn read_record(path: &Path) -> Option<LockRecord> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    let file = open_checked(&mut opts, path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_RECORD_LEN + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_RECORD_LEN {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}

/// Largest lock record [`read_record`] will read.
pub const MAX_RECORD_LEN: u64 = 64 * 1024;

/// Open a lock file refusing anything but a regular file this user owns.
///
/// Unix: `O_NOFOLLOW` (a symlink at the lock path fails the open with
/// `ELOOP` instead of locking or truncating whatever it points at) and
/// `O_NONBLOCK` (a FIFO planted at the path cannot hang the open); then
/// `fstat` of the OPENED fd — not a racy `stat` of the path — must say
/// regular file and our euid. Windows: regular-file check only; the directory
/// is the per-user app-data tree; every open denies delete sharing.
fn open_checked(opts: &mut OpenOptions, path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        // No FILE_SHARE_DELETE: a lock file cannot be deleted while anyone
        // has it open (see the module docs, and `PaneLock::remove`).
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(sys::FILE_SHARE_READ_WRITE);
    }
    let file = opts.open(path)?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a regular file", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if md.uid() != me {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is owned by uid {}, not {me}", path.display(), md.uid()),
            ));
        }
    }
    Ok(file)
}

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    /// Unix unlinks never block an open: nothing to wait out.
    pub fn removal_in_progress(_e: &io::Error) -> bool {
        false
    }

    /// Whether `path` still names the file `file` has open (same device and
    /// inode). A missing path is `false`.
    pub fn still_names(file: &File, path: &Path) -> io::Result<bool> {
        let held = file.metadata()?;
        match std::fs::symlink_metadata(path) {
            Ok(now) => Ok(now.dev() == held.dev() && now.ino() == held.ino()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `Ok(true)` acquired, `Ok(false)` held elsewhere.
    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        loop {
            // SAFETY: a valid, open fd owned by `file` for the whole call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(true);
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
                    return Ok(false)
                }
                _ => return Err(err),
            }
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, FILE_SHARE_READ, FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK,
        LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// Every lock-file open's share mode: read and write, never delete.
    pub const FILE_SHARE_READ_WRITE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;

    /// `ERROR_SHARING_VIOLATION`, as `io::Error::raw_os_error` reports it.
    pub const ERROR_SHARING_VIOLATION: i32 =
        windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32;

    /// `ERROR_DELETE_PENDING`: the file is being deleted and cannot be opened.
    const ERROR_DELETE_PENDING: i32 = 303;

    /// An open refused because a sweep is deleting the file this instant: a
    /// sharing violation against the deleter's handle, or delete-pending.
    pub fn removal_in_progress(e: &io::Error) -> bool {
        matches!(
            e.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION) | Some(ERROR_DELETE_PENDING)
        )
    }

    /// Always true: no handle to a lock file allows delete sharing, so the
    /// file a handle has open cannot have been deleted out from under it.
    pub fn still_names(_file: &File, _path: &Path) -> io::Result<bool> {
        Ok(true)
    }

    /// The locked byte sits at offset 2^62: far past any record, so the
    /// mandatory range lock never covers a byte a reader reads.
    const LOCK_OFFSET_HIGH: u32 = 0x4000_0000;

    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        // SAFETY: OVERLAPPED is plain data; all-zero is its documented initial
        // state, and only the offset fields are then set.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.Anonymous.Anonymous.Offset = 0;
        ov.Anonymous.Anonymous.OffsetHigh = LOCK_OFFSET_HIGH;
        // SAFETY: a valid handle owned by `file`; `ov` outlives the call, which
        // completes synchronously with LOCKFILE_FAIL_IMMEDIATELY on a handle
        // std opened without FILE_FLAG_OVERLAPPED.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut ov,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Ok(false)
        } else {
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pty-holder-lock-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pty_holder_lock_is_exclusive_and_released_on_drop() {
        let dir = tmpdir("excl");
        let path = dir.join("p.lock");
        let mut first = match PaneLock::try_acquire(&path).unwrap() {
            TryLock::Acquired(l) => l,
            TryLock::Held => panic!("fresh lock must be acquirable"),
        };
        // A second open file description in the SAME process conflicts too —
        // which is what lets these tests stand in for a second process.
        assert!(matches!(
            PaneLock::try_acquire(&path).unwrap(),
            TryLock::Held
        ));
        assert!(matches!(
            PaneLock::try_acquire_existing(&path).unwrap(),
            TryLock::Held
        ));

        let rec = LockRecord {
            holder_pid: 99,
            child_pid: None,
            versions: vec![1],
            started_at_unix_ms: 5,
            holder_build: "b".into(),
            child_start: Some("linux:boot:1".into()),
            scope_unit: None,
            holder_start: None,
            ended: None,
        };
        first.write_record(&rec).unwrap();
        // Readable while held — on Windows too, because the locked byte is
        // past EOF.
        assert_eq!(read_record(&path), Some(rec));

        drop(first);
        // Polled, not asserted once: a child another test forks in this
        // process between our open and its exec shares the open file
        // description, and with it the lock, until that exec closes it.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            PaneLock::try_acquire_existing(&path).unwrap(),
            TryLock::Acquired(_)
        ) {
            assert!(std::time::Instant::now() < end, "lock not released on drop");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The boot sweep's removal (Phase 3): a racer that opened the lock file
    /// before the sweep unlinked it must not count the old file's lock as the
    /// pane's.
    #[cfg(unix)]
    #[test]
    fn pty_holder_boot_sweep_lock_removal_defeats_an_open_racer() {
        let dir = tmpdir("remove");
        let path = dir.join("r.lock");
        let TryLock::Acquired(sweeper) = PaneLock::try_acquire(&path).unwrap() else {
            panic!("fresh lock");
        };
        // Opened the old file before the unlink, will lock it after.
        let racer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(sweeper.remove().unwrap());
        assert!(!path.exists());
        assert!(matches!(
            PaneLock::try_lock_file(racer, &path).unwrap(),
            Locked::Unlinked
        ));
        // A fresh acquisition creates (and holds) a NEW file.
        assert!(matches!(
            PaneLock::try_acquire(&path).unwrap(),
            TryLock::Acquired(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_boot_sweep_lock_record_from_a_phase_2_holder_still_parses() {
        let old = br#"{"holder_pid":7,"child_pid":8,"versions":[2],"started_at_unix_ms":1,"holder_build":"b"}"#;
        let rec: LockRecord = serde_json::from_slice(old).unwrap();
        assert_eq!((rec.child_start, rec.scope_unit), (None, None));
    }

    /// Finding 9bbe1079: a lock held by no live holder (here: by another open
    /// description of this process, released shortly — what a fork-before-exec
    /// or a prober does) is waited out; a lock whose record names a live holder
    /// is `Held` at once.
    #[test]
    fn pty_holder_lock_retries_a_transient_hold_but_never_a_live_holder() {
        let dir = tmpdir("transient");
        let path = dir.join("t.lock");
        let TryLock::Acquired(transient) = PaneLock::try_acquire(&path).unwrap() else {
            panic!("fresh lock");
        };
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            drop(transient);
        });
        assert!(
            matches!(
                PaneLock::try_acquire_unless_live(&path).unwrap(),
                TryLock::Acquired(_)
            ),
            "a transient hold with no live holder on record is waited out"
        );
        release.join().unwrap();

        let TryLock::Acquired(mut live) = PaneLock::try_acquire(&path).unwrap() else {
            panic!("lock");
        };
        let me = std::process::id();
        live.write_record(&LockRecord {
            holder_pid: me,
            child_pid: None,
            versions: vec![3],
            started_at_unix_ms: 0,
            holder_build: "b".into(),
            child_start: None,
            scope_unit: None,
            holder_start: crate::proc_identity::start_token(me),
            ended: None,
        })
        .unwrap();
        assert!(record_names_live_holder(read_record(&path).as_ref()));
        let asked = std::time::Instant::now();
        assert!(matches!(
            PaneLock::try_acquire_unless_live(&path).unwrap(),
            TryLock::Held
        ));
        assert!(
            asked.elapsed() < std::time::Duration::from_millis(TRANSIENT_HOLD_BACKOFF_MS[0]),
            "no retry against a live holder"
        );
        drop(live);
        assert!(!record_names_live_holder(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pty_holder_lock_probe_never_creates() {
        let dir = tmpdir("nocreate");
        let path = dir.join("absent.lock");
        let err = PaneLock::try_acquire_existing(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!path.exists());
        assert_eq!(read_record(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn pty_holder_lock_refuses_symlinks_fifos_and_oversized_records() {
        let dir = tmpdir("nofollow");
        let target = dir.join("target");
        std::fs::write(&target, b"precious").unwrap();
        let link = dir.join("l.lock");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(PaneLock::try_acquire(&link).is_err());
        assert!(PaneLock::try_acquire_existing(&link).is_err());
        assert_eq!(read_record(&link), None);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"precious",
            "target untouched"
        );

        let fifo = dir.join("f.lock");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // Must return (not hang on the FIFO) and refuse it.
        assert!(PaneLock::try_acquire_existing(&fifo).is_err());
        assert_eq!(read_record(&fifo), None);

        let big = dir.join("big.lock");
        std::fs::write(&big, vec![b' '; MAX_RECORD_LEN as usize + 1]).unwrap();
        assert_eq!(read_record(&big), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn pty_holder_lock_file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir("mode");
        let path = dir.join("m.lock");
        let _l = PaneLock::try_acquire(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "lock file mode {mode:o} is group/other-accessible"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
