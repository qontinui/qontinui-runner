//! The one tracked-file guard both fleet provisioners consult before writing.
//!
//! [`crate::fleet_commands`] and [`crate::fleet_skills`] each write a bundled
//! tree into a spawned session's `<cwd>/.claude/…`, unconditionally. That is
//! correct for the case they were built for — a fresh agent worktree, where
//! nothing tracks those paths and the alternative is a session with no fleet
//! commands or skills at all.
//!
//! It is wrong where the spawn cwd is a checkout that **tracks** the
//! destination. An unconditional write there silently replaces the repo's own
//! content with the binary's embedded copy and leaves the tree dirty, which then
//! blocks a pull, muddies a diff, and can be committed by an agent that never
//! touched the file.
//!
//! So: **existing + tracked ⇒ skip.** Untracked or absent ⇒ write, as before.
//! Unknown ⇒ write, too — EXCEPT inside a repository (next section).
//!
//! ## Which repos this actually changes, measured
//!
//! Measured 2026-08-30 with `git ls-files` against both checkouts:
//!
//! | Repo | Tracks | Effect of this guard |
//! |---|---|---|
//! | `qontinui-claude-config` | **all 7** bundled commands (plus ~90 others) | every command skipped; 0 written |
//! | `qontinui-dev-notes` | exactly `vet-plan.md` + `implement-plan.md` | those 2 skipped; the other 5 written |
//!
//! The `qontinui-claude-config` row is the important one and is **not** the
//! narrow two-file case: a session spawned with that repo as its cwd now
//! provisions ZERO fleet commands. That is intended — it is the canonical source
//! of those very bodies, and its own copies are the ones a session there should
//! resolve — but it is a real blast radius, so it is stated rather than implied.
//!
//! **A tracked file outranks an account override.** `crate::agent_commands`
//! resolves `fresh fetch → disk cache → embedded default`, and this guard sits
//! AFTER that resolution: whatever won, a tracked destination is still skipped.
//! In a tracked checkout the override layer is therefore inert. That follows
//! from the rule — an override written over tracked content dirties the tree
//! exactly as an embedded default would — but it is a behaviour change worth
//! naming, because nothing in the log line says "an override lost".
//!
//! ## Fail-soft is a hard requirement
//!
//! Every failure mode of the probe resolves to "nothing is tracked", i.e. to the
//! pre-existing write behaviour:
//!
//! - no `git` binary on `PATH` (spawn error),
//! - the destination's directory does not exist or is unreadable,
//! - the path is not inside any git repository,
//! - a `.git` that exists but cannot be read,
//! - any non-zero exit, any signal, any unparseable output,
//! - **and a `git` that HANGS** — the probe carries its own wall-clock bound
//!   ([`PROBE_TIMEOUT`]) and kills the child when it expires. Without that, the
//!   list above would be an enumeration with a hole in it: a probe that never
//!   returns is a spawn that never starts, which is the exact outcome this
//!   contract exists to forbid.
//!
//! A skipped write must NEVER become an aborted spawn, and neither must a failed
//! or slow probe. The consequence of guessing "not tracked" is exactly the
//! behaviour that shipped before this guard; the consequence of guessing
//! "tracked" would be a session missing its commands. The asymmetry is why the
//! default is write.
//!
//! ## UNKNOWN inside a repository: write only ABSENT destinations
//!
//! One failure arm is narrowed rather than written through: the probe did not
//! answer (spawn error, timeout, non-zero exit) **and** a `.git` entry exists
//! at the probed root or one of its ancestors. That is a checkout whose git
//! could not be asked — exactly the case where an unconditional write is most
//! likely to overwrite content the repository tracks and leave a dirty tree
//! that blocks the next pull (plan
//! `2026-10-04-shared-checkouts-pull-deterministically`, Phase 2). There the
//! provisioners write a destination only when it does not exist yet; an
//! existing file is left alone, tracked or not.
//!
//! - The repository test is a FILESYSTEM test (`.git` present in an ancestor),
//!   never a second git spawn, so it cannot itself fail or hang. It is a
//!   heuristic: a `.git` that is not a usable repository still counts, which
//!   errs toward not overwriting.
//! - **The cost, stated:** in that arm an existing but STALE untracked file is
//!   not refreshed — the session gets the copy already on disk rather than the
//!   binary's. Missing files are still written, so no session loses a command
//!   it would otherwise have had, and the spawn still never aborts.
//! - A failure that is not transient — git's `safe.directory` "dubious
//!   ownership" refusal (exit 128) is one — puts the repository in this arm on
//!   EVERY spawn, so its provisioned files stop refreshing across runner
//!   upgrades until the cause is fixed. It is visible, not silent: each pass
//!   logs a `warn` and records `probe_unknown_in_repo` skips in its report.
//! - `fleet_skills` applies this arm per SKILL, not per file: a skill whose
//!   directory already exists is kept whole, so a session never gets an old
//!   `SKILL.md` beside new helper files.
//! - The probe failure is logged once per probe at `warn`, naming the root and
//!   which arm it resolved to, so an UNKNOWN never passes silently.
//!
//! ## One process spawn per provisioning pass, not one per file
//!
//! The provisioners write 7 commands and ~13 skill files. Probing each
//! separately would mean 20 synchronous `git` spawns on the async spawn path,
//! ahead of terminal creation — on Windows, seconds of added latency for what
//! used to be a handful of `fs::write` calls. [`TrackedPaths::probe`] instead
//! runs `git ls-files` ONCE over the destination subtree and answers every
//! subsequent question from memory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::capability_manifest::SkipReason;

/// Wall-clock bound on the one `git ls-files` the probe runs. Generous relative
/// to a local index read (milliseconds), tight relative to a spawn the operator
/// is waiting on. Expiry is a fail-soft UNKNOWN (see the module doc), never an error.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the probe checks whether the child has exited.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The set of paths git tracks under one destination directory, as paths
/// RELATIVE to that directory.
///
/// Built once per provisioning pass by [`TrackedPaths::probe`]. An empty set is
/// the fail-soft answer to every failure. Outside a repository it is
/// indistinguishable from a genuinely untracked directory — deliberately, since
/// both mean "write as before"; inside one, a failure also sets
/// `unknown_in_repo`, which keeps existing destinations.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackedPaths {
    relative: HashSet<PathBuf>,
    /// The probe did NOT answer and the probed root sits inside a repository
    /// (a `.git` entry at the root or an ancestor). In that state every
    /// EXISTING destination is skipped — see the module doc's "UNKNOWN inside a
    /// repository" section.
    unknown_in_repo: bool,
}

impl TrackedPaths {
    /// Run one `git ls-files` over `root` and collect what it tracks.
    ///
    /// `git -C <root> ls-files -z -- .` lists tracked paths under the cwd,
    /// relative to it, so the output maps straight onto the relative
    /// destination paths both provisioners already compute. NUL-delimited, so a
    /// path containing a newline or a quote cannot desync the parse.
    ///
    /// Returns an EMPTY set on every failure — see the module doc's fail-soft
    /// contract — and, when that failure happened inside a repository, marks
    /// the result UNKNOWN-in-repo so [`Self::should_skip`] protects existing
    /// files. This function never panics and never propagates.
    pub(crate) fn probe(root: &Path) -> Self {
        if !root.is_dir() {
            return Self::default();
        }
        let Some(stdout) = run_bounded_git_ls_files(root) else {
            let unknown_in_repo = has_git_ancestor(root);
            tracing::warn!(
                root = %root.display(),
                unknown_in_repo,
                "provision guard: tracked-file probe did not answer (git missing, failed or timed out); {}",
                if unknown_in_repo {
                    "root is inside a repository, so only ABSENT destinations will be written"
                } else {
                    "root is not inside a repository, so destinations are written as before"
                }
            );
            return Self {
                relative: HashSet::new(),
                unknown_in_repo,
            };
        };
        let relative = stdout
            .split(|b| *b == 0)
            .filter(|seg| !seg.is_empty())
            .filter_map(|seg| std::str::from_utf8(seg).ok())
            .map(|s| PathBuf::from(s.trim_end_matches('/')))
            .collect();
        Self {
            relative,
            unknown_in_repo: false,
        }
    }

    /// True iff the probe could not answer for a root inside a repository.
    pub(crate) fn is_unknown_in_repo(&self) -> bool {
        self.unknown_in_repo
    }

    /// True iff git tracks `relative` (a path relative to the probed root).
    pub(crate) fn contains(&self, relative: &Path) -> bool {
        self.relative.contains(relative)
    }

    /// True iff `dst` should be SKIPPED: it already exists on disk AND either git
    /// tracks it or the probe was UNKNOWN inside a repository. `relative` is
    /// `dst`'s path relative to the probed root.
    ///
    /// The existence half matters because a tracked path the user has DELETED
    /// is not content this guard can clobber, and skipping it would leave the
    /// session without that command for no gain. The cost is stated rather than
    /// hidden: writing the embedded body to a tracked-but-deleted path makes git
    /// report it MODIFIED rather than deleted. That is a smaller wrong than
    /// either alternative, but it is not nothing — do not read the existence
    /// check as "restoring a deleted file is harmless".
    ///
    /// When the probe was UNKNOWN inside a repository, every EXISTING `dst` is
    /// skipped whether or not it is tracked: nothing could say it is safe to
    /// overwrite. An absent `dst` is still written.
    pub(crate) fn should_skip(&self, dst: &Path, relative: &Path) -> bool {
        self.skip_reason(dst, relative).is_some()
    }

    /// Why `dst` is skipped, or `None` when it should be written. The two arms
    /// of [`Self::should_skip`] carry DIFFERENT reasons into the provisioning
    /// report: a tracked destination is [`SkipReason::GitTracked`], an existing
    /// destination kept only because the probe could not answer inside a
    /// repository is [`SkipReason::ProbeUnknownInRepo`].
    pub(crate) fn skip_reason(&self, dst: &Path, relative: &Path) -> Option<SkipReason> {
        if !dst.exists() {
            return None;
        }
        if self.contains(relative) {
            Some(SkipReason::GitTracked)
        } else if self.unknown_in_repo {
            Some(SkipReason::ProbeUnknownInRepo)
        } else {
            None
        }
    }
}

/// True iff `root` or any ancestor carries a `.git` entry (a directory in a
/// primary checkout, a file in a linked worktree). A pure filesystem test, so it
/// cannot fail or hang the way a second git spawn could. (`repo_tenant` has the
/// same predicate, but this module is also compiled into the `qontinui-runner`
/// binary's own module tree, which does not carry `repo_tenant`.)
fn has_git_ancestor(root: &Path) -> bool {
    root.ancestors().any(|dir| dir.join(".git").exists())
}

/// Spawn `git ls-files` under `root` and return its stdout, or `None` on any
/// failure — including a child that outlives [`PROBE_TIMEOUT`], which is killed.
///
/// Implementation notes that are load-bearing rather than incidental:
///
/// - stdout goes to a TEMP FILE, not a pipe. A pipe would deadlock this
///   polling loop the moment git's output exceeded the pipe buffer, since
///   nothing drains it while we wait — and the whole point of the loop is to be
///   able to kill a child that does not finish.
/// - `GIT_DIR` / `GIT_WORK_TREE` are REMOVED from the child's environment.
///   `git -C` does not override an inherited `GIT_DIR`, so a runner spawned from
///   a hook or a wrapper that exports one would otherwise have the probe consult
///   the WRONG repository — and a false "tracked" silently drops a command from
///   a session.
/// - `--literal-pathspecs` disables pathspec magic and globbing, so no path this
///   module is ever handed can be reinterpreted as a pattern.
fn run_bounded_git_ls_files(root: &Path) -> Option<Vec<u8>> {
    let out_file = tempfile::NamedTempFile::new().ok()?;
    let handle = out_file.reopen().ok()?;

    // `no_window` rather than `Command::new`: this probe runs on EVERY session
    // spawn, so on Windows a bare `Command` pops a console window every time
    // (`process_helpers::console_window_guard` enforces this crate-wide).
    let mut child = crate::process_helpers::no_window("git")
        .arg("-C")
        .arg(root)
        .arg("--literal-pathspecs")
        .arg("ls-files")
        .arg("-z")
        .arg("--")
        .arg(".")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::from(handle))
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Fail-soft: kill and report UNKNOWN. Reap so the child
                    // cannot become a zombie for the process's lifetime.
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(PROBE_POLL_INTERVAL);
            }
            Err(_) => return None,
        }
    };

    if !status.success() {
        return None;
    }
    std::fs::read(out_file.path()).ok()
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

    /// Assert `dir` is not inside ANY git repository.
    ///
    /// Guards the one test that means to exercise the "not a repository" arm:
    /// if `TMPDIR` happened to sit inside a checkout, that test would silently
    /// decay into a duplicate of the untracked-file test and still pass, leaving
    /// the arm it names unverified.
    pub(crate) fn assert_not_in_any_repo(dir: &Path) {
        let inside = Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("rev-parse")
            .arg("--is-inside-work-tree")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !inside,
            "{} is inside a git repo, so this test cannot exercise the \
             not-a-repository arm it exists for",
            dir.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{assert_not_in_any_repo, git_add, git_init};
    use super::*;

    #[test]
    fn a_tracked_file_is_reported_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        let f = tmp.path().join("tracked.md");
        std::fs::write(&f, b"body").unwrap();
        git_add(tmp.path(), &f);

        let tracked = TrackedPaths::probe(tmp.path());
        assert!(tracked.contains(Path::new("tracked.md")));
        assert!(tracked.should_skip(&f, Path::new("tracked.md")));
    }

    /// The probe reports paths RELATIVE to the probed root, including through
    /// subdirectories — the shape `fleet_skills` needs, since a skill is a
    /// directory.
    #[test]
    fn a_tracked_file_in_a_subdirectory_is_keyed_by_its_relative_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        let sub = tmp.path().join("skill-a");
        std::fs::create_dir_all(&sub).unwrap();
        let f = sub.join("SKILL.md");
        std::fs::write(&f, b"body").unwrap();
        git_add(tmp.path(), &f);

        let tracked = TrackedPaths::probe(tmp.path());
        assert!(tracked.contains(&PathBuf::from("skill-a").join("SKILL.md")));
        assert!(!tracked.contains(Path::new("SKILL.md")));
    }

    #[test]
    fn an_untracked_file_in_a_repo_is_not_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        let f = tmp.path().join("untracked.md");
        std::fs::write(&f, b"body").unwrap();

        assert!(!TrackedPaths::probe(tmp.path()).should_skip(&f, Path::new("untracked.md")));
    }

    /// Fail-soft: a path in no repository at all must read as NOT tracked, so
    /// the caller writes exactly as it did before this guard existed.
    #[test]
    fn a_file_outside_any_repo_is_not_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        let f = tmp.path().join("loose.md");
        std::fs::write(&f, b"body").unwrap();

        let tracked = TrackedPaths::probe(tmp.path());
        assert!(!tracked.contains(Path::new("loose.md")));
        assert!(!tracked.should_skip(&f, Path::new("loose.md")));
    }

    /// A probe that cannot answer INSIDE a repository protects every existing
    /// destination (tracked or not) and still lets an absent one be written.
    /// The repository here has a `.git` FILE pointing at a gitdir that does not
    /// exist, so `git ls-files` fails while the filesystem test still sees a
    /// repository — the exact shape of a broken linked worktree.
    #[test]
    fn an_unanswered_probe_inside_a_repo_writes_only_absent_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        std::fs::write(
            tmp.path().join(".git"),
            b"gitdir: /nonexistent/qontinui-provision-guard-test\n",
        )
        .unwrap();
        let sub = tmp.path().join("commands");
        std::fs::create_dir_all(&sub).unwrap();
        let existing = sub.join("existing.md");
        std::fs::write(&existing, b"body").unwrap();
        let absent = sub.join("absent.md");

        let tracked = TrackedPaths::probe(&sub);
        assert!(
            tracked.is_unknown_in_repo(),
            "the probe failed inside a repo"
        );
        assert!(
            tracked.should_skip(&existing, Path::new("existing.md")),
            "an existing file must not be overwritten when git could not be asked"
        );
        assert!(
            !tracked.should_skip(&absent, Path::new("absent.md")),
            "an absent destination is still written"
        );
    }

    /// The narrowed arm must NOT fire outside a repository: there an unanswered
    /// probe keeps the pre-guard write-everything behaviour.
    #[test]
    fn an_unanswered_probe_outside_any_repo_still_writes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        let f = tmp.path().join("loose.md");
        std::fs::write(&f, b"body").unwrap();

        let tracked = TrackedPaths::probe(tmp.path());
        assert!(!tracked.is_unknown_in_repo());
        assert!(!tracked.should_skip(&f, Path::new("loose.md")));
    }

    /// Fail-soft: a directory that does not exist yields an empty set rather
    /// than a spawn attempt or a panic.
    #[test]
    fn a_missing_root_probes_to_nothing_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracked = TrackedPaths::probe(&tmp.path().join("does-not-exist"));
        assert!(!tracked.contains(Path::new("anything.md")));
    }

    /// `should_skip` requires BOTH halves: a tracked path whose file has been
    /// deleted is written, not skipped. Pinned because the doc comment on
    /// `should_skip` explains the cost of that choice, and a silent flip would
    /// make that explanation wrong.
    #[test]
    fn a_tracked_but_deleted_path_is_not_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        git_init(tmp.path());
        let f = tmp.path().join("gone.md");
        std::fs::write(&f, b"body").unwrap();
        git_add(tmp.path(), &f);
        std::fs::remove_file(&f).unwrap();

        let tracked = TrackedPaths::probe(tmp.path());
        assert!(
            tracked.contains(Path::new("gone.md")),
            "git still tracks the path"
        );
        assert!(
            !tracked.should_skip(&f, Path::new("gone.md")),
            "but with no file on disk there is nothing to clobber, so we write"
        );
    }
}
