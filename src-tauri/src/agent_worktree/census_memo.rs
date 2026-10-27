//! Per-walk and cross-walk memos that let the worktree census ask each git
//! question once instead of once per row.
//!
//! Plan `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! (Phase 1). Measured offline on spaceship (2026-10-23): one census row cost
//! 12–13 `git` calls (~37 processes with the `cmd\git.exe` launcher and its
//! conhost), and five of them were per-REPO facts recomputed for every row.
//!
//! Two memos, with different lifetimes because their answers age differently:
//!
//! * [`WalkMemo`] — lives for ONE census walk. The trunk ref name and its
//!   commit are properties of a repository's shared refs (`refs/remotes/*` in
//!   the git COMMON dir), so every worktree of a repo shares them. They can
//!   move between walks (a fetch, a `remote set-head`), so the memo is rebuilt
//!   every walk — the walk's own `fetch_trunk` runs first and the memo is
//!   seeded after it.
//! * [`LandedMemo`] — process-global, across walks. `merge-base --is-ancestor
//!   HEAD <trunk>` and `git cherry <trunk> HEAD` are pure functions of the two
//!   commits, so a measured answer for `(common dir, HEAD oid, trunk oid)`
//!   never goes stale. Only `Some(_)` answers are stored: an honest unknown is
//!   re-asked next walk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A resolved trunk: its ref name (`origin/main`) and, when it could be read,
/// the commit it pointed at when the walk read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Trunk {
    pub name: String,
    pub oid: Option<String>,
}

/// Per-walk memo of trunk resolution, keyed by git COMMON dir.
#[derive(Default)]
pub(super) struct WalkMemo {
    trunks: HashMap<PathBuf, Option<Trunk>>,
    /// How many times a trunk was actually RESOLVED (not served from the
    /// memo) — the observable the "once per common dir" test pins.
    pub resolutions: usize,
}

impl WalkMemo {
    /// The trunk for the checkout at `dir`, resolving it with `resolve` only
    /// the first time its common dir is seen this walk. A checkout whose
    /// common dir cannot be read from disk is resolved every time — the
    /// uncached behaviour, never a shared answer it cannot justify.
    pub fn trunk_for(
        &mut self,
        dir: &Path,
        resolve: &mut dyn FnMut(&Path) -> Option<Trunk>,
    ) -> Option<Trunk> {
        let Some(key) = qontinui_runner_lib::git_layout::common_dir_key(dir) else {
            self.resolutions += 1;
            return resolve(dir);
        };
        if let Some(hit) = self.trunks.get(&key) {
            return hit.clone();
        }
        self.resolutions += 1;
        let t = resolve(dir);
        self.trunks.insert(key, t.clone());
        t
    }

    /// Record `trunk` — resolved by the caller — for `dir`'s common dir,
    /// REPLACING anything already held. The walk seeds each repository right
    /// after that repository's own `fetch_trunk`, and that reading is the fresh
    /// one; a row of the same repository reached earlier in the walk may have
    /// memoised a pre-fetch commit, which must not survive the seed.
    pub fn seed(&mut self, dir: &Path, trunk: Option<Trunk>) {
        self.resolutions += 1;
        if let Some(key) = qontinui_runner_lib::git_layout::common_dir_key(dir) {
            self.trunks.insert(key, trunk);
        }
    }
}

/// Upper bound on [`LandedMemo`] answers across all repositories. Each
/// repository holds answers for its CURRENT trunk commit only (see
/// [`LandedMemo`]), so live entries are bounded by the worktree count — the
/// fleet's largest box carries ~2,000 — and this cap is a backstop: when it is
/// reached the memo is cleared, a cold walk costs what every walk cost before
/// the memo, and nothing is ever answered wrongly.
pub(super) const LANDED_MEMO_CAP: usize = 8_192;

/// One repository's answers: the trunk commit they were measured against, and
/// `HEAD oid -> landed` for that trunk.
struct RepoAnswers {
    trunk: String,
    by_head: HashMap<String, bool>,
}

/// Process-global memo of measured `landed_in_main` answers, keyed by git
/// common dir. Storing an answer for a NEW trunk commit drops that
/// repository's answers for the old one: once origin's trunk moves they are
/// never looked up again, and keeping them would only grow the memo until the
/// cap cleared everything (a fully cold walk on exactly the busiest repos).
pub(super) struct LandedMemo {
    map: Mutex<HashMap<PathBuf, RepoAnswers>>,
}

impl LandedMemo {
    pub fn global() -> &'static LandedMemo {
        static M: OnceLock<LandedMemo> = OnceLock::new();
        M.get_or_init(LandedMemo::new)
    }

    pub fn new() -> LandedMemo {
        LandedMemo {
            map: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, common: &Path, head: &str, trunk: &str) -> Option<bool> {
        let map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let repo = map.get(common)?;
        if repo.trunk != trunk {
            return None;
        }
        repo.by_head.get(head).copied()
    }

    pub fn put(&self, common: &Path, head: &str, trunk: &str, landed: bool) {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let total: usize = map.values().map(|r| r.by_head.len()).sum();
        if total >= LANDED_MEMO_CAP {
            map.clear();
        }
        let repo = map
            .entry(common.to_path_buf())
            .or_insert_with(|| RepoAnswers {
                trunk: trunk.to_string(),
                by_head: HashMap::new(),
            });
        if repo.trunk != trunk {
            // The trunk moved: every answer held is for a commit no row will
            // be compared against again.
            repo.trunk = trunk.to_string();
            repo.by_head.clear();
        }
        repo.by_head.insert(head.to_string(), landed);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(|r| r.by_head.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked_pair(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let primary = root.join("primary");
        let common = primary.join(".git");
        std::fs::create_dir_all(&common).unwrap();
        let mut wts = Vec::new();
        for name in ["a", "b"] {
            let admin = common.join("worktrees").join(name);
            std::fs::create_dir_all(&admin).unwrap();
            std::fs::write(admin.join("commondir"), "../..\n").unwrap();
            let wt = root.join(name);
            std::fs::create_dir_all(&wt).unwrap();
            std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
            wts.push(wt);
        }
        (primary, wts[0].clone(), wts[1].clone())
    }

    #[test]
    fn a_trunk_is_resolved_once_per_common_dir() {
        let t = tempfile::tempdir().unwrap();
        let (primary, a, b) = linked_pair(t.path());
        let other = t.path().join("other");
        std::fs::create_dir_all(other.join(".git")).unwrap();

        let mut calls = 0usize;
        let mut resolve = |_: &Path| {
            calls += 1;
            Some(Trunk {
                name: "origin/main".to_string(),
                oid: Some("abc".to_string()),
            })
        };
        let mut memo = WalkMemo::default();
        for d in [&primary, &a, &b, &primary, &other, &other] {
            assert_eq!(
                memo.trunk_for(d, &mut resolve).map(|t| t.name),
                Some("origin/main".to_string())
            );
        }
        // primary + its two linked worktrees share one common dir; `other` is
        // a second repository.
        assert_eq!(calls, 2);
        assert_eq!(memo.resolutions, 2);
    }

    #[test]
    fn a_seed_replaces_a_trunk_memoised_earlier_in_the_walk() {
        let t = tempfile::tempdir().unwrap();
        let (primary, a, _b) = linked_pair(t.path());
        let mut memo = WalkMemo::default();
        let mut stale = |_: &Path| {
            Some(Trunk {
                name: "origin/main".to_string(),
                oid: Some("old".to_string()),
            })
        };
        let _ = memo.trunk_for(&a, &mut stale);
        memo.seed(
            &primary,
            Some(Trunk {
                name: "origin/main".to_string(),
                oid: Some("new".to_string()),
            }),
        );
        let got = memo.trunk_for(&a, &mut |_: &Path| -> Option<Trunk> {
            panic!("must be served from the seed")
        });
        assert_eq!(got.and_then(|t| t.oid).as_deref(), Some("new"));
    }

    #[test]
    fn a_dir_with_no_readable_git_dir_is_resolved_every_time() {
        let t = tempfile::tempdir().unwrap();
        let mut calls = 0usize;
        let mut resolve = |_: &Path| {
            calls += 1;
            None
        };
        let mut memo = WalkMemo::default();
        assert!(memo.trunk_for(t.path(), &mut resolve).is_none());
        assert!(memo.trunk_for(t.path(), &mut resolve).is_none());
        assert_eq!(calls, 2);
    }

    #[test]
    fn an_unresolvable_trunk_is_memoised_as_unresolvable_for_the_walk() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join(".git")).unwrap();
        let mut calls = 0usize;
        let mut resolve = |_: &Path| {
            calls += 1;
            None
        };
        let mut memo = WalkMemo::default();
        assert!(memo.trunk_for(t.path(), &mut resolve).is_none());
        assert!(memo.trunk_for(t.path(), &mut resolve).is_none());
        assert_eq!(calls, 1);
    }

    #[test]
    fn the_landed_memo_keys_on_all_three_parts_and_clears_when_full() {
        let m = LandedMemo::new();
        let c = Path::new("/c");
        m.put(c, "h1", "t1", true);
        assert_eq!(m.get(c, "h1", "t1"), Some(true));
        assert_eq!(m.get(c, "h1", "t2"), None, "a moved trunk is a miss");
        assert_eq!(m.get(c, "h2", "t1"), None, "a moved HEAD is a miss");
        assert_eq!(m.get(Path::new("/d"), "h1", "t1"), None, "another repo");
        // The cap is a backstop over every repository's answers, all on the
        // same trunk so nothing is dropped for a trunk move.
        let d = Path::new("/d");
        for i in 0..LANDED_MEMO_CAP {
            m.put(d, &format!("x{i}"), "t1", false);
        }
        assert!(m.len() <= LANDED_MEMO_CAP);
        assert_eq!(m.get(c, "h1", "t1"), None, "cleared when full");
    }

    #[test]
    fn a_moved_trunk_drops_that_repos_old_answers_only() {
        let m = LandedMemo::new();
        let (c, d) = (Path::new("/c"), Path::new("/d"));
        m.put(c, "h1", "t1", true);
        m.put(c, "h2", "t1", false);
        m.put(d, "h1", "t1", true);
        assert_eq!(m.len(), 3);
        m.put(c, "h1", "t2", true);
        assert_eq!(m.get(c, "h1", "t2"), Some(true));
        assert_eq!(
            m.get(c, "h2", "t1"),
            None,
            "answers for the old trunk are gone"
        );
        assert_eq!(
            m.get(d, "h1", "t1"),
            Some(true),
            "another repo is untouched"
        );
        assert_eq!(m.len(), 2, "growth is bounded by live keys");
    }
}
