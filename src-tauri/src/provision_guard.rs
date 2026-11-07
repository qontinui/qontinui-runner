//! The one tracked-file guard every fleet provisioner consults before writing.
//!
//! [`crate::fleet_commands`], [`crate::fleet_skills`] and the two layers of
//! `.claude/agents` ([`crate::fleet_agents`], then the checkout overlay in
//! `agent_runtime::provision_agent_definitions_from_root`, which copies from a
//! checkout rather than from the binary) each write a tree into a spawned
//! session's `<cwd>/.claude/…`. Writing unconditionally is
//! correct for the case they were built for — a fresh agent worktree, where
//! nothing tracks those paths and the alternative is a session with no fleet
//! commands, skills or subagents at all.
//!
//! It is wrong where the spawn cwd is a checkout that **tracks** the
//! destination. An unconditional write there silently replaces the repo's own
//! content with the provisioner's copy and leaves the tree dirty, which then
//! blocks a pull, muddies a diff, and can be committed by an agent that never
//! touched the file.
//!
//! So: **existing + tracked ⇒ skip.** Untracked or absent ⇒ write, as before.
//! Unknown ⇒ write, too — EXCEPT inside a repository (see "UNKNOWN inside a
//! repository" below).
//!
//! ## Which repos this actually changes
//!
//! `qontinui-claude-config` is the one that matters, and it is **not** a narrow
//! case: it tracks every bundled command, so a session spawned with that repo as
//! its cwd provisions ZERO fleet commands. That is intended — it is the canonical source
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
//! Every failure mode of the probe resolves to "nothing is tracked", which means
//! the pre-existing write behaviour. A probe that did not answer inside a
//! repository is narrowed further, as the next section describes. None of them
//! aborts anything:
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
//! ## UNKNOWN inside a repository: keep what is already on disk
//!
//! One failure arm is narrowed rather than written through: the probe did not
//! answer (spawn error, timeout, non-zero exit, or its output could not be stored
//! or read back) **and** a `.git` entry exists
//! at the probed root or one of its ancestors. That is a checkout whose git
//! could not be asked — exactly the case where an unconditional write is most
//! likely to overwrite content the repository tracks and leave a dirty tree
//! that blocks the next pull (plan
//! `2026-10-04-shared-checkouts-pull-deterministically`, Phase 2). There
//! `fleet_commands` writes a command only when it does not exist yet (an
//! existing one is left alone, tracked or not), and `fleet_skills` decides per
//! skill (below).
//!
//! - The repository test is a FILESYSTEM test (`.git` present in an ancestor),
//!   never a second git spawn, so it cannot itself fail or hang. It is a
//!   heuristic: a `.git` that is not a usable repository still counts, which
//!   errs toward not overwriting.
//! - **The cost, stated (`fleet_commands`; skills are below):** in that arm an
//!   existing but STALE untracked file is
//!   not refreshed — the session gets the copy already on disk rather than the
//!   binary's. Missing files are still written, so no session loses a command
//!   it would otherwise have had, and the spawn still never aborts.
//! - A failure that is not transient — git's `safe.directory` "dubious
//!   ownership" refusal (exit 128) is one — puts the repository in this arm on
//!   EVERY spawn, so its provisioned files stop refreshing across runner
//!   upgrades until the cause is fixed. It is visible, not silent: each pass
//!   logs a `warn` and records `probe_unknown_in_repo` skips in its report.
//! - `fleet_skills` applies this arm per SKILL, not per file: a skill whose
//!   `SKILL.md` already exists is kept whole, and a skill without one is
//!   written whole (its existing helper files overwritten), so a session never
//!   gets an old `SKILL.md` beside new helpers or the reverse. The accepted
//!   cost of the second half: a helper the repository really tracks, with no
//!   tracked `SKILL.md` beside it, is overwritten in this arm — the one case
//!   where it can dirty a tracked file.
//! - The probe failure is logged once per probe at `warn`, naming the root,
//!   which arm it resolved to and the [`ProbeFailure`] that put it there, so an
//!   UNKNOWN never passes silently. The cause matters most for the non-transient
//!   case above: `dubious ownership` is only readable in git's own stderr.
//!
//! ## One process spawn per provisioning pass, not one per file
//!
//! The provisioners write dozens of commands and skill files. Probing each
//! separately would mean that many synchronous `git` spawns on the async spawn
//! path, ahead of terminal creation — on Windows, seconds of added latency on top
//! of the file writes themselves. [`TrackedPaths::probe`] instead
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

/// Upper bound, in characters, on the git stderr line a [`ProbeFailure`] carries
/// into a log line.
const PROBE_STDERR_MAX_CHARS: usize = 200;

/// Why [`run_bounded_git_ls_files`] could not answer.
///
/// Carried only as far as the log line: the keep decision is the same for all
/// three, and the provisioning report records the keep rather than its cause.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProbeFailure {
    /// git could not be started (no `git` on `PATH`), could not be polled, or its
    /// stdout could not be stored or read back. `stage` names which.
    Io { stage: &'static str, error: String },
    /// git outlived [`PROBE_TIMEOUT`] and was killed.
    TimedOut,
    /// git ran and refused. `code` is `None` for a signal; `stderr` is its first
    /// non-empty line, bounded. Empty means git said nothing OR that its stderr
    /// could not be captured, so it never proves git was silent.
    Exit { code: Option<i32>, stderr: String },
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeFailure::Io { stage, error } => write!(f, "{stage}: {error}"),
            ProbeFailure::TimedOut => write!(f, "git timed out after {PROBE_TIMEOUT:?}"),
            ProbeFailure::Exit { code, stderr } => {
                match code {
                    Some(code) => write!(f, "git exited {code}")?,
                    None => write!(f, "git was ended by a signal")?,
                }
                if stderr.is_empty() {
                    Ok(())
                } else {
                    write!(f, ": {stderr}")
                }
            }
        }
    }
}

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
    /// destination paths the provisioners already compute. NUL-delimited, so a
    /// path containing a newline or a quote cannot desync the parse.
    ///
    /// Returns an EMPTY set on every failure — see the module doc's fail-soft
    /// contract — and, when that failure happened inside a repository, marks
    /// the result UNKNOWN-in-repo so [`Self::skip_reason`] reports existing
    /// files as keepable (each provisioner decides how it uses that — see the
    /// module doc). This function never panics and never propagates.
    pub(crate) fn probe(root: &Path) -> Self {
        if !root.is_dir() {
            return Self::default();
        }
        let stdout = match run_bounded_git_ls_files(root) {
            Ok(stdout) => stdout,
            Err(cause) => return Self::unanswered(root, &cause),
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

    /// The fail-soft result of a probe that did not answer: logged at the level
    /// its arm deserves, and marked UNKNOWN-in-repo when `root` is inside one.
    fn unanswered(root: &Path, cause: &ProbeFailure) -> Self {
        let unknown_in_repo = has_git_ancestor(root);
        // `warn` only where the answer changes behaviour. Outside any
        // repository `git ls-files` ALWAYS fails (exit 128), so a warn
        // there would fire on every spawn into a plain directory and bury
        // the in-repo warning that matters.
        if unknown_in_repo {
            tracing::warn!(
                root = %root.display(),
                %cause,
                "provision guard: tracked-file probe did not answer; root is inside a repository, so existing destinations are kept (fleet_skills decides per skill)"
            );
        } else {
            tracing::debug!(
                root = %root.display(),
                %cause,
                "provision guard: tracked-file probe did not answer; root is not inside a repository, so destinations are written as before"
            );
        }
        Self {
            relative: HashSet::new(),
            unknown_in_repo,
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

    /// Why `dst` should be SKIPPED, or `None` when it should be written. It is
    /// skipped when it already exists on disk AND either git tracks it
    /// ([`SkipReason::GitTracked`]) or the probe was UNKNOWN inside a repository
    /// ([`SkipReason::ProbeUnknownInRepo`]). `relative` is `dst`'s path relative
    /// to the probed root.
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

/// Keep decisions for a FLAT destination directory that more than one layer
/// writes into, taken ONCE, before any layer writes.
///
/// `.claude/agents/` is provisioned in two layers: the embedded floor
/// ([`crate::fleet_agents`]) and then the checkout overlay
/// (`agent_runtime::provision_agent_definitions_from_root`). Asking
/// [`TrackedPaths::skip_reason`] per layer would be wrong in the UNKNOWN arm: a
/// file the floor has just written now EXISTS, so the overlay would keep the
/// floor's copy instead of the checkout's. Deciding up front keys the keep on
/// what was on disk before this pass, which is the only content the guard
/// exists to protect.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeepDecisions {
    reasons: std::collections::HashMap<String, SkipReason>,
}

impl KeepDecisions {
    /// Decide, for each candidate file `name` directly under `dst_dir`, whether
    /// it must be kept as it is on disk. `tracked` must have been probed over
    /// `dst_dir` itself, so `name` is also its relative path.
    pub(crate) fn decide<'a>(
        tracked: &TrackedPaths,
        dst_dir: &Path,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let reasons = names
            .into_iter()
            .filter_map(|name| {
                tracked
                    .skip_reason(&dst_dir.join(name), Path::new(name))
                    .map(|reason| (name.to_string(), reason))
            })
            .collect();
        Self { reasons }
    }

    /// Why `name` is kept, or `None` when this pass may write it.
    pub(crate) fn reason(&self, name: &str) -> Option<&SkipReason> {
        self.reasons.get(name)
    }
}

/// True iff `root` or any ancestor carries a `.git` entry (a directory in a
/// primary checkout, a file in a linked worktree). A pure filesystem test, so it
/// cannot fail or hang the way a second git spawn could. (`repo_tenant` has the
/// same predicate, but this module is also compiled into the `qontinui-runner`
/// binary's own module tree, which does not carry `repo_tenant`.)
///
/// The walk runs over the root AS GIVEN and over its symlink-resolved form.
/// The lexical walk alone misses the measured case: a session spawned at a
/// workspace root whose `.claude` is a SYMLINK into a checkout
/// (`<workspace>/.claude -> qontinui-claude-config/.claude`) probes
/// `<workspace>/.claude/skills`, whose lexical ancestors hold no `.git` at all,
/// while the files it writes are the checkout's tracked ones (operator box,
/// 2026-10-13 22:03:52: `wrote 44/44` into that path with no skip line).
/// `canonicalize` is a filesystem read, not a spawn; if it fails, the lexical
/// answer stands.
fn has_git_ancestor(root: &Path) -> bool {
    let any_git = |p: &Path| p.ancestors().any(|dir| dir.join(".git").exists());
    any_git(root) || std::fs::canonicalize(root).is_ok_and(|real| any_git(&real))
}

/// Spawn `git ls-files` under `root` and return its stdout, or the
/// [`ProbeFailure`] that stopped it — including a child that outlives
/// [`PROBE_TIMEOUT`], which is killed.
///
/// Implementation notes that are load-bearing rather than incidental:
///
/// - stdout goes to a TEMP FILE, not a pipe. A pipe would deadlock this
///   polling loop the moment git's output exceeded the pipe buffer, since
///   nothing drains it while we wait — and the whole point of the loop is to be
///   able to kill a child that does not finish.
/// - The repository-local git environment (`GIT_DIR`, `GIT_WORK_TREE`,
///   `GIT_INDEX_FILE`, …) is REMOVED from the child's environment by
///   [`crate::process_helpers::no_window`], which applies the one shared
///   scrub (`git_posture::scrub_repo_local_git_env`) to every git it builds.
///   `git -C` does not override an inherited `GIT_DIR`, so a runner spawned from
///   a hook or a wrapper that exports one would otherwise have the probe consult
///   the WRONG repository — and a false "tracked" silently drops a command from
///   a session.
/// - `--literal-pathspecs` disables pathspec magic and globbing, so no path this
///   module is ever handed can be reinterpreted as a pattern.
fn run_bounded_git_ls_files(root: &Path) -> Result<Vec<u8>, ProbeFailure> {
    let io = |stage: &'static str| {
        move |e: std::io::Error| ProbeFailure::Io {
            stage,
            error: e.to_string(),
        }
    };
    let out_file = tempfile::NamedTempFile::new().map_err(io("create the output file"))?;
    let handle = out_file.reopen().map_err(io("open the output file"))?;
    // A temp file for the same reason as stdout, and only ever read on failure.
    // Capturing it is a diagnostic, so it must not be able to change the answer:
    // a file that cannot be made falls back to discarding stderr rather than
    // turning a probe that would have answered into an UNKNOWN.
    let err_file = tempfile::NamedTempFile::new().ok();
    let err_stdio = err_file
        .as_ref()
        .and_then(|f| f.reopen().ok())
        .map_or_else(Stdio::null, Stdio::from);

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
        .stdin(Stdio::null())
        // Tracing would put its own lines ahead of git's error, and
        // `first_stderr_line` would report those instead of the cause. Best
        // effort: a trace2 target set in git config still applies, and then the
        // log names a trace line rather than the cause.
        .env_remove("GIT_TRACE")
        .env_remove("GIT_TRACE2")
        .env_remove("GIT_TRACE2_PERF")
        .env_remove("GIT_TRACE2_EVENT")
        .env_remove("GIT_TRACE_SETUP")
        .stdout(Stdio::from(handle))
        .stderr(err_stdio)
        .spawn()
        .map_err(io("start git"))?;

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
                    return Err(ProbeFailure::TimedOut);
                }
                std::thread::sleep(PROBE_POLL_INTERVAL);
            }
            Err(e) => return Err(io("poll git")(e)),
        }
    };

    if !status.success() {
        let stderr = err_file
            .and_then(|f| std::fs::read(f.path()).ok())
            .unwrap_or_default();
        return Err(ProbeFailure::Exit {
            code: status.code(),
            stderr: first_stderr_line(&stderr),
        });
    }
    std::fs::read(out_file.path()).map_err(io("read the output file"))
}

/// The first non-empty line of git's stderr, bounded to
/// [`PROBE_STDERR_MAX_CHARS`] so a log line cannot grow with what git printed.
fn first_stderr_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(PROBE_STDERR_MAX_CHARS)
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Tempdir git helpers shared by this module's tests and by the tests of the
    //! modules that consume the guard, so they do not each re-spell `git init` +
    //! `git add`.

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
        assert!(tracked.skip_reason(&f, Path::new("tracked.md")).is_some());
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

        assert!(!TrackedPaths::probe(tmp.path())
            .skip_reason(&f, Path::new("untracked.md"))
            .is_some());
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
        assert!(tracked.skip_reason(&f, Path::new("loose.md")).is_none());
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
            tracked
                .skip_reason(&existing, Path::new("existing.md"))
                .is_some(),
            "an existing file must not be overwritten when git could not be asked"
        );
        assert!(
            !tracked
                .skip_reason(&absent, Path::new("absent.md"))
                .is_some(),
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
        assert!(tracked.skip_reason(&f, Path::new("loose.md")).is_none());
    }

    /// The measured shape: a workspace root (no `.git` anywhere above it) whose
    /// `.claude` is a SYMLINK into a checkout. The lexical ancestors of
    /// `<workspace>/.claude/skills` carry no `.git`, so an unanswered probe used
    /// to read as "outside any repository" and overwrite the checkout's tracked
    /// files. The resolved path's ancestors do carry one.
    #[test]
    fn an_unanswered_probe_through_a_symlink_into_a_repo_keeps_existing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        let checkout = tmp.path().join("checkout");
        let real_claude = checkout.join(".claude");
        let real_skills = real_claude.join("skills");
        std::fs::create_dir_all(&real_skills).unwrap();
        // A `.git` file whose gitdir is gone: the probe cannot answer.
        std::fs::write(
            checkout.join(".git"),
            b"gitdir: /nonexistent/qontinui-provision-guard-symlink-test\n",
        )
        .unwrap();
        let existing = real_skills.join("existing.md");
        std::fs::write(&existing, b"the checkout's own body").unwrap();

        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let link = workspace.join(".claude");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&real_claude, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&real_claude, &link);
        if let Err(e) = linked {
            // Windows without Developer Mode cannot create a symlink; say so
            // rather than pass on a fixture that was never built.
            eprintln!("SKIP: cannot create a directory symlink here ({e})");
            return;
        }
        let probed = link.join("skills");
        assert!(
            !probed.ancestors().any(|d| d.join(".git").exists()),
            "fixture: the LEXICAL ancestors must carry no .git, or this pins nothing"
        );

        let tracked = TrackedPaths::probe(&probed);
        assert!(
            tracked.is_unknown_in_repo(),
            "a probe that failed under a symlink into a repo is UNKNOWN in that repo"
        );
        assert_eq!(
            tracked.skip_reason(&probed.join("existing.md"), Path::new("existing.md")),
            Some(SkipReason::ProbeUnknownInRepo),
            "the checkout's existing file must be kept, not overwritten"
        );
    }

    /// Fail-soft: a directory that does not exist yields an empty set rather
    /// than a spawn attempt or a panic.
    #[test]
    fn a_missing_root_probes_to_nothing_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracked = TrackedPaths::probe(&tmp.path().join("does-not-exist"));
        assert!(!tracked.contains(Path::new("anything.md")));
    }

    /// `skip_reason` requires BOTH halves: a tracked path whose file has been
    /// deleted is written, not skipped. Pinned because the doc comment on
    /// `skip_reason` explains the cost of that choice, and a silent flip would
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
            tracked.skip_reason(&f, Path::new("gone.md")).is_none(),
            "but with no file on disk there is nothing to clobber, so we write"
        );
    }

    /// A refusal carries git's exit code and its own words into the failure, so
    /// the `warn` on a checkout stuck in the UNKNOWN arm names the cause (a
    /// `safe.directory` refusal is exit 128 too, and only its text tells it from
    /// any other). Asserted by shape rather than by text: git translates it.
    #[test]
    fn a_refused_probe_names_the_exit_code_and_gits_own_words() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join(".git"),
            b"gitdir: /nonexistent/qontinui-provision-guard-failure-test\n",
        )
        .unwrap();

        let failure = run_bounded_git_ls_files(tmp.path()).expect_err("a dead .git refuses");
        let ProbeFailure::Exit { code, stderr } = &failure else {
            panic!("a dead .git is a refusal, not {failure:?}");
        };
        assert_eq!(*code, Some(128));
        assert!(!stderr.is_empty(), "git's own words must be captured");
        assert!(!stderr.contains('\n'), "one line: {stderr:?}");
        assert_eq!(failure.to_string(), format!("git exited 128: {stderr}"));
    }

    #[test]
    fn the_failure_text_says_nothing_it_did_not_observe() {
        let silent = ProbeFailure::Exit {
            code: Some(1),
            stderr: String::new(),
        };
        assert_eq!(silent.to_string(), "git exited 1");
        let signalled = ProbeFailure::Exit {
            code: None,
            stderr: String::new(),
        };
        assert_eq!(signalled.to_string(), "git was ended by a signal");
        assert!(ProbeFailure::TimedOut.to_string().contains("timed out"));
    }

    /// The first line that says anything, cut at a CHARACTER boundary: a byte
    /// slice would panic on a translated message, and a panic here would be an
    /// aborted spawn, which the module doc forbids.
    #[test]
    fn the_stderr_line_is_the_first_non_empty_one_and_is_bounded() {
        assert_eq!(
            first_stderr_line(b"\n  \nfatal: one\nfatal: two\n"),
            "fatal: one"
        );
        assert_eq!(first_stderr_line(b""), "");
        let long = "é".repeat(PROBE_STDERR_MAX_CHARS + 50);
        assert_eq!(
            first_stderr_line(long.as_bytes()).chars().count(),
            PROBE_STDERR_MAX_CHARS
        );
    }
}
