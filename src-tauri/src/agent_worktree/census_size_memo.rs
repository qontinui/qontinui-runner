//! A time-bounded memo of the census's directory SIZES.
//!
//! The worktree census sizes every row's `node_modules` and cargo `target`
//! with a full walk of the tree. Plan
//! `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! paces the census from the end of each walk and makes each row's git work
//! cheap, so walks repeat far more often than before, and without this memo
//! every walk would re-size the same multi-gigabyte trees.
//!
//! What it does and does not hide:
//!
//! * Only the byte TOTAL is memoised. Whether the directory exists and whether
//!   it is a junction are read live by the caller every time, so a removal or
//!   a junction swap shows at once.
//! * An entry answers only while the directory's own modification time is
//!   the one it was measured at. Adding or removing a direct child (a
//!   `cargo clean` then a rebuild, an `npm install`) moves that time, and the
//!   tree is sized again.
//! * A size that could not be measured is never memoised: an unreadable root
//!   costs one walk's zero, not a TTL's.
//! * An operator's explicit refresh ([`clear`], from `spawn_census_rebuild`)
//!   empties the memo, so `?refresh=1` sizes everything afresh.
//!
//! Growth inside an unchanged directory (files rewritten deeper in the tree)
//! shows within the TTL.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Env override for the memo's time-to-live (seconds). `0` disables it.
pub const SIZE_TTL_SECS_ENV: &str = "QONTINUI_CENSUS_SIZE_TTL_SECS";

/// Default TTL: 30 minutes. A tree's size is a reporting figure (reclaimable
/// bytes, biggest-first ranking), never a reap decision, so half an hour of
/// lag on growth deep inside an unchanged directory is the price of not
/// re-walking every `target` on every census.
const DEFAULT_SIZE_TTL_SECS: u64 = 1_800;

/// Upper bound on entries; cleared when full (a cold walk costs what every
/// walk used to cost, and nothing is ever answered from a stale entry).
const SIZE_MEMO_CAP: usize = 16_384;

fn ttl() -> Duration {
    let secs = std::env::var(SIZE_TTL_SECS_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SIZE_TTL_SECS);
    Duration::from_secs(secs)
}

/// One memoised size: when it was measured, the directory's modification time
/// at that moment, and the bytes.
#[derive(Clone, Copy)]
struct Entry {
    at: Instant,
    dir_mtime: Option<SystemTime>,
    bytes: u64,
}

/// The memo's storage. Production uses the one process-global instance
/// ([`memo`]); a test builds its own, so a `clear` in one test can never
/// empty another's entries.
type Memo = Mutex<HashMap<PathBuf, Entry>>;

fn memo() -> &'static Memo {
    static M: OnceLock<Memo> = OnceLock::new();
    M.get_or_init(Memo::default)
}

/// Forget every memoised size — an explicit refresh asked for fresh numbers.
pub(super) fn clear() {
    clear_in(memo());
}

fn clear_in(m: &Memo) {
    m.lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// The byte total for `dir`: the memoised one when it was measured within the
/// TTL and the directory has not changed since, else `measure()`'s answer.
/// `measure` returns `None` when the size could not be read; that is reported
/// as 0, as before, and never memoised.
pub(super) fn bytes_or_measure(dir: &Path, measure: impl FnOnce() -> Option<u64>) -> u64 {
    let dir_mtime = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    bytes_or_measure_in(memo(), dir, Instant::now(), dir_mtime, ttl(), measure)
}

fn bytes_or_measure_in(
    m: &Memo,
    dir: &Path,
    now: Instant,
    dir_mtime: Option<SystemTime>,
    ttl: Duration,
    measure: impl FnOnce() -> Option<u64>,
) -> u64 {
    if ttl.is_zero() {
        return measure().unwrap_or(0);
    }
    {
        let map = m.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = map.get(dir) {
            // An unknown mtime never matches: no shared answer it cannot justify.
            let unchanged = dir_mtime.is_some() && e.dir_mtime == dir_mtime;
            if unchanged && now.saturating_duration_since(e.at) < ttl {
                return e.bytes;
            }
        }
    }
    let Some(bytes) = measure() else {
        return 0;
    };
    let mut map = m.lock().unwrap_or_else(|p| p.into_inner());
    if map.len() >= SIZE_MEMO_CAP {
        map.clear();
    }
    map.insert(
        dir.to_path_buf(),
        Entry {
            at: now,
            dir_mtime,
            bytes,
        },
    );
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique(tag: &str) -> PathBuf {
        PathBuf::from(format!(
            "/census-size-memo-test/{tag}/{}/{:?}",
            std::process::id(),
            Instant::now()
        ))
    }

    fn mtime(secs: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    #[test]
    fn a_fresh_entry_is_served_without_measuring() {
        let m = Memo::default();
        let d = unique("fresh");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || Some(100)),
            100
        );
        let mut called = false;
        let got = bytes_or_measure_in(&m, &d, t0 + Duration::from_secs(10), mtime(1), ttl, || {
            called = true;
            Some(999)
        });
        assert_eq!(got, 100);
        assert!(!called, "inside the TTL the tree must not be re-walked");
    }

    #[test]
    fn an_expired_entry_is_measured_again() {
        let m = Memo::default();
        let d = unique("expired");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(60);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || Some(100)),
            100
        );
        let t1 = t0 + Duration::from_secs(61);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t1, mtime(1), ttl, || Some(250)),
            250
        );
        let t2 = t0 + Duration::from_secs(62);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t2, mtime(1), ttl, || Some(7)),
            250
        );
    }

    #[test]
    fn a_changed_directory_is_measured_again_inside_the_ttl() {
        let m = Memo::default();
        let d = unique("changed");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || Some(100)),
            100
        );
        // A child added or removed (a clean + rebuild) moved the dir's mtime.
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(2), ttl, || Some(5)),
            5
        );
        // An unknown mtime never matches a memoised one.
        assert_eq!(bytes_or_measure_in(&m, &d, t0, None, ttl, || Some(6)), 6);
        assert_eq!(bytes_or_measure_in(&m, &d, t0, None, ttl, || Some(8)), 8);
    }

    #[test]
    fn an_unmeasured_size_is_reported_as_zero_and_never_memoised() {
        let m = Memo::default();
        let d = unique("unmeasured");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || None), 0);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || Some(42)),
            42
        );
    }

    #[test]
    fn clear_forgets_every_memoised_size() {
        let m = Memo::default();
        let d = unique("clear");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || Some(100)),
            100
        );
        clear_in(&m);
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), ttl, || Some(3)),
            3
        );
    }

    #[test]
    fn a_zero_ttl_disables_the_memo() {
        let m = Memo::default();
        let d = unique("zero");
        let t0 = Instant::now();
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), Duration::ZERO, || Some(1)),
            1
        );
        assert_eq!(
            bytes_or_measure_in(&m, &d, t0, mtime(1), Duration::ZERO, || Some(2)),
            2
        );
    }

    #[test]
    fn entries_are_per_path() {
        let m = Memo::default();
        let a = unique("a");
        let b = unique("b");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(
            bytes_or_measure_in(&m, &a, t0, mtime(1), ttl, || Some(10)),
            10
        );
        assert_eq!(
            bytes_or_measure_in(&m, &b, t0, mtime(1), ttl, || Some(20)),
            20
        );
        assert_eq!(
            bytes_or_measure_in(&m, &a, t0, mtime(1), ttl, || Some(0)),
            10
        );
    }
}
