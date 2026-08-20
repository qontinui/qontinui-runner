//! Visibility-tiered webview emission — the backend half of root cause A4
//! (plan `2026-07-28-runner-many-sessions-performance` Phase 5).
//!
//! Phase 2 made the FRONTEND stop paying for panes the operator cannot see: a
//! hidden pane drops its xterm writes, its ack timer and its `ResizeObserver`.
//! But the runner still emitted every chunk of every session to the webview,
//! and every window still deserialized it. With 40 streaming sessions and one
//! focused pane, 39 sessions' worth of IPC still crossed the bridge to be
//! thrown away.
//!
//! This module carries the missing half: each session has a **visibility
//! tier**, reported by the frontend through `terminal_set_visibility`, and the
//! `terminal-output` webview leg is served according to it.
//!
//! | tier         | webview `terminal-output`                  | state tracking fed by |
//! |--------------|--------------------------------------------|-----------------------|
//! | `focused`    | every coalesced frame (unchanged)          | the pane + page tap   |
//! | `background` | coalesced to ≥ the background flush interval | the page tap |
//! | `unwatched`  | nothing at all, or coalesced to ≥ the unwatched flush interval | [`ACTIVITY_EVENT`], or the page tap |
//!
//! **Only the webview leg is tiered.** The SSE broadcast
//! (`GET /terminals/{id}/stream`), the WS relay used by the mobile client and
//! the coord output pipe all ride the same per-chunk call and receive every
//! chunk, in order, byte-identical, in every tier. External consumers keep
//! their contract — see `admit_to_webview` in [`super::session`], which returns
//! a webview-only verdict and is consulted only after the mirror legs have been
//! dispatched.
//!
//! The scrollback ring is likewise untiered: `tee_into_scrollback` runs on the
//! read, upstream of emission, so an `unwatched` session accumulates exactly
//! as much history as a focused one. That is what makes the tier reversible —
//! the frontend replays the ring (`terminal_get_scrollback`, which resets flow
//! control) when a pane mounts or is revealed, so an upgrade is gap-free.
//!
//! ## Why a sweeper task and not the reader thread alone
//!
//! Both deferred jobs this module owns — flushing a held `background` window
//! and emitting the ≤1 Hz `unwatched` digest — must fire when a session goes
//! QUIET, which is exactly when the reader thread is parked in a blocking
//! `read()` on the PTY. A reader-thread-only implementation can only act on
//! the next byte, so the tail of a burst (the part carrying the prompt that
//! moves a state chip) would sit undelivered until the session spoke again —
//! possibly never. One process-wide sweeper at [`SWEEP_TICK`] closes both,
//! costing a handful of relaxed atomic loads per session per tick and doing
//! real work only for sessions that have something outstanding.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Tauri event name for the lightweight activity digest that feeds session
/// state tracking for `unwatched` sessions. Payload shape:
/// `{ terminalId, totalBytesProduced, bytesDelta, lines }`.
pub const ACTIVITY_EVENT: &str = "terminal-activity";

/// DEFAULT minimum spacing between webview `terminal-output` events for a
/// `background` session. Chunks that arrive inside the window accumulate and
/// leave as ONE event, so a hidden pane costs at most 4 IPC dispatches per
/// second no matter how fast its PTY talks.
///
/// This is the **default**, not the value in force: the spacing actually used
/// is [`crate::settings::PerformanceSettings::background_flush_interval`],
/// resolved once per session at spawn and passed into
/// [`BackgroundHold::take_if_due`]. This constant is what
/// `default_background_flush_interval_ms` returns, so "no `performance` key in
/// settings.json" and "the constant the tier always used" cannot drift apart.
pub const BACKGROUND_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// Byte cap for a held `background` window: flush early rather than ship one
/// enormous event. Backpressure still applies on top of this — the emission
/// gate pauses the tier once the unacked gap crosses its high watermark.
pub const BACKGROUND_HOLD_BYTE_CAP: usize = 256 * 1024;

/// Minimum spacing between [`ACTIVITY_EVENT`] digests for one session — the
/// "≤1 Hz per session" budget.
pub const ACTIVITY_DIGEST_INTERVAL: Duration = Duration::from_millis(1000);

/// How many trailing non-empty rendered lines a digest carries. Matches the
/// frontend's `lastOutputLines` window (`hotStore.setTabOutputLines`, 20), so
/// a digest-fed tab and a tap-fed tab publish the same shape.
pub const ACTIVITY_DIGEST_LINES: usize = 20;

/// Sweeper cadence. Set by the tighter of the two deadlines it enforces
/// ([`BACKGROUND_FLUSH_INTERVAL`]); the digest is spaced by its own
/// per-session elapsed check, so a faster tick never emits digests faster
/// than [`ACTIVITY_DIGEST_INTERVAL`].
///
/// Deliberately NOT tied to the operator's configured flush interval. It is
/// the resolution of the *quiet-tail* path only — the reader thread flushes a
/// due window on the very next chunk, so a configured interval below this tick
/// is served at full precision while a session is talking, and rounded up to
/// the tick only for the tail of a burst. Driving the sweep from a per-session
/// setting would mean one timer per session, which is the cost this single
/// process-wide sweeper exists to avoid.
const SWEEP_TICK: Duration = Duration::from_millis(250);

/// How much of a session's per-chunk service the operator is actually
/// consuming, as reported by the frontend.
///
/// Ordered by how much is watched: [`Self::Focused`] is the most visible and
/// wins every merge (see [`VisibilityState::report`]). The default for a
/// session no window has ever reported is `Focused` — a frontend that never
/// calls `terminal_set_visibility` (an older webview bundle, or the window
/// between spawn and the first reconcile) gets today's behavior exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VisibilityTier {
    /// A pane is mounted and visible: emit every coalesced frame.
    Focused,
    /// A pane is mounted but hidden (parked, compacted, non-maximized zone):
    /// nothing renders it, but the page-level output tap still feeds state
    /// tracking from it, so emission continues — coalesced to the configured
    /// background flush interval (default [`BACKGROUND_FLUSH_INTERVAL`]).
    Background,
    /// No pane is mounted for this terminal in any window. By default the
    /// webview leg is off entirely and state tracking is fed by
    /// [`ACTIVITY_EVENT`]; a positive
    /// `PerformanceSettings::unwatched_flush_interval_ms` instead coalesces to
    /// that cadence, and the page tap feeds tracking as it does for
    /// `background` (the digest stands down — see
    /// `TerminalSession::emit_activity_digest_if_due`).
    Unwatched,
}

impl VisibilityTier {
    /// Wire encoding, matching the frontend's `VisibilityTier` union.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Focused => "focused",
            Self::Background => "background",
            Self::Unwatched => "unwatched",
        }
    }

    /// Parse the wire encoding. Unknown values are rejected rather than
    /// silently defaulted: a typo that quietly pinned every session to
    /// `focused` would look exactly like the feature not working.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "focused" => Some(Self::Focused),
            "background" => Some(Self::Background),
            "unwatched" => Some(Self::Unwatched),
            _ => None,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Focused => 0,
            Self::Background => 1,
            Self::Unwatched => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Background,
            2 => Self::Unwatched,
            _ => Self::Focused,
        }
    }
}

/// One session's visibility, merged across every webview window that reports
/// on it.
///
/// Per-window, because a terminal popped out into a `term-N` window is a
/// different pane in a different JS context from the docked one, and each
/// window can only speak for itself. A single shared slot would let the main
/// window's "nobody here is showing it" overwrite the pop-out's "it is on
/// screen" and go dark on a pane the operator is watching. The merge is
/// "most visible wins", so a stale report can only ever cost service, never
/// remove it.
pub struct VisibilityState {
    /// Merged tier, read by the reader thread on every chunk. A relaxed load
    /// of one `AtomicU8` — the tier is a service level, not a synchronization
    /// point, and a chunk served under the previous tier is harmless (the
    /// frontend resyncs from the ring on upgrade regardless).
    effective: AtomicU8,
    /// window label → that window's report. Touched only on layout changes.
    per_window: Mutex<HashMap<String, VisibilityTier>>,
}

impl Default for VisibilityState {
    fn default() -> Self {
        Self::new()
    }
}

impl VisibilityState {
    pub fn new() -> Self {
        Self {
            effective: AtomicU8::new(VisibilityTier::Focused.as_u8()),
            per_window: Mutex::new(HashMap::new()),
        }
    }

    /// The merged tier. Lock-free.
    pub fn tier(&self) -> VisibilityTier {
        VisibilityTier::from_u8(self.effective.load(Ordering::Relaxed))
    }

    /// Record `tier` as `window`'s view of this session and return the new
    /// merged tier.
    pub fn report(&self, window: &str, tier: VisibilityTier) -> VisibilityTier {
        let mut map = self.per_window.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(window.to_string(), tier);
        self.recompute(&map)
    }

    /// Drop reports from windows that no longer exist, returning the new
    /// merged tier. A closed pop-out that last said `focused` would otherwise
    /// pin its terminals at full rate forever.
    pub fn retain_windows(&self, live: &BTreeSet<String>) -> VisibilityTier {
        let mut map = self.per_window.lock().unwrap_or_else(|e| e.into_inner());
        let before = map.len();
        map.retain(|label, _| live.contains(label));
        if map.len() == before {
            return self.tier();
        }
        self.recompute(&map)
    }

    fn recompute(&self, map: &HashMap<String, VisibilityTier>) -> VisibilityTier {
        // Empty map = nobody has ever reported = today's behavior.
        let merged = map
            .values()
            .copied()
            .min()
            .unwrap_or(VisibilityTier::Focused);
        self.effective.store(merged.as_u8(), Ordering::Relaxed);
        merged
    }

    /// Test/diagnostic visibility into the per-window reports.
    #[cfg(test)]
    pub fn reporting_windows(&self) -> Vec<String> {
        let map = self.per_window.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<String> = map.keys().cloned().collect();
        v.sort();
        v
    }
}

/// The accumulated webview window of a `background` session.
///
/// A background session's chunks are appended here instead of being emitted,
/// and leave as ONE `terminal-output` event once the window is due. Both the
/// reader thread (on the next chunk) and the sweeper (when the session went
/// quiet mid-window) drain it through the same mutex, which is also what keeps
/// this session's webview events in stream order.
///
/// Pure and clock-injected so the cadence is unit-testable without a PTY.
pub struct BackgroundHold {
    pending: Vec<u8>,
    /// Absolute stream offset of the FIRST held byte — the replay boundary the
    /// frontend dedups a scrollback fetch against, exactly as for a coalesced
    /// sync frame.
    pending_offset: u64,
    last_flush: Instant,
}

impl BackgroundHold {
    pub fn new(now: Instant) -> Self {
        Self {
            pending: Vec::new(),
            pending_offset: 0,
            last_flush: now,
        }
    }

    /// Accumulate one chunk into the open window.
    pub fn push(&mut self, payload: &[u8], offset: u64) {
        if self.pending.is_empty() {
            self.pending_offset = offset;
        }
        self.pending.extend_from_slice(payload);
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Whether the held window should leave now: `interval` elapsed since the
    /// last flush, or the byte cap tripped (flush early rather than ship one
    /// huge event).
    ///
    /// The spacing is a PARAMETER rather than a constant because it is
    /// operator-tunable per tier
    /// ([`crate::settings::PerformanceSettings::background_flush_interval`] /
    /// [`crate::settings::PerformanceSettings::unwatched_flush_interval`]) and
    /// a session's tier can change under a non-empty hold. Passing it in keeps
    /// this type interval-injected as well as clock-injected, so both cadences
    /// are unit-testable without a PTY. `Duration::ZERO` is meaningful: it
    /// means "no coalescing", every chunk leaves as it arrives.
    pub fn is_due(&self, now: Instant, interval: Duration) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        self.pending.len() >= BACKGROUND_HOLD_BYTE_CAP
            || now.saturating_duration_since(self.last_flush) >= interval
    }

    /// Take the held window if it is due, restarting the spacing clock.
    pub fn take_if_due(&mut self, now: Instant, interval: Duration) -> Option<(Vec<u8>, u64)> {
        if !self.is_due(now, interval) {
            return None;
        }
        self.take_now(now)
    }

    /// Take the held window unconditionally — the tier changed out of
    /// `background`, so held bytes must ship before anything emitted under the
    /// new tier or the webview would see them out of order.
    pub fn take_now(&mut self, now: Instant) -> Option<(Vec<u8>, u64)> {
        self.last_flush = now;
        if self.pending.is_empty() {
            return None;
        }
        Some((std::mem::take(&mut self.pending), self.pending_offset))
    }
}

/// Per-session cadence gate for [`ACTIVITY_EVENT`].
///
/// Clock- and counter-injected: `due` is a pure function of "how many bytes has
/// the PTY produced since the last digest" and "how long ago was it", so the
/// ≤1 Hz budget is unit-testable without a PTY or a Tauri app.
pub struct ActivityDigestState {
    last_emit: Instant,
    last_total: u64,
}

impl ActivityDigestState {
    pub fn new(now: Instant) -> Self {
        Self {
            last_emit: now,
            last_total: 0,
        }
    }

    /// Bytes to report if a digest is due, else `None`.
    ///
    /// An idle session is skipped WITHOUT restarting the clock: a session that
    /// says nothing for a minute and then speaks gets its digest on the very
    /// next sweep rather than waiting out a fresh interval.
    pub fn take_if_due(&mut self, now: Instant, total_produced: u64) -> Option<u64> {
        if total_produced <= self.last_total {
            return None;
        }
        if now.saturating_duration_since(self.last_emit) < ACTIVITY_DIGEST_INTERVAL {
            return None;
        }
        let delta = total_produced - self.last_total;
        self.last_total = total_produced;
        self.last_emit = now;
        Some(delta)
    }

    /// Re-baseline without emitting — used when a session leaves `unwatched`,
    /// so the first digest after it goes dark again reports only bytes
    /// produced since then rather than everything the tap already consumed.
    pub fn rebaseline(&mut self, now: Instant, total_produced: u64) {
        self.last_total = total_produced;
        self.last_emit = now;
    }
}

/// Tauri `terminal-activity` wire shape.
///
/// `bytesDelta` (not the digest text length) is what the frontend accrues into
/// its activity sparkline, so an `unwatched` tab's bars stay comparable with a
/// focused tab's — the digest's own payload is a 20-line screen tail, which
/// bears no relation to how much the PTY actually produced.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalActivityWire<'a> {
    pub terminal_id: &'a str,
    /// Absolute monotonic byte count, same counter `terminal-output` offsets
    /// are stamped from.
    pub total_bytes_produced: u64,
    /// Bytes produced since this session's previous digest.
    pub bytes_delta: u64,
    /// Trailing non-empty lines of the RENDERED screen (server-side VT grid).
    ///
    /// Deliberately the grid and not the raw byte stream: this replaces the
    /// frontend's ANSI-strip fallback, which is what an unmounted tab used to
    /// get (`useSessionStateTracking.ts` — `bufferReader` first, regex-strip
    /// only when no xterm buffer exists, i.e. exactly the unmounted case).
    /// The grid resolves cursor motion, rewrites and full-frame TUI redraws
    /// the same way xterm does, so the digest is strictly more faithful than
    /// the path it replaces.
    pub lines: Vec<String>,
}

/// One sweep: flush any held window whose configured flush deadline has passed
/// and emit any due `unwatched` digest. Both are per-session no-ops with a
/// couple of relaxed atomic loads when there is nothing outstanding.
pub fn sweep_once(live_windows: &BTreeSet<String>, windows_changed: bool) {
    use tauri::Manager;

    let Some(app) = crate::tauri_app_handle::current() else {
        return;
    };
    let Some(tm) = app.try_state::<Arc<crate::terminal::TerminalManager>>() else {
        return;
    };
    for (_, session) in tm.sessions_snapshot() {
        if windows_changed {
            session.retain_visibility_windows(live_windows);
        }
        session.flush_background_window_if_due();
        session.emit_activity_digest_if_due();
    }
}

/// Spawn the visibility sweeper for the process lifetime. Detached; each tick
/// is best-effort and the loop never exits.
///
/// Rides `spawn_blocking` for the same reason the fleet grid scanners do: a
/// sweep takes per-session locks and can materialize a screen tail, which is
/// CPU + lock work rather than I/O, and must never park a tokio worker.
pub fn spawn_sweeper() {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(SWEEP_TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!("terminal visibility: sweeper started");
        // Window membership changes only when a pop-out opens or closes, so
        // the O(sessions) prune runs on the edge, not every tick.
        let mut known_windows: BTreeSet<String> = BTreeSet::new();
        loop {
            ticker.tick().await;
            let live = live_window_labels();
            let changed = live != known_windows;
            if changed {
                known_windows = live.clone();
            }
            if let Err(e) = tokio::task::spawn_blocking(move || sweep_once(&live, changed)).await {
                warn!(error = %e, "terminal visibility: sweep task panicked");
            }
        }
    });
}

fn live_window_labels() -> BTreeSet<String> {
    use tauri::Manager;

    crate::tauri_app_handle::current()
        .map(|app| app.webview_windows().keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_wire_encoding_round_trips() {
        for tier in [
            VisibilityTier::Focused,
            VisibilityTier::Background,
            VisibilityTier::Unwatched,
        ] {
            assert_eq!(VisibilityTier::parse(tier.as_str()), Some(tier));
            assert_eq!(VisibilityTier::from_u8(tier.as_u8()), tier);
        }
        assert_eq!(VisibilityTier::parse("hidden"), None);
        assert_eq!(VisibilityTier::parse(""), None);
    }

    /// A session nobody has reported on behaves exactly as it did before
    /// Phase 5. This is the back-compat guarantee for an older webview bundle
    /// and for the window between spawn and the first reconcile.
    #[test]
    fn unreported_session_defaults_to_focused() {
        let state = VisibilityState::new();
        assert_eq!(state.tier(), VisibilityTier::Focused);
    }

    #[test]
    fn report_drives_the_effective_tier() {
        let state = VisibilityState::new();
        assert_eq!(
            state.report("main", VisibilityTier::Unwatched),
            VisibilityTier::Unwatched
        );
        assert_eq!(state.tier(), VisibilityTier::Unwatched);
        assert_eq!(
            state.report("main", VisibilityTier::Background),
            VisibilityTier::Background
        );
        assert_eq!(state.tier(), VisibilityTier::Background);
    }

    /// Rapid toggling must land on the LAST report, not on some latched
    /// extreme — a pane flickering visible/hidden during a layout animation
    /// would otherwise strand the session at whichever tier it touched first.
    #[test]
    fn rapid_toggling_converges_on_the_last_report() {
        let state = VisibilityState::new();
        for _ in 0..50 {
            state.report("main", VisibilityTier::Unwatched);
            state.report("main", VisibilityTier::Focused);
            state.report("main", VisibilityTier::Background);
        }
        assert_eq!(state.tier(), VisibilityTier::Background);
        assert_eq!(
            state.report("main", VisibilityTier::Focused),
            VisibilityTier::Focused
        );
        assert_eq!(state.tier(), VisibilityTier::Focused);
    }

    /// The pop-out case: the main window cannot take a terminal dark just
    /// because it isn't showing it — a `term-N` window might be.
    #[test]
    fn most_visible_window_wins_the_merge() {
        let state = VisibilityState::new();
        state.report("main", VisibilityTier::Unwatched);
        state.report("term-3", VisibilityTier::Focused);
        assert_eq!(state.tier(), VisibilityTier::Focused);

        // The pop-out hides it → background is now the most visible report.
        state.report("term-3", VisibilityTier::Background);
        assert_eq!(state.tier(), VisibilityTier::Background);

        // Both go dark → unwatched.
        state.report("term-3", VisibilityTier::Unwatched);
        assert_eq!(state.tier(), VisibilityTier::Unwatched);
    }

    #[test]
    fn closing_a_window_drops_its_report() {
        let state = VisibilityState::new();
        state.report("main", VisibilityTier::Unwatched);
        state.report("term-3", VisibilityTier::Focused);
        assert_eq!(state.tier(), VisibilityTier::Focused);

        let live: BTreeSet<String> = ["main".to_string()].into_iter().collect();
        assert_eq!(state.retain_windows(&live), VisibilityTier::Unwatched);
        assert_eq!(state.reporting_windows(), vec!["main".to_string()]);
    }

    /// Pruning when EVERY reporting window is gone must fall back to the
    /// default rather than to the last merged value: with no window left to
    /// speak for the session, "serve it fully" is the only safe answer.
    #[test]
    fn pruning_every_window_falls_back_to_focused() {
        let state = VisibilityState::new();
        state.report("term-1", VisibilityTier::Unwatched);
        assert_eq!(state.tier(), VisibilityTier::Unwatched);
        assert_eq!(
            state.retain_windows(&BTreeSet::new()),
            VisibilityTier::Focused
        );
        assert!(state.reporting_windows().is_empty());
    }

    /// Pruning with nothing to prune must not disturb the merged tier.
    #[test]
    fn retain_windows_is_a_noop_when_all_reporters_are_live() {
        let state = VisibilityState::new();
        state.report("main", VisibilityTier::Unwatched);
        let live: BTreeSet<String> = ["main".to_string(), "term-9".to_string()]
            .into_iter()
            .collect();
        assert_eq!(state.retain_windows(&live), VisibilityTier::Unwatched);
    }

    // ---- background hold window ----

    /// The headline `background` property: a burst of chunks inside one 250 ms
    /// window leaves as exactly ONE event, byte-for-byte the concatenation,
    /// stamped at the FIRST held byte's offset.
    #[test]
    fn background_hold_coalesces_a_burst_into_one_flush() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        hold.push(b"alpha", 100);
        hold.push(b"beta", 105);
        hold.push(b"gamma", 109);
        assert!(hold
            .take_if_due(t0 + Duration::from_millis(249), BACKGROUND_FLUSH_INTERVAL)
            .is_none());

        let (payload, offset) = hold
            .take_if_due(t0 + Duration::from_millis(250), BACKGROUND_FLUSH_INTERVAL)
            .expect("the 250 ms window elapsed");
        assert_eq!(payload, b"alphabetagamma");
        assert_eq!(offset, 100, "stamped at the first held byte");
        assert!(hold.is_empty());
    }

    /// Spacing is measured from the previous FLUSH, so a session talking
    /// continuously still costs at most one event per interval.
    #[test]
    fn background_hold_spacing_restarts_from_each_flush() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        hold.push(b"one", 0);
        let t1 = t0 + Duration::from_millis(300);
        assert!(hold.take_if_due(t1, BACKGROUND_FLUSH_INTERVAL).is_some());

        hold.push(b"two", 3);
        assert!(
            hold.take_if_due(t1 + Duration::from_millis(200), BACKGROUND_FLUSH_INTERVAL)
                .is_none(),
            "only 200 ms since the last flush"
        );
        assert!(hold
            .take_if_due(t1 + Duration::from_millis(260), BACKGROUND_FLUSH_INTERVAL)
            .is_some());
    }

    /// A firehose must not buffer unboundedly waiting for the clock.
    #[test]
    fn background_hold_flushes_early_on_the_byte_cap() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        hold.push(&vec![b'x'; BACKGROUND_HOLD_BYTE_CAP], 0);
        let (payload, _) = hold
            .take_if_due(t0, BACKGROUND_FLUSH_INTERVAL)
            .expect("the byte cap trips regardless of elapsed time");
        assert_eq!(payload.len(), BACKGROUND_HOLD_BYTE_CAP);
    }

    /// An empty window is never "due" — the sweeper must do nothing for a
    /// quiet background session.
    #[test]
    fn background_hold_is_never_due_while_empty() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        assert!(!hold.is_due(t0 + Duration::from_secs(60), BACKGROUND_FLUSH_INTERVAL));
        assert!(hold
            .take_if_due(t0 + Duration::from_secs(60), BACKGROUND_FLUSH_INTERVAL)
            .is_none());
        // ...and an interval of zero does not turn "empty" into "due": no
        // coalescing still means nothing to ship when nothing was pushed.
        assert!(!hold.is_due(t0, Duration::ZERO));
    }

    /// A tier change out of `background` ships held bytes immediately, so
    /// nothing emitted under the new tier can overtake them.
    #[test]
    fn background_hold_take_now_ignores_the_clock() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        hold.push(b"held", 7);
        let (payload, offset) = hold.take_now(t0).expect("unconditional");
        assert_eq!(payload, b"held");
        assert_eq!(offset, 7);
        assert!(hold.take_now(t0).is_none(), "nothing left to ship");
    }

    /// The spacing honoured is the one PASSED IN, not the compiled-in default
    /// — the whole point of making it a parameter. An operator who raises
    /// `background_flush_interval_ms` to 1 s must not get a 250 ms flush.
    #[test]
    fn background_hold_honours_a_caller_supplied_interval() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        hold.push(b"slow", 0);
        let slow = Duration::from_millis(1000);
        assert!(
            hold.take_if_due(t0 + Duration::from_millis(400), slow)
                .is_none(),
            "400 ms is due under the 250 ms default but NOT under a 1 s setting"
        );
        assert!(hold.take_if_due(t0 + slow, slow).is_some());
    }

    /// `0` is the documented "no coalescing" setting: a held chunk is due
    /// immediately. It must not be read as "never due" (a `>=` vs `>` slip),
    /// which would buffer an emitting tier forever.
    #[test]
    fn background_hold_zero_interval_is_due_immediately() {
        let t0 = Instant::now();
        let mut hold = BackgroundHold::new(t0);
        hold.push(b"now", 0);
        let (payload, offset) = hold
            .take_if_due(t0, Duration::ZERO)
            .expect("a zero interval means no coalescing");
        assert_eq!(payload, b"now");
        assert_eq!(offset, 0);
    }

    // ---- activity digest cadence ----

    #[test]
    fn activity_digest_is_capped_at_one_per_second() {
        let t0 = Instant::now();
        let mut st = ActivityDigestState::new(t0);
        // Produced bytes but the interval hasn't elapsed.
        assert_eq!(st.take_if_due(t0 + Duration::from_millis(999), 500), None);
        assert_eq!(
            st.take_if_due(t0 + Duration::from_millis(1000), 500),
            Some(500)
        );
        // Immediately after, more bytes — still rate-limited.
        assert_eq!(st.take_if_due(t0 + Duration::from_millis(1250), 900), None);
        assert_eq!(
            st.take_if_due(t0 + Duration::from_millis(2000), 900),
            Some(400),
            "delta since the PREVIOUS digest, not since spawn"
        );
    }

    /// An idle session emits nothing and does not consume its interval, so the
    /// first byte after a long silence is reported on the next sweep.
    #[test]
    fn activity_digest_skips_idle_without_restarting_the_clock() {
        let t0 = Instant::now();
        let mut st = ActivityDigestState::new(t0);
        for i in 1..=10 {
            assert_eq!(st.take_if_due(t0 + Duration::from_secs(i), 0), None);
        }
        assert_eq!(
            st.take_if_due(t0 + Duration::from_secs(10), 42),
            Some(42),
            "no waiting out a fresh interval after silence"
        );
    }

    /// Leaving `unwatched` re-baselines, so the next dark window reports only
    /// what it actually missed rather than replaying the tap-fed stretch.
    #[test]
    fn activity_digest_rebaseline_drops_the_tap_fed_stretch() {
        let t0 = Instant::now();
        let mut st = ActivityDigestState::new(t0);
        assert_eq!(st.take_if_due(t0 + Duration::from_secs(1), 100), Some(100));
        // Tier upgraded; the page tap consumed the next 5000 bytes.
        st.rebaseline(t0 + Duration::from_secs(2), 5100);
        assert_eq!(
            st.take_if_due(t0 + Duration::from_secs(3), 5150),
            Some(50),
            "only the bytes produced since the re-baseline"
        );
    }
}
