//! The one home for wall-clock reads (Unix epoch milliseconds / seconds).
//!
//! The runner used to define `now_ms` privately in ~40 modules; plan
//! `2026-10-04-runner-time-backoff-and-http-client-helpers-are-re-rolled-per-module`
//! collapses them here. `scripts/check_helper_redefinition.py` fails on a new
//! `duration_since(..UNIX_EPOCH)` anywhere else.
//!
//! Every function returns `0` when the system clock is set before 1970 (never a
//! negative value and never a panic), so a caller never has to handle the error
//! arm of `SystemTime::duration_since`.
//!
//! LIB-only (declared in `lib.rs`'s inline `pub mod util`); the bin re-exports it
//! from its own `util/mod.rs`, so both crates spell `crate::util::time`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the epoch for `t`, `0` when `t` is before it.
fn epoch_ms_of(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Seconds since the epoch for `t`, `0` when `t` is before it.
fn epoch_secs_of(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Current Unix time in milliseconds; `0` when the clock is before 1970.
pub fn now_ms() -> u64 {
    epoch_ms_of(SystemTime::now())
}

/// Current Unix time in milliseconds as `i64` (for database bind sites and
/// signed protocol fields); `0` when the clock is before 1970.
pub fn now_ms_i64() -> i64 {
    now_ms() as i64
}

/// Current Unix time in whole seconds; `0` when the clock is before 1970.
pub fn now_secs() -> u64 {
    epoch_secs_of(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn now_ms_is_monotonic_and_close_to_system_time() {
        let a = now_ms();
        let b = now_ms();
        let sys = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test host clock is after 1970")
            .as_millis() as u64;
        assert!(b >= a);
        assert!(sys >= a && sys - a < 1_000, "now_ms {a} vs system {sys}");
        assert!((now_ms_i64() - sys as i64).abs() < 1_000);
        assert!(now_secs().abs_diff(sys / 1000) <= 1);
    }

    #[test]
    fn pre_epoch_clock_reads_as_zero() {
        let before = UNIX_EPOCH - Duration::from_secs(5);
        assert_eq!(epoch_ms_of(before), 0);
        assert_eq!(epoch_secs_of(before), 0);
    }

    #[test]
    fn post_epoch_values_are_exact() {
        let t = UNIX_EPOCH + Duration::from_millis(1_234_567);
        assert_eq!(epoch_ms_of(t), 1_234_567);
        assert_eq!(epoch_secs_of(t), 1_234);
    }
}
