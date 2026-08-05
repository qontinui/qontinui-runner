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
//! [`ScanGate`] remembers each terminal's `grid_generation` counter and
//! reports whether it has advanced, so an idle session costs one atomic load
//! per tick instead of a grid lock plus a full screen render plus a
//! regex/substring pass.
//!
//! ## Why this is semantics-preserving
//!
//! The gate counter is [`TerminalSession::grid_generation`][gg], bumped by the
//! reader thread *after* every `parser.advance` (and by `resize`, which
//! rewrites rendered text with no bytes involved) — see `session::advance_grid`.
//!
//! The property the gate needs is NOT "if it has not moved, no byte reached the
//! parser". It is the converse: **if it has not moved since a scanner's last
//! observation, every byte counted at that observation had already reached the
//! parser** — so the screen that scanner rendered was the final one, and there
//! is nothing left to see. That is exactly what a counter bumped *after* the
//! grid mutation gives, and exactly what a counter bumped *before* it does not:
//! `total_bytes_produced` moves in `session::tee_into_scrollback`, several
//! statements before the grid lock is even taken, so a scanner tick landing in
//! that window would store a watermark for bytes the grid had not yet drawn,
//! render the pre-advance screen, and then — once the terminal blocked on input
//! and the byte counter stopped moving — skip that screen forever. The affected
//! event class is "the last chunk before idle", which is precisely when a
//! prompt appears.
//!
//! Given a counter with that ordering, the per-scanner reasoning holds:
//!
//! - `auto_response` is edge-triggered (`collect_rising_edges`): identical text
//!   produces identical per-rule match flags, hence zero rising edges.
//! - `context_watcher` fires at most once per session and re-evaluates a pure
//!   predicate over the same text.
//! - `usage_limit` is debounce-triggered; skipping means a limit message that
//!   is *still frozen on screen* no longer re-fires the hint every 300 s. That
//!   is the intended reading of the debounce (one hint per appearance), and a
//!   genuinely new appearance is always followed by a bump of the counter that
//!   painted it.
//!
//! Reading the counter *before* rendering is safe in both directions: a stale
//! read (the bump lands after the load) costs one deferred scan on the next
//! tick, and a fresh read guarantees the render that follows already includes
//! the counted mutation, because the bump happens after the grid lock is
//! released.
//!
//! A terminal the gate has never seen is ALWAYS scanned (its first screen may
//! already carry text — e.g. a resumed session), and the gate is pruned to the
//! live set each tick so it cannot outgrow the session table.
//!
//! [gg]: super::session::TerminalSession::grid_generation

use std::collections::{HashMap, HashSet};

/// Per-terminal `grid_generation` watermarks for one scanner.
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

    /// Should `terminal_id` be scanned this tick, given its current
    /// grid-generation counter? Records `total` as the new watermark whenever
    /// it returns `true`, so the next tick compares against what was actually
    /// scanned.
    ///
    /// The caller MUST pass a counter that is bumped only after the grid
    /// mutation is visible (`TerminalSession::grid_generation`) — see the
    /// module docs for what a pre-mutation counter breaks.
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
