//! The one tracked-file guard both fleet provisioners consult before writing.
//!
//! [`crate::fleet_commands`] and [`crate::fleet_skills`] each write a bundled
//! tree into a spawned session's `<cwd>/.claude/…`, unconditionally. That is
//! correct for the case they were built for — a fresh agent worktree, where
//! nothing tracks those paths and the alternative is a session with no fleet
//! commands or skills at all.
//!
//! It is wrong where the spawn cwd is a checkout that **tracks** the
//! destination. Two repos in this fleet track exactly
//! `.claude/commands/vet-plan.md` and `.claude/commands/implement-plan.md` —
//! `qontinui-claude-config` (the canonical source of those very bodies) and
//! `qontinui-dev-notes`. Provisioning into either silently replaces the repo's
//! own content with the binary's embedded copy and leaves the tree dirty, which
//! then blocks a pull, muddies a diff, and can be committed by an agent that
//! never touched the file.
//!
//! So: **existing + tracked ⇒ skip.** Untracked, absent, or unknown ⇒ write, as
//! before.
//!
//! ## Fail-soft is a hard requirement
//!
//! Every failure mode of the check resolves to "not tracked", i.e. to the
//! pre-existing write behaviour:
//!
//! - no `git` binary on `PATH` (spawn error),
//! - the destination's directory does not exist or is unreadable,
//! - the path is not inside any git repository,
//! - a `.git` that exists but cannot be read,
//! - any non-zero exit, any signal, any unexpected output.
//!
//! A skipped write must NEVER become an aborted spawn, and neither must a
//! failed *probe*. The consequence of guessing "not tracked" is exactly the
//! behaviour that shipped before this guard; the consequence of guessing
//! "tracked" would be a session missing its commands. The asymmetry is why the
//! default is write.

use std::path::Path;
use std::process::{Command, Stdio};

/// True iff `dst` exists AND is tracked in the git repository that encloses it.
///
/// Detected with `git ls-files --error-unmatch -- <file>`, run with the
/// destination's own directory as cwd so git resolves the enclosing repo the
/// same way a human standing there would (this respects worktrees, submodules
/// and `GIT_DIR` without this module having to model any of them).
///
/// Returns `false` on every error — see the module doc's fail-soft contract.
/// The probe never panics and never propagates: the caller gets a bare bool and
/// writes when it is false.
pub(crate) fn is_git_tracked(dst: &Path) -> bool {
    // Absent files are never "tracked" for our purposes: there is nothing to
    // clobber, so the write is unambiguously correct. (A deleted-but-tracked
    // path would report tracked to git; restoring it is not a clobber.)
    if !dst.exists() {
        return false;
    }
    let Some(dir) = dst.parent() else {
        return false;
    };
    let Some(name) = dst.file_name() else {
        return false;
    };
    if !dir.is_dir() {
        return false;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg("--")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match output {
        // Exit 0 means git matched the pathspec against the index: tracked.
        Ok(o) => o.status.success(),
        // No `git` on PATH, permission denied spawning it, …
        Err(_) => false,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Tempdir git helpers shared by this module's tests and by the two
    //! provisioners' tracked/untracked arm tests, so the three do not each
    //! re-spell `git init` + `git add`.

    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Initialise a real repo in `dir` (quiet, no global config dependence).
    pub(crate) fn git_init(dir: &Path) {
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("run git")
                .success();
            assert!(ok, "git {args:?} should succeed");
        }
    }

    pub(crate) fn git_add(dir: &Path, path: &Path) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("add")
            .arg("--")
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git add")
            .success();
        assert!(ok, "git add should succeed");
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{git_add, git_init};
    use super::*;

    #[test]
    fn a_tracked_file_is_reported_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        let f = tmp.path().join("tracked.md");
        std::fs::write(&f, b"body").unwrap();
        git_add(tmp.path(), &f);
        assert!(is_git_tracked(&f));
    }

    #[test]
    fn an_untracked_file_in_a_repo_is_not_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        let f = tmp.path().join("untracked.md");
        std::fs::write(&f, b"body").unwrap();
        assert!(!is_git_tracked(&f));
    }

    /// Fail-soft: a path in no repository at all must read as NOT tracked, so
    /// the caller writes exactly as it did before this guard existed.
    #[test]
    fn a_file_outside_any_repo_is_not_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let f = tmp.path().join("loose.md");
        std::fs::write(&f, b"body").unwrap();
        assert!(!is_git_tracked(&f));
    }

    #[test]
    fn an_absent_file_is_not_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        assert!(!is_git_tracked(&tmp.path().join("nope.md")));
    }
}
