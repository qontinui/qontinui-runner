//! Is a resolved tool SETTLED, or is an installer still writing it?
//!
//! `npm install --global @anthropic-ai/claude-code` re-links `bin/claude` and
//! then writes the 340 MB native image behind it. A 100 ms sampler on
//! 2026-11-10 saw `bin/claude` resolve to an executable, non-stub ELF that was
//! still being written: as small as 11.8 MB of 341.9 MB, with partial-ELF
//! windows of p50 1.0 s and max 1.7 s, and a settle time of p95 3.1 s and
//! max 3.8 s (coord finding `18ee5471-7b84-469b-927f-6128bfcb7263`). A
//! resolver that launches the first hit can start a truncated image there. The quiescence predicate is the
//! parent plan's D1 (`2026-11-10-migration-holds-for-missing-claude-cli`): the
//! target's change time is at least [`SETTLE_MIN_AGE`] old. A write, or a
//! rename into place, moves the target's change time, so an install still in
//! progress reads as [`Settle::Changing`]. [`settle_of`] follows links, so a
//! re-link alone, which changes only the link, is not seen, and a binary installed long ago costs one `stat`.
//!
//! ONE Rust predicate, so the Rust consumers cannot disagree about "settled":
//! - the native identity shim (`qontinui-shim`'s `run_identity`, plan
//!   `2026-11-10-shim-settle-wait-and-in-memory-launch-liveness` Phase 3);
//! - the parent plan's planned Phase 4 migration hold
//!   (`terminal/cli_availability.rs`, not built yet), which must call
//!   [`settle_of`] rather than carry a second copy.
//!
//! The bash identity shim (`identity_shim.bash`) carries its own copy, as it
//! does for `is_npm_placeholder`. It works in whole seconds
//! (`now - ctime >= 3`), so it is stricter than this predicate by up to 1 s.
//!
//! A change time that cannot be read is [`Settle::Unknown`]. Callers that gate a
//! LAUNCH treat it as settled: waiting on it would delay every launch by the
//! full bound on a box that has no readable change time, and launching at once
//! is the behaviour that predates this check.
//!
//! ## Windows limits
//! - **The probe sees npm's wrapper, not the image.** On Windows `PATH`
//!   resolves npm's cmd-shim wrapper (`claude.cmd`, or the bare `sh` wrapper
//!   under MSYS), so the check reads the wrapper's times, not those of the
//!   `claude.exe` behind it. Coverage there is incidental: it holds only when
//!   the install also rewrites the wrapper.
//! - **`max(created, modified)` can read the write START.** Windows has no
//!   stable change time on the pinned toolchain, and an in-place write, or an
//!   extractor that resets mtime, leaves a time near when the write began. So a
//!   write that runs longer than [`SETTLE_MIN_AGE`] can read as
//!   [`Settle::Settled`] while it is still in progress.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// How long a target's change time must be in the past before it reads as
/// settled (the parent plan's D1).
pub const SETTLE_MIN_AGE: Duration = Duration::from_secs(2);

/// What [`judge`] and [`settle_of`] conclude about one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    /// The target last changed at least the minimum age ago.
    Settled,
    /// The target changed less than the minimum age ago, or its change time is
    /// in the future (`age` is then zero): an install may still be writing it.
    Changing { age: Duration },
    /// The change time could not be read (no such file, no permission, a
    /// platform with no change time).
    Unknown,
}

/// The pure predicate: `changed_at` is settled at `now` when it is at least
/// `min` in the past. A `changed_at` in the future is `Changing` with a zero
/// age: a clock skewed under an installer is no evidence the write finished.
pub fn judge(changed_at: SystemTime, now: SystemTime, min: Duration) -> Settle {
    match now.duration_since(changed_at) {
        Ok(age) if age >= min => Settle::Settled,
        Ok(age) => Settle::Changing { age },
        Err(_) => Settle::Changing {
            age: Duration::ZERO,
        },
    }
}

/// [`judge`] the target `path` names, following symlinks, against
/// [`SETTLE_MIN_AGE`]. A failed read is [`Settle::Unknown`].
pub fn settle_of(path: &Path, now: SystemTime) -> Settle {
    match changed_at(path) {
        Some(changed) => judge(changed, now, SETTLE_MIN_AGE),
        None => Settle::Unknown,
    }
}

/// The target's change time: `st_ctime` with its nanoseconds. `metadata`
/// follows symlinks, so a long-standing `bin/claude` link to a freshly
/// rewritten image reads the image's time.
#[cfg(unix)]
fn changed_at(path: &Path) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    let secs = u64::try_from(meta.ctime()).ok()?;
    let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

/// Windows has no stable change time on the pinned toolchain
/// (`MetadataExt::change_time` is the unstable `windows_change_time`,
/// rust-lang/rust#121478), so the later of creation and last write stands in.
/// See the module docs' "Windows limits" for what that misses.
#[cfg(windows)]
fn changed_at(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    match (meta.created().ok(), meta.modified().ok()) {
        (Some(c), Some(m)) => Some(c.max(m)),
        (one, other) => one.or(other),
    }
}

#[cfg(not(any(unix, windows)))]
fn changed_at(_path: &Path) -> Option<SystemTime> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64, millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis)
    }

    #[test]
    fn judge_settles_at_exactly_the_minimum_age() {
        let changed = at(1_000, 0);
        let min = SETTLE_MIN_AGE;
        assert_eq!(
            judge(changed, changed, min),
            Settle::Changing {
                age: Duration::ZERO
            },
            "age 0"
        );
        assert_eq!(
            judge(changed, at(1_001, 999), min),
            Settle::Changing {
                age: Duration::from_millis(1_999)
            },
            "age 1.999 s"
        );
        assert_eq!(
            judge(changed, at(1_002, 0), min),
            Settle::Settled,
            "age 2 s"
        );
        assert_eq!(
            judge(changed, at(1_003, 0), min),
            Settle::Settled,
            "age 3 s"
        );
    }

    #[test]
    fn judge_reads_a_future_change_time_as_changing() {
        assert_eq!(
            judge(at(1_005, 0), at(1_000, 0), SETTLE_MIN_AGE),
            Settle::Changing {
                age: Duration::ZERO
            }
        );
    }

    #[test]
    fn settle_of_a_fresh_file_is_changing_and_settles_three_seconds_later() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("claude");
        std::fs::write(&file, b"image").unwrap();
        // Judged against the file's OWN change time, not the wall clock, so a
        // saturated box cannot age it past the minimum before the first probe.
        let now = changed_at(&file).expect("a readable change time") + Duration::from_millis(500);
        assert!(
            matches!(settle_of(&file, now), Settle::Changing { .. }),
            "a file written just now: {:?}",
            settle_of(&file, now)
        );
        assert_eq!(
            settle_of(&file, now + Duration::from_secs(3)),
            Settle::Settled
        );
    }

    #[test]
    fn settle_of_a_missing_path_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            settle_of(&tmp.path().join("absent"), SystemTime::now()),
            Settle::Unknown
        );
    }

    /// The target's time is read, not the link's: a link made long before a
    /// rewrite of its target must read as changing. The `now` is chosen so the
    /// two disagree — the link's own time is settled at it, the target's is
    /// not — so a probe that read the link would fail the first assert.
    #[cfg(unix)]
    #[test]
    fn settle_of_a_symlink_reads_its_target() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("claude.exe");
        let link = tmp.path().join("claude");
        std::fs::write(&target, b"old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let link_meta = std::fs::symlink_metadata(&link).unwrap();
        let link_changed = SystemTime::UNIX_EPOCH
            + Duration::new(
                u64::try_from(link_meta.ctime()).unwrap(),
                u32::try_from(link_meta.ctime_nsec()).unwrap(),
            );
        std::thread::sleep(Duration::from_millis(1_200));
        let fresh = tmp.path().join("claude.exe.new");
        std::fs::write(&fresh, b"new").unwrap();
        std::fs::rename(&fresh, &target).unwrap();
        let target_changed = changed_at(&target).unwrap();
        assert!(
            target_changed >= link_changed + Duration::from_secs(1),
            "the rewrite must land at least 1 s after the link"
        );
        let now = target_changed + Duration::from_secs(1);
        assert_eq!(
            judge(link_changed, now, SETTLE_MIN_AGE),
            Settle::Settled,
            "control: the link's own time is settled at this now"
        );
        assert_eq!(
            settle_of(&link, now),
            Settle::Changing {
                age: Duration::from_secs(1)
            },
            "settle_of must follow the link to its rewritten target"
        );
    }
}
