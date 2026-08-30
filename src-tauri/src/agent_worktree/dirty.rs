//! Reclaim-scoped worktree dirtiness.
//!
//! A worktree is "dirty" for reclaim purposes when it holds work that would
//! be LOST by removing it. Plain `git status --porcelain` non-emptiness is a
//! bad proxy for that, because the runner writes its own untracked
//! scaffolding into every agent worktree it allocates:
//!
//! | Path | Written by |
//! |---|---|
//! | `.claude/` | agent scaffolding (agents, settings) materialized per session |
//! | `.coord-mcp-status` | [`crate::coord_mcp`]'s degraded breadcrumb |
//! | `.mcp.json` | the coord-mcp proxy config written at worktree provisioning |
//!
//! None of those are operator work, and every one of them makes an otherwise
//! pristine worktree read as dirty forever. Since both the census
//! ([`super::census`]) and the reclaim executor ([`super::reclaim`]) refuse
//! to act on a dirty worktree, that self-inflicted dirtiness made agent
//! worktrees **permanently unreclaimable**.
//!
//! Measured on the operator's box 2026-07-28, over 5,322 backlog worktrees:
//! 1,696 were dirty from `.claude/` alone and a further 96 from
//! `.coord-mcp-status` / `.mcp.json` — **~34% false-dirty** — against exactly
//! ONE worktree holding genuine uncommitted work. Without this predicate,
//! arming reclaim would silently refuse a third of every future backlog.
//!
//! ## Scope — deliberately NOT applied to the canonical checkout
//!
//! [`super::census::compute_canonical_is_dirty`] and the shared-branch switch
//! guard in [`super`] ask a *different* question: "is it safe to move HEAD in
//! the operator's real checkout?" There, ANY uncommitted state matters,
//! including scaffolding, because a branch switch can carry or clobber it.
//! Those keep plain porcelain non-emptiness. This module is only for the
//! "is it safe to DELETE this disposable worktree?" question.
//!
//! ## Conservative by construction
//!
//! Only **untracked** (`??`) entries are ever ignored, and only when the path
//! is exactly a scaffolding entry or lives beneath a scaffolding directory. A
//! *tracked* modification is always dirty — including one inside `.claude/`
//! (e.g. ` M .claude/settings.json` in a repo that commits its agent config),
//! because that is a real edit to a real file. Anything unparseable is dirty.

/// Untracked paths the runner writes into worktrees it provisions. Matched
/// exactly, or as a directory prefix (`.claude/` covers `.claude/agents/x`).
pub(crate) const RUNNER_SCAFFOLDING: &[&str] = &[".claude", ".coord-mcp-status", ".mcp.json"];

/// Strip git's quoting from a porcelain path. git quotes paths containing
/// spaces or unusual bytes (`?? "a b.txt"`); ordinary paths are bare.
fn unquote(path: &str) -> &str {
    path.strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path)
}

/// Is `path` runner-written scaffolding?
///
/// Matches the entry itself (`.mcp.json`) or anything beneath a scaffolding
/// directory (`.claude/agents/foo.md`, and the bare-directory form `.claude/`
/// git emits when the whole dir is untracked). Deliberately NOT a plain
/// `starts_with`, so a sibling like `.mcp.json.bak` stays dirty.
fn is_scaffolding_path(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let p = p.trim_end_matches('/');
    RUNNER_SCAFFOLDING
        .iter()
        .any(|s| p == *s || p.starts_with(&format!("{s}/")))
}

/// Can this porcelain line be ignored when deciding reclaim-dirtiness?
fn is_ignorable_line(line: &str) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return true;
    }
    // ONLY untracked entries qualify. Tracked changes (" M ", "A  ", "R  ",
    // conflicts, …) are always real.
    match line.strip_prefix("?? ") {
        Some(rest) => is_scaffolding_path(unquote(rest.trim())),
        None => false,
    }
}

/// Reclaim-scoped dirtiness of a `git status --porcelain` payload.
///
/// `true` when at least one line represents work that removal would lose.
/// Pure — the unit-testable core of both call sites.
pub(crate) fn porcelain_is_dirty(porcelain: &str) -> bool {
    porcelain.lines().any(|l| !is_ignorable_line(l))
}

// ---------------------------------------------------------------------------
// Tri-state probe — "we could not read this tree" is NOT "this tree is clean"
// ---------------------------------------------------------------------------

/// The three answers a reclaim-scoped dirtiness probe can give.
///
/// A tri-state for the same reason
/// [`super::orphan_target_reaper::DirtyProbe`] is one, and for the same reason
/// [`super::census::compute_canonical_is_dirty`] is an `Option<bool>`: the
/// two-state version could not tell "this tree has no uncommitted work" apart
/// from "the probe never answered".
///
/// **Why that distinction is load-bearing.** Before the bounded-subprocess
/// work, a `git status` blocked by a concurrent `index.lock` did not return
/// AT ALL, so the reclaim executor's `Remove` guard could never be reached
/// with a fabricated "clean". Bounding the child fixed the thread leak and
/// created a new hazard in its place: the probe now returns, and a
/// two-state mapping turned the timeout into a confident `false`. That
/// `false` is precisely the value that lets an armed `Remove` proceed, and
/// `remove_worktree`'s own `git worktree remove --force` degrades to
/// `std::fs::remove_dir_all` — so a wedged git would have DELETED
/// uncommitted work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirtyVerdict {
    /// `git status --porcelain` answered and reported nothing that removal
    /// would lose (runner scaffolding does not count — see the module docs).
    Clean,
    /// `git status --porcelain` answered and reported real work.
    Dirty,
    /// The probe did not answer: it timed out and was killed, could not be
    /// spawned, or exited non-zero against a tree that DOES have a `.git`.
    /// Never "clean" — every caller must treat this as "do not delete".
    Unknown,
}

impl DirtyVerdict {
    /// May a destructive `Remove` proceed on the strength of this verdict?
    ///
    /// ONLY [`DirtyVerdict::Clean`]. This is the whole safety property, spelled
    /// as one predicate so both the reclaim executor and the census publish the
    /// same answer and a future call site cannot re-invent the `unwrap_or(false)`.
    pub(crate) fn permits_removal(self) -> bool {
        matches!(self, DirtyVerdict::Clean)
    }

    /// The boolean a `bool`-typed consumer must see — `true` for anything that
    /// is not provably clean, so an unreadable tree reads as "has work".
    pub(crate) fn as_conservative_bool(self) -> bool {
        !self.permits_removal()
    }
}

/// Map one bounded `git status --porcelain` outcome onto a [`DirtyVerdict`].
///
/// `has_git` is whether the candidate carries a `.git` entry at all. A
/// directory with NO `.git` cannot be a worktree holding uncommitted work, so
/// a failed probe there is genuinely [`DirtyVerdict::Clean`] — the same
/// carve-out [`super::reclaim`]'s backstop sweep has always made. With a
/// `.git` present, EVERY degrade is [`DirtyVerdict::Unknown`].
pub(crate) fn verdict_from_outcome(
    outcome: &crate::process_helpers::ProbeOutcome,
    has_git: bool,
) -> DirtyVerdict {
    match outcome {
        crate::process_helpers::ProbeOutcome::Captured(stdout) => {
            if porcelain_is_dirty(&String::from_utf8_lossy(stdout)) {
                DirtyVerdict::Dirty
            } else {
                DirtyVerdict::Clean
            }
        }
        crate::process_helpers::ProbeOutcome::Degraded(_) if !has_git => DirtyVerdict::Clean,
        crate::process_helpers::ProbeOutcome::Degraded(_) => DirtyVerdict::Unknown,
    }
}

/// Run one bounded, reclaim-scoped dirtiness probe against `path`.
///
/// The single seam both [`super::reclaim::worktree_is_dirty`] and
/// [`super::census`]'s `is_dirty` go through, so the two can never disagree
/// about what an unreadable tree means.
pub(crate) fn probe_reclaim_dirty(
    path: &std::path::Path,
    timeout: std::time::Duration,
    label: &str,
) -> DirtyVerdict {
    let Some(path_str) = path.to_str() else {
        // An un-stringable path is a tree we cannot even ask about.
        return DirtyVerdict::Unknown;
    };
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["-C", path_str, "status", "--porcelain"]);
    probe_reclaim_dirty_with(path, cmd, timeout, label)
}

/// [`probe_reclaim_dirty`] with the command injected — the seam a regression
/// test uses to drive a genuinely hung child through the real
/// [`crate::process_helpers::run_probe`] path.
pub(crate) fn probe_reclaim_dirty_with(
    path: &std::path::Path,
    cmd: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> DirtyVerdict {
    let has_git = path.join(".git").exists();
    let outcome = crate::process_helpers::run_probe(cmd, timeout, label);
    let verdict = verdict_from_outcome(&outcome, has_git);
    if verdict == DirtyVerdict::Unknown {
        // WARN, not debug: this is the arm that REFUSES a delete. If a tree
        // is stuck here forever, the log is where that becomes visible.
        tracing::warn!(
            path = %path.display(),
            "{label}: dirtiness UNKNOWN (probe degraded on a tree that HAS a .git) — \
             refusing to treat it as clean"
        );
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tree_is_not_dirty() {
        assert!(!porcelain_is_dirty(""));
        assert!(!porcelain_is_dirty("\n"));
        assert!(!porcelain_is_dirty("   \n\n"));
    }

    /// The whole point: each scaffolding path alone leaves the tree clean.
    #[test]
    fn runner_scaffolding_alone_is_not_dirty() {
        assert!(!porcelain_is_dirty("?? .claude/"));
        assert!(!porcelain_is_dirty("?? .claude/agents/reviewer.md"));
        assert!(!porcelain_is_dirty("?? .coord-mcp-status"));
        assert!(!porcelain_is_dirty("?? .mcp.json"));
        assert!(!porcelain_is_dirty(
            "?? .claude/\n?? .coord-mcp-status\n?? .mcp.json\n"
        ));
    }

    #[test]
    fn real_work_is_dirty() {
        assert!(porcelain_is_dirty(" M src/main.rs"));
        assert!(porcelain_is_dirty("?? src/new_file.rs"));
        assert!(porcelain_is_dirty("A  src/added.rs"));
        assert!(porcelain_is_dirty("R  old.rs -> new.rs"));
        assert!(porcelain_is_dirty("UU conflicted.rs"));
        assert!(porcelain_is_dirty("D  removed.rs"));
    }

    /// A TRACKED edit inside a scaffolding dir is real work — only untracked
    /// entries are ignorable. Guards the obvious over-broad implementation.
    #[test]
    fn tracked_change_under_scaffolding_is_still_dirty() {
        assert!(porcelain_is_dirty(" M .claude/settings.json"));
        assert!(porcelain_is_dirty("M  .mcp.json"));
        assert!(porcelain_is_dirty("A  .claude/agents/new.md"));
    }

    /// Prefix matching must not swallow siblings that merely share a prefix.
    #[test]
    fn scaffolding_prefix_does_not_over_match() {
        assert!(porcelain_is_dirty("?? .mcp.json.bak"));
        assert!(porcelain_is_dirty("?? .claudex/thing"));
        assert!(porcelain_is_dirty("?? .coord-mcp-status-old"));
        assert!(porcelain_is_dirty("?? src/.claude/nested.txt"));
    }

    #[test]
    fn scaffolding_mixed_with_real_work_is_dirty() {
        assert!(porcelain_is_dirty("?? .claude/\n M src/main.rs"));
        assert!(porcelain_is_dirty(" M src/main.rs\n?? .mcp.json"));
    }

    #[test]
    fn quoted_and_crlf_paths_are_handled() {
        assert!(!porcelain_is_dirty("?? \".coord-mcp-status\""));
        assert!(!porcelain_is_dirty("?? .claude/\r\n?? .mcp.json\r\n"));
        assert!(porcelain_is_dirty("?? \"src/a b.rs\""));
    }

    /// Windows-style separators appear in some git configurations.
    #[test]
    fn backslash_separators_are_normalized() {
        assert!(!porcelain_is_dirty("?? .claude\\agents\\x.md"));
    }

    /// Anything we cannot parse as an ignorable untracked entry is dirty —
    /// unknown shapes must never silently authorize a delete.
    #[test]
    fn unparseable_lines_are_dirty() {
        assert!(porcelain_is_dirty("garbage without status code"));
        assert!(porcelain_is_dirty("??no-space-after-code"));
    }

    // ── Tri-state verdict regression tests (2026-08-30 round-2 review) ──
    //
    // These pin CRITICAL-1/2: a `git status` that never answered must never
    // produce a removal-permitting verdict. Each fails if the mapping is
    // reverted to the pre-review `Degraded(_) => false` / `unwrap_or(false)`.

    use crate::process_helpers::{DegradeReason, ProbeOutcome};

    /// A child killed at its budget is UNKNOWN, and UNKNOWN never permits a
    /// removal. This is the exact shape the `index.lock` hang produces.
    #[test]
    fn a_timed_out_status_is_unknown_and_never_permits_removal() {
        let timed_out = ProbeOutcome::Degraded(DegradeReason::TimedOut {
            pid: 4242,
            reaped: true,
        });
        let v = verdict_from_outcome(&timed_out, /* has_git */ true);
        assert_eq!(v, DirtyVerdict::Unknown);
        assert!(
            !v.permits_removal(),
            "a tree we could not read must never authorize a delete"
        );
        assert!(
            v.as_conservative_bool(),
            "a bool-typed consumer must see an unreadable tree as dirty"
        );
    }

    /// A spawn failure and a non-zero exit degrade the same way — the
    /// carve-out is about `.git`, not about which failure mode it was.
    #[test]
    fn every_degrade_on_a_git_bearing_tree_is_unknown() {
        for reason in [
            DegradeReason::Status,
            DegradeReason::SpawnError,
            DegradeReason::TimedOut {
                pid: 1,
                reaped: false,
            },
        ] {
            assert_eq!(
                verdict_from_outcome(&ProbeOutcome::Degraded(reason), true),
                DirtyVerdict::Unknown
            );
        }
    }

    /// The one carve-out: a directory with no `.git` cannot be holding
    /// uncommitted work, so a failed probe there stays Clean (parity with the
    /// backstop sweep, which has always made this distinction).
    #[test]
    fn a_degrade_on_a_dir_with_no_git_is_clean() {
        let v = verdict_from_outcome(&ProbeOutcome::Degraded(DegradeReason::Status), false);
        assert_eq!(v, DirtyVerdict::Clean);
        assert!(v.permits_removal());
    }

    /// The success arms still answer from the porcelain payload.
    #[test]
    fn captured_output_still_drives_the_verdict() {
        assert_eq!(
            verdict_from_outcome(&ProbeOutcome::Captured(b"".to_vec()), true),
            DirtyVerdict::Clean
        );
        assert_eq!(
            verdict_from_outcome(&ProbeOutcome::Captured(b"?? .claude/\n".to_vec()), true),
            DirtyVerdict::Clean
        );
        assert_eq!(
            verdict_from_outcome(&ProbeOutcome::Captured(b" M src/main.rs\n".to_vec()), true),
            DirtyVerdict::Dirty
        );
    }

    /// End-to-end through the REAL bounded-probe path: a child that never
    /// exits, a short budget, a tree that has a `.git`. Must come back
    /// UNKNOWN inside the budget — not Clean, and not by hanging.
    #[test]
    fn a_genuinely_hung_status_child_yields_unknown_within_the_budget() {
        use std::time::{Duration, Instant};

        let tmp = tempfile::tempdir().expect("tempdir");
        // A linked worktree carries `.git` as a FILE; either shape counts.
        std::fs::write(tmp.path().join(".git"), "gitdir: /nowhere\n").expect("write .git");

        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = crate::process_helpers::no_window("cmd.exe");
            c.args(["/C", "ping -n 60 127.0.0.1"]);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = crate::process_helpers::no_window("sh");
            c.args(["-c", "sleep 60"]);
            c
        };

        let budget = Duration::from_millis(400);
        let started = Instant::now();
        let verdict = probe_reclaim_dirty_with(tmp.path(), cmd, budget, "test: hung git status");
        let elapsed = started.elapsed();

        assert_eq!(
            verdict,
            DirtyVerdict::Unknown,
            "a hung `git status` must be UNKNOWN, never Clean"
        );
        assert!(!verdict.permits_removal());
        assert!(
            elapsed < budget * 8,
            "the probe held its thread for {elapsed:?} against a {budget:?} budget"
        );
    }
}
