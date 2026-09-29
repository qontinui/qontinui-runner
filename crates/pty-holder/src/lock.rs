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
//! The lock file is never unlinked by a holder. Unlinking a lock file while
//! locked lets a second holder lock a fresh inode under the same name while a
//! third still holds the old one — two "exclusive" holders. Reaping dead lock
//! files is a later phase's boot sweep, done under the lock.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a holder writes into its lock file once it holds the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub holder_pid: u32,
    /// The pane's child pid; `null` until Phase 2 spawns one.
    pub child_pid: Option<u32>,
    /// Every protocol version the holder speaks.
    pub versions: Vec<u32>,
    pub started_at_unix_ms: u64,
    pub holder_build: String,
}

/// An acquired pane lock. Dropping it releases the lock (by closing the file).
#[derive(Debug)]
pub struct PaneLock {
    file: File,
    path: PathBuf,
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
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = open_checked(&mut opts, path)?;
        Self::try_lock_file(file, path)
    }

    /// Like [`PaneLock::try_acquire`], but never creates the file: a missing
    /// lock file is `NotFound`. What a prober uses, so probing cannot litter.
    pub fn try_acquire_existing(path: &Path) -> io::Result<TryLock> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        let file = open_checked(&mut opts, path)?;
        Self::try_lock_file(file, path)
    }

    fn try_lock_file(file: File, path: &Path) -> io::Result<TryLock> {
        if sys::try_lock_exclusive(&file)? {
            Ok(TryLock::Acquired(PaneLock {
                file,
                path: path.to_path_buf(),
            }))
        } else {
            Ok(TryLock::Held)
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

    pub fn path(&self) -> &Path {
        &self.path
    }
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
/// is the per-user app-data tree.
fn open_checked(opts: &mut OpenOptions, path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
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
    use std::os::unix::io::AsRawFd;

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
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

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
        };
        first.write_record(&rec).unwrap();
        // Readable while held — on Windows too, because the locked byte is
        // past EOF.
        assert_eq!(read_record(&path), Some(rec));

        drop(first);
        assert!(matches!(
            PaneLock::try_acquire_existing(&path).unwrap(),
            TryLock::Acquired(_)
        ));
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
