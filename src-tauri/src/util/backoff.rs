//! Pure arithmetic for capped exponential backoff.
//!
//! No jitter, no retry loop, no error classification: those differ per call
//! site and stay local. A call site keeps its own state machine (for example
//! `agent_token`'s terminal `rejected` latch) and calls these for the delay.
//! Plan `2026-10-04-runner-time-backoff-and-http-client-helpers-are-re-rolled-per-module` D3.
//!
//! LIB-only (declared in `lib.rs`'s inline `pub mod util`); the bin re-exports it.

use std::time::Duration;

/// Delay after `consecutive_failures` failures in a row: zero for none, else
/// `base * 2^(n-1)`, saturating, clamped to `cap`.
///
/// Exactly the arithmetic `agent_pusher::backoff_delay_secs` always had.
pub fn capped_doubling(base: Duration, consecutive_failures: u32, cap: Duration) -> Duration {
    if consecutive_failures == 0 {
        return Duration::ZERO;
    }
    let shift = consecutive_failures - 1;
    // 2^shift overflows u32 at 32; any such factor is already far beyond a cap.
    let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    base.saturating_mul(factor).min(cap)
}

/// The next delay of an incremental ladder: `cur * 2`, saturating, clamped to `cap`.
pub fn next_doubled(cur: Duration, cap: Duration) -> Duration {
    cur.saturating_mul(2).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn zero_failures_is_zero() {
        assert_eq!(
            capped_doubling(secs(5 * MIN), 0, secs(60 * MIN)),
            Duration::ZERO
        );
    }

    #[test]
    fn agent_pusher_ladder_5m_10m_20m_40m_60m_60m() {
        let got: Vec<u64> = (1..=6)
            .map(|n| capped_doubling(secs(5 * MIN), n, secs(60 * MIN)).as_secs() / MIN)
            .collect();
        assert_eq!(got, vec![5, 10, 20, 40, 60, 60]);
    }

    #[test]
    fn huge_failure_counts_saturate_to_cap() {
        for n in [31, 32, 33, 64, 1000, u32::MAX] {
            assert_eq!(capped_doubling(secs(5), n, secs(900)), secs(900));
        }
        // A zero base stays zero however many failures there were.
        assert_eq!(
            capped_doubling(Duration::ZERO, u32::MAX, secs(900)),
            Duration::ZERO
        );
    }

    #[test]
    fn cap_below_base_clamps_the_first_failure() {
        assert_eq!(capped_doubling(secs(100), 1, secs(30)), secs(30));
    }

    #[test]
    fn next_doubled_ladder_clamps() {
        let cap = Duration::from_millis(500);
        let mut cur = Duration::from_millis(50);
        let mut got = vec![];
        for _ in 0..6 {
            cur = next_doubled(cur, cap);
            got.push(cur.as_millis());
        }
        assert_eq!(got, vec![100, 200, 400, 500, 500, 500]);
        assert_eq!(next_doubled(Duration::MAX, secs(1)), secs(1));
    }
}
