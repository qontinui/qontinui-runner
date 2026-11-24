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
//!
//! One qontinui shim dir carries neither a prefixed name nor the marker: the
//! PERSISTENT identity dir (`~/.qontinui/runner/identity-shim`) that the
//! Settings opt-in prepends to the user PATH. [`is_persistent_identity_dir`]
//! recognises it by path, which means reading the ambient home.

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

/// Whether `dir` is the PERSISTENT identity-shim dir,
/// [`crate::profile_cli::identity_shim_dir`] (`~/.qontinui/runner/identity-shim`).
///
/// That dir is the one qontinui shim dir [`is_qontinui_shim_dir`] cannot
/// recognise. Its name carries no shim-dir prefix and it holds no
/// [`IDENTITY_MARKER`], because the Settings opt-in materializes only the
/// `qontinui-shim` stub into it as `claude.exe`. The opt-in also PREPENDS it to
/// the user PATH, so a walker that wants the REAL `claude` must skip it too, or
/// it takes the stub. The native stub's `own_shim_dirs` and `process_tree`'s
/// `is_shim_dir_exe_path` exclude it by path with comparisons of their own.
///
/// This reads the ambient home (`$QONTINUI_HOME`, else the user's home), so a
/// walker over many entries resolves it once and calls
/// [`is_persistent_identity_dir_in`] instead. Trailing `/` and `\` are
/// ignored. On Windows the two spellings also compare with `/` and `\` alike
/// and ASCII-case-insensitively. `false` when no home resolves (then no
/// persistent dir exists to match).
pub fn is_persistent_identity_dir(dir: &Path) -> bool {
    is_persistent_identity_dir_in(dir, crate::profile_cli::identity_shim_dir().as_deref())
}

/// [`is_persistent_identity_dir`] against an already-resolved persistent dir,
/// so a PATH walk reads the ambient home once and gives one answer for the
/// whole walk. `None` (no home) matches nothing.
pub fn is_persistent_identity_dir_in(dir: &Path, persistent: Option<&Path>) -> bool {
    persistent.is_some_and(|persistent| same_dir_spelling(dir, persistent, cfg!(windows)))
}

/// Pure core of [`is_persistent_identity_dir`]: whether `a` and `b` spell the
/// same directory, ignoring trailing separators. `windows` folds `/` into `\`
/// and compares ASCII-case-insensitively, as that filesystem does. An empty
/// spelling never matches.
fn same_dir_spelling(a: &Path, b: &Path, windows: bool) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy();
        let s = if windows {
            s.replace('/', "\\")
        } else {
            s.into_owned()
        };
        s.trim_end_matches(['/', '\\']).to_string()
    };
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if windows {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_dir_spelling_ignores_trailing_separators_and_folds_windows_spelling() {
        let p = |s: &str| std::path::PathBuf::from(s);
        assert!(same_dir_spelling(
            &p("/home/u/.qontinui/runner/identity-shim/"),
            &p("/home/u/.qontinui/runner/identity-shim"),
            false
        ));
        // Unix paths are case-sensitive, and `\` is an ordinary character there.
        assert!(!same_dir_spelling(
            &p("/home/u/.qontinui/runner/Identity-Shim"),
            &p("/home/u/.qontinui/runner/identity-shim"),
            false
        ));
        assert!(same_dir_spelling(
            &p(r"C:/Users/X/.qontinui/runner/identity-shim\"),
            &p(r"c:\users\x\.qontinui\runner\IDENTITY-SHIM"),
            true
        ));
        // A different dir, a parent, and the empty spelling never match.
        assert!(!same_dir_spelling(
            &p(r"C:\Users\x\.qontinui\runner"),
            &p(r"C:\Users\x\.qontinui\runner\identity-shim"),
            true
        ));
        assert!(!same_dir_spelling(&p(""), &p(""), false));
        assert!(!same_dir_spelling(&p("/"), &p("/"), false));
    }

    #[test]
    fn the_persistent_identity_dir_is_recognised_and_its_siblings_are_not() {
        let _amb = crate::test_env::isolated_ambient();
        let persistent =
            crate::profile_cli::identity_shim_dir().expect("the fixture provides a home");
        assert!(is_persistent_identity_dir(&persistent));
        let trailing = format!("{}/", persistent.display());
        assert!(is_persistent_identity_dir(Path::new(&trailing)));
        // Its parent, a sibling, and a qontinui shim dir by name are not it.
        assert!(!is_persistent_identity_dir(persistent.parent().unwrap()));
        assert!(!is_persistent_identity_dir(
            &persistent.with_file_name("identity-shim-old")
        ));
        assert!(!is_persistent_identity_dir(Path::new(
            "/tmp/qontinui-identity-abc"
        )));
        // The name-and-marker predicate still does not recognise it, which is
        // why this second predicate exists.
        assert!(!is_qontinui_shim_dir(&persistent));
    }

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
