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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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

fn last_fetch_at() -> &'static Mutex<HashMap<PathBuf, Instant>> {
    static LAST_FETCH: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    LAST_FETCH.get_or_init(|| Mutex::new(HashMap::new()))
}

/// True when `key` is due a fetch (never fetched, or last fetch is at least
/// [`tree_fetch_interval`] ago). Records `now` as the attempt time when it
/// returns true, so back-to-back ticks — and the other checkouts sharing the
/// key — don't double-fetch.
pub(super) fn fetch_due(key: &Path, now: Instant) -> bool {
    let interval = tree_fetch_interval();
    let mut map = last_fetch_at().lock().unwrap_or_else(|p| p.into_inner());
    let due = match map.get(key) {
        Some(prev) => now.duration_since(*prev) >= interval,
        None => true,
    };
    if due {
        map.insert(key.to_path_buf(), now);
    }
    due
}

/// The one question the publisher's directory loop asks: should THIS
/// directory run the fetch now? `None` for a directory git cannot open, and
/// for one whose common dir was already fetched this interval.
pub(super) fn fetch_target(dir: &Path, now: Instant) -> Option<PathBuf> {
    let key = fetch_key(dir)?;
    fetch_due(&key, now).then_some(key)
}

type RefSyncKey = (PathBuf, String);

/// One recorded ref-sync attempt: when, from which directory, and — once it
/// finished — the `chosen_option` it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RefSyncAttempt {
    pub at: Instant,
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
/// attempted the same ref sync within [`tree_fetch_interval`] —
/// `refs/heads/<branch>` is one ref for all of them. `prior.outcome` carries
/// that attempt's own result once [`record_ref_sync_outcome`] has run, so the
/// shared answer reports what actually happened rather than hiding it. A
/// directory whose common dir cannot be read is always `Ok(())`: no shared
/// answer it cannot justify.
pub(super) fn claim_ref_sync(dir: &Path, branch: &str, now: Instant) -> Result<(), RefSyncAttempt> {
    let Some(common) = fetch_key(dir) else {
        return Ok(());
    };
    let interval = tree_fetch_interval();
    let mut map = last_ref_sync().lock().unwrap_or_else(|p| p.into_inner());
    let key = (common, branch.to_string());
    if let Some(prior) = map.get(&key) {
        if now.duration_since(prior.at) < interval {
            return Err(prior.clone());
        }
    }
    map.insert(
        key,
        RefSyncAttempt {
            at: now,
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
        assert_eq!(fetch_target(&t.path().join("h"), now), None, "husk");
        assert_eq!(
            fetch_target(&t.path().join("z"), now),
            None,
            "pruned worktree"
        );
        // And it stays unfetched on every later cycle.
        assert_eq!(
            fetch_target(&t.path().join("h"), now + Duration::from_secs(3600)),
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
            .filter(|d| fetch_target(d, now).is_some())
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
        assert!(dirs.iter().all(|d| fetch_target(d, soon).is_none()));
        // Past the interval: the same two again.
        let later = now + tree_fetch_interval();
        assert_eq!(
            dirs.iter()
                .filter(|d| fetch_target(d, later).is_some())
                .count(),
            2
        );
    }

    #[test]
    fn a_linked_worktree_met_first_fetches_for_its_primary() {
        let t = tempfile::tempdir().unwrap();
        let _ = layout(t.path());
        let now = Instant::now();
        assert!(fetch_target(&t.path().join("a"), now).is_some());
        assert!(fetch_target(&t.path().join("p"), now).is_none());
        assert!(fetch_target(&t.path().join("b"), now).is_none());
    }

    #[test]
    fn ref_sync_is_shared_per_common_dir() {
        let t = tempfile::tempdir().unwrap();
        let _ = layout(t.path());
        let now = Instant::now();
        assert_eq!(claim_ref_sync(&t.path().join("p"), "main", now), Ok(()));
        let prior = claim_ref_sync(&t.path().join("a"), "main", now)
            .expect_err("a linked worktree shares refs/heads/main with its primary");
        assert_eq!(prior.dir, t.path().join("p"));
        assert_eq!(
            prior.outcome, None,
            "the first attempt has not finished yet"
        );
        record_ref_sync_outcome(&t.path().join("p"), "main", "ref_sync_failed");
        let prior = claim_ref_sync(&t.path().join("b"), "main", now).unwrap_err();
        assert_eq!(prior.outcome.as_deref(), Some("ref_sync_failed"));
        // A non-claimant's outcome is ignored.
        record_ref_sync_outcome(&t.path().join("a"), "main", "default_ref_sync");
        let prior = claim_ref_sync(&t.path().join("b"), "main", now).unwrap_err();
        assert_eq!(prior.outcome.as_deref(), Some("ref_sync_failed"));
        assert_eq!(
            claim_ref_sync(&t.path().join("a"), "master", now),
            Ok(()),
            "another ref"
        );
        assert_eq!(
            claim_ref_sync(&t.path().join("q"), "main", now),
            Ok(()),
            "another repo"
        );
        let later = now + tree_fetch_interval();
        assert_eq!(claim_ref_sync(&t.path().join("b"), "main", later), Ok(()));
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
        assert!(fetch_due(&p, t0), "first call should be due");
        assert!(
            !fetch_due(&p, Instant::now()),
            "immediate re-call should not be due"
        );
    }
}
