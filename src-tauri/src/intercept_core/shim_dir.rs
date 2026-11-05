//! Recognising a qontinui shim dir — ONE predicate for every PATH walker.
//!
//! The runner materializes two families of PATH-shim dirs: the always-on
//! session-restore identity dir (`qontinui-identity-<build-tag>`, sealed by an
//! [`IDENTITY_MARKER`] file) and the per-terminal install-interception dir
//! (`qontinui-shim-<terminal_id>`). A shim that walks `PATH` for the REAL tool
//! must skip EVERY such dir, not only its own: a PATH inherited from another
//! runner session (a supervisor started inside a session, then a temp runner
//! spawned from it) carries that session's shim dir too, and two shims that
//! each resolve the other as "the real tool" `exec` each other for ever — same
//! pid, so the loop is invisible in a process tree (plan
//! `2026-11-04-identity-shim-foreign-shim-ping-pong-and-secondary-runner-pty-socket-overflow`,
//! Phase 1; coord finding `a4ae1a29`).
//!
//! The predicate keys on the dir NAME (and, name-independently, on the identity
//! marker), so it also recognises a shim's OWN dir under any spelling of its
//! path — the MSYS `/c/…` and Windows `C:\…` spellings share the final name —
//! which is the own-dir defect of coord finding `085b93cb`.
//!
//! It lives in the LIB crate because the native `qontinui-shim` stub is a
//! second binary and cannot import the runner bin's modules (see
//! [`super`]). The bash and `.cmd` shim templates carry a shell
//! transliteration of it; keep the three in step.

use std::path::Path;

/// Filename prefix of the content-addressed identity shim dir
/// (`qontinui-identity-<build-tag>`).
pub const IDENTITY_DIR_PREFIX: &str = "qontinui-identity-";

/// Filename prefix of a per-terminal install-interception shim dir
/// (`qontinui-shim-<terminal_id>`).
pub const SHIM_DIR_PREFIX: &str = "qontinui-shim-";

/// Completion marker written LAST inside a materialized identity dir. Its
/// presence also identifies the dir as a qontinui shim dir whatever it is
/// named.
pub const IDENTITY_MARKER: &str = ".qontinui-identity-build";

/// Whether `dir`'s final path component names a qontinui shim dir. Pure:
/// splits on BOTH `/` and `\` whatever the host (so a Windows-spelled path
/// reads the same on every host), ignores trailing separators, and compares
/// ASCII-case-insensitively (Windows filesystems are case-insensitive and WMI
/// can report case-mangled paths).
pub fn has_shim_dir_name(dir: &str) -> bool {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    let name = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let name = name.to_ascii_lowercase();
    name.starts_with(IDENTITY_DIR_PREFIX) || name.starts_with(SHIM_DIR_PREFIX)
}

/// Whether `dir` is a qontinui shim dir: its final name starts with
/// [`IDENTITY_DIR_PREFIX`] or [`SHIM_DIR_PREFIX`], OR it holds an
/// [`IDENTITY_MARKER`]. An empty path (an empty `PATH` entry) is never one —
/// joining the marker onto it would probe the current directory instead.
pub fn is_qontinui_shim_dir(dir: &Path) -> bool {
    let spelled = dir.to_string_lossy();
    if spelled.trim_end_matches(['/', '\\']).is_empty() {
        return false;
    }
    has_shim_dir_name(&spelled) || dir.join(IDENTITY_MARKER).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shim_dir_name_prefix_is_a_shim_dir() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["qontinui-identity-abc", "qontinui-shim-term-1"] {
            let dir = tmp.path().join(name);
            // Not even created: the name alone decides.
            assert!(is_qontinui_shim_dir(&dir), "{name}");
            let trailing = format!("{}/", dir.display());
            assert!(is_qontinui_shim_dir(Path::new(&trailing)), "{trailing}");
        }
        // Windows and MSYS spellings of one dir answer alike on every host.
        assert!(has_shim_dir_name(
            r"C:\Users\x\AppData\Local\Temp\qontinui-identity-ab\"
        ));
        assert!(has_shim_dir_name(
            "/c/Users/x/AppData/Local/Temp/qontinui-identity-ab"
        ));
        assert!(has_shim_dir_name(r"C:\Temp\QONTINUI-SHIM-x"));
    }

    #[test]
    fn the_identity_marker_makes_any_dir_a_shim_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("identity-shim");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_qontinui_shim_dir(&dir), "no marker yet");
        std::fs::write(dir.join(IDENTITY_MARKER), b"tag").unwrap();
        assert!(is_qontinui_shim_dir(&dir));
    }

    #[test]
    fn a_plain_dir_is_not_a_shim_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_qontinui_shim_dir(&dir));
        // A name merely CONTAINING a prefix is not one.
        assert!(!has_shim_dir_name("/opt/my-qontinui-identity-x"));
        assert!(!has_shim_dir_name("/opt/qontinui-identity"));
        assert!(!is_qontinui_shim_dir(Path::new("")));
        assert!(!is_qontinui_shim_dir(Path::new("/")));
    }
}
