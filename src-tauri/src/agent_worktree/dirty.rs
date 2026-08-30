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

    /// Did the probe actually MEASURE this tree?
    ///
    /// The companion to [`Self::as_conservative_bool`], and the reason both
    /// exist: that method deliberately collapses `Dirty` and `Unknown` onto
    /// the same `true` so a `bool`-typed wire field fails closed — but a
    /// **measured** WIP tree and an **unreadable** one need different operator
    /// action, and a triage list that renders copy-paste shell commands for
    /// both is lying about one of them. Publish this beside the bool
    /// (`census::WorktreeCensus::is_dirty_known`) so the distinction survives
    /// the collapse instead of being re-derived from nothing downstream.
    pub(crate) fn is_known(self) -> bool {
        !matches!(self, DirtyVerdict::Unknown)
    }
}

// ---------------------------------------------------------------------------
// `.git` presence — where the two-state collapse tried to come back
// ---------------------------------------------------------------------------

/// Whether a candidate directory carries a `.git` entry — **and whether we
/// could tell**.
///
/// The carve-out in [`verdict_from_outcome`] turns a degraded probe into
/// [`DirtyVerdict::Clean`], the one verdict [`DirtyVerdict::permits_removal`]
/// accepts. It is sound only while its "there is no `.git`" half is an actual
/// MEASUREMENT of absence. `Path::exists()` cannot supply one: it is
/// `fs::metadata(p).is_ok()`, so it traverses symlinks and folds *every*
/// error — `ENOENT`, `EACCES` on the parent, `ENOTDIR`, `EIO`, `ELOOP`, a
/// dead network mount — into the same `false`. Measured on a Linux box,
/// 2026-08-30:
///
/// | Shape on disk | `join(".git").exists()` | the truth |
/// |---|---|---|
/// | dangling `.git` symlink (gitdir moved / volume unmounted) | `false` | a worktree |
/// | parent directory mode `000` | `false` | unknowable |
///
/// Both of those landed in the removal-permitting arm: `git status` returns
/// `fatal: not a git repository` (exit 128) → `Degraded(Status)`, the
/// carve-out read `has_git == false` → `Clean`, the census published
/// `is_dirty = false`, coord's inviolable G1 gate passed, and
/// `remove_worktree`'s `git worktree remove --force` failure fell through to
/// `std::fs::remove_dir_all` — which succeeds, because the *files* were
/// always readable. That is the same two-state conflation
/// [`DirtyVerdict`] exists to remove, one level down, so this is a tri-state
/// too: only [`GitPresence::Absent`] is a measurement of absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitPresence {
    /// A `.git` entry is there. **Every** shape counts, and all of them are
    /// the worktree case: a `.git` DIRECTORY (a primary clone), a `.git` FILE
    /// (`gitdir: <path>` — a linked worktree, which is what every agent
    /// worktree here is), or a SYMLINK — **including a dangling one**, whose
    /// tree still holds the operator's uncommitted files even though git
    /// refuses to talk about it.
    Present,
    /// Measured absence: the lookup answered `NotFound`. A directory with no
    /// `.git` cannot be a worktree holding uncommitted work, so this is the
    /// only value the [`DirtyVerdict::Clean`] carve-out may rest on — the
    /// same carve-out [`super::reclaim`]'s backstop sweep has always made.
    Absent,
    /// The lookup failed for some OTHER reason (`EACCES` on the parent,
    /// `ENOTDIR`, `EIO`, `ELOOP`, a dead mount). Not evidence of absence, and
    /// therefore never a licence to delete.
    Undetermined,
}

impl GitPresence {
    /// May the [`DirtyVerdict::Clean`] carve-out rest on this reading?
    /// [`GitPresence::Absent`] only — spelled as one predicate so a future
    /// call site cannot re-invent `!has_git`.
    pub(crate) fn permits_clean_carve_out(self) -> bool {
        matches!(self, GitPresence::Absent)
    }
}

/// Probe `.git` presence without ever answering [`GitPresence::Absent`] on an
/// error we did not understand.
///
/// `symlink_metadata` rather than `metadata` / `exists`, for two reasons:
///
/// * it does **not** traverse the final component, so a `.git` symlink whose
///   target was moved or lives on an unmounted volume reads `Ok` — `Present`
///   — instead of the `ENOENT` a traversing stat reports from the *target*;
/// * it returns the `io::Error`, so `NotFound` (a real measurement) can be
///   told apart from every other failure (an absence of measurement).
///
/// A `.git` **file** — the linked-worktree gitfile, and the common case here
/// — is `Ok` and therefore `Present`; the entry's *shape* is never inspected,
/// because the question is "could this tree hold uncommitted work", not "is
/// this a valid repository". A worktree root that itself does not exist reads
/// `NotFound` → `Absent`, which is correct (no directory, no work) and is
/// independently guarded by the callers' `root_exists` checks.
pub(crate) fn probe_git_presence(worktree: &std::path::Path) -> GitPresence {
    match std::fs::symlink_metadata(worktree.join(".git")) {
        Ok(_) => GitPresence::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GitPresence::Absent,
        // EACCES on the parent, ENOTDIR, EIO, ELOOP, a dead network mount:
        // we did not learn anything. Fail closed.
        Err(_) => GitPresence::Undetermined,
    }
}

/// Map one bounded `git status --porcelain` outcome onto a [`DirtyVerdict`].
///
/// `git_presence` is the [`probe_git_presence`] reading for the candidate. A
/// directory MEASURED to have no `.git` cannot be a worktree holding
/// uncommitted work, so a failed probe there is genuinely
/// [`DirtyVerdict::Clean`] — the same carve-out [`super::reclaim`]'s backstop
/// sweep has always made. With a `.git` present, **or with its presence
/// undetermined**, EVERY degrade is [`DirtyVerdict::Unknown`].
pub(crate) fn verdict_from_outcome(
    outcome: &crate::process_helpers::ProbeOutcome,
    git_presence: GitPresence,
) -> DirtyVerdict {
    match outcome {
        crate::process_helpers::ProbeOutcome::Captured(stdout) => {
            if porcelain_is_dirty(&String::from_utf8_lossy(stdout)) {
                DirtyVerdict::Dirty
            } else {
                DirtyVerdict::Clean
            }
        }
        crate::process_helpers::ProbeOutcome::Degraded(_)
            if git_presence.permits_clean_carve_out() =>
        {
            DirtyVerdict::Clean
        }
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
    let git_presence = probe_git_presence(path);
    let outcome = crate::process_helpers::run_probe(cmd, timeout, label);
    let verdict = verdict_from_outcome(&outcome, git_presence);
    if verdict == DirtyVerdict::Unknown {
        // WARN, not debug: this is the arm that REFUSES a delete. If a tree
        // is stuck here forever, the log is where that becomes visible. The
        // `.git` reading is carried because the two arms need different
        // operator action — a `Present` tree has a wedged git, an
        // `Undetermined` one has a broken path (permissions, a dead mount).
        let because = match git_presence {
            GitPresence::Present => "the tree HAS a .git",
            GitPresence::Undetermined => "we could not even determine whether it has a .git",
            // Unreachable: `Absent` takes the Clean carve-out above.
            GitPresence::Absent => "unreachable",
        };
        tracing::warn!(
            path = %path.display(),
            git_presence = ?git_presence,
            "{label}: dirtiness UNKNOWN (probe degraded and {because}) — \
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
        let v = verdict_from_outcome(&timed_out, GitPresence::Present);
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
                verdict_from_outcome(&ProbeOutcome::Degraded(reason), GitPresence::Present),
                DirtyVerdict::Unknown
            );
        }
    }

    /// The one carve-out: a directory with no `.git` cannot be holding
    /// uncommitted work, so a failed probe there stays Clean (parity with the
    /// backstop sweep, which has always made this distinction).
    #[test]
    fn a_degrade_on_a_dir_with_no_git_is_clean() {
        let v = verdict_from_outcome(
            &ProbeOutcome::Degraded(DegradeReason::Status),
            GitPresence::Absent,
        );
        assert_eq!(v, DirtyVerdict::Clean);
        assert!(v.permits_removal());
    }

    /// The success arms still answer from the porcelain payload.
    #[test]
    fn captured_output_still_drives_the_verdict() {
        assert_eq!(
            verdict_from_outcome(&ProbeOutcome::Captured(b"".to_vec()), GitPresence::Present),
            DirtyVerdict::Clean
        );
        assert_eq!(
            verdict_from_outcome(
                &ProbeOutcome::Captured(b"?? .claude/\n".to_vec()),
                GitPresence::Present
            ),
            DirtyVerdict::Clean
        );
        assert_eq!(
            verdict_from_outcome(
                &ProbeOutcome::Captured(b" M src/main.rs\n".to_vec()),
                GitPresence::Present
            ),
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

    // ── `.git`-presence regression tests (2026-08-30 round-3 review) ──
    //
    // The carve-out's input used to be `path.join(".git").exists()`, which is
    // `fs::metadata(p).is_ok()`: it TRAVERSES symlinks and maps every error
    // onto `false`. So "there is no `.git`" and "I could not determine
    // whether there is a `.git`" both reached the one arm whose
    // `permits_removal()` is `true` — the same two-state collapse
    // `DirtyVerdict` exists to remove.
    //
    // Every case below asserts BOTH the new tri-state answer and what the old
    // `exists()` predicate answered, via `legacy_exists_presence`. Revert
    // `probe_git_presence` to `exists()` and the two dangling/unreadable
    // tests fail on their verdict assertions.

    /// The PRE-FIX predicate, kept in the tests ONLY so each case can state
    /// what it used to answer. Being two-state, `exists()` can only ever
    /// produce `Present`/`Absent` — it has no way to say `Undetermined`,
    /// which is precisely the defect.
    fn legacy_exists_presence(worktree: &std::path::Path) -> GitPresence {
        if worktree.join(".git").exists() {
            GitPresence::Present
        } else {
            GitPresence::Absent
        }
    }

    /// A `Degraded` outcome — the shape every one of these cases produces in
    /// the field (`fatal: not a git repository`, exit 128).
    fn degraded() -> ProbeOutcome {
        ProbeOutcome::Degraded(DegradeReason::Status)
    }

    /// Baseline — the carve-out MUST survive the fix. A directory with
    /// genuinely no `.git` still degrades to `Clean`, or the ~34% false-dirty
    /// backlog this module exists for becomes permanently unreclaimable.
    #[test]
    fn a_dir_with_no_git_at_all_is_measured_absent_and_stays_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(probe_git_presence(tmp.path()), GitPresence::Absent);
        // Old and new agree here — this is the one case `exists()` got right.
        assert_eq!(legacy_exists_presence(tmp.path()), GitPresence::Absent);

        let v = verdict_from_outcome(&degraded(), probe_git_presence(tmp.path()));
        assert_eq!(v, DirtyVerdict::Clean);
        assert!(v.permits_removal(), "the carve-out must survive");
    }

    /// A linked worktree carries `.git` as a FILE (`gitdir: <path>`) — which
    /// is what EVERY agent worktree here is, so this is the common case, not
    /// an edge case. Present, therefore never the carve-out.
    #[test]
    fn a_git_gitfile_reads_present_and_refuses_the_carve_out() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join(".git"),
            "gitdir: /home/x/repo/.git/worktrees/wt\n",
        )
        .expect("write gitfile");

        assert_eq!(probe_git_presence(tmp.path()), GitPresence::Present);
        assert_eq!(legacy_exists_presence(tmp.path()), GitPresence::Present);

        let v = verdict_from_outcome(&degraded(), probe_git_presence(tmp.path()));
        assert_eq!(v, DirtyVerdict::Unknown);
        assert!(!v.permits_removal());
    }

    /// A primary clone's `.git` DIRECTORY.
    #[test]
    fn a_git_directory_reads_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        assert_eq!(probe_git_presence(tmp.path()), GitPresence::Present);
        assert_eq!(
            verdict_from_outcome(&degraded(), probe_git_presence(tmp.path())),
            DirtyVerdict::Unknown
        );
    }

    /// **The reproduced data-loss shape.** `.git` is a symlink to a gitdir
    /// that was moved, or that lives on a volume no longer mounted. The tree
    /// beneath it still holds the operator's uncommitted files, but:
    ///
    /// 1. `git status --porcelain` → `fatal: not a git repository`, exit 128
    ///    → `Degraded(Status)`;
    /// 2. `exists()` TRAVERSES the symlink, hits `ENOENT` on the *target*,
    ///    and answers `false` — so the pre-fix carve-out said `Clean`;
    /// 3. the census published `is_dirty = false`, coord's G1 gate passed,
    ///    `git worktree remove --force` failed, and the fallback
    ///    `std::fs::remove_dir_all` SUCCEEDED — the files were readable all
    ///    along.
    ///
    /// `symlink_metadata` does not traverse, so the link itself is `Ok`.
    #[cfg(unix)]
    #[test]
    fn a_dangling_git_symlink_reads_present_though_exists_says_false() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let git = tmp.path().join(".git");
        std::os::unix::fs::symlink("/nonexistent/moved-gitdir", &git).expect("symlink");
        // The work this tree holds, which the old path deleted.
        std::fs::write(tmp.path().join("uncommitted.rs"), "fn main() {}\n").expect("write");

        assert!(
            std::fs::symlink_metadata(&git).is_ok(),
            "the link itself is there — only its target is gone"
        );
        assert!(
            !git.exists(),
            "precondition: exists() traverses and folds the dangling link to false"
        );

        // The old predicate — and the delete it authorized.
        assert_eq!(legacy_exists_presence(tmp.path()), GitPresence::Absent);
        let old = verdict_from_outcome(&degraded(), legacy_exists_presence(tmp.path()));
        assert_eq!(old, DirtyVerdict::Clean);
        assert!(
            old.permits_removal(),
            "this is the pre-fix behaviour: a tree holding WIP authorized its own deletion"
        );

        // The fixed predicate.
        assert_eq!(probe_git_presence(tmp.path()), GitPresence::Present);
        let now = verdict_from_outcome(&degraded(), probe_git_presence(tmp.path()));
        assert_eq!(now, DirtyVerdict::Unknown);
        assert!(!now.permits_removal());
    }

    /// The parent directory cannot be read at all (`EACCES`). `exists()`
    /// swallows the error into `false`; `symlink_metadata` reports it, and an
    /// error we did not understand is `Undetermined`, never absence.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_parent_directory_is_undetermined_though_exists_says_false() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().join("wt");
        std::fs::create_dir(&wt).expect("mkdir wt");
        std::fs::write(wt.join(".git"), "gitdir: /somewhere\n").expect("write gitfile");
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

        let presence = probe_git_presence(&wt);
        let legacy = legacy_exists_presence(&wt);
        // Restore BEFORE asserting so a failure still lets the tempdir clean up.
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).expect("restore");

        if presence == GitPresence::Present {
            // root (or a CAP_DAC_OVERRIDE sandbox) ignores mode bits, so the
            // stat succeeded and there is no unreadable case to assert.
            eprintln!("skipping: this process can read a mode-000 directory (root?)");
            return;
        }

        assert_eq!(
            legacy,
            GitPresence::Absent,
            "precondition: exists() maps EACCES to false, exactly like ENOENT"
        );
        let old = verdict_from_outcome(&degraded(), legacy);
        assert_eq!(old, DirtyVerdict::Clean);
        assert!(
            old.permits_removal(),
            "pre-fix, an unreadable tree authorized its own deletion"
        );

        assert_eq!(
            presence,
            GitPresence::Undetermined,
            "an error that is not NotFound is not a measurement of absence"
        );
        let now = verdict_from_outcome(&degraded(), presence);
        assert_eq!(now, DirtyVerdict::Unknown);
        assert!(!now.permits_removal());
    }

    /// End-to-end through the REAL probe seam both the census and the reclaim
    /// executor call: a dangling `.git` plus a git that fails must come back
    /// UNKNOWN, so `as_conservative_bool()` publishes `is_dirty = true`.
    #[cfg(unix)]
    #[test]
    fn a_dangling_git_symlink_never_permits_removal_through_the_real_seam() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink("/nonexistent/moved-gitdir", tmp.path().join(".git"))
            .expect("symlink");

        // Stands in for `git status` exiting 128 on a broken gitdir.
        let mut cmd = crate::process_helpers::no_window("sh");
        cmd.args(["-c", "exit 128"]);

        let verdict = probe_reclaim_dirty_with(
            tmp.path(),
            cmd,
            std::time::Duration::from_secs(5),
            "test: broken gitdir",
        );
        assert_eq!(verdict, DirtyVerdict::Unknown);
        assert!(!verdict.permits_removal());
        assert!(
            verdict.as_conservative_bool(),
            "the census must publish is_dirty = true for a tree it could not read"
        );
    }
}
