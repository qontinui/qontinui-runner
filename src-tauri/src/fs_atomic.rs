use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-global monotonic counter for temp-file names. Combined with the pid
/// and a timestamp it guarantees a UNIQUE temp path per `atomic_write` call, so
/// two concurrent writers to the same target within one clock tick can never
/// share a temp path (which would let one rename a half-written temp into place).
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Atomically write `data` to `path` using a temp-file-then-rename pattern.
///
/// On Windows, `fs::rename` uses MoveFileExW with MOVEFILE_REPLACE_EXISTING
/// since Rust 1.58, so the swap is atomic on NTFS just like POSIX rename.
///
/// The temp file name is `{name}.tmp.{pid}.{seq}.{nanos}` — the pid
/// disambiguates concurrent processes, the process-global `{seq}` counter
/// disambiguates concurrent same-process writers (e.g. the device-JWT refresher
/// thread and an interactive write hitting the same store), and `{nanos}` is a
/// human-readable timestamp. `{nanos}` alone was NOT unique: `SystemTime::now()`
/// can return the same value on two threads within one tick, so two writers
/// shared a temp path and one renamed the other's half-written temp over the
/// store.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write: path has no parent directory",
        )
    })?;
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write: path has no UTF-8 file name",
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        "{}.tmp.{}.{}.{}",
        file_name,
        std::process::id(),
        seq,
        nanos
    ));

    let result = (|| -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }

    result
}

/// [`atomic_write`], but the file is owner-only from the moment it exists.
///
/// For credential stores, `atomic_write` followed by a permission fix is NOT
/// equivalent to this. Two things go wrong with that ordering:
///
/// 1. `atomic_write` creates its temp with `File::create`, i.e. default umask
///    (typically `0644`) / parent-inherited ACL. The ciphertext is therefore
///    other-user-readable for the WHOLE write, not merely for a sliver after
///    the rename.
/// 2. The rename installs a NEW inode. If the post-hoc hardening then fails, a
///    store that was `0600` from the previous save is left world-readable —
///    the failure path silently DE-hardens a file that was already safe.
///
/// Hardening the temp before the rename removes both: the bytes are never
/// visible to another user, and a hardening failure aborts the write instead of
/// publishing a loosened file. The rename itself preserves the temp's
/// permissions on both platforms (POSIX rename keeps the inode's mode;
/// `MoveFileExW` keeps the file's explicit DACL).
pub fn atomic_write_owner_only(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write_owner_only: path has no parent directory",
        )
    })?;
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write_owner_only: path has no UTF-8 file name",
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        "{}.tmp.{}.{}.{}",
        file_name,
        std::process::id(),
        seq,
        nanos
    ));

    let result = (|| -> io::Result<()> {
        // Unix: create at 0600 so the bytes are never group/other-readable.
        // Windows has no create-mode equivalent, so the DACL is stamped on the
        // temp immediately after creation and before any rename.
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = fs::File::create(&tmp)?;

        #[cfg(windows)]
        crate::fs_perms::restrict_to_owner(&tmp)?;

        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);

        // Belt-and-braces on unix: `mode()` applies only on create, and this
        // path always creates, but re-asserting costs one syscall and makes the
        // guarantee independent of that subtlety.
        #[cfg(unix)]
        crate::fs_perms::restrict_to_owner(&tmp)?;

        fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }

    result
}

/// Atomically CREATE `path` with `data`, failing with
/// [`io::ErrorKind::AlreadyExists`] if it is already there.
///
/// [`atomic_write`] is atomic but not exclusive: its `fs::rename` replaces the
/// target unconditionally, so "check `path.exists()`, then write" is a TOCTOU
/// window — two processes racing a first-launch mint both see no file, both
/// generate a value, and the loser's rename overwrites the winner's. For
/// `~/.qontinui/machine.json` that means the loser can hand coord a different
/// `device_id` than the winner already registered (coord UPSERTs
/// `ON CONFLICT (device_id)` → two rows for one machine). This function closes
/// that window: exactly one caller can win, and the loser learns it lost.
///
/// Two-stage, so the published file is never observed half-written:
///
/// 1. Write + fsync a unique temp (same naming as [`atomic_write`]).
/// 2. `fs::hard_link(tmp, path)` — an atomic, fail-if-exists publish on both
///    POSIX and NTFS (unlike `fs::rename`, which replaces). Then unlink the
///    temp, leaving one name for the inode.
///
/// If the filesystem does not support hard links at all (exFAT/FAT32 home
/// directory, some network mounts) step 2 fails with something other than
/// `AlreadyExists`; we then fall back to `create_new` directly on the target.
/// That fallback keeps exclusivity — the property that matters here — and
/// gives up only the no-torn-read guarantee, which costs at most a transient
/// parse error in a reader (no reader of `machine.json` mints on a read).
pub fn atomic_create_new(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_create_new: path has no parent directory",
        )
    })?;
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_create_new: path has no UTF-8 file name",
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        "{}.new.{}.{}.{}",
        file_name,
        std::process::id(),
        seq,
        nanos
    ));

    let write_tmp = (|| -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_tmp {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    let published = match fs::hard_link(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            // Hard links unsupported/denied — publish by exclusive create.
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .and_then(|mut f| {
                    f.write_all(data)?;
                    f.flush()?;
                    f.sync_all()
                })
        }
    };

    let _ = fs::remove_file(&tmp);
    published
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn cleans_up_temp_on_rename_failure() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("subdir");
        fs::create_dir(&target_dir).unwrap();
        let path = target_dir.join("file.txt");
        atomic_write(&path, b"content").unwrap();

        let entries: Vec<_> = fs::read_dir(&target_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries.len(), 1);
    }

    /// Concurrent writers to the SAME target must each get a unique temp path,
    /// so none renames another's half-written temp into place. Before the pid +
    /// counter fix, two threads whose `SystemTime::now()` collided within one
    /// tick shared `{name}.tmp.{nanos}` and could corrupt the target.
    ///
    /// The invariant asserted: every write succeeds, a concurrent reader never
    /// observes a value that isn't one of the exact payloads (no torn/mixed
    /// content), the final file is one full payload, and no temp debris is left.
    #[test]
    fn concurrent_writers_never_corrupt_the_target() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.bin");
        // Distinct, fixed-length payloads so a torn write is detectable.
        let payloads: Vec<Vec<u8>> = (0..4)
            .map(|i| vec![b'A' + i as u8; 4096])
            .collect::<Vec<_>>();
        // Seed so a reader always has a full file to read.
        atomic_write(&path, &payloads[0]).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let mut writers = Vec::new();
        for payload in payloads.clone() {
            let path = path.clone();
            let stop = Arc::clone(&stop);
            writers.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    atomic_write(&path, &payload).expect("atomic_write must succeed");
                }
                stop.store(true, Ordering::SeqCst);
            }));
        }

        // Reader: every readable snapshot must equal one WHOLE payload exactly.
        let mut reads = 0usize;
        loop {
            if let Ok(bytes) = fs::read(&path) {
                assert!(
                    payloads.iter().any(|p| p == &bytes),
                    "reader observed a torn/mixed file ({} bytes) — atomic_write temp names \
                     collided across concurrent writers",
                    bytes.len()
                );
                reads += 1;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
        for w in writers {
            w.join().expect("writer thread panicked");
        }
        assert!(reads > 0, "the reader never ran");

        // Final state is one whole payload…
        let final_bytes = fs::read(&path).unwrap();
        assert!(payloads.iter().any(|p| p == &final_bytes));
        // …and no `.tmp.` debris is left behind.
        let debris: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(debris.is_empty(), "temp files left behind: {debris:?}");
    }

    // ------------------------------------------------------------------
    // `atomic_create_new` — atomic AND exclusive.
    // ------------------------------------------------------------------

    #[test]
    fn create_new_writes_an_absent_file_and_leaves_no_debris() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        atomic_create_new(&path, b"minted").expect("an absent target must be created");
        assert_eq!(fs::read(&path).unwrap(), b"minted");

        let entries: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["machine.json".to_string()]);
    }

    /// THE property: a second caller must LOSE, and must not have touched the
    /// winner's bytes. This is what `atomic_write` (whose `fs::rename` replaces
    /// unconditionally) could not give the first-launch `device_id` mint —
    /// two processes past the same `path.exists()` check both minted, and the
    /// loser's rename overwrote an id the winner may already have registered
    /// with coord.
    #[test]
    fn create_new_refuses_an_existing_file_and_preserves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        fs::write(&path, b"winner").unwrap();

        let err = atomic_create_new(&path, b"loser").expect_err("an existing target must lose");
        assert_eq!(
            err.kind(),
            io::ErrorKind::AlreadyExists,
            "the loser must be able to DETECT that it lost, got: {err}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            b"winner",
            "the winner's bytes must be untouched"
        );

        let debris: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "machine.json")
            .collect();
        assert!(debris.is_empty(), "temp files left behind: {debris:?}");
    }

    /// Under a real thread race, EXACTLY ONE caller wins and every loser sees
    /// `AlreadyExists` — never a partial or mixed payload.
    #[test]
    fn create_new_has_exactly_one_winner_under_contention() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("machine.json"));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let payload = format!("candidate-{i}");
                    barrier.wait();
                    atomic_create_new(&path, payload.as_bytes()).map(|()| payload)
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winners: Vec<&String> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(winners.len(), 1, "exactly one caller may create the file");
        for r in &results {
            if let Err(e) = r {
                assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "unexpected: {e}");
            }
        }
        assert_eq!(
            fs::read(path.as_path()).unwrap(),
            winners[0].as_bytes(),
            "the published file must be the winner's payload, whole"
        );
    }
}
