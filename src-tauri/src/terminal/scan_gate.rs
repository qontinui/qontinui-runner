//! Change detection for the periodic full-fleet grid scanners.
//!
//! Three independent loops sweep every live terminal on a ~1.5 s cadence
//! ([`super::auto_response::scan_grids_once`],
//! [`super::usage_limit::scan_grids_once`],
//! [`super::context_watcher::scan_terminals_once`]). Each one, per session,
//! takes that session's grid lock and materializes a full `rows`-line
//! `text_snapshot()` `String` before scanning it — contending with the reader
//! thread, which already takes the grid lock three to four times per chunk.
//!
//! With many sessions open, the overwhelming majority are idle: their rendered
//! screen is byte-for-byte what the previous tick already scanned. A
//! [`ScanGate`] remembers each terminal's `total_bytes_produced` counter and
//! reports whether it has advanced, so an idle session costs one relaxed
//! atomic load per tick instead of a grid lock plus a full screen render plus a
//! regex/substring pass.
//!
//! ## Why this is semantics-preserving
//!
//! The counter is bumped by the reader thread for every byte fed to the VT
//! parser (`session::tee_into_scrollback`). If it has not moved, no byte
//! reached the parser, so the grid — and therefore the rendered text every
//! scanner reads — is identical to what that scanner already evaluated:
//!
//! - `auto_response` is edge-triggered (`collect_rising_edges`): identical text
//!   produces identical per-rule match flags, hence zero rising edges.
//! - `context_watcher` fires at most once per session and re-evaluates a pure
//!   predicate over the same text.
//! - `usage_limit` is debounce-triggered; skipping means a limit message that
//!   is *still frozen on screen* no longer re-fires the hint every 300 s. That
//!   is the intended reading of the debounce (one hint per appearance), and a
//!   genuinely new appearance always advances the counter that painted it.
//!
//! A terminal the gate has never seen is ALWAYS scanned (its first screen may
//! already carry text — e.g. a resumed session), and the gate is pruned to the
//! live set each tick so it cannot outgrow the session table.

use std::collections::{HashMap, HashSet};

/// Per-terminal `total_bytes_produced` watermarks for one scanner.
///
/// Each scanner owns its own instance: they run on different ticks and one
/// scanner observing a change must not consume it on another's behalf.
#[derive(Default)]
pub(super) struct ScanGate {
    seen: HashMap<String, u64>,
}

impl ScanGate {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Should `terminal_id` be scanned this tick, given its current byte
    /// counter? Records `total` as the new watermark whenever it returns
    /// `true`, so the next tick compares against what was actually scanned.
    pub(super) fn should_scan(&mut self, terminal_id: &str, total: u64) -> bool {
        match self.seen.get_mut(terminal_id) {
            Some(prev) if *prev == total => false,
            Some(prev) => {
                *prev = total;
                true
            }
            None => {
                self.seen.insert(terminal_id.to_string(), total);
                true
            }
        }
    }

    /// Drop watermarks for terminals that have gone away.
    pub(super) fn retain_live(&mut self, live: &HashSet<&String>) {
        self.seen.retain(|tid, _| live.contains(tid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sighting_always_scans() {
        let mut gate = ScanGate::new();
        assert!(gate.should_scan("t1", 0));
        assert!(gate.should_scan("t2", 4096));
    }

    #[test]
    fn unchanged_counter_skips() {
        let mut gate = ScanGate::new();
        assert!(gate.should_scan("t1", 100));
        assert!(!gate.should_scan("t1", 100));
        assert!(!gate.should_scan("t1", 100));
    }

    #[test]
    fn advanced_counter_scans_again() {
        let mut gate = ScanGate::new();
        assert!(gate.should_scan("t1", 100));
        assert!(!gate.should_scan("t1", 100));
        assert!(gate.should_scan("t1", 101));
        assert!(!gate.should_scan("t1", 101));
    }

    #[test]
    fn terminals_are_tracked_independently() {
        let mut gate = ScanGate::new();
        assert!(gate.should_scan("a", 10));
        assert!(gate.should_scan("b", 10));
        assert!(!gate.should_scan("a", 10));
        assert!(gate.should_scan("b", 20));
        assert!(!gate.should_scan("a", 10));
    }

    #[test]
    fn retain_live_drops_dead_terminals_and_rearms_them() {
        let mut gate = ScanGate::new();
        assert!(gate.should_scan("a", 10));
        assert!(gate.should_scan("b", 10));

        let b = "b".to_string();
        let live: HashSet<&String> = [&b].into_iter().collect();
        gate.retain_live(&live);

        // "b" kept its watermark; "a" was dropped, so a terminal reusing that
        // id is treated as brand new and scanned.
        assert!(!gate.should_scan("b", 10));
        assert!(gate.should_scan("a", 10));
    }
}
