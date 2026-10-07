//! One canonical resolver for a repo's **trunk** remote-tracking ref.
//!
//! Five call sites in this binary needed to name "the branch this repo lands
//! on", and every one of them spelled it `origin/main` — either literally, or
//! as an unverified `origin/HEAD` read with a `main` fallback bolted on.
//! Measured on this fleet 2026-08-19: **five governed `qontinui-*` repos have
//! a non-`main` trunk** (mobile, navigation, research, workflow-ui,
//! workflow-utils), so every one of those sites answered wrong — permanently,
//! not transiently — for a fifth of the governed set.
//!
//! `agent_worktree::census` was fixed first (PR
//! [`qontinui-runner#1066`], landed `26346e439`) and grew a private
//! `resolve_trunk_ref`. Its own plan
//! (`2026-08-08-runner-census-landed-in-main-trunk-agnostic-and-fresh`,
//! "Follow-ups identified but NOT owned by this plan") recorded the remaining
//! sites and named the fix: *"The resolver this plan adds is the natural thing
//! for all three to share, but it is currently private to `agent_worktree`.
//! Exporting it is the obvious first step."* This module is that export — the
//! resolver lifted out of `census.rs` unchanged, with the duplicate
//! `origin/HEAD` readers in `fleet` and `agent_worktree` folded onto it too.
//!
//! **Bin target only.** `agent_worktree`, `fleet`, `build_drift` and `mcp` are
//! all declared in `main.rs`, not `lib.rs`, so this module lives there with
//! them. A `--lib` test run compiles none of it (see the plan's Phase 4
//! warning about vacuous greens).

use std::path::Path;

use std::process::Command;

use crate::process_helpers::{run_probe, ProbeOutcome};

/// Budget for a trunk-resolution git read.
///
/// Every caller is a periodic one — the 300s worktree census, the 900s build
/// drift check, the 60s tree publisher, the 60s MCP probe executor — and each
/// runs on a blocking-pool thread. The commands are local plumbing reads
/// (`symbolic-ref`, `rev-parse`, `config`, `for-each-ref`, `ls-remote` is NOT
/// in the allowlist), so a healthy call is milliseconds and the only realistic
/// hang is a lock or a stalled filesystem. Without this bound one wedged repo
/// removed a pool thread permanently, on four independent timers.
const TRUNK_GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Every git subcommand this module can run. Both are read-only, which is
/// what lets [`crate::mcp::probe_executor`] — whose whole contract is that a
/// probe never writes — call the resolver without widening its own
/// `READ_ONLY_GIT` allowlist. `probe_executor`'s
/// `trunk_subcommands_are_read_only` test pins that containment so a future
/// rung added here cannot silently break the probe's guarantee.
pub(crate) const TRUNK_GIT_SUBCOMMANDS: &[&str] = &["symbolic-ref", "rev-parse"];

// The ONE repository-local list and scrub — lib-side in
// `qontinui_runner_lib::git_posture`, because `process_helpers::no_window`
// (compiled into both crates) applies the same scrub to every git the runner
// starts.
use qontinui_runner_lib::git_posture::scrub_repo_local_git_env;

/// Run a git query against `repo`, returning trimmed stdout on success.
///
/// Refuses any subcommand not on [`TRUNK_GIT_SUBCOMMANDS`] — the same
/// defense-in-depth shape as `probe_executor::git_read`. It also keeps the
/// constant load-bearing rather than test-only, so the containment
/// `probe_executor` asserts is a property of the code path, not of a list
/// that happens to sit beside it.
///
/// `git` is the command to build on — [`host_git`] in production — and is
/// scrubbed here ([`scrub_repo_local_git_env`]), after anything it carries,
/// so an inherited `GIT_DIR` cannot answer for `repo`.
fn git_capture(git: Command, repo: &Path, args: &[&str]) -> Option<String> {
    match args.first() {
        Some(sub) if TRUNK_GIT_SUBCOMMANDS.contains(sub) => {}
        _ => return None,
    }
    let mut cmd = git;
    scrub_repo_local_git_env(&mut cmd);
    cmd.arg("-C").arg(repo).args(args);
    let ProbeOutcome::Captured(stdout) = run_probe(cmd, TRUNK_GIT_TIMEOUT, "git_trunk: git") else {
        return None;
    };
    Some(String::from_utf8_lossy(&stdout).trim().to_string())
}

/// The `git` every production resolution starts from: the fleet's
/// [`crate::process_helpers::no_window`] posture, already scrubbed of the
/// repository-local environment — and [`git_capture`] scrubs again, after
/// whatever a seam test's command carries.
fn host_git() -> Command {
    crate::process_helpers::no_window("git")
}

/// Resolve the repo's trunk remote-tracking ref for `repo` — e.g.
/// `origin/main`, `origin/master`.
///
/// Resolution order:
///
/// 1. `refs/remotes/origin/HEAD` — the symbolic ref `git clone` writes,
///    pointing at the remote's default branch. It lives in the `.git`
///    COMMON dir, so every linked worktree of a repo sees it; `git worktree
///    add` cannot leave it unset (verified across five real worktrees,
///    2026-08-19).
/// 2. `origin/main` — the historical behaviour, kept as the fallback for a
///    clone whose `origin/HEAD` was never written.
/// 3. `None` — genuinely unresolvable.
///
/// **Every rung is verified with `rev-parse` before it is returned.** That
/// matters most for rung 1: `origin/HEAD` is written at clone time and is
/// NOT auto-refreshed when the remote's default branch changes (`git remote
/// set-head origin -a` is the refresh). A stale-but-present `origin/HEAD`
/// would otherwise resolve to a *wrong* trunk, which is worse than an honest
/// `None` — a wrong trunk can answer `Some(true)` in
/// `census::compute_landed_in_main` and let a worktree be reclaimed.
/// Present-and-unresolvable therefore falls through to rung 2 rather than
/// being trusted.
///
/// There is deliberately NO "configured trunk" rung: nothing in this repo
/// writes such a key, and an unread config would be a rung that silently
/// always misses while reading like coverage.
///
/// The repository-local scrub covers this function's callers' own git too
/// (`census::compute_landed_in_main` and its sibling census reads, and
/// `mcp::probe_executor`'s worktree-state probe — `ahead`/`behind` and
/// `has_unpushed`, which gates reclaim): every git they build comes from
/// [`crate::process_helpers::no_window`] / `tokio_no_window`, which apply
/// [`scrub_repo_local_git_env`] at construction, so the trunk NAME read here
/// and the git that follows it answer for the same repo (coord finding
/// `f110e194-7178-40ff-8952-906e238ba3aa`).
pub(crate) fn resolve_trunk_ref(repo: &Path) -> Option<String> {
    resolve_trunk_ref_on(repo, &host_git)
}

/// [`resolve_trunk_ref`] building each `git` from `git` — the seam that lets a
/// test hand it a command carrying a decoy `GIT_DIR` without touching this
/// process's environment, which parallel tests share.
fn resolve_trunk_ref_on(repo: &Path, git: &dyn Fn() -> Command) -> Option<String> {
    // (1) origin/HEAD — the remote's declared default branch.
    if let Some(head) = git_capture(
        git(),
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        let head = head.trim();
        // Verify it actually resolves: a stale origin/HEAD naming a deleted
        // branch must NOT be trusted as the trunk.
        if !head.is_empty()
            && git_capture(git(), repo, &["rev-parse", "--verify", "--quiet", head]).is_some()
        {
            return Some(head.to_string());
        }
    }

    // (2) The historical default, still verified before use.
    if git_capture(
        git(),
        repo,
        &["rev-parse", "--verify", "--quiet", "origin/main"],
    )
    .is_some()
    {
        return Some("origin/main".to_string());
    }

    // (3) Honest unknown.
    None
}

/// The trunk's *branch* name — [`resolve_trunk_ref`] with the `origin/`
/// prefix stripped (`origin/master` -> `master`).
///
/// Callers that need a local branch name, a refspec, or an `ls-remote`
/// argument want this; callers comparing against a remote-tracking ref want
/// [`resolve_trunk_ref`]. `None` propagates the same honest unknown — a
/// caller that must have *some* name should spell its own
/// `.unwrap_or_else(|| "main".to_string())` so the guess is visible at the
/// call site rather than buried in here.
///
/// As with [`resolve_trunk_ref`], the git a caller runs afterwards —
/// `fleet::resolve_default_branch`'s behind-check and pull, `agent_worktree`'s
/// fork-base `rev-parse` and `worktree add` — is built by
/// [`crate::process_helpers::no_window`] and so carries the same scrub (coord
/// finding `f110e194-7178-40ff-8952-906e238ba3aa`).
pub(crate) fn resolve_trunk_branch(repo: &Path) -> Option<String> {
    resolve_trunk_branch_on(repo, &host_git)
}

/// [`resolve_trunk_branch`] over [`resolve_trunk_ref_on`]'s seam.
fn resolve_trunk_branch_on(repo: &Path, git: &dyn Fn() -> Command) -> Option<String> {
    resolve_trunk_ref_on(repo, git)?
        .strip_prefix("origin/")
        .map(str::to_string)
}

/// Test support for proving a PRODUCTION git path ignores an INHERITED
/// repository-local environment — the shape the scrub exists for, which a
/// seam or a hand-built `Command` can only imitate.
///
/// The variable must be inherited, i.e. present in the process environment,
/// but this process's environment is shared by every test running in
/// parallel. So the parent test re-executes this very test binary
/// ([`std::env::current_exe`]) to run ONE `#[ignore]`d child test, with
/// `GIT_DIR` naming a decoy repo set on that child process alone. The child
/// calls the real production function and asserts it answered for the repo
/// it was named, plus an in-child control proving the inheritance is real.
/// libtest flags are passed explicitly, so the re-exec depends on nothing
/// the outer harness (cargo test, nextest) passed to the parent.
#[cfg(test)]
pub(crate) mod inherited_git_dir_reexec {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Printed by a child only after its last assertion. The parent requires
    /// it, so a child that returned early — or a filter that matched nothing
    /// — cannot pass vacuously.
    pub(crate) const ASSERTED_MARKER: &str = "INHERITED-GIT-DIR-CHILD-ASSERTED";

    /// Set on the child alongside `GIT_DIR`; its absence tells a child it is
    /// not under the parent (e.g. a `--include-ignored` sweep), where it
    /// returns without asserting — and without the marker.
    pub(crate) const CHILD_ENV: &str = "QONTINUI_TEST_INHERITED_GIT_DIR_CHILD";

    /// The child's inputs, read from its environment; `None` outside a parent.
    pub(crate) fn child_input(name: &str) -> Option<String> {
        std::env::var_os(CHILD_ENV)?;
        Some(std::env::var(name).unwrap_or_else(|_| panic!("the parent must set {name}")))
    }

    /// A one-commit repo whose `refs/remotes/origin/main` points at that
    /// commit; with `unlanded`, one more commit sits on HEAD that the trunk
    /// does not have. Two of these — one each way — are a target/decoy pair
    /// whose landed-in-main and `git cherry` answers DIFFER, which is what a
    /// re-exec child needs to tell which repo a git call actually read. Built
    /// with the repository-local scrub and hermetic commit config, so it is
    /// correct even when the test itself runs under a git hook.
    pub(crate) fn repo_with_origin_main(unlanded: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("utf-8 tempdir").to_string();
        let git = |args: &[&str]| -> String {
            let mut cmd = Command::new("git");
            cmd.args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "-C",
                path.as_str(),
            ])
            .args(args);
            super::scrub_repo_local_git_env(&mut cmd);
            let out = cmd.output().expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), b"x").expect("write");
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);
        let head = git(&["rev-parse", "HEAD"]);
        git(&["update-ref", "refs/remotes/origin/main", &head]);
        if unlanded {
            std::fs::write(dir.path().join("b.txt"), b"y").expect("write");
            git(&["add", "b.txt"]);
            git(&["commit", "-q", "-m", "c2"]);
        }
        dir
    }

    /// Asserts the child's own `GIT_DIR` is set to `decoy_git_dir`. Callers pass
    /// the value they read back from the child's environment, so this proves
    /// only that `GIT_DIR` is SET in the child; that it is the decoy — and
    /// takes effect — is proven by the control that follows it (unscrubbed git
    /// answering with the decoy's repo).
    pub(crate) fn assert_inherited_git_dir(decoy_git_dir: &str) {
        assert_eq!(
            std::env::var("GIT_DIR").ok().as_deref(),
            Some(decoy_git_dir),
            "control: the child must INHERIT the decoy GIT_DIR, or it proves nothing"
        );
    }

    /// Run the `#[ignore]`d test `child` (its name inside `module_path`, as
    /// `module_path!()` spells it) in a re-executed copy of this test binary,
    /// with `GIT_DIR=<decoy_git_dir>` and `envs` set on the child only, git's
    /// global and system config replaced by an empty file, and no other
    /// repository-local or command-scope git variable inherited. Asserts the
    /// child exited 0, ran exactly one test, and reached its last assertion.
    pub(crate) fn run_child(
        module_path: &str,
        child: &str,
        decoy_git_dir: &Path,
        envs: &[(&str, &OsStr)],
    ) {
        // `module_path!()` leads with the crate name; libtest names a test
        // by its path inside the crate.
        let (_, in_crate) = module_path
            .split_once("::")
            .expect("a test module path names its crate");
        let test_name = format!("{in_crate}::{child}");

        let cfg = tempfile::tempdir().expect("tempdir");
        let empty_global: PathBuf = cfg.path().join("empty-global.gitconfig");
        std::fs::write(&empty_global, "").expect("write empty git config");

        let mut cmd = Command::new(std::env::current_exe().expect("the test binary"));
        cmd.args([
            "--exact",
            test_name.as_str(),
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        for var in qontinui_runner_lib::git_posture::REPO_LOCAL_GIT_ENV
            .iter()
            .chain(qontinui_runner_lib::git_posture::COMMAND_SCOPE_GIT_CONFIG_ENV)
        {
            cmd.env_remove(var);
        }
        cmd.env("GIT_DIR", decoy_git_dir)
            .env("GIT_CONFIG_GLOBAL", &empty_global)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(CHILD_ENV, "1");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-exec the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "child {test_name} failed ({}):\n{stdout}\n{stderr}",
            out.status
        );
        assert!(
            // `--nocapture` interleaves the child's own output after the
            // `...`, so the name line and the summary are checked apart.
            stdout.contains(&format!("test {test_name} ..."))
                && stdout.contains("test result: ok. 1 passed;"),
            "child ran no test (filter mismatch?):\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains(ASSERTED_MARKER),
            "child {test_name} returned before its assertions:\n{stdout}\n{stderr}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp git repo with one commit. Returns a `git` runner bound
    /// to it.
    fn fixture(path: &Path) -> impl Fn(&[&str]) + '_ {
        let dir = path.to_str().unwrap();
        let git = move |args: &[&str]| {
            let mut cmd = Command::new("git");
            // Hermetic commits whatever the box's global config says (the
            // same overrides build_drift's `fixture_git` uses).
            cmd.args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ]);
            cmd.args([&["-C", dir], args].concat());
            // Under a git hook these would point the fixture at the CALLER's
            // repo — the same scrub production applies.
            scrub_repo_local_git_env(&mut cmd);
            let out = cmd.output().unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);
        git
    }

    /// A `master`-trunk repo resolves to `origin/master`, and the branch
    /// helper strips the remote prefix. Every hardcoded-`origin/main` site
    /// this module replaced answered wrong here.
    #[test]
    fn resolves_a_master_trunk_repo_and_strips_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = fixture(path);
        let head = git_capture(host_git(), path, &["rev-parse", "HEAD"]).unwrap();
        git(&["update-ref", "refs/remotes/origin/master", &head]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ]);

        // Guard the premise: no stray origin/main can be carrying the pass.
        assert!(
            git_capture(
                host_git(),
                path,
                &["rev-parse", "--verify", "--quiet", "origin/main"]
            )
            .is_none(),
            "fixture must NOT have an origin/main, or the test proves nothing"
        );

        assert_eq!(resolve_trunk_ref(path).as_deref(), Some("origin/master"));
        assert_eq!(resolve_trunk_branch(path).as_deref(), Some("master"));
    }

    /// A stale `origin/HEAD` — present, but naming a branch that no longer
    /// resolves — falls through to rung 2 instead of being trusted. A wrong
    /// trunk is worse than an honest unknown: it can answer `Some(true)` on
    /// a reclaim gate.
    #[test]
    fn stale_origin_head_falls_through_to_origin_main() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let git = fixture(path);
        let head = git_capture(host_git(), path, &["rev-parse", "HEAD"]).unwrap();
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/deleted-default",
        ]);
        git(&["update-ref", "refs/remotes/origin/main", &head]);

        assert_eq!(resolve_trunk_ref(path).as_deref(), Some("origin/main"));
        assert_eq!(resolve_trunk_branch(path).as_deref(), Some("main"));
    }

    /// Neither rung resolves → honest `None`, never a guessed `main`. The
    /// guess, where a caller needs one, is spelled at the call site.
    #[test]
    fn unresolvable_trunk_is_none_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let _git = fixture(path);

        assert!(resolve_trunk_ref(path).is_none());
        assert!(resolve_trunk_branch(path).is_none());
    }

    /// A non-repo path is unresolvable, not a panic — every caller runs this
    /// against paths that may have been removed under it.
    #[test]
    fn a_non_repo_path_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-repo");
        std::fs::create_dir_all(&path).unwrap();
        assert!(resolve_trunk_ref(&path).is_none());
    }
    /// A fixture whose `origin/HEAD` names `origin/<trunk>`.
    fn repo_with_trunk(trunk: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        {
            let git = fixture(dir.path());
            let head = git_capture(host_git(), dir.path(), &["rev-parse", "HEAD"]).unwrap();
            git(&["update-ref", &format!("refs/remotes/origin/{trunk}"), &head]);
            git(&[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                &format!("refs/remotes/origin/{trunk}"),
            ]);
        }
        dir
    }

    /// An inherited `GIT_DIR` naming a DECOY repo does not decide the trunk:
    /// the runner repo's trunk is `main`, the decoy's `master`, and the
    /// resolver answers `main`. Every `build_drift` / `fleet` /
    /// `agent_worktree` caller turns this name into a refspec, an
    /// `ls-remote` argument or a fork base in the REAL repo, so the decoy's
    /// answer would act there. The decoy rides on the `Command`, never on this
    /// process's environment, which parallel tests share.
    #[test]
    fn an_inherited_git_dir_does_not_decide_the_trunk() {
        let runner = repo_with_trunk("main");
        let decoy = repo_with_trunk("master");
        let decoy_git_dir = decoy.path().join(".git");
        let with_decoy = || {
            let mut cmd = host_git();
            cmd.env("GIT_DIR", &decoy_git_dir);
            cmd
        };

        // Control: unscrubbed, `-C <runner>` loses to the inherited GIT_DIR.
        let out = with_decoy()
            .arg("-C")
            .arg(runner.path())
            .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "origin/master",
            "control: an inherited GIT_DIR must win over -C, or this test proves nothing"
        );

        assert_eq!(
            resolve_trunk_branch_on(runner.path(), &with_decoy).as_deref(),
            Some("main")
        );
    }

    /// The regression the command-scope exemption exists for: on a box whose
    /// repos need an env-injected `safe.directory` (a `GIT_CONFIG_COUNT`
    /// overlay, or `git -c`'s `GIT_CONFIG_PARAMETERS`), the resolver must
    /// still read the trunk. `GIT_TEST_ASSUME_DIFFERENT_OWNER` makes git
    /// treat the fixture as foreign-owned; everything rides on the `Command`
    /// through the seam, never on this process's shared environment.
    #[test]
    fn an_env_injected_safe_directory_still_resolves_the_trunk() {
        let repo = repo_with_trunk("main");
        // Both arms run against a KNOWN config: an empty global file, no
        // system file, and none of the box's own command-scope overlay (which
        // the scrub now passes through). A CI image with `safe.directory=*`
        // in its global or system config would otherwise satisfy the control.
        let cfg_dir = tempfile::tempdir().unwrap();
        let empty_global = cfg_dir.path().join("empty-global.gitconfig");
        std::fs::write(&empty_global, "").unwrap();
        let foreign = |overlay: &'static [(&'static str, &'static str)]| {
            let empty_global = empty_global.clone();
            move || {
                let mut cmd = host_git();
                cmd.env("GIT_CONFIG_GLOBAL", &empty_global)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env_remove("GIT_CONFIG_PARAMETERS")
                    .env_remove("GIT_CONFIG_COUNT");
                cmd.env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
                for (k, v) in overlay {
                    cmd.env(k, v);
                }
                cmd
            }
        };

        // Control: foreign-owned with no overlay, git refuses the repo.
        assert_eq!(
            resolve_trunk_branch_on(repo.path(), &foreign(&[])),
            None,
            "control: a foreign-owned repo with no safe.directory must be refused, \
             or this test proves nothing"
        );

        for overlay in [
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "safe.directory"),
                ("GIT_CONFIG_VALUE_0", "*"),
            ][..],
            &[("GIT_CONFIG_PARAMETERS", "'safe.directory'='*'")][..],
        ] {
            assert_eq!(
                resolve_trunk_branch_on(repo.path(), &foreign(overlay)).as_deref(),
                Some("main"),
                "the overlay {overlay:?} must survive the scrub"
            );
        }
    }

    const TARGET_ENV: &str = "QONTINUI_TEST_TRUNK_TARGET";

    /// Child of [`the_production_resolver_ignores_an_inherited_git_dir`]:
    /// skipped in a normal run, run only re-executed by it.
    #[test]
    #[ignore = "re-executed by the_production_resolver_ignores_an_inherited_git_dir"]
    fn inherited_git_dir_child_resolves_the_named_repos_trunk() {
        use super::inherited_git_dir_reexec as reexec;
        let Some(target) = reexec::child_input(TARGET_ENV) else {
            eprintln!("not under the re-exec parent; nothing to assert");
            return;
        };
        let decoy_git_dir = reexec::child_input("GIT_DIR").expect("set with the child flag");
        reexec::assert_inherited_git_dir(&decoy_git_dir);
        let target = Path::new(&target);

        // Control: unscrubbed (a raw `Command`, not the scrubbed
        // `no_window` posture), the inherited GIT_DIR beats `-C <target>`.
        let out = Command::new("git")
            .arg("-C")
            .arg(target)
            .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .output()
            .expect("git runs");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "origin/master",
            "control: the inherited decoy must answer unscrubbed git"
        );

        assert_eq!(resolve_trunk_branch(target).as_deref(), Some("main"));
        assert_eq!(resolve_trunk_ref(target).as_deref(), Some("origin/main"));
        println!("{}", reexec::ASSERTED_MARKER);
    }

    /// The REAL entry point — [`resolve_trunk_branch`] over [`host_git`], no
    /// seam — under a `GIT_DIR` the process INHERITED (set on a re-executed
    /// child only): the target's trunk is `main`, the decoy's `master`, and
    /// the answer is `main`. [`an_inherited_git_dir_does_not_decide_the_trunk`]
    /// proves the scrub through the seam; this proves the production wiring.
    #[test]
    fn the_production_resolver_ignores_an_inherited_git_dir() {
        let target = repo_with_trunk("main");
        let decoy = repo_with_trunk("master");
        assert_eq!(resolve_trunk_branch(target.path()).as_deref(), Some("main"));
        assert_eq!(
            resolve_trunk_branch(decoy.path()).as_deref(),
            Some("master")
        );
        super::inherited_git_dir_reexec::run_child(
            module_path!(),
            "inherited_git_dir_child_resolves_the_named_repos_trunk",
            &decoy.path().join(".git"),
            &[(TARGET_ENV, target.path().as_os_str())],
        );
    }
}
