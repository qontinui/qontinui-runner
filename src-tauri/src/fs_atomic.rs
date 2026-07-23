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
}
