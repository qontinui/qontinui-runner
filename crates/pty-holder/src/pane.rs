//! Pane identity and the paths derived from it.
//!
//! The pane DIRECTORY is supplied by the runner on the holder's command line
//! (`--pane-dir <abs>`), resolved there through `instance::scope_path` so it is
//! namespaced per runner instance (plan D13, vetted 2026-09-27). The holder
//! never recomputes it: a holder outlives the runner that spawned it and must
//! not re-derive an instance identity from an environment it no longer shares.
//!
//! Inside that directory a pane owns:
//!
//! - `<pane-id>.lock` — the advisory lock, taken BEFORE any endpoint exists,
//!   holding a JSON [`crate::lock::LockRecord`];
//! - `<pane-id>.sock` — the Unix-domain socket (Unix only);
//! - `<pane-id>.spec` — the child spec the RUNNER writes (0600) before it
//!   spawns the holder, and the holder consumes and unlinks at start-up
//!   (`crate::spec`);
//! - `<pane-id>.log` — the holder's own, size-capped diagnostic log. A holder
//!   has no stderr once it is detached, so this is where it says why an
//!   endpoint failed without ending the pane.
//!
//! On Windows the endpoint is a named pipe, whose namespace is global rather
//! than a directory, so [`pipe_name`] folds the pane directory into the name.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Longest accepted pane id.
pub const MAX_PANE_ID_LEN: usize = 64;

/// A validated pane id: 1..=64 of `[A-Za-z0-9_-]`.
///
/// The character set is what makes it safe to splice into a file name AND a
/// pipe name with no escaping: no separators, no `..`, no NUL, no case-folding
/// hazards beyond letters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(String);

impl PaneId {
    pub fn new(id: &str) -> Result<Self, InvalidPaneId> {
        let ok = !id.is_empty()
            && id.len() <= MAX_PANE_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if ok {
            Ok(PaneId(id.to_string()))
        } else {
            Err(InvalidPaneId(id.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pane id that failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPaneId(pub String);

impl fmt::Display for InvalidPaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid pane id {:?}: expected 1..={MAX_PANE_ID_LEN} of [A-Za-z0-9_-]",
            self.0
        )
    }
}

impl std::error::Error for InvalidPaneId {}

/// `<pane-dir>/<pane-id>.lock`
pub fn lock_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.lock", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.sock` — the Unix endpoint.
pub fn socket_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.sock", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.spec` — the child spec (`crate::spec`).
pub fn spec_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.spec", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.log` — the holder's diagnostic log.
pub fn log_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.log", pane.as_str()))
}

/// Create the pane dir if needed, and refuse one this user cannot trust. Used
/// by BOTH sides: the runner before it writes a spec into it, the holder
/// before it takes its lock there.
///
/// The path must be absolute — the runner resolves it and passes it whole
/// (plan D13). Unix: a directory we CREATE is made 0700 (the umask can only
/// narrow it). A PRE-EXISTING one must be a real directory (not a symlink),
/// ours, and not group/other-writable — anyone who could write there could
/// already have planted a lock file, a socket or a spec, so it is refused
/// (`PermissionDenied`) rather than chmod-repaired. A pre-existing directory
/// that is merely group/other-READABLE is tightened to 0700: nothing could
/// have been planted, and the endpoint's path should not be listable.
pub fn prepare_private_dir(pane_dir: &Path) -> io::Result<()> {
    if !pane_dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is not absolute; the runner resolves it and passes it whole",
                pane_dir.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(pane_dir)?;
        check_private_dir(pane_dir)?;
        let md = std::fs::symlink_metadata(pane_dir)?;
        if md.mode() & 0o077 != 0 {
            std::fs::set_permissions(pane_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(windows)]
    {
        // The directory inherits the per-user app-data ACL it is created under;
        // the Windows access barrier is the pipe's DACL (transport::windows).
        std::fs::create_dir_all(pane_dir)?;
        check_private_dir(pane_dir)?;
    }
    Ok(())
}

/// The Windows named-pipe name for a pane:
/// `\\.\pipe\qontinui-pty-holder-<16 hex of FNV-1a(pane dir)>-<pane-id>`.
///
/// The pipe namespace is machine-global, so the pane directory — which is what
/// namespaces panes per runner instance — is folded in by hash; a secondary or
/// temp runner therefore never lands on the primary's pipe. The hash is over
/// the directory's raw OS-string bytes (UTF-16 units on Windows), never a lossy
/// text form, and is case-folded for ASCII letters only, since Windows paths
/// are case-insensitive and the runner may spell one differently across
/// restarts. Pure, so it is unit-tested on every OS.
pub fn pipe_name(pane_dir: &Path, pane: &PaneId) -> String {
    format!(
        r"\\.\pipe\qontinui-pty-holder-{:016x}-{}",
        dir_hash(pane_dir),
        pane.as_str()
    )
}

/// Refuse a pane directory this user cannot trust (plan D5, both sides).
///
/// Unix: the path must be a real directory (not a symlink), owned by this
/// process's effective uid, and not group- or other-WRITABLE — anyone who can
/// write there can plant a lock file or an endpoint. `NotFound` passes through
/// unchanged so callers can tell "no pane dir yet" from "refused"; every
/// refusal is `PermissionDenied`.
///
/// Windows: only existence is checked. The directory inherits the per-user
/// app-data ACL it is created under, and the client proves the pipe server
/// against the lock record rather than trusting the directory.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    let md = std::fs::symlink_metadata(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let refuse = |why: String| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("pane dir {}: {why}", dir.display()),
            ))
        };
        if md.file_type().is_symlink() {
            return refuse("is a symlink".into());
        }
        if !md.is_dir() {
            return refuse("is not a directory".into());
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if md.uid() != me {
            return refuse(format!("owned by uid {}, not {me}", md.uid()));
        }
        if md.mode() & 0o022 != 0 {
            return refuse(format!(
                "mode {:o} is group/other-writable",
                md.mode() & 0o777
            ));
        }
    }
    #[cfg(windows)]
    {
        if !md.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("pane dir {} is not a directory", dir.display()),
            ));
        }
    }
    Ok(())
}

fn dir_hash(pane_dir: &Path) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |unit: u16| {
        let unit = if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
            unit + 32
        } else {
            unit
        };
        for b in unit.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    };
    // A trailing separator must not change the name.
    let trimmed = trim_trailing_separators(pane_dir);
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in trimmed.as_os_str().encode_wide() {
            // `/` and `\` name the same directory on Windows.
            feed(if unit == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                unit
            });
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        for b in trimmed.as_os_str().as_bytes() {
            feed(u16::from(*b));
        }
    }
    h
}

fn trim_trailing_separators(p: &Path) -> PathBuf {
    // `components()` normalizes away a trailing separator and repeated ones.
    p.components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_holder_pane_id_validation() {
        for ok in ["a", "pane-1", "A_b-9", &"x".repeat(MAX_PANE_ID_LEN)] {
            assert!(PaneId::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "..",
            "a/b",
            "a\\b",
            "a.lock",
            "a b",
            "é",
            "a\0",
            &"x".repeat(MAX_PANE_ID_LEN + 1),
        ] {
            assert!(PaneId::new(bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn pty_holder_check_private_dir_refusals() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "pty-holder-pdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let set = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
        };
        set(&base, 0o700);
        assert!(check_private_dir(&base).is_ok());
        set(&base, 0o755);
        assert!(check_private_dir(&base).is_ok(), "readable is not writable");
        for m in [0o775, 0o757, 0o777] {
            set(&base, m);
            let e = check_private_dir(&base).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{m:o}");
        }
        set(&base, 0o700);
        let link = base.with_extension("lnk");
        std::os::unix::fs::symlink(&base, &link).unwrap();
        assert_eq!(
            check_private_dir(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let file = base.join("f");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(
            check_private_dir(&file).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            check_private_dir(&base.join("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pty_holder_paths_and_pipe_name() {
        let dir = Path::new("/state/runner-a/panes");
        let p = PaneId::new("p1").unwrap();
        assert_eq!(lock_path(dir, &p), dir.join("p1.lock"));
        assert_eq!(socket_path(dir, &p), dir.join("p1.sock"));
        assert_eq!(spec_path(dir, &p), dir.join("p1.spec"));
        assert_eq!(log_path(dir, &p), dir.join("p1.log"));

        let name = pipe_name(dir, &p);
        assert!(name.starts_with(r"\\.\pipe\qontinui-pty-holder-"), "{name}");
        assert!(name.ends_with("-p1"), "{name}");
        // Stable across calls, insensitive to a trailing separator and ASCII case.
        assert_eq!(name, pipe_name(dir, &p));
        assert_eq!(name, pipe_name(Path::new("/state/runner-a/panes/"), &p));
        assert_eq!(name, pipe_name(Path::new("/STATE/Runner-A/panes"), &p));
        // A different instance directory is a different pipe.
        assert_ne!(name, pipe_name(Path::new("/state/runner-b/panes"), &p));
        // A different pane is a different pipe.
        assert_ne!(name, pipe_name(dir, &PaneId::new("p2").unwrap()));
    }
}
