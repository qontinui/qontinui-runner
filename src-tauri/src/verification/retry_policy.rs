//! Retry policy for WSM refusal verdicts.
//!
//! When the World State Verifier judges a worker action to be a no-op
//! (Refused), the agentic loop should retry with a different candidate
//! rather than advance. This tracker caps consecutive refusals and
//! applies exponential backoff to prevent infinite no-op loops — a
//! failure mode called out in the SEAgent / OSWorld-G literature.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry the worker step after waiting `wait_ms`. Do not advance
    /// `consecutive_passes`; inject "avoid repeating refused action"
    /// guidance into the next worker prompt.
    Retry { wait_ms: u64 },
    /// Refusal cap reached. Escalate: convert the verdict to Fail with
    /// an observation explaining the cap, and let the outer loop decide
    /// whether to continue or give up.
    EscalateToFail,
}

#[derive(Debug, Clone)]
pub struct RefusalRetryTracker {
    consecutive: u32,
    cap: u32,
    backoff_ms: Vec<u64>,
}

impl Default for RefusalRetryTracker {
    fn default() -> Self {
        Self::new(3, vec![500, 1000, 2000])
    }
}

impl RefusalRetryTracker {
    pub fn new(cap: u32, backoff_ms: Vec<u64>) -> Self {
        assert!(cap > 0, "cap must be positive");
        assert!(!backoff_ms.is_empty(), "backoff_ms must be non-empty");
        Self {
            consecutive: 0,
            cap,
            backoff_ms,
        }
    }

    pub fn consecutive(&self) -> u32 {
        self.consecutive
    }

    pub fn cap(&self) -> u32 {
        self.cap
    }

    /// Record a refusal and return the retry decision.
    pub fn record_refusal(&mut self) -> RetryDecision {
        self.consecutive += 1;
        if self.consecutive >= self.cap {
            return RetryDecision::EscalateToFail;
        }
        // Index into the backoff schedule, clamped to the last entry.
        // `consecutive` is 1-based after increment, so the first refusal
        // hits backoff_ms[0].
        let idx = (self.consecutive as usize - 1).min(self.backoff_ms.len() - 1);
        RetryDecision::Retry {
            wait_ms: self.backoff_ms[idx],
        }
    }

    /// Reset the consecutive counter. Call on any non-refusal verdict.
    pub fn record_non_refusal(&mut self) {
        self.consecutive = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_refusal_retries_with_first_backoff() {
        let mut t = RefusalRetryTracker::new(3, vec![500, 1000, 2000]);
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 500 });
        assert_eq!(t.consecutive(), 1);
    }

    #[test]
    fn second_refusal_escalates_backoff() {
        let mut t = RefusalRetryTracker::new(3, vec![500, 1000, 2000]);
        t.record_refusal();
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 1000 });
    }

    #[test]
    fn hitting_cap_escalates_to_fail() {
        let mut t = RefusalRetryTracker::new(3, vec![500, 1000, 2000]);
        t.record_refusal();
        t.record_refusal();
        // Third refusal hits the cap.
        assert_eq!(t.record_refusal(), RetryDecision::EscalateToFail);
    }

    #[test]
    fn non_refusal_resets_counter() {
        let mut t = RefusalRetryTracker::new(3, vec![500, 1000, 2000]);
        t.record_refusal();
        t.record_refusal();
        t.record_non_refusal();
        assert_eq!(t.consecutive(), 0);
        // Starts from the first backoff again.
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 500 });
    }

    #[test]
    fn backoff_clamps_when_consecutive_exceeds_schedule() {
        let mut t = RefusalRetryTracker::new(10, vec![100, 200]);
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 100 });
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 200 });
        // 3rd, 4th ... clamp to last backoff entry.
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 200 });
        assert_eq!(t.record_refusal(), RetryDecision::Retry { wait_ms: 200 });
    }

    #[test]
    fn default_tracker_has_cap_3() {
        let t = RefusalRetryTracker::default();
        assert_eq!(t.cap(), 3);
    }

    #[test]
    #[should_panic(expected = "cap must be positive")]
    fn zero_cap_rejected() {
        let _ = RefusalRetryTracker::new(0, vec![500]);
    }

    #[test]
    #[should_panic(expected = "backoff_ms must be non-empty")]
    fn empty_backoff_rejected() {
        let _ = RefusalRetryTracker::new(3, vec![]);
    }
}
