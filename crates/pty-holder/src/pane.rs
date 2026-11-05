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
//! - (no socket: the Unix-domain socket lives OUTSIDE the pane directory, in
//!   the per-user [`socket_root`], named by a hash of the pane dir and pane id
//!   — see [`socket_path`] for why);
//! - `<pane-id>.spec` — the child spec the RUNNER writes (0600) before it
//!   spawns the holder, and the holder consumes and unlinks at start-up
//!   (`crate::spec`);
//! - `<pane-id>.log` — the holder's own, size-capped diagnostic log. A holder
//!   has no stderr once it is detached, so this is where it says why an
//!   endpoint failed without ending the pane;
//! - `<pane-id>.scrollback` — the pane's durable output tail, 0600, replaced
//!   atomically via `<pane-id>.scrollback.tmp` (`crate::scrollback`, plan D7).
//!   Like the lock file it outlives the holder; that is its purpose;
//! - `<pane-id>.upgrade` — a holder upgrade's snapshot (plan Phase 8, Unix),
//!   alive only between the predecessor writing it and the successor
//!   consuming it (or the predecessor's rollback unlinking it);
//! - `<pane-id>.report` — the holder's one-line ready/error report, written
//!   (atomically, 0600) only when it was started with `--report`: the Windows
//!   WMI spawn route gives the holder no stdout pipe to print it on (Phase 3).
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

/// The Unix endpoint a holder BINDS: `<root>/<16 hex>.sock`, where `<root>`
/// is [`socket_root`] and the 16 hex digits are the first 8 bytes of
/// `sha256(pane-dir bytes ‖ 0x00 ‖ pane-id)` ([`socket_name`]).
///
/// The socket does NOT live in the pane directory, unlike every other per-pane
/// file. A Unix socket's path must fit `sockaddr_un.sun_path` (107 bytes on
/// Linux, 103 on macOS), and a secondary runner's pane dir alone
/// (`…/instances/<name>/home/runner/pty-panes/`) already overflowed it at 133
/// bytes, so its panes silently fell back to in-process PTYs (plan
/// `2026-11-04-identity-shim-foreign-shim-ping-pong-and-secondary-runner-pty-socket-overflow`,
/// Phase 4). Here the length is bounded whatever the pane dir: at most
/// `/run/user/<uid>/qontinui-pty/` plus 21 bytes (55 for a 10-digit uid), and
/// its staged twin (`crate::transport::staged_socket_path`) four more (59).
///
/// The path is a function of `(pane_dir, pane, uid)` and the filesystem —
/// never of the environment — because a holder outlives the runner that
/// spawned it and a later runner, whose environment may differ, must derive
/// the same path to adopt it (the spec is deleted when the holder reads it, and
/// the lock record carries no endpoint). The pane dir is hashed in, so two
/// runner instances sharing a pane id never share a socket. A CLIENT does not
/// call this: it tries every [`socket_candidates`] entry, so a holder bound
/// under the other root (the runtime dir appeared or vanished since) or by a
/// pre-Phase-4 build (in the pane dir) is still reached.
///
/// Creates the root when missing. Fails when it cannot be created or is not
/// this user's ([`socket_root`]); every caller already treats an endpoint
/// error as "unreachable".
#[cfg(unix)]
pub fn socket_path(pane_dir: &Path, pane: &PaneId) -> io::Result<PathBuf> {
    Ok(socket_root()?.join(socket_name(pane_dir, pane)))
}

/// Every path a holder for this pane may be listening on, most likely first,
/// WITHOUT creating anything: the socket under the root a holder would bind in
/// now, then under the other root, then the pre-Phase-4
/// `<pane-dir>/<pane-id>.sock` (a holder started by an older build keeps
/// serving there until it ends — a runner restart onto this build must still
/// reach it to adopt or upgrade it; omitted when it is too long for any holder
/// to have bound it). A root that does not exist or fails the ownership/mode
/// check ([`check_socket_root`]) contributes nothing; the stricter runtime-dir
/// test only decides where to BIND, so a holder bound under `/run/user` stays
/// reachable if that dir's mode drifts later. Used by `transport::connect`
/// and by the sweep, which removes a dead pane's socket wherever it lies; the
/// entries are all derived from THIS pane, so no other pane's file is ever
/// named.
#[cfg(unix)]
pub fn socket_candidates(pane_dir: &Path, pane: &PaneId) -> Vec<PathBuf> {
    socket_candidates_in(
        Path::new(RUN_USER),
        Path::new(TMP),
        crate::transport::own_uid(),
        pane_dir,
        pane,
    )
}

#[cfg(unix)]
fn socket_candidates_in(
    run_user: &Path,
    tmp: &Path,
    uid: u32,
    pane_dir: &Path,
    pane: &PaneId,
) -> Vec<PathBuf> {
    let name = socket_name(pane_dir, pane);
    let mut roots = root_choices(run_user, tmp, uid);
    let runtime_root = run_user.join(uid.to_string()).join("qontinui-pty");
    if !roots.contains(&runtime_root) {
        roots.push(runtime_root);
    }
    let mut out: Vec<PathBuf> = roots
        .into_iter()
        .filter(|root| check_socket_root(root, uid).is_ok())
        .map(|root| root.join(&name))
        .collect();
    let legacy = pane_dir.join(format!("{}.sock", pane.as_str()));
    {
        use std::os::unix::ffi::OsStrExt;
        if legacy.as_os_str().as_bytes().len() <= crate::transport::max_path_len() {
            out.push(legacy);
        }
    }
    out
}

/// The socket's file name: 16 hex of `sha256(pane dir ‖ 0x00 ‖ pane id)` plus
/// `.sock`. The pane dir is canonicalized first when it exists (it does at
/// spawn, connect, adoption and sweep), so a pane dir reached through a
/// symlinked ancestor names the same socket as its real path; a dir that is
/// gone hashes its spelling, minus any trailing or doubled separator.
#[cfg(unix)]
fn socket_name(pane_dir: &Path, pane: &PaneId) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let dir = std::fs::canonicalize(pane_dir).unwrap_or_else(|_| trim_trailing_separators(pane_dir));
    let mut h = Sha256::new();
    h.update(dir.as_os_str().as_bytes());
    h.update([0u8]);
    h.update(pane.as_str().as_bytes());
    let digest = h.finalize();
    let mut name = String::with_capacity(16 + 5);
    for b in &digest[..8] {
        let _ = write!(name, "{b:02x}");
    }
    name.push_str(".sock");
    name
}

#[cfg(unix)]
const RUN_USER: &str = "/run/user";
#[cfg(unix)]
const TMP: &str = "/tmp";

/// The directory a holder binds its socket in, for this effective uid:
/// `/run/user/<uid>/qontinui-pty` when `/run/user/<uid>` exists, is a real
/// directory owned by `<uid>` and has mode 0700 (the systemd-logind runtime
/// dir); otherwise `/tmp/qontinui-pty-<uid>`.
///
/// Created 0700 when missing. A pre-existing root must be a real directory
/// (not a symlink) owned by this uid and not group/other-writable, or it is
/// refused with `PermissionDenied` — in a world-writable `/tmp` another user
/// can create the name first, and anyone who could write there could plant a
/// socket. A merely group/other-READABLE one is tightened to 0700.
///
/// `$XDG_RUNTIME_DIR` is deliberately NOT read: the holder runs on under
/// `systemd-run --scope` after its runner exits, and the next runner (started
/// by the supervisor, by systemd, or by hand) may carry a different
/// environment, so an env-derived root would let spawn and adoption disagree.
///
/// A socket here outlives its pane only until the pane's dir is swept (the
/// sweep removes it with the pane's lock). One whose pane dir is deleted
/// unswept — a removed temp-runner instance — stays until the root is cleared:
/// `/run/user/<uid>` at logout or reboot, `/tmp` by the OS's tmp cleaner.
#[cfg(unix)]
pub fn socket_root() -> io::Result<PathBuf> {
    socket_root_in(
        Path::new(RUN_USER),
        Path::new(TMP),
        crate::transport::own_uid(),
    )
}

/// [`socket_root`] with its two bases and the uid injected, so the choice and
/// the refusals are tested without touching the real `/run/user` or `/tmp`.
#[cfg(unix)]
fn socket_root_in(run_user: &Path, tmp: &Path, uid: u32) -> io::Result<PathBuf> {
    let root = root_choices(run_user, tmp, uid).swap_remove(0);
    prepare_socket_root(&root, uid)?;
    Ok(root)
}

/// The candidate roots, preferred first: the runtime-dir root only while
/// `<run_user>/<uid>` is a real 0700 directory owned by `uid`, then always the
/// `/tmp` one.
#[cfg(unix)]
fn root_choices(run_user: &Path, tmp: &Path, uid: u32) -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let runtime = run_user.join(uid.to_string());
    let runtime_ok = std::fs::symlink_metadata(&runtime).is_ok_and(|md| {
        md.file_type().is_dir() && md.uid() == uid && md.mode() & 0o777 == 0o700
    });
    let tmp_root = tmp.join(format!("qontinui-pty-{uid}"));
    if runtime_ok {
        vec![runtime.join("qontinui-pty"), tmp_root]
    } else {
        vec![tmp_root]
    }
}

/// Create `root` 0700 if missing (its parent must exist: `/run/user/<uid>` or
/// `/tmp`), then refuse it unless [`check_socket_root`] passes, and set it to
/// exactly 0700.
#[cfg(unix)]
fn prepare_socket_root(root: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    check_socket_root(root, uid)?;
    let md = std::fs::symlink_metadata(root)?;
    // Exactly 0700: tightens a readable one, and restores owner bits a
    // restrictive umask stripped from the `mkdir` (bind needs `wx`).
    if md.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Refuse a socket root that is a symlink, not a directory, owned by another
/// uid, or group/other-writable (`PermissionDenied`). `NotFound` passes
/// through. Creates nothing — the read-only half a client and the sweep use.
#[cfg(unix)]
fn check_socket_root(root: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::symlink_metadata(root)?;
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("pty socket root {}: {why}", root.display()),
        ))
    };
    if md.file_type().is_symlink() {
        return refuse("is a symlink".into());
    }
    if !md.is_dir() {
        return refuse("is not a directory".into());
    }
    if md.uid() != uid {
        return refuse(format!("owned by uid {}, not {uid}", md.uid()));
    }
    if md.mode() & 0o022 != 0 {
        return refuse(format!(
            "mode {:o} is group/other-writable",
            md.mode() & 0o777
        ));
    }
    Ok(())
}

/// `<pane-dir>/<pane-id>.spec` — the child spec (`crate::spec`).
pub fn spec_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.spec", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.scrollback` — the durable output tail
/// (`crate::scrollback`).
pub fn scrollback_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.scrollback", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.scrollback.tmp` — where a tail rewrite is written
/// before it is renamed over [`scrollback_path`].
pub fn scrollback_tmp_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.scrollback.tmp", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.report` — the holder's ready/error report file.
pub fn report_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.report", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.report-tmp` — where a report is written before it is
/// renamed over [`report_path`].
pub fn report_tmp_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.report-tmp", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.orphan` — the orphan ledger's entry for a dead
/// holder whose leftovers could not be attributed (`crate::sweep`).
pub fn orphan_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.orphan", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.upgrade` — a holder upgrade's snapshot (plan Phase
/// 8): written 0600 by the predecessor, consumed (unlinked on open) by the
/// successor, and unlinked by the predecessor on every rollback.
pub fn snapshot_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.upgrade", pane.as_str()))
}

/// `<pane-dir>/<pane-id>.upgrade-tmp` — where a snapshot is written before it
/// is renamed over [`snapshot_path`].
pub fn snapshot_tmp_path(pane_dir: &Path, pane: &PaneId) -> PathBuf {
    pane_dir.join(format!("{}.upgrade-tmp", pane.as_str()))
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

    /// A unique, private scratch directory under the temp dir.
    #[cfg(unix)]
    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pty-holder-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        prepare_private_dir(&d).unwrap();
        d
    }

    /// Plan 2026-11-04 Phase 4 (the arming test): the socket and its staged
    /// `.upg` twin fit `sun_path` whatever the pane dir, the path is stable,
    /// and the pane dir is part of the name.
    ///
    /// It calls the real `socket_root()`, so it creates (if missing) the real
    /// per-user root — `/run/user/<uid>/qontinui-pty` or
    /// `/tmp/qontinui-pty-<uid>` — and fails where that root is refused.
    #[cfg(unix)]
    #[test]
    fn socket_path_fits_sun_path_for_long_pane_dir() {
        let max = crate::transport::max_path_len();
        let long = PathBuf::from(format!("/{}", "d".repeat(199)));
        assert_eq!(long.as_os_str().len(), 200);
        let other = PathBuf::from(format!("/{}", "e".repeat(199)));
        let p = PaneId::new(&"p".repeat(MAX_PANE_ID_LEN)).unwrap();

        // The real root on this machine, and both roots `socket_root` can
        // choose for the longest 32-bit uid.
        let worst_roots = [
            PathBuf::from(format!("{RUN_USER}/{}/qontinui-pty", u32::MAX)),
            PathBuf::from(format!("{TMP}/qontinui-pty-{}", u32::MAX)),
        ];
        for root in [socket_root().unwrap()].into_iter().chain(worst_roots) {
            let sock = root.join(socket_name(&long, &p));
            let staged = crate::transport::staged_path(&sock);
            assert!(sock.as_os_str().len() <= max, "{}", sock.display());
            assert!(staged.as_os_str().len() <= max, "{}", staged.display());
            assert_ne!(sock, root.join(socket_name(&other, &p)));
        }

        // The public function: same answer on every call (spawn and a later
        // adoption agree), and under the real root.
        let sock = socket_path(&long, &p).unwrap();
        assert_eq!(sock, socket_path(&long, &p).unwrap());
        assert_eq!(sock.parent().unwrap(), socket_root().unwrap());
        assert!(sock.as_os_str().len() <= max, "{}", sock.display());
        let staged = crate::transport::staged_socket_path(&long, &p).unwrap();
        assert!(staged.as_os_str().len() <= max, "{}", staged.display());
        assert_ne!(sock, socket_path(&other, &p).unwrap());
        // A different pane in the same dir is a different socket; a trailing
        // separator is the same dir.
        assert_ne!(sock, socket_path(&long, &PaneId::new("q").unwrap()).unwrap());
        let mut trailing = long.clone().into_os_string();
        trailing.push("/");
        assert_eq!(sock, socket_path(Path::new(&trailing), &p).unwrap());
    }

    /// A client's candidates: the preferred root, the other root, then the
    /// pre-Phase-4 path in the pane dir; a root that is missing or refused
    /// contributes nothing, and nothing is created. A pane dir reached through
    /// a symlink names the same socket as its real path.
    #[cfg(unix)]
    #[test]
    fn pty_holder_socket_candidates_cover_both_roots_and_the_legacy_path() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::transport::own_uid();
        let base = scratch("scand");
        let run_user = base.join("run-user");
        let tmp = base.join("tmp");
        let rt = run_user.join(uid.to_string());
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pane_dir = base.join("panes");
        prepare_private_dir(&pane_dir).unwrap();
        let p = PaneId::new("p1").unwrap();
        let name = socket_name(&pane_dir, &p);
        let legacy = pane_dir.join("p1.sock");

        // Neither root exists yet: only the legacy path, and nothing created.
        assert_eq!(
            socket_candidates_in(&run_user, &tmp, uid, &pane_dir, &p),
            vec![legacy.clone()]
        );
        assert!(!rt.join("qontinui-pty").exists());
        assert!(!tmp.join(format!("qontinui-pty-{uid}")).exists());

        // Both roots exist: preferred first.
        let run_root = socket_root_in(&run_user, &tmp, uid).unwrap();
        let tmp_root = tmp.join(format!("qontinui-pty-{uid}"));
        prepare_socket_root(&tmp_root, uid).unwrap();
        assert_eq!(
            socket_candidates_in(&run_user, &tmp, uid, &pane_dir, &p),
            vec![run_root.join(&name), tmp_root.join(&name), legacy.clone()]
        );
        // A refused root is skipped.
        std::fs::set_permissions(&tmp_root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            socket_candidates_in(&run_user, &tmp, uid, &pane_dir, &p),
            vec![run_root.join(&name), legacy.clone()]
        );

        // The runtime dir's mode drifts after a holder bound under it: binding
        // moves to /tmp, but the /run/user socket is still a candidate.
        std::fs::set_permissions(&tmp_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            socket_candidates_in(&run_user, &tmp, uid, &pane_dir, &p),
            vec![tmp_root.join(&name), run_root.join(&name), legacy.clone()]
        );

        // A pane dir too long for any holder to have bound in: no legacy entry.
        let long = base.join("x".repeat(120));
        prepare_private_dir(&long).unwrap();
        assert_eq!(
            socket_candidates_in(&run_user, &tmp, uid, &long, &p),
            vec![
                tmp_root.join(socket_name(&long, &p)),
                run_root.join(socket_name(&long, &p))
            ]
        );

        // Through a symlinked ancestor: the same socket name.
        let link = base.join("link");
        std::os::unix::fs::symlink(&base, &link).unwrap();
        assert_eq!(socket_name(&link.join("panes"), &p), name);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The root is `<run_user>/<uid>/qontinui-pty` only when the runtime dir
    /// is a real 0700 directory owned by the uid; otherwise `<tmp>/qontinui-pty-<uid>`.
    #[cfg(unix)]
    #[test]
    fn pty_holder_socket_root_choice() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::transport::own_uid();
        let mode = |p: &Path| {
            std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
        };

        // A 0700 runtime dir owned by us: the root goes under it, created 0700.
        let base = scratch("sroot-run");
        let run_user = base.join("run-user");
        let tmp = base.join("tmp");
        std::fs::create_dir_all(run_user.join(uid.to_string())).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        let rt = run_user.join(uid.to_string());
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = socket_root_in(&run_user, &tmp, uid).unwrap();
        assert_eq!(root, rt.join("qontinui-pty"));
        assert_eq!(mode(&root), 0o700);
        // Idempotent.
        assert_eq!(socket_root_in(&run_user, &tmp, uid).unwrap(), root);

        // A runtime dir that is not 0700: the /tmp fallback.
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = socket_root_in(&run_user, &tmp, uid).unwrap();
        assert_eq!(root, tmp.join(format!("qontinui-pty-{uid}")));
        assert_eq!(mode(&root), 0o700);

        // No runtime dir at all: the /tmp fallback.
        let none = base.join("no-run-user");
        assert_eq!(
            socket_root_in(&none, &tmp, uid).unwrap(),
            tmp.join(format!("qontinui-pty-{uid}"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A root another user owns, a symlink, a non-directory or a
    /// group/other-writable directory is refused; a readable one is tightened.
    #[cfg(unix)]
    #[test]
    fn pty_holder_socket_root_refusals() {
        use std::os::unix::fs::PermissionsExt;
        let uid = crate::transport::own_uid();
        let base = scratch("sroot-refuse");
        let none = base.join("no-run-user");

        // Owned by someone else: we create it as ourselves, then ask for it as
        // `uid + 1` — the ownership check is against the uid asked for. The
        // runtime dir for that uid is ours too, so it is not chosen either.
        let other = uid.wrapping_add(1);
        let run_user = base.join("run-user");
        std::fs::create_dir_all(run_user.join(other.to_string())).unwrap();
        std::fs::set_permissions(
            run_user.join(other.to_string()),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let e = socket_root_in(&run_user, &base, other).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert!(e.to_string().contains("owned by uid"), "{e}");

        let tmp = base.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let root = tmp.join(format!("qontinui-pty-{uid}"));

        // A symlink to a directory we own.
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &root).unwrap();
        let e = socket_root_in(&none, &tmp, uid).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        std::fs::remove_file(&root).unwrap();

        // A plain file.
        std::fs::write(&root, b"").unwrap();
        let e = socket_root_in(&none, &tmp, uid).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        std::fs::remove_file(&root).unwrap();

        // Group/other-writable: refused, never repaired.
        std::fs::create_dir(&root).unwrap();
        for m in [0o770, 0o707, 0o777] {
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(m)).unwrap();
            let e = socket_root_in(&none, &tmp, uid).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{m:o}");
        }
        // Missing owner bits (a restrictive umask at mkdir): restored.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(socket_root_in(&none, &tmp, uid).unwrap(), root);
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // Merely readable: tightened.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(socket_root_in(&none, &tmp, uid).unwrap(), root);
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pty_holder_paths_and_pipe_name() {
        let dir = Path::new("/state/runner-a/panes");
        let p = PaneId::new("p1").unwrap();
        assert_eq!(lock_path(dir, &p), dir.join("p1.lock"));
        assert_eq!(spec_path(dir, &p), dir.join("p1.spec"));
        assert_eq!(log_path(dir, &p), dir.join("p1.log"));
        assert_eq!(scrollback_path(dir, &p), dir.join("p1.scrollback"));
        assert_eq!(report_path(dir, &p), dir.join("p1.report"));

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
