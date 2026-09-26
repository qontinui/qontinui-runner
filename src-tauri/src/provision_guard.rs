//! The one tracked-file guard every session-asset provisioner consults before
//! writing.
//!
//! [`crate::fleet_commands`], [`crate::fleet_skills`] and the subagent
//! definitions ([`crate::fleet_agents`] plus the checkout overlay in
//! `agent_runtime`) each write a bundled tree into a spawned session's
//! `<cwd>/.claude/…`, unconditionally. That is
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
//! So: **existing + tracked ⇒ skip.** Untracked, absent, or unknown ⇒ write, as
//! before.
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

/// Wall-clock bound on the one `git ls-files` the probe runs. Generous relative
/// to a local index read (milliseconds), tight relative to a spawn the operator
/// is waiting on. Expiry is a fail-soft "nothing tracked", never an error.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the probe checks whether the child has exited.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The set of paths git tracks under one destination directory, as paths
/// RELATIVE to that directory.
///
/// Built once per provisioning pass by [`TrackedPaths::probe`]. An empty set is
/// the fail-soft answer to every failure, and is indistinguishable from a
/// genuinely untracked directory — deliberately, since both mean "write as
/// before".
#[derive(Clone, Debug, Default)]
pub(crate) struct TrackedPaths {
    relative: HashSet<PathBuf>,
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
    /// contract. This function never panics and never propagates.
    pub(crate) fn probe(root: &Path) -> Self {
        if !root.is_dir() {
            return Self::default();
        }
        let Some(stdout) = run_bounded_git_ls_files(root) else {
            return Self::default();
        };
        let relative = stdout
            .split(|b| *b == 0)
            .filter(|seg| !seg.is_empty())
            .filter_map(|seg| std::str::from_utf8(seg).ok())
            .map(|s| PathBuf::from(s.trim_end_matches('/')))
            .collect();
        Self { relative }
    }

    /// True iff git tracks `relative` (a path relative to the probed root).
    pub(crate) fn contains(&self, relative: &Path) -> bool {
        self.relative.contains(relative)
    }

    /// True iff `dst` should be SKIPPED: it already exists on disk AND git
    /// tracks it. `relative` is `dst`'s path relative to the probed root.
    ///
    /// The existence half matters because a tracked path the user has DELETED
    /// is not content this guard can clobber, and skipping it would leave the
    /// session without that command for no gain. The cost is stated rather than
    /// hidden: writing the embedded body to a tracked-but-deleted path makes git
    /// report it MODIFIED rather than deleted. That is a smaller wrong than
    /// either alternative, but it is not nothing — do not read the existence
    /// check as "restoring a deleted file is harmless".
    pub(crate) fn should_skip(&self, dst: &Path, relative: &Path) -> bool {
        dst.exists() && self.contains(relative)
    }
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

/// `Some(why)` when `dst` resolves to `src` or to a path inside it, following
/// symlinks — the destination IS the canonical source a provisioner would copy
/// from (a workspace-root cwd whose `.claude` links into
/// `qontinui-claude-config/.claude`), so writing there overwrites the source. Either side may not exist yet, so each is resolved through its
/// nearest existing ancestor ([`canonicalize_through_ancestors`]). `None` —
/// "not the same tree" — also when either side cannot be resolved at all,
/// which on a real filesystem means neither exists and there is nothing of the
/// source's to overwrite.
pub(crate) fn destination_is_source(dst: &Path, src: &Path) -> Option<String> {
    let dst_real = canonicalize_through_ancestors(dst)?;
    let src_real = canonicalize_through_ancestors(src)?;
    dst_real.starts_with(&src_real).then(|| {
        format!(
            "{} resolves to {}, which is the source {} it would be copied from",
            dst.display(),
            dst_real.display(),
            src_real.display()
        )
    })
}

/// `Some(why)` when `cwd` sits inside a git work tree of the same repository as
/// the checkout at `checkout` — the same repository, not merely the same path:
/// both git COMMON dirs canonicalize to one directory. Both sides go through the
/// same resolver, so a `checkout` whose own `.git` is a FILE (itself a linked
/// worktree, or a `--separate-git-dir` checkout) is still recognised.
///
/// The walk-up from `cwd` finds its NEAREST `.git`, so the stand-down
/// deliberately covers any cwd nested anywhere inside a work tree of that
/// repository, not only its top level: a `.claude/` written into a
/// subdirectory of the canonical repo is untracked litter there as well.
///
/// Why identity and not only [`destination_is_source`]: a linked worktree OF the
/// canonical checkout (`agent-worktrees/<id>/qontinui-claude-config`) has a real
/// `.claude/` directory at a path that is not the canonical one, so the path
/// compare misses and no symlink stands it down. The only protection left would
/// be [`TrackedPaths`], which is fail-soft by design — a probe that errors or
/// exceeds [`PROBE_TIMEOUT`] reads "nothing tracked" and WRITES, replacing the
/// canonical sources in that worktree with the binary's embedded copies, a diff
/// an agent working there can commit. Every worktree of a repository shares its
/// common dir, so this catches the whole family with nothing to fail soft.
///
/// Plain file I/O, no `git` process: see [`git_common_dir`]. `None` when either
/// side cannot be resolved — `cwd` in no repository, or no `.git` directly at
/// `checkout` (it is NOT walked up from: a missing canonical checkout must not
/// resolve to whatever repository happens to enclose it) — which leaves the
/// decision to the other guards.
pub(crate) fn same_repository(cwd: &Path, checkout: &Path) -> Option<String> {
    let common = std::fs::canonicalize(git_common_dir(cwd)?).ok()?;
    let canonical = std::fs::canonicalize(common_dir_at(checkout)?).ok()?;
    (common == canonical).then(|| {
        format!(
            "{} is a work tree of the repository at {} (git common dir {}), \
             which is the source its assets would be copied from",
            cwd.display(),
            checkout.display(),
            common.display()
        )
    })
}

/// The git common dir of the nearest enclosing repository of `start`, found by
/// walking up to the first `.git`:
///
/// - a `.git` DIRECTORY is the common dir itself (a primary checkout);
/// - a `.git` FILE reads `gitdir: <path>` (relative paths resolve against the
///   directory holding the file). For a linked worktree that path is
///   `<common>/worktrees/<name>`, whose `commondir` file names the common dir
///   (relative to the gitdir); when it has none, the parent of a `worktrees`
///   directory is taken. Any other gitdir (a submodule's `modules/<name>`, a
///   `--separate-git-dir`) is its own common dir — taking its grandparent would
///   misattribute a submodule to its superproject.
///
/// `None` when no `.git` is found or a `.git` file is unreadable or malformed.
fn git_common_dir(start: &Path) -> Option<PathBuf> {
    common_dir_at(start.ancestors().find(|dir| dir.join(".git").exists())?)
}

/// [`git_common_dir`] for the `.git` directly at `holder`, with no walk-up.
fn common_dir_at(holder: &Path) -> Option<PathBuf> {
    let dot_git = holder.join(".git");
    if !dot_git.exists() {
        return None;
    }
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = holder.join(contents.lines().next()?.strip_prefix("gitdir:")?.trim());
    if let Ok(commondir) = std::fs::read_to_string(gitdir.join("commondir")) {
        return Some(gitdir.join(commondir.trim()));
    }
    let parent = gitdir.parent()?;
    if parent.file_name().is_some_and(|n| n == "worktrees") {
        return parent.parent().map(Path::to_path_buf);
    }
    Some(gitdir)
}

/// `std::fs::canonicalize` for a path that may not exist yet: canonicalize its
/// nearest existing ancestor and re-append the missing tail. `None` when no
/// ancestor resolves, or the tail holds a component with no file name (`..`).
fn canonicalize_through_ancestors(path: &Path) -> Option<PathBuf> {
    let mut tail = Vec::new();
    let mut current = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(current) {
            return Some(tail.iter().rev().fold(real, |acc, part| acc.join(part)));
        }
        tail.push(current.file_name()?.to_os_string());
        current = current.parent()?;
    }
}

/// `Some(why)` when writing `path` would pass through a symlink below `base`:
/// some EXISTING component of `path` strictly below `base` — `path` itself
/// included — is a symlink, so the write would land somewhere other than its
/// lexical destination. `base` itself is not examined. A component that does
/// not exist ends the walk: nothing below it can be a symlink yet.
///
/// The rule every session-asset provisioner applies, independent of any
/// workspace root: never write through a symlink. A per-file symlink into a
/// canonical checkout is invisible to [`TrackedPaths`], which asks the SESSION
/// repo, and `std::fs::copy` onto a file's own target truncates it.
pub(crate) fn symlink_below(base: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(base).ok()?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&current)
                    .map(|t| format!(" -> {}", t.display()))
                    .unwrap_or_default();
                return Some(format!("{}{target} is a symlink", current.display()));
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// [`symlink_below`] for a whole asset kind: `dir` is `<cwd>/.claude/<kind>`,
/// and both `.claude` and `<kind>` are examined. `Some(why)` means the kind
/// stands down — even `create_dir_all` would build inside the link's target.
pub(crate) fn redirected_asset_dir(dir: &Path) -> Option<String> {
    let cwd = dir.parent()?.parent()?;
    symlink_below(cwd, dir)
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

    /// Fail-soft: a directory that does not exist yields an empty set rather
    /// than a spawn attempt or a panic.
    #[test]
    fn a_missing_root_probes_to_nothing_tracked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracked = TrackedPaths::probe(&tmp.path().join("does-not-exist"));
        assert!(!tracked.contains(Path::new("anything.md")));
    }

    /// Lay out a linked worktree's gitdir by hand: `<common>/worktrees/<name>`,
    /// with a `commondir` file when `commondir` is true, and `<wt>/.git` naming
    /// it via `gitdir_line`.
    fn fake_worktree(common: &Path, wt: &Path, gitdir_line: &str, commondir: bool) {
        let gitdir = common.join("worktrees").join(wt.file_name().unwrap());
        std::fs::create_dir_all(&gitdir).unwrap();
        if commondir {
            std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        }
        std::fs::create_dir_all(wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {gitdir_line}\n")).unwrap();
    }

    fn real(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    /// A RELATIVE `gitdir:` resolves against the directory holding the `.git`
    /// file, and both the `commondir` file and its absence (the parent of the
    /// `worktrees` directory) land on the same common dir.
    #[test]
    fn a_relative_gitdir_resolves_to_the_common_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let common = tmp.path().join("repo").join(".git");
        std::fs::create_dir_all(&common).unwrap();
        for (name, commondir) in [("wt-a", true), ("wt-b", false)] {
            let wt = tmp.path().join(name);
            fake_worktree(
                &common,
                &wt,
                &format!("../repo/.git/worktrees/{name}"),
                commondir,
            );
            assert_eq!(
                git_common_dir(&wt.join("nested")).map(|p| real(&p)),
                Some(real(&common)),
                "{name}"
            );
        }
    }

    /// A submodule's `.git` FILE points at `<super>/.git/modules/<name>`, which
    /// is its own common dir. Taking the gitdir's grandparent would answer
    /// `<super>/.git` and misattribute the submodule to its superproject.
    #[test]
    fn a_submodule_is_not_attributed_to_its_superproject() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let superproject = tmp.path().join("super");
        let modules = superproject.join(".git").join("modules").join("sub");
        std::fs::create_dir_all(&modules).unwrap();
        let sub = superproject.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        assert_eq!(git_common_dir(&sub).map(|p| real(&p)), Some(real(&modules)));
        assert_eq!(same_repository(&sub, &superproject), None);
    }

    /// The canonical checkout may itself be a linked worktree, its `.git` a
    /// FILE: a sibling worktree of the same repository is still the same
    /// repository. Comparing against `canonicalize(<checkout>/.git)` — the file
    /// itself — silently switched the guard off here.
    #[test]
    fn a_checkout_whose_git_is_a_file_is_still_recognised() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let common = tmp.path().join("main").join(".git");
        std::fs::create_dir_all(&common).unwrap();
        let checkout = tmp.path().join("checkout");
        let sibling = tmp.path().join("sibling");
        for wt in [&checkout, &sibling] {
            let name = wt.file_name().unwrap().to_string_lossy();
            let line = common.join("worktrees").join(&*name);
            fake_worktree(&common, wt, &line.to_string_lossy(), true);
        }

        assert!(same_repository(&sibling, &checkout).is_some());
    }

    /// A worktree of an UNRELATED repository is not the checkout's repository,
    /// so a mutation that made the identity check always answer "same" goes red
    /// here rather than silently standing every session down.
    #[test]
    fn a_worktree_of_another_repository_is_not_the_same_repository() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let checkout = tmp.path().join("config");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        let other = tmp.path().join("other").join(".git");
        std::fs::create_dir_all(&other).unwrap();
        let wt = tmp.path().join("wt");
        fake_worktree(&other, &wt, "../other/.git/worktrees/wt", true);

        assert!(git_common_dir(&wt).is_some());
        assert_eq!(same_repository(&wt, &checkout), None);
    }

    /// A `.git` file with no `gitdir:` line resolves to nothing — and so to no
    /// stand-down — rather than to a guessed directory.
    #[test]
    fn a_malformed_git_file_resolves_to_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join(".git"), "not a gitdir line\n").unwrap();
        assert_eq!(git_common_dir(tmp.path()), None);
        std::fs::write(tmp.path().join(".git"), "").unwrap();
        assert_eq!(git_common_dir(tmp.path()), None);
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
