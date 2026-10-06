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

/// The REPOSITORY-LOCAL git environment variables this crate scrubs: what
/// `git rev-parse --local-env-vars` prints (git 2.47.3) MINUS
/// [`COMMAND_SCOPE_GIT_CONFIG_ENV`] — exactly the set git itself clears when
/// it crosses into another repository (a submodule). Any of them inherited by
/// a child makes git read a repository, index, object store or config file
/// OTHER than the one `-C` / `current_dir` names: `-C` does not override an
/// inherited `GIT_DIR`, so a runner started from a git hook (which exports
/// `GIT_DIR`) or any shell that exported these would answer about the
/// CALLER's repo. `GIT_CONFIG` (the legacy whole-config-FILE override) stays
/// on the list: it names a file, which is repo-locating.
/// `repo_local_git_env_covers_gits_own_list` pins this against the installed
/// git, so a git that grows the list fails a test rather than silently
/// reopening the hole.
pub(crate) const REPO_LOCAL_GIT_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// The names on git's `--local-env-vars` list that are NOT scrubbed, because
/// they carry COMMAND-SCOPE config (`git -c k=v`, and the `GIT_CONFIG_COUNT`
/// overlay with its numbered `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`
/// pairs, which are not on git's list at all) rather than a repository
/// location. git's own submodule code (`prepare_submodule_repo_env`) keeps
/// exactly these two when it clears the rest — measured: `GIT_CONFIG_COUNT=1
/// GIT_CONFIG_KEY_0=foo.bar … git -c baz.q=p submodule foreach` still sees
/// `COUNT=1` and `'baz.q'='p'` inside the submodule. Scrubbing them would drop
/// the operator's env-injected `safe.directory` (so `rev-parse` fails with
/// "dubious ownership" on a box that needs it) and the agent session's
/// env-injected credential helper / proxy / CA settings
/// (`git_posture::non_interactive_git_env`), which a runner started from such
/// a session inherits and `build_drift`'s fetch / `ls-remote` need.
pub(crate) const COMMAND_SCOPE_GIT_CONFIG_ENV: &[&str] =
    &["GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT"];

/// Remove every [`REPO_LOCAL_GIT_ENV`] variable from `cmd`'s child
/// environment, whether inherited from this process or set on `cmd` earlier.
/// Command-scope config ([`COMMAND_SCOPE_GIT_CONFIG_ENV`] and the numbered
/// `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` pairs) passes through untouched,
/// as it does across git's own submodule boundary.
///
/// The ONE scrub for runner git that must read the repo it names: this
/// module's resolver and [`crate::build_drift`]'s probes both call it, so the
/// list cannot drift between them. It removes no part of
/// [`crate::process_helpers::no_window`]'s posture, which carries no
/// repository-local entry by construction.
pub(crate) fn scrub_repo_local_git_env(cmd: &mut Command) {
    for var in REPO_LOCAL_GIT_ENV {
        cmd.env_remove(var);
    }
}

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
/// [`crate::process_helpers::no_window`] posture, carrying whatever this
/// process inherited — which [`git_capture`] then scrubs.
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
/// The repository-local scrub ([`scrub_repo_local_git_env`]) covers ONLY the
/// git reads made here — the trunk NAME. A caller that then runs its own git
/// in the same repo (`fleet::resolve_default_branch`'s behind-check and pull
/// callers, `agent_worktree`'s fork-base `rev-parse`) still inherits whatever this process inherited, so under an inherited
/// `GIT_DIR` it would act on the caller's repo with a correctly-named trunk.
/// Scrubbing those callers' own git is a recorded follow-up, not something
/// this function provides.
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
/// As with [`resolve_trunk_ref`], the scrub fixes only this name read, not
/// any git the caller runs afterwards.
pub(crate) fn resolve_trunk_branch(repo: &Path) -> Option<String> {
    resolve_trunk_branch_on(repo, &host_git)
}

/// [`resolve_trunk_branch`] over [`resolve_trunk_ref_on`]'s seam.
fn resolve_trunk_branch_on(repo: &Path, git: &dyn Fn() -> Command) -> Option<String> {
    resolve_trunk_ref_on(repo, git)?
        .strip_prefix("origin/")
        .map(str::to_string)
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

    /// The scrub removes every name on the list — each reported by
    /// `get_envs` as a removal (`None`), so neither an inherited nor an
    /// earlier-set value reaches the child — and leaves command-scope config
    /// (the `GIT_CONFIG_COUNT` overlay, its numbered pairs, and
    /// `GIT_CONFIG_PARAMETERS`) exactly as set, as git's own submodule
    /// boundary does.
    #[test]
    fn the_scrub_removes_the_list_and_keeps_command_scope_config() {
        let mut cmd = Command::new("git");
        let kept = [
            ("GIT_CONFIG_PARAMETERS", "'baz.q'='p'"),
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "safe.directory"),
            ("GIT_CONFIG_VALUE_0", "*"),
            ("GIT_CONFIG_KEY_1", "credential.helper"),
            ("GIT_CONFIG_VALUE_1", "!gh auth git-credential"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ];
        for (k, v) in kept {
            cmd.env(k, v);
        }
        scrub_repo_local_git_env(&mut cmd);
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for var in REPO_LOCAL_GIT_ENV {
            assert!(
                envs.iter().any(|(k, v)| k == var && v.is_none()),
                "{var} must be removed; envs: {envs:?}"
            );
        }
        for (k, v) in kept {
            assert!(
                envs.iter()
                    .any(|(name, val)| name == k && val.as_deref() == Some(v)),
                "{k}={v} is command-scope, not repo-local, and must survive: {envs:?}"
            );
        }
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
        let foreign = |overlay: &'static [(&'static str, &'static str)]| {
            move || {
                let mut cmd = host_git();
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

    /// The list is git's, not a hand-picked subset: every name the installed
    /// git reports as repository-local is on it, except the named
    /// command-scope exemption ([`COMMAND_SCOPE_GIT_CONFIG_ENV`] — config, not
    /// a repository location, and kept by git's own submodule boundary), which
    /// must itself be on git's list so it cannot exempt a name git never
    /// reported. Skipped only when no `git` can be spawned at all.
    #[test]
    fn repo_local_git_env_covers_gits_own_list() {
        let Ok(out) = Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
        else {
            eprintln!("git is not installed; skipping");
            return;
        };
        assert!(out.status.success(), "{out:?}");
        let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        assert!(
            names.iter().any(|n| n == "GIT_DIR"),
            "git's list must at least name GIT_DIR, or this test proves nothing: {names:?}"
        );
        for exempt in COMMAND_SCOPE_GIT_CONFIG_ENV {
            assert!(
                names.iter().any(|n| n == exempt),
                "exemption {exempt} is not on git's list: {names:?}"
            );
            assert!(
                !REPO_LOCAL_GIT_ENV.contains(exempt),
                "{exempt} is both exempt and scrubbed"
            );
        }
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| {
                !REPO_LOCAL_GIT_ENV.contains(&n.as_str())
                    && !COMMAND_SCOPE_GIT_CONFIG_ENV.contains(&n.as_str())
            })
            .collect();
        assert!(
            missing.is_empty(),
            "REPO_LOCAL_GIT_ENV is missing git's {missing:?}"
        );
    }
}
