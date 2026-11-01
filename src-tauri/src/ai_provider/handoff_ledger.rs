//! The in-process handoff ledger: every account handoff of the last
//! [`HANDOFF_WINDOW`], with the load each one charged its target.
//!
//! Two readers use it:
//!
//! - `AccountSelectionMode::AlternatingHandoff` continues its 1,2,1,3,...
//!   sequence from the targets in the window
//!   ([`super::account_usage::pick_migration_target`]).
//! - Every pace reader adds an account's decayed [`HandoffLedger::charge_sum`]
//!   to its probed utilization (`effective_utilization`, see
//!   [`super::config::under_pace_shortfall`]). A session moved onto an account
//!   burns its load over hours, and the usage probe shows almost none of it a
//!   minute later; without the charge every handoff in a burst ranks the same
//!   snapshot and lands on the same account.
//!
//! **The charge lives here, not on the usage sample.** Every usage-limit hint
//! re-records the 60 s-cached probe bodies into the selection snapshot before
//! it picks (`commands::ai_settings::record_usage_snapshot`), and that
//! replaces the whole sample — a charge stored on the sample would be wiped
//! once per hint, which is exactly the burst it exists for. A fresh probe does
//! not clear a charge either: each one decays linearly to zero over
//! [`HANDOFF_WINDOW`], standing in for the probe gradually absorbing the real
//! burn.
//!
//! **Lock order.** [`lock`] is taken BEFORE the usage snapshot
//! (`config::USAGE_SNAPSHOT`) is read, and `pick_migration_target` holds it
//! across those reads so a burst of concurrent handoffs each sees the ones
//! before it. `std::sync::Mutex` is not reentrant, so the pace readers take
//! the charge as an argument and never lock the ledger themselves.
//!
//! The ledger lives in this process, so a runner restart starts it empty.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How far back the ledger remembers a handoff, and the time over which a
/// handoff's load charge decays to zero.
pub const HANDOFF_WINDOW: Duration = Duration::from_secs(3 * 3600);

/// One handoff: `(when, target config dir, load charge)`. The charge is in
/// utilization units (0.0–1.0) as it stood when the handoff was recorded; it is
/// read back decayed ([`HandoffLedger::charge_sum`]).
pub type Handoff = (Instant, String, f64);

/// Handoffs, oldest first, pruned to [`HANDOFF_WINDOW`] on every access path
/// that mutates or ranks. `Instant` is monotonic, so a wall-clock step neither
/// drops nor keeps entries wrongly.
#[derive(Debug, Default)]
pub struct HandoffLedger {
    entries: VecDeque<Handoff>,
}

static HANDOFFS: Mutex<HandoffLedger> = Mutex::new(HandoffLedger::new());

impl HandoffLedger {
    pub const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    /// Drop every entry older than [`HANDOFF_WINDOW`] at `now`.
    pub fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|(at, _, _)| now.saturating_duration_since(*at) > HANDOFF_WINDOW)
        {
            self.entries.pop_front();
        }
    }

    /// Record a handoff to `config_dir` at `now`, charging it `charge`.
    pub fn push(&mut self, now: Instant, config_dir: &str, charge: f64) {
        self.entries
            .push_back((now, config_dir.to_string(), charge.max(0.0)));
    }

    /// Remove the most recent handoff to `config_dir`, and its charge with it.
    /// `false` when there was none.
    pub fn unrecord(&mut self, config_dir: &str) -> bool {
        match self.entries.iter().rposition(|(_, d, _)| d == config_dir) {
            Some(i) => {
                self.entries.remove(i);
                true
            }
            None => false,
        }
    }

    /// The handoff targets in the ledger, oldest first.
    pub fn targets(&self) -> impl DoubleEndedIterator<Item = &str> + Clone + '_ {
        self.entries.iter().map(|(_, d, _)| d.as_str())
    }

    /// The load still charged to `config_dir` at `now`: each of its handoffs'
    /// charge, decayed linearly from full at the handoff to zero at
    /// [`HANDOFF_WINDOW`] after it.
    pub fn charge_sum(&self, config_dir: &str, now: Instant) -> f64 {
        self.entries
            .iter()
            .filter(|(_, d, _)| d == config_dir)
            .map(|(at, _, charge)| decayed(*charge, now.saturating_duration_since(*at)))
            .sum()
    }

    /// [`Self::charge_sum`] for every account with a handoff in the ledger.
    /// An account absent from the map carries no charge.
    pub fn charge_sums(&self, now: Instant) -> HashMap<String, f64> {
        let mut sums: HashMap<String, f64> = HashMap::new();
        for (at, dir, charge) in &self.entries {
            *sums.entry(dir.clone()).or_insert(0.0) +=
                decayed(*charge, now.saturating_duration_since(*at));
        }
        sums
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// `charge` decayed over `age`: linear from `charge` at age zero to `0.0` at
/// [`HANDOFF_WINDOW`] and beyond.
fn decayed(charge: f64, age: Duration) -> f64 {
    if age >= HANDOFF_WINDOW {
        return 0.0;
    }
    charge * (1.0 - age.as_secs_f64() / HANDOFF_WINDOW.as_secs_f64())
}

/// The process ledger, poison-tolerant. Take it BEFORE reading the usage
/// snapshot, never while holding it (module doc, "Lock order").
pub fn lock() -> MutexGuard<'static, HandoffLedger> {
    HANDOFFS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Every account's decayed charge right now, read under a short-lived lock
/// that is released before the caller reads the usage snapshot. For readers
/// that rank but do not record (spawn-time `pick_best_account`).
pub fn charge_sums_now() -> HashMap<String, f64> {
    let mut ledger = lock();
    let now = Instant::now();
    ledger.prune(now);
    ledger.charge_sums(now)
}

/// Record a handoff whose target the caller chose itself (an explicit
/// migration target), so `AlternatingHandoff` sees it and the target carries
/// the effective `HANDOFF_LOAD_ESTIMATE` charge like any picked handoff.
pub fn record_handoff(config_dir: &str) {
    // Settings are read before the ledger lock so nothing nests inside it.
    let charge = super::config::effective_handoff_load_estimate();
    let mut ledger = lock();
    let now = Instant::now();
    ledger.prune(now);
    ledger.push(now, config_dir, charge);
}

/// Withdraw the most recent handoff to `config_dir`, and its charge: the
/// migration it was recorded for was refused or failed, so the session never
/// reached that account — neither the alternation nor the pace readers may
/// count it.
pub fn unrecord_handoff(config_dir: &str) {
    lock().unrecord(config_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-12;

    #[test]
    fn prune_drops_entries_older_than_the_window() {
        let t0 = Instant::now();
        let mut ledger = HandoffLedger::new();
        ledger.push(t0, "old", 0.02);
        ledger.push(t0 + Duration::from_secs(10), "edge", 0.02);
        ledger.push(t0 + Duration::from_secs(20), "new", 0.02);
        ledger.prune(t0 + HANDOFF_WINDOW + Duration::from_secs(5));
        assert_eq!(ledger.targets().collect::<Vec<_>>(), vec!["edge", "new"]);
    }

    #[test]
    fn charge_decays_linearly_to_zero_at_the_window() {
        let t0 = Instant::now();
        let mut ledger = HandoffLedger::new();
        ledger.push(t0, "a", 0.02);
        assert!((ledger.charge_sum("a", t0) - 0.02).abs() < EPS);
        let half = t0 + HANDOFF_WINDOW / 2;
        assert!((ledger.charge_sum("a", half) - 0.01).abs() < EPS);
        assert_eq!(ledger.charge_sum("a", t0 + HANDOFF_WINDOW), 0.0);
        assert_eq!(
            ledger.charge_sum("a", t0 + HANDOFF_WINDOW + Duration::from_secs(1)),
            0.0
        );
        assert_eq!(ledger.charge_sum("other", t0), 0.0);
    }

    #[test]
    fn charges_to_one_account_add_and_others_are_separate() {
        let t0 = Instant::now();
        let mut ledger = HandoffLedger::new();
        ledger.push(t0, "a", 0.02);
        ledger.push(t0, "b", 0.03);
        ledger.push(t0, "a", 0.02);
        assert!((ledger.charge_sum("a", t0) - 0.04).abs() < EPS);
        let sums = ledger.charge_sums(t0);
        assert!((sums["a"] - 0.04).abs() < EPS);
        assert!((sums["b"] - 0.03).abs() < EPS);
        assert_eq!(sums.get("c"), None);
    }

    #[test]
    fn unrecord_removes_exactly_one_entry_and_its_charge() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(60);
        let mut ledger = HandoffLedger::new();
        ledger.push(t0, "a", 0.02);
        ledger.push(t0, "b", 0.02);
        ledger.push(t1, "a", 0.05);
        assert!(ledger.unrecord("a"));
        assert_eq!(ledger.len(), 2);
        // The MOST RECENT entry went (the 0.05 one); the earlier 0.02 stays.
        assert!((ledger.charge_sum("a", t1) - decayed(0.02, Duration::from_secs(60))).abs() < EPS);
        assert_eq!(ledger.targets().collect::<Vec<_>>(), vec!["a", "b"]);
        assert!(ledger.unrecord("a"));
        assert_eq!(ledger.charge_sum("a", t1), 0.0);
        assert!(!ledger.unrecord("a"), "nothing left to withdraw");
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn a_negative_charge_is_stored_as_zero() {
        let t0 = Instant::now();
        let mut ledger = HandoffLedger::new();
        ledger.push(t0, "a", -0.5);
        assert_eq!(ledger.charge_sum("a", t0), 0.0);
    }
}
