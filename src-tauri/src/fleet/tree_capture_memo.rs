//! The tree publisher's reuse of an unchanged tree's `status` / `ls-files`.
//!
//! Plan `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! (Phase 5, design decision D3). After Phase 4 a publisher capture spawns
//! only `git status --porcelain=v1` and `git ls-files --others
//! --exclude-standard`; everything else is read in-process. Most of the
//! hundreds of `qontinui-*` directories the publisher walks every cycle have
//! not changed since the last one, so those two answers are reused while
//!
//! * the tree's FINGERPRINT is unchanged — `(mtime, length)` of its git dir's
//!   `HEAD` and `index`, of the common dir's `info/exclude` and of the root
//!   `.gitignore` (the two ignore files an operator edits to heal an
//!   untracked-file report), the worktree root's own mtime, and the commit
//!   HEAD names (already read in-process, so a commit or reset made from
//!   another worktree of the repository moves it too); and
//! * the cached answer is younger than `COORD_TREE_CAPTURE_MAX_AGE_SECS`
//!   (default [`DEFAULT_MAX_AGE_SECS`]; `0` disables the reuse entirely).
//!
//! ## What the fingerprint does not see
//!
//! A change that moves none of those is invisible until the max age expires,
//! so `dirty`, `dirty_files` and `untracked_count` can lag by up to the max
//! age after any of:
//!
//! * an edit to an already-tracked file (moves neither `index` nor the root);
//! * a file created, deleted or renamed BELOW the root — that moves only its
//!   own directory's mtime, and the fingerprint reads the root's alone;
//! * an edit to a nested `.gitignore`, to `core.excludesFile`, or to any
//!   config `status` reads.
//!
//! That is the bound D3 accepts: every consumer that MUTATES a tree re-checks
//! live git at apply time (the pull executor's `symbolic-ref` + `status`,
//! `restore_default`'s re-verification), and the in-process facts — HEAD,
//! branch, the behind counts — are recomputed every capture, so they follow
//! every fetch. Set the max age to `0` where a report must never lag.
//!
//! **An answer is stored only when the fingerprint did not move while it was
//! captured.** `git status` can rewrite the index itself (refreshing stat
//! data), and an edit can land mid-capture; either way the fingerprint taken
//! after the capture differs from the one taken before it, and the next cycle
//! captures again rather than reusing an answer of unknown vintage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// The env knob for the reuse bound, in seconds; `0` disables the reuse.
pub(super) const MAX_AGE_ENV: &str = "COORD_TREE_CAPTURE_MAX_AGE_SECS";

/// The default reuse bound: five minutes — five 60 s publisher cycles.
pub(super) const DEFAULT_MAX_AGE_SECS: u64 = 300;

/// Entries kept before the memo is emptied. Directories the publisher no
/// longer walks (a removed worktree) are never visited again, so without a
/// bound their entries would live for the process's life.
const MEMO_CAP: usize = 4096;

/// The reuse bound `raw` (the env value) selects: `None` when the reuse is
/// disabled (`0`), the default when unset or unparseable.
pub(super) fn parse_max_age(raw: Option<&str>) -> Option<Duration> {
    let secs = raw
        .map(str::trim)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_AGE_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// [`parse_max_age`] of [`MAX_AGE_ENV`].
pub(super) fn max_age() -> Option<Duration> {
    parse_max_age(std::env::var(MAX_AGE_ENV).ok().as_deref())
}

/// `(mtime, length)` of a file, `None` when it cannot be read.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// What must not have moved for a cached answer to be reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fingerprint {
    head_file: Stamp,
    index_file: Stamp,
    exclude_file: Stamp,
    root_gitignore: Stamp,
    root_mtime: Option<SystemTime>,
    head_oid: String,
}

/// The fingerprint of the checkout at `dir` whose HEAD names `head_oid`;
/// `None` — never reuse — when its git dir does not resolve or its `HEAD`
/// file cannot be read. An absent ignore file stamps as `None`, so creating
/// one moves the fingerprint too.
pub(super) fn fingerprint(dir: &Path, head_oid: &str) -> Option<Fingerprint> {
    let (git_dir, common_dir) = qontinui_runner_lib::git_layout::git_dir_and_common_dir(dir)?;
    let head_file = stamp(&git_dir.join("HEAD"));
    head_file?;
    Some(Fingerprint {
        head_file,
        index_file: stamp(&git_dir.join("index")),
        exclude_file: stamp(&common_dir.join("info").join("exclude")),
        root_gitignore: stamp(&dir.join(".gitignore")),
        root_mtime: std::fs::metadata(dir).ok().and_then(|m| m.modified().ok()),
        head_oid: head_oid.to_string(),
    })
}

/// The two CLI answers a capture spawns for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalFacts {
    /// `git status --porcelain=v1` stdout, as captured.
    pub status_porcelain: String,
    /// `git ls-files --others --exclude-standard` line count; `None` when that
    /// call failed.
    pub untracked_count: Option<i32>,
}

struct Entry {
    fingerprint: Fingerprint,
    captured_at: Instant,
    facts: LocalFacts,
}

/// Per-directory cache of [`LocalFacts`].
pub(super) struct CaptureMemo {
    map: Mutex<HashMap<PathBuf, Entry>>,
}

impl CaptureMemo {
    pub(super) fn global() -> &'static CaptureMemo {
        static M: OnceLock<CaptureMemo> = OnceLock::new();
        M.get_or_init(CaptureMemo::new)
    }

    pub(super) fn new() -> CaptureMemo {
        CaptureMemo {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// The local facts for `dir` (whose HEAD names `head_oid`) at `now`: the
    /// cached answer when the fingerprint is unchanged and the answer is
    /// younger than `max_age`, otherwise `capture()` — stored for next time
    /// only when the fingerprint held still across it. `max_age: None`
    /// disables the reuse: every call captures and nothing is kept. A failed
    /// capture (`None`) drops any cached answer.
    pub(super) fn local_facts(
        &self,
        dir: &Path,
        head_oid: &str,
        now: Instant,
        max_age: Option<Duration>,
        capture: impl FnOnce() -> Option<LocalFacts>,
    ) -> Option<LocalFacts> {
        let Some(max_age) = max_age else {
            self.lock().remove(dir);
            return capture();
        };
        let before = fingerprint(dir, head_oid);
        if let Some(fp) = &before {
            if let Some(e) = self.lock().get(dir) {
                if e.fingerprint == *fp && now.saturating_duration_since(e.captured_at) < max_age {
                    return Some(e.facts.clone());
                }
            }
        }
        let facts = capture();
        let mut map = self.lock();
        match (&facts, before) {
            (Some(f), Some(fp)) if fingerprint(dir, head_oid).as_ref() == Some(&fp) => {
                if map.len() >= MEMO_CAP && !map.contains_key(dir) {
                    map.clear();
                }
                map.insert(
                    dir.to_path_buf(),
                    Entry {
                        fingerprint: fp,
                        captured_at: now,
                        facts: f.clone(),
                    },
                );
            }
            _ => {
                map.remove(dir);
            }
        }
        facts
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Entry>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout shape the fingerprint reads — a `.git` dir with `HEAD` and
    /// `index` — and no real git: the capture is a counting closure.
    fn tree(root: &Path) -> PathBuf {
        let dir = root.join("qontinui-probe");
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("index"), b"DIRC0001").unwrap();
        dir
    }

    fn facts(n: usize) -> LocalFacts {
        LocalFacts {
            status_porcelain: format!(" M file{n}.txt\n"),
            untracked_count: Some(n as i32),
        }
    }

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";
    const AGE: Option<Duration> = Some(Duration::from_secs(300));

    #[test]
    fn unchanged_tree_reuses_status_within_max_age() {
        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let mut captures = 0usize;
        for step in 0..3u64 {
            let got =
                memo.local_facts(&dir, OID, now + Duration::from_secs(60 * step), AGE, || {
                    captures += 1;
                    Some(facts(captures))
                });
            // Every cycle answers with the FIRST capture's facts.
            assert_eq!(got, Some(facts(1)), "cycle {step}");
        }
        assert_eq!(
            captures, 1,
            "one capture for three cycles of an unchanged tree"
        );
    }

    #[test]
    fn index_or_head_change_forces_recapture() {
        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let mut captures = 0usize;
        let mut cycle = |memo: &CaptureMemo, oid: &str, captures: &mut usize| {
            memo.local_facts(&dir, oid, now, AGE, || {
                *captures += 1;
                Some(facts(*captures))
            })
        };
        assert_eq!(cycle(&memo, OID, &mut captures), Some(facts(1)));
        assert_eq!(cycle(&memo, OID, &mut captures), Some(facts(1)), "reused");

        // `git add` / `git commit` rewrite the index (a different length,
        // so the change is visible whatever the mtime granularity).
        std::fs::write(dir.join(".git").join("index"), b"DIRC0002-longer").unwrap();
        assert_eq!(
            cycle(&memo, OID, &mut captures),
            Some(facts(2)),
            "index moved"
        );
        assert_eq!(
            cycle(&memo, OID, &mut captures),
            Some(facts(2)),
            "reused again"
        );

        // `git checkout <other>` rewrites HEAD.
        std::fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/feature-x\n").unwrap();
        assert_eq!(
            cycle(&memo, OID, &mut captures),
            Some(facts(3)),
            "HEAD moved"
        );

        // A commit or reset made elsewhere moves the commit HEAD names.
        let other = "fedcba9876543210fedcba9876543210fedcba98";
        assert_eq!(
            cycle(&memo, other, &mut captures),
            Some(facts(4)),
            "HEAD oid moved"
        );

        // A file created at the worktree root moves the root's mtime.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("new-untracked.txt"), b"x").unwrap();
        assert_eq!(
            cycle(&memo, other, &mut captures),
            Some(facts(5)),
            "root moved"
        );
        assert_eq!(captures, 5);
    }

    /// Editing an ignore file is how an operator heals an untracked-file
    /// report, and an in-place edit moves neither `index` nor the root's
    /// mtime — so the ignore files are in the fingerprint themselves.
    #[test]
    fn an_ignore_file_edit_forces_recapture() {
        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let gitignore = dir.join(".gitignore");
        std::fs::write(&gitignore, b"a\n").unwrap();
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let mut captures = 0usize;
        let cycle = |memo: &CaptureMemo, captures: &mut usize| {
            memo.local_facts(&dir, OID, now, AGE, || {
                *captures += 1;
                Some(facts(*captures))
            })
        };
        assert_eq!(cycle(&memo, &mut captures), Some(facts(1)));
        assert_eq!(cycle(&memo, &mut captures), Some(facts(1)), "reused");

        // An in-place edit of the root .gitignore (a different length, so
        // the change is visible whatever the mtime granularity).
        std::fs::write(&gitignore, b"a\nscratch/\n").unwrap();
        assert_eq!(
            cycle(&memo, &mut captures),
            Some(facts(2)),
            ".gitignore moved"
        );
        assert_eq!(cycle(&memo, &mut captures), Some(facts(2)), "reused");

        // Creating, then editing, the common dir's info/exclude — inside the
        // git dir, so the root's mtime cannot carry it.
        let info = dir.join(".git").join("info");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("exclude"), b"x\n").unwrap();
        assert_eq!(
            cycle(&memo, &mut captures),
            Some(facts(3)),
            "info/exclude created"
        );
        std::fs::write(info.join("exclude"), b"x\nscratch.txt\n").unwrap();
        assert_eq!(
            cycle(&memo, &mut captures),
            Some(facts(4)),
            "info/exclude edited"
        );
        assert_eq!(captures, 4);
    }

    #[test]
    fn max_age_forces_recapture() {
        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let mut captures = 0usize;
        let mut at = |offset: u64, captures: &mut usize| {
            memo.local_facts(&dir, OID, now + Duration::from_secs(offset), AGE, || {
                *captures += 1;
                Some(facts(*captures))
            })
        };
        assert_eq!(at(0, &mut captures), Some(facts(1)));
        assert_eq!(at(299, &mut captures), Some(facts(1)), "inside the bound");
        assert_eq!(
            at(300, &mut captures),
            Some(facts(2)),
            "at the bound: recaptured"
        );
        // The bound restarts from the recapture.
        assert_eq!(at(599, &mut captures), Some(facts(2)));
        assert_eq!(at(600, &mut captures), Some(facts(3)));
        assert_eq!(captures, 3);
    }

    #[test]
    fn max_age_zero_disables_the_skip() {
        assert_eq!(parse_max_age(Some("0")), None, "0 disables");
        assert_eq!(parse_max_age(Some(" 0 ")), None);
        assert_eq!(
            parse_max_age(None),
            Some(Duration::from_secs(300)),
            "default"
        );
        assert_eq!(parse_max_age(Some("junk")), Some(Duration::from_secs(300)));
        assert_eq!(parse_max_age(Some("45")), Some(Duration::from_secs(45)));

        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let mut captures = 0usize;
        for _ in 0..3 {
            memo.local_facts(&dir, OID, now, parse_max_age(Some("0")), || {
                captures += 1;
                Some(facts(captures))
            });
        }
        assert_eq!(captures, 3, "every cycle captures");
        assert_eq!(memo.len(), 0, "and nothing is kept");
    }

    #[test]
    fn a_failed_capture_is_not_cached_and_drops_the_old_answer() {
        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let memo = CaptureMemo::new();
        let now = Instant::now();
        assert_eq!(
            memo.local_facts(&dir, OID, now, AGE, || Some(facts(1))),
            Some(facts(1))
        );
        // Move the index so the next cycle captures — and that capture fails.
        std::fs::write(dir.join(".git").join("index"), b"DIRC-moved-again").unwrap();
        assert_eq!(memo.local_facts(&dir, OID, now, AGE, || None), None);
        assert_eq!(memo.len(), 0);
    }

    #[test]
    fn a_fingerprint_that_moves_during_the_capture_is_not_stored() {
        // `git status` refreshing the index mid-capture: the answer is used
        // this cycle, and the next cycle captures again.
        let t = tempfile::tempdir().unwrap();
        let dir = tree(t.path());
        let index = dir.join(".git").join("index");
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let got = memo.local_facts(&dir, OID, now, AGE, || {
            std::fs::write(&index, b"DIRC-refreshed-by-status").unwrap();
            Some(facts(1))
        });
        assert_eq!(got, Some(facts(1)));
        assert_eq!(memo.len(), 0, "not stored");
        let mut captures = 0;
        memo.local_facts(&dir, OID, now, AGE, || {
            captures += 1;
            Some(facts(2))
        });
        assert_eq!(captures, 1, "recaptured");
        assert_eq!(memo.len(), 1, "and now it held still, so it is kept");
    }

    #[test]
    fn a_dir_with_no_git_dir_is_never_reused() {
        let t = tempfile::tempdir().unwrap();
        let memo = CaptureMemo::new();
        let now = Instant::now();
        let mut captures = 0;
        for _ in 0..2 {
            memo.local_facts(t.path(), OID, now, AGE, || {
                captures += 1;
                Some(facts(1))
            });
        }
        assert_eq!(captures, 2);
        assert_eq!(memo.len(), 0);
    }
}
