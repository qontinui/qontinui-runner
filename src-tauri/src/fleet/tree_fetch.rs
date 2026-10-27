//! Which `qontinui-*` directories the tree publisher fetches, and how often.
//!
//! The publisher runs a best-effort `git fetch origin --prune` before each
//! directory's capture so its behind-counts are honest. Until plan
//! `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! (Phase 2) it keyed that fetch on the DIRECTORY and ran it before checking
//! whether the directory was a git checkout at all. Measured on spaceship
//! (2026-10-23): D:/qontinui-root held 368 `qontinui-*` directories — 31
//! clones, 137 linked worktrees and 200 husks with no `.git` — and in a 90 s
//! window 29 of 62 fetches went to husks (an instant failure, 3 processes each)
//! and 17 to linked worktrees, whose `refs/remotes/*` are their primary's.
//!
//! So the fetch is keyed on the git COMMON dir: a directory git cannot open is
//! never fetched, and one fetch per interval refreshes every checkout that
//! shares the refs. The pull executor's `<default>:<default>` ref sync is
//! deduplicated the same way — the local default branch ref is shared too.
//!
//! "Once" means once per PUBLISHER CYCLE as well as once per interval. The
//! directory loop walks hundreds of checkouts in order and a cycle can outlast
//! the interval, so a time gate alone would let a linked worktree reached late
//! in the cycle re-fetch the repository its primary fetched at the start. Each
//! cycle therefore takes a generation ([`begin_cycle`]) and an attempt recorded
//! in the same generation is always shared.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The publisher's cycle generation; 0 until the first cycle begins.
static CYCLE: AtomicU64 = AtomicU64::new(0);

/// Begin a publisher cycle: every fetch and ref sync attempted until the next
/// call shares one generation. Returns it (never 0).
pub(super) fn begin_cycle() -> u64 {
    CYCLE.fetch_add(1, Ordering::SeqCst) + 1
}

/// The generation [`begin_cycle`] last returned; `None` before the first.
fn current_cycle() -> Option<u64> {
    match CYCLE.load(Ordering::SeqCst) {
        0 => None,
        c => Some(c),
    }
}

/// When a prior attempt answers for this one: it ran within the interval, or
/// in the same publisher cycle. A caller outside any cycle passes `None` and
/// gets the time rule alone.
fn shared_with(
    prior_at: Instant,
    prior_cycle: Option<u64>,
    now: Instant,
    cycle: Option<u64>,
) -> bool {
    now.saturating_duration_since(prior_at) < tree_fetch_interval()
        || (cycle.is_some() && prior_cycle == cycle)
}

/// Resolve the per-repo fetch interval. Reads `COORD_TREE_FETCH_INTERVAL_SECS`,
/// falling back to `COORD_TREE_PUBLISH_INTERVAL_SECS`, then 60; floored at 5s.
pub(super) fn tree_fetch_interval() -> Duration {
    let secs: u64 = std::env::var("COORD_TREE_FETCH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            std::env::var("COORD_TREE_PUBLISH_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(60)
        .max(5);
    Duration::from_secs(secs)
}

/// The key a fetch of `dir` is recorded under: its git COMMON dir. `None`
/// when `dir` is not a checkout git can open — no `.git`, an unparseable one,
/// or a pruned worktree's dangling `gitdir:` — and so must not be fetched.
pub(super) fn fetch_key(dir: &Path) -> Option<PathBuf> {
    if qontinui_runner_lib::git_layout::git_provably_absent(dir) {
        return None;
    }
    qontinui_runner_lib::git_layout::common_dir_key(dir)
}

type FetchAttempt = (Instant, Option<u64>);

fn last_fetch_at() -> &'static Mutex<HashMap<PathBuf, FetchAttempt>> {
    static LAST_FETCH: OnceLock<Mutex<HashMap<PathBuf, FetchAttempt>>> = OnceLock::new();
    LAST_FETCH.get_or_init(|| Mutex::new(HashMap::new()))
}

/// True when `key` is due a fetch: never fetched, or the last attempt is
/// neither within [`tree_fetch_interval`] nor in `cycle`. Records the attempt
/// when it returns true, so back-to-back cycles — and the other checkouts
/// sharing the key — don't double-fetch.
pub(super) fn fetch_due(key: &Path, now: Instant, cycle: Option<u64>) -> bool {
    let mut map = last_fetch_at().lock().unwrap_or_else(|p| p.into_inner());
    let due = match map.get(key) {
        Some((at, prior_cycle)) => !shared_with(*at, *prior_cycle, now, cycle),
        None => true,
    };
    if due {
        map.insert(key.to_path_buf(), (now, cycle));
    }
    due
}

/// The one question a directory loop asks: should THIS directory run the
/// fetch now? `None` for a directory git cannot open, and for one whose common
/// dir was already fetched this interval or this `cycle` (the publisher passes
/// its [`begin_cycle`] generation; a caller outside it passes `None`).
pub(super) fn fetch_target(dir: &Path, now: Instant, cycle: Option<u64>) -> Option<PathBuf> {
    let key = fetch_key(dir)?;
    fetch_due(&key, now, cycle).then_some(key)
}

type RefSyncKey = (PathBuf, String);

/// One recorded ref-sync attempt: when, from which directory, and — once it
/// finished — the `chosen_option` it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RefSyncAttempt {
    pub at: Instant,
    pub cycle: Option<u64>,
    pub dir: PathBuf,
    pub outcome: Option<String>,
}

fn last_ref_sync() -> &'static Mutex<HashMap<RefSyncKey, RefSyncAttempt>> {
    static LAST: OnceLock<Mutex<HashMap<RefSyncKey, RefSyncAttempt>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(HashMap::new()))
}

/// For the pull executor's `fetch origin <branch>:<branch>` from `dir`:
/// `Ok(())` when this directory should run it now (and the attempt is
/// recorded), `Err(prior)` when a checkout sharing `dir`'s common dir already
/// attempted the same ref sync within [`tree_fetch_interval`] or in the current
/// publisher cycle — `refs/heads/<branch>` is one ref for all of them.
/// `prior.outcome` carries that attempt's own result once
/// [`record_ref_sync_outcome`] has run, so the shared answer reports what
/// actually happened rather than hiding it. A directory whose common dir cannot
/// be read is always `Ok(())`: no shared answer it cannot justify.
///
/// The pull executor runs only from the publisher's directory loop, so the
/// cycle is the one [`begin_cycle`] last opened.
pub(super) fn claim_ref_sync(dir: &Path, branch: &str, now: Instant) -> Result<(), RefSyncAttempt> {
    claim_ref_sync_in(dir, branch, now, current_cycle())
}

fn claim_ref_sync_in(
    dir: &Path,
    branch: &str,
    now: Instant,
    cycle: Option<u64>,
) -> Result<(), RefSyncAttempt> {
    let Some(common) = fetch_key(dir) else {
        return Ok(());
    };
    let mut map = last_ref_sync().lock().unwrap_or_else(|p| p.into_inner());
    let key = (common, branch.to_string());
    if let Some(prior) = map.get(&key) {
        if shared_with(prior.at, prior.cycle, now, cycle) {
            return Err(prior.clone());
        }
    }
    map.insert(
        key,
        RefSyncAttempt {
            at: now,
            cycle,
            dir: dir.to_path_buf(),
            outcome: None,
        },
    );
    Ok(())
}

/// Record the outcome of the ref sync `dir` claimed, so a sharing checkout's
/// `ref_sync_shared` answer can name it. A no-op when `dir` is not the
/// recorded claimant (a later claim replaced it) or its common dir is unread.
pub(super) fn record_ref_sync_outcome(dir: &Path, branch: &str, outcome: &str) {
    let Some(common) = fetch_key(dir) else {
        return;
    };
    let mut map = last_ref_sync().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(a) = map.get_mut(&(common, branch.to_string())) {
        if a.dir == dir {
            a.outcome = Some(outcome.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<root>/p` (a `.git` dir), `<root>/a` and `<root>/b` linked to it, a
    /// second clone `<root>/q`, a husk `<root>/h` and a pruned worktree
    /// `<root>/z` whose `gitdir:` names a directory that is gone.
    fn layout(root: &Path) -> Vec<PathBuf> {
        let common = root.join("p").join(".git");
        std::fs::create_dir_all(&common).unwrap();
        for wt in ["a", "b"] {
            let admin = common.join("worktrees").join(wt);
            std::fs::create_dir_all(&admin).unwrap();
            std::fs::write(admin.join("commondir"), "../..\n").unwrap();
            let d = root.join(wt);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        }
        std::fs::create_dir_all(root.join("q").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("h")).unwrap();
        let z = root.join("z");
        std::fs::create_dir_all(&z).unwrap();
        std::fs::write(
            z.join(".git"),
            format!(
                "gitdir: {}\n",
                common.join("worktrees").join("gone").display()
            ),
        )
        .unwrap();
        ["p", "a", "b", "q", "h", "z"]
            .iter()
            .map(|n| root.join(n))
            .collect()
    }

    #[test]
    fn no_fetch_for_a_dir_without_dot_git() {
        let t = tempfile::tempdir().unwrap();
        let dirs = layout(t.path());
        let now = Instant::now();
        assert_eq!(
            fetch_target(&t.path().join("h"), now, Some(1)),
            None,
            "husk"
        );
        assert_eq!(
            fetch_target(&t.path().join("z"), now, Some(1)),
            None,
            "pruned worktree"
        );
        // And it stays unfetched on every later cycle.
        assert_eq!(
            fetch_target(
                &t.path().join("h"),
                now + Duration::from_secs(3600),
                Some(2)
            ),
            None
        );
        drop(dirs);
    }

    #[test]
    fn one_fetch_per_common_dir() {
        let t = tempfile::tempdir().unwrap();
        let dirs = layout(t.path());
        let now = Instant::now();
        let fetched: Vec<&PathBuf> = dirs
            .iter()
            .filter(|d| fetch_target(d, now, Some(1)).is_some())
            .collect();
        // p (shared by a and b) and q: two repositories, two fetches — and the
        // checkout that runs each is the first one the walk meets.
        assert_eq!(
            fetched,
            vec![&t.path().join("p"), &t.path().join("q")],
            "one fetch per common dir"
        );
        // The next cycle, inside the interval: nothing.
        let soon = now + Duration::from_secs(1);
        assert!(dirs
            .iter()
            .all(|d| fetch_target(d, soon, Some(2)).is_none()));
        // Past the interval: the same two again.
        let later = now + tree_fetch_interval();
        assert_eq!(
            dirs.iter()
                .filter(|d| fetch_target(d, later, Some(3)).is_some())
                .count(),
            2
        );
    }

    #[test]
    fn a_linked_worktree_met_first_fetches_for_its_primary() {
        let t = tempfile::tempdir().unwrap();
        let _ = layout(t.path());
        let now = Instant::now();
        assert!(fetch_target(&t.path().join("a"), now, Some(1)).is_some());
        assert!(fetch_target(&t.path().join("p"), now, Some(1)).is_none());
        assert!(fetch_target(&t.path().join("b"), now, Some(1)).is_none());
    }

    #[test]
    fn ref_sync_is_shared_per_common_dir() {
        let t = tempfile::tempdir().unwrap();
        let _ = layout(t.path());
        let now = Instant::now();
        assert_eq!(
            claim_ref_sync_in(&t.path().join("p"), "main", now, Some(1)),
            Ok(())
        );
        let prior = claim_ref_sync_in(&t.path().join("a"), "main", now, Some(1))
            .expect_err("a linked worktree shares refs/heads/main with its primary");
        assert_eq!(prior.dir, t.path().join("p"));
        assert_eq!(
            prior.outcome, None,
            "the first attempt has not finished yet"
        );
        record_ref_sync_outcome(&t.path().join("p"), "main", "ref_sync_failed");
        let prior = claim_ref_sync_in(&t.path().join("b"), "main", now, Some(1)).unwrap_err();
        assert_eq!(prior.outcome.as_deref(), Some("ref_sync_failed"));
        // A non-claimant's outcome is ignored.
        record_ref_sync_outcome(&t.path().join("a"), "main", "default_ref_sync");
        let prior = claim_ref_sync_in(&t.path().join("b"), "main", now, Some(1)).unwrap_err();
        assert_eq!(prior.outcome.as_deref(), Some("ref_sync_failed"));
        assert_eq!(
            claim_ref_sync_in(&t.path().join("a"), "master", now, Some(1)),
            Ok(()),
            "another ref"
        );
        assert_eq!(
            claim_ref_sync_in(&t.path().join("q"), "main", now, Some(1)),
            Ok(()),
            "another repo"
        );
        let later = now + tree_fetch_interval();
        assert_eq!(
            claim_ref_sync_in(&t.path().join("b"), "main", later, Some(2)),
            Ok(())
        );
    }

    #[test]
    fn tree_fetch_interval_floored_at_5s() {
        assert!(tree_fetch_interval() >= Duration::from_secs(5));
    }

    #[test]
    fn fetch_due_first_call_true_then_immediate_false() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().to_path_buf();
        let t0 = Instant::now();
        assert!(fetch_due(&p, t0, None), "first call should be due");
        assert!(
            !fetch_due(&p, Instant::now(), None),
            "immediate re-call should not be due"
        );
    }

    #[test]
    fn a_cycle_longer_than_the_interval_still_fetches_each_repo_once() {
        // The publisher walks hundreds of checkouts in order; a cycle can
        // outlast the interval. A linked worktree met past the interval in the
        // SAME cycle must not re-fetch the repository its primary fetched.
        let t = tempfile::tempdir().unwrap();
        let _ = layout(t.path());
        let start = Instant::now();
        let late = start + tree_fetch_interval() + Duration::from_secs(1);
        assert!(fetch_target(&t.path().join("p"), start, Some(7)).is_some());
        assert!(
            fetch_target(&t.path().join("a"), late, Some(7)).is_none(),
            "same cycle: shared even past the interval"
        );
        assert!(
            fetch_target(&t.path().join("b"), late, Some(8)).is_some(),
            "the next cycle, past the interval: due again"
        );
        // Outside any cycle only the time rule applies.
        let later = late + tree_fetch_interval();
        assert!(fetch_target(&t.path().join("p"), later, None).is_some());
    }

    #[test]
    fn a_ref_sync_is_shared_for_the_whole_cycle() {
        let t = tempfile::tempdir().unwrap();
        let _ = layout(t.path());
        let start = Instant::now();
        let late = start + tree_fetch_interval() + Duration::from_secs(1);
        assert_eq!(
            claim_ref_sync_in(&t.path().join("p"), "main", start, Some(7)),
            Ok(())
        );
        assert!(claim_ref_sync_in(&t.path().join("a"), "main", late, Some(7)).is_err());
        assert_eq!(
            claim_ref_sync_in(&t.path().join("b"), "main", late, Some(8)),
            Ok(())
        );
    }

    #[test]
    fn begin_cycle_never_returns_zero_and_moves_forward() {
        let a = begin_cycle();
        let b = begin_cycle();
        assert!(a > 0 && b > a);
        assert!(current_cycle().is_some_and(|c| c >= b));
    }
}
