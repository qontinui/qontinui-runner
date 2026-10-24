//! A time-bounded memo of the census's directory SIZES.
//!
//! The worktree census sizes every row's `node_modules` and cargo `target`
//! with a full walk of the tree. Plan
//! `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! paces the census from the end of each walk and makes each row's git work
//! cheap, so the walk repeats far more often than the ~52 minutes it took on
//! spaceship (2026-10-23) — and without this memo it would re-walk the same
//! multi-gigabyte trees at that higher rate.
//!
//! Only the byte TOTAL is memoised. Whether the directory exists and whether
//! it is a junction are read live by the caller every time, so a removal or a
//! junction swap is never hidden by a cached answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Env override for the memo's time-to-live (seconds). `0` disables it.
pub const SIZE_TTL_SECS_ENV: &str = "QONTINUI_CENSUS_SIZE_TTL_SECS";

/// Default TTL: 30 minutes — no more often than the pre-pacing walk sized a
/// row, and short enough that a tree that grew or shrank shows within the hour.
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

fn memo() -> &'static Mutex<HashMap<PathBuf, (Instant, u64)>> {
    static M: OnceLock<Mutex<HashMap<PathBuf, (Instant, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The byte total for `dir`: the memoised one when it was measured within the
/// TTL, else `measure()`'s answer (which is then memoised).
pub(super) fn bytes_or_measure(dir: &Path, measure: impl FnOnce() -> u64) -> u64 {
    bytes_or_measure_at(dir, Instant::now(), ttl(), measure)
}

fn bytes_or_measure_at(
    dir: &Path,
    now: Instant,
    ttl: Duration,
    measure: impl FnOnce() -> u64,
) -> u64 {
    if ttl.is_zero() {
        return measure();
    }
    {
        let map = memo().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, bytes)) = map.get(dir) {
            if now.saturating_duration_since(*at) < ttl {
                return *bytes;
            }
        }
    }
    let bytes = measure();
    let mut map = memo().lock().unwrap_or_else(|p| p.into_inner());
    if map.len() >= SIZE_MEMO_CAP {
        map.clear();
    }
    map.insert(dir.to_path_buf(), (now, bytes));
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

    #[test]
    fn a_fresh_entry_is_served_without_measuring() {
        let d = unique("fresh");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(bytes_or_measure_at(&d, t0, ttl, || 100), 100);
        let mut called = false;
        let got = bytes_or_measure_at(&d, t0 + Duration::from_secs(10), ttl, || {
            called = true;
            999
        });
        assert_eq!(got, 100);
        assert!(!called, "inside the TTL the tree must not be re-walked");
    }

    #[test]
    fn an_expired_entry_is_measured_again() {
        let d = unique("expired");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(60);
        assert_eq!(bytes_or_measure_at(&d, t0, ttl, || 100), 100);
        assert_eq!(
            bytes_or_measure_at(&d, t0 + Duration::from_secs(61), ttl, || 250),
            250
        );
        assert_eq!(
            bytes_or_measure_at(&d, t0 + Duration::from_secs(62), ttl, || 7),
            250
        );
    }

    #[test]
    fn a_zero_ttl_disables_the_memo() {
        let d = unique("zero");
        let t0 = Instant::now();
        assert_eq!(bytes_or_measure_at(&d, t0, Duration::ZERO, || 1), 1);
        assert_eq!(bytes_or_measure_at(&d, t0, Duration::ZERO, || 2), 2);
    }

    #[test]
    fn entries_are_per_path() {
        let a = unique("a");
        let b = unique("b");
        let t0 = Instant::now();
        let ttl = Duration::from_secs(1800);
        assert_eq!(bytes_or_measure_at(&a, t0, ttl, || 10), 10);
        assert_eq!(bytes_or_measure_at(&b, t0, ttl, || 20), 20);
        assert_eq!(bytes_or_measure_at(&a, t0, ttl, || 0), 10);
    }
}
