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

/// Every git subcommand this module can run. Both are read-only, which is
/// what lets [`crate::mcp::probe_executor`] — whose whole contract is that a
/// probe never writes — call the resolver without widening its own
/// `READ_ONLY_GIT` allowlist. `probe_executor`'s
/// `trunk_subcommands_are_read_only` test pins that containment so a future
/// rung added here cannot silently break the probe's guarantee.
pub(crate) const TRUNK_GIT_SUBCOMMANDS: &[&str] = &["symbolic-ref", "rev-parse"];

/// Run a git query against `repo`, returning trimmed stdout on success.
///
/// Refuses any subcommand not on [`TRUNK_GIT_SUBCOMMANDS`] — the same
/// defense-in-depth shape as `probe_executor::git_read`. It also keeps the
/// constant load-bearing rather than test-only, so the containment
/// `probe_executor` asserts is a property of the code path, not of a list
/// that happens to sit beside it.
fn git_capture(repo: &Path, args: &[&str]) -> Option<String> {
    match args.first() {
        Some(sub) if TRUNK_GIT_SUBCOMMANDS.contains(sub) => {}
        _ => return None,
    }
    let dir = repo.to_str()?;
    let mut full: Vec<&str> = vec!["-C", dir];
    full.extend_from_slice(args);
    let out = crate::process_helpers::no_window("git")
        .args(&full)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
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
pub(crate) fn resolve_trunk_ref(repo: &Path) -> Option<String> {
    // (1) origin/HEAD — the remote's declared default branch.
    if let Some(head) = git_capture(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        let head = head.trim();
        // Verify it actually resolves: a stale origin/HEAD naming a deleted
        // branch must NOT be trusted as the trunk.
        if !head.is_empty()
            && git_capture(repo, &["rev-parse", "--verify", "--quiet", head]).is_some()
        {
            return Some(head.to_string());
        }
    }

    // (2) The historical default, still verified before use.
    if git_capture(repo, &["rev-parse", "--verify", "--quiet", "origin/main"]).is_some() {
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
pub(crate) fn resolve_trunk_branch(repo: &Path) -> Option<String> {
    resolve_trunk_ref(repo)?
        .strip_prefix("origin/")
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Build a temp git repo with one commit. Returns a `git` runner bound
    /// to it.
    fn fixture(path: &Path) -> impl Fn(&[&str]) + '_ {
        let dir = path.to_str().unwrap();
        let git = move |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", dir], args].concat())
                .output()
                .unwrap();
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
        let head = git_capture(path, &["rev-parse", "HEAD"]).unwrap();
        git(&["update-ref", "refs/remotes/origin/master", &head]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ]);

        // Guard the premise: no stray origin/main can be carrying the pass.
        assert!(
            git_capture(path, &["rev-parse", "--verify", "--quiet", "origin/main"]).is_none(),
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
        let head = git_capture(path, &["rev-parse", "HEAD"]).unwrap();
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
}
