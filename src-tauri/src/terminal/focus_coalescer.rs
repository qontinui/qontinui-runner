//! Coalesced view-switching for unattended spawns (plan
//! `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`,
//! D3 / Phase 4).
//!
//! Every runner-spawned terminal used to move the operator's view twice: the
//! frontend auto-selected each externally-created tab on `terminal-created`,
//! and every gate continuation, condition check and looping agent's first
//! spawn emitted `terminal-focus-request`, which switches the main view to the
//! Terminal panel. On 2026-10-27 that was 46 view switches in 20 minutes, 19 of
//! them inside 17 seconds.
//!
//! The rule here: across the origins whose spawns always switch the operator's
//! view ([`switches_the_view`]: continuations and condition checks) at most ONE
//! decision surfaces per [`FOCUS_COALESCE_WINDOW`]. The first spawn of a burst
//! still surfaces (the reason the focus request exists — plan
//! `2026-06-07-gate-continuation-auto-surface-and-focus`); the rest appear as
//! tabs without stealing the view. Operator origins always surface and open no
//! window. A retried continuation is a new request: if its first attempt never
//! started but took the window, the retry's create is suppressed. Its tab is
//! still there, but it does not surface later.
//!
//! Every other unattended origin normally only selects a tab: a boot resume, a
//! respawn or usage-limit migration, a steward, a scheduled task, orchestration
//! fan-out, a looping agent (only its first spawn sends a focus request, and its
//! relaunches must not hold the window), and [`SpawnOrigin::Unknown`] (a caller
//! this runner cannot name, such as an agent's HTTP `POST /terminals` or a
//! UI-Bridge or relay create). These have a TAB-ONLY window of their own. Such a request surfaces only when
//! neither window is open, and its surface opens only the tab-only window. So a
//! burst of them is still held to one surface per window, while none of them
//! can suppress the next real gate continuation's view switch: that switch is
//! never retried, so a window it did not get would leave an operator on another
//! panel seeing nothing.
//!
//! One decision per spawn. The tab-selection half rides `terminal-created`,
//! which the [`TerminalManager`](super::TerminalManager) emits at PTY create;
//! the main-view half is the later `terminal-focus-request`, for the origins
//! that send one. The create-time decision is recorded against the terminal id
//! and the first focus request for that id TAKES it, so the two halves can never
//! disagree (a suppressed create never gets a view switch). A REFOCUS (a duplicate anchor folded onto a live tab) only PEEKS the
//! recorded decision, so it can never consume the original's and hand that
//! terminal a fresh decision later. A peeked suppression stays suppressed; a
//! peeked surface is NOT replayed — the refocus is a new request and is
//! decided against the window as it stands now, as is one with nothing
//! recorded.
//!
//! A suppression is never silent: it is a `debug!`, a `/health`
//! `focusCoalescer.suppressedTotal` count, and one `info!` `focus: coalesced N
//! unattended focus requests`, logged at the first decision or `/health` read
//! after every open window has closed.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, info};

use crate::coord_drain_state::SpawnOrigin;

/// At most one view-switching surface, plus one tab-only surface, per this
/// window (D3).
pub const FOCUS_COALESCE_WINDOW: Duration = Duration::from_secs(60);

/// How long a create-time decision waits for its focus request. A
/// continuation's create -> start verdict was measured at up to 38 s for the
/// create alone; ten minutes covers any start wait, and an entry that is
/// never taken (a site that emits no focus request) ages out.
const PENDING_DECISION_TTL: Duration = Duration::from_secs(600);

/// Hard bound on recorded create-time decisions, oldest dropped first.
const PENDING_DECISION_CAP: usize = 512;

/// The pure decision for ONE window: does a view-switching request from
/// `origin` at `now` surface, given that window's last surface at
/// `last_surface`?
///
/// Operator origins always surface. An unattended origin surfaces when that
/// window has had no surface yet, or when at least `window` has elapsed since
/// its last one. [`Coalescer::decide`] checks a tab-only request against both
/// windows.
pub fn should_surface(
    now: Instant,
    last_surface: Option<Instant>,
    origin: SpawnOrigin,
    window: Duration,
) -> bool {
    if !origin.is_autonomous() {
        return true;
    }
    match last_surface {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= window,
    }
}

/// Whether every spawn from `origin` emits a `terminal-focus-request`, the
/// request that moves the operator's main view: continuations and condition
/// checks (`CoordDispatch`). A looping agent sends one only on its first spawn,
/// so it is not one of these: its relaunches would otherwise hold the window a
/// continuation's only view switch needs. Exhaustive, so a new origin has to say
/// which it is.
fn switches_the_view(origin: SpawnOrigin) -> bool {
    match origin {
        SpawnOrigin::GateContinuation
        | SpawnOrigin::UnitContinuation
        | SpawnOrigin::CoordDispatch => true,
        SpawnOrigin::LoopingAgent
        | SpawnOrigin::Steward
        | SpawnOrigin::Respawn
        | SpawnOrigin::Scheduler
        | SpawnOrigin::Orchestration
        | SpawnOrigin::BootResume
        | SpawnOrigin::OperatorTerminal
        | SpawnOrigin::OperatorChat
        | SpawnOrigin::Unknown => false,
    }
}

/// The process-global coalescer state. Time is injected into every method so
/// the state machine is tested without a clock.
#[derive(Debug, Default)]
struct Coalescer {
    /// The instant the last view-switching request surfaced (the shared
    /// window; a tab-only origin never sets it).
    last_surface: Option<Instant>,
    /// Wall-clock twin of `last_surface`, for `/health`.
    last_surface_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The instant the last tab-only request surfaced (the tab-only window).
    last_tab_only_surface: Option<Instant>,
    /// Wall-clock twin of `last_tab_only_surface`, for `/health`.
    last_tab_only_surface_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Suppressed requests since boot.
    suppressed_total: u64,
    /// Suppressed requests not yet reported by the window-close `info!`.
    suppressed_in_window: u64,
    /// Create-time decisions awaiting their focus request: (terminal id,
    /// surfaced, decided at).
    pending: VecDeque<(String, bool, Instant)>,
}

impl Coalescer {
    /// If every open window has closed with suppressions in it, return that
    /// count and reset it — the caller logs it once.
    fn take_closed_window(&mut self, now: Instant, window: Duration) -> Option<u64> {
        let last = self.last_surface.max(self.last_tab_only_surface)?;
        if self.suppressed_in_window == 0 || now.saturating_duration_since(last) < window {
            return None;
        }
        Some(std::mem::take(&mut self.suppressed_in_window))
    }

    /// Decide one request and update the state. Returns (surfaced, the
    /// closed-window count to log, if any).
    ///
    /// A view-switching origin is checked against, and opens, the shared
    /// window. A tab-only origin is checked against both windows and opens only
    /// its own (module doc). A suppressed request of either kind is counted.
    fn decide(
        &mut self,
        now: Instant,
        wall: chrono::DateTime<chrono::Utc>,
        origin: SpawnOrigin,
        window: Duration,
    ) -> (bool, Option<u64>) {
        let closed = self.take_closed_window(now, window);
        let tab_only = !switches_the_view(origin);
        let surface = should_surface(now, self.last_surface, origin, window)
            && (!tab_only || should_surface(now, self.last_tab_only_surface, origin, window));
        if origin.is_autonomous() {
            if !surface {
                self.suppressed_total += 1;
                self.suppressed_in_window += 1;
            } else if tab_only {
                self.last_tab_only_surface = Some(now);
                self.last_tab_only_surface_at = Some(wall);
            } else {
                self.last_surface = Some(now);
                self.last_surface_at = Some(wall);
            }
        }
        (surface, closed)
    }

    /// The create-time decision ([`decide_for_create`]): decide, and record
    /// it for an unattended origin so the terminal's focus request reuses it.
    /// An operator create records nothing (it always surfaces).
    fn decide_for_create(
        &mut self,
        now: Instant,
        wall: chrono::DateTime<chrono::Utc>,
        origin: SpawnOrigin,
        terminal_id: &str,
        window: Duration,
    ) -> (bool, Option<u64>) {
        let out = self.decide(now, wall, origin, window);
        if origin.is_autonomous() {
            self.record_pending(terminal_id, out.0, now);
        }
        out
    }

    /// The focus-request decision ([`decide_for_focus`]): TAKE the recorded
    /// create-time decision when there is one, else decide fresh.
    fn decide_for_focus(
        &mut self,
        now: Instant,
        wall: chrono::DateTime<chrono::Utc>,
        origin: SpawnOrigin,
        terminal_id: &str,
        window: Duration,
    ) -> (bool, Option<u64>) {
        match self.take_pending(terminal_id, now) {
            Some(surface) => (surface, self.take_closed_window(now, window)),
            None => self.decide(now, wall, origin, window),
        }
    }

    /// The refocus decision ([`decide_for_refocus`]): PEEK the recorded
    /// create-time decision — never consume it, so the original terminal's
    /// own focus request still finds it. A peeked suppression short-circuits
    /// to `false`; anything else (a peeked surface, or nothing recorded) is
    /// decided fresh, so a duplicate re-delivered later cannot replay an old
    /// surface past the window that is open now. A refocus is a request of its
    /// own, so its suppression is counted, unlike a focus request that takes a
    /// create-time suppression already counted at the create.
    fn decide_for_refocus(
        &mut self,
        now: Instant,
        wall: chrono::DateTime<chrono::Utc>,
        origin: SpawnOrigin,
        terminal_id: &str,
        window: Duration,
    ) -> (bool, Option<u64>) {
        match self.peek_pending(terminal_id, now) {
            Some(false) => {
                let closed = self.take_closed_window(now, window);
                self.suppressed_total += 1;
                self.suppressed_in_window += 1;
                (false, closed)
            }
            _ => self.decide(now, wall, origin, window),
        }
    }

    /// Record a create-time decision for `terminal_id`.
    fn record_pending(&mut self, terminal_id: &str, surface: bool, now: Instant) {
        self.prune(now);
        while self.pending.len() >= PENDING_DECISION_CAP {
            self.pending.pop_front();
        }
        self.pending
            .push_back((terminal_id.to_string(), surface, now));
    }

    /// Take the create-time decision recorded for `terminal_id`, if any.
    fn take_pending(&mut self, terminal_id: &str, now: Instant) -> Option<bool> {
        self.prune(now);
        let idx = self
            .pending
            .iter()
            .position(|(id, _, _)| id == terminal_id)?;
        self.pending.remove(idx).map(|(_, surface, _)| surface)
    }

    /// The create-time decision recorded for `terminal_id`, left in place.
    fn peek_pending(&mut self, terminal_id: &str, now: Instant) -> Option<bool> {
        self.prune(now);
        self.pending
            .iter()
            .find(|(id, _, _)| id == terminal_id)
            .map(|(_, surface, _)| *surface)
    }

    fn prune(&mut self, now: Instant) {
        while self
            .pending
            .front()
            .is_some_and(|(_, _, at)| now.saturating_duration_since(*at) >= PENDING_DECISION_TTL)
        {
            self.pending.pop_front();
        }
    }
}

/// The `/health` `focusCoalescer` block for `c` (pure).
fn health_json_of(c: &Coalescer) -> serde_json::Value {
    serde_json::json!({
        "windowSecs": FOCUS_COALESCE_WINDOW.as_secs(),
        "suppressedTotal": c.suppressed_total,
        "lastSurfaceAt": c.last_surface_at.map(|t| t.to_rfc3339()),
        "lastTabOnlySurfaceAt": c.last_tab_only_surface_at.map(|t| t.to_rfc3339()),
    })
}

static COALESCER: Mutex<Option<Coalescer>> = Mutex::new(None);

fn with_coalescer<R>(f: impl FnOnce(&mut Coalescer) -> R) -> R {
    let mut guard = COALESCER.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(Coalescer::default))
}

fn log_closed_window(count: Option<u64>) {
    if let Some(n) = count {
        info!(
            "focus: coalesced {n} unattended focus requests window_secs={}",
            FOCUS_COALESCE_WINDOW.as_secs()
        );
    }
}

fn log_suppressed(what: &str, surface: bool, origin: SpawnOrigin, terminal_id: &str) {
    if !surface {
        debug!(
            "focus: suppressed unattended {what} origin={} terminal_id={terminal_id}",
            origin.as_wire()
        );
    }
}

/// The create-time decision for a terminal `origin` is spawning: `true` when
/// its tab should be selected (`terminal-created` `surface`). Recorded so the
/// terminal's later focus request ([`decide_for_focus`]) reuses it.
pub fn decide_for_create(origin: SpawnOrigin, terminal_id: &str) -> bool {
    let now = Instant::now();
    let (surface, closed) = with_coalescer(|c| {
        c.decide_for_create(
            now,
            chrono::Utc::now(),
            origin,
            terminal_id,
            FOCUS_COALESCE_WINDOW,
        )
    });
    log_closed_window(closed);
    log_suppressed("tab selection", surface, origin, terminal_id);
    surface
}

/// Whether a `terminal-focus-request` for `terminal_id` from `origin` is
/// emitted: the create-time decision when one is recorded (taken), else a
/// fresh one.
pub fn decide_for_focus(origin: SpawnOrigin, terminal_id: &str) -> bool {
    let now = Instant::now();
    let (surface, closed) = with_coalescer(|c| {
        c.decide_for_focus(
            now,
            chrono::Utc::now(),
            origin,
            terminal_id,
            FOCUS_COALESCE_WINDOW,
        )
    });
    log_closed_window(closed);
    log_suppressed("focus request", surface, origin, terminal_id);
    surface
}

/// Whether a REFOCUS of the live `terminal_id` (a duplicate anchor folded
/// onto it) switches the view. It PEEKS the recorded create-time decision and
/// never consumes it, so the terminal's own focus request still finds it. A
/// peeked suppression stays suppressed; anything else (a peeked surface, or
/// nothing recorded) is decided fresh, so a duplicate re-delivered later cannot
/// replay an old surface past the window that is open now.
pub fn decide_for_refocus(origin: SpawnOrigin, terminal_id: &str) -> bool {
    let now = Instant::now();
    let (surface, closed) = with_coalescer(|c| {
        c.decide_for_refocus(
            now,
            chrono::Utc::now(),
            origin,
            terminal_id,
            FOCUS_COALESCE_WINDOW,
        )
    });
    log_closed_window(closed);
    log_suppressed("refocus", surface, origin, terminal_id);
    surface
}

/// `/health` `focusCoalescer`. `lastSurfaceAt` is the shared window's last
/// surface and `lastTabOnlySurfaceAt` the tab-only window's; `null` means no
/// such request has surfaced since boot.
pub fn health_json() -> serde_json::Value {
    let now = Instant::now();
    let (value, closed) = with_coalescer(|c| {
        let closed = c.take_closed_window(now, FOCUS_COALESCE_WINDOW);
        (health_json_of(c), closed)
    });
    log_closed_window(closed);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 46 `gate-continuation terminal session started` timestamps of
    /// 2026-10-27 (`.dev-logs/primary.log` on spaceship, ANSI-stripped), as
    /// milliseconds after the first one, 20:30:24.933Z.
    const BURST_2026_10_27_MS: [u64; 46] = [
        0,         // 20:30:24.933
        2_888,     // 20:30:27.821
        4_333,     // 20:30:29.266
        4_426,     // 20:30:29.359
        6_624,     // 20:30:31.557
        6_686,     // 20:30:31.619
        6_760,     // 20:30:31.693
        10_323,    // 20:30:35.256
        10_359,    // 20:30:35.292
        10_400,    // 20:30:35.333
        10_726,    // 20:30:35.659
        14_735,    // 20:30:39.668
        14_825,    // 20:30:39.758
        14_899,    // 20:30:39.832
        15_750,    // 20:30:40.683
        15_785,    // 20:30:40.718
        15_860,    // 20:30:40.793
        15_973,    // 20:30:40.906
        16_345,    // 20:30:41.278
        109_136,   // 20:32:14.069
        987_368,   // 20:46:52.301
        987_516,   // 20:46:52.449
        1_018_710, // 20:47:23.643
        1_159_382, // 20:49:44.315
        1_160_564, // 20:49:45.497
        1_160_631, // 20:49:45.564
        1_160_728, // 20:49:45.661
        1_162_141, // 20:49:47.074
        1_181_050, // 20:50:05.983
        1_181_106, // 20:50:06.039
        1_181_161, // 20:50:06.094
        1_181_200, // 20:50:06.133
        1_181_313, // 20:50:06.246
        1_181_422, // 20:50:06.355
        1_189_979, // 20:50:14.912
        1_203_740, // 20:50:28.673
        1_203_908, // 20:50:28.841
        1_206_953, // 20:50:31.885
        1_207_815, // 20:50:32.748
        1_209_891, // 20:50:34.824
        1_213_583, // 20:50:38.516
        1_213_791, // 20:50:38.724
        1_217_896, // 20:50:42.829
        1_217_971, // 20:50:42.904
        1_218_515, // 20:50:43.448
        1_220_700, // 20:50:45.633
    ];

    /// Drive `offsets_ms` through a fresh coalescer as `origin` requests and
    /// return the offsets that surfaced.
    fn surfaced(offsets_ms: &[u64], origin: SpawnOrigin) -> Vec<u64> {
        let base = Instant::now();
        let mut c = Coalescer::default();
        offsets_ms
            .iter()
            .copied()
            .filter(|ms| {
                let now = base + Duration::from_millis(*ms);
                c.decide(now, chrono::Utc::now(), origin, FOCUS_COALESCE_WINDOW)
                    .0
            })
            .collect()
    }

    #[test]
    fn the_2026_10_27_burst_surfaces_five_of_46() {
        // Hand computation, 60 s windows opened by each surface:
        //   20:30:24.933 surfaces; the next 18 (to 20:30:41.278) fall inside.
        //   20:32:14.069 surfaces (109 s after the last surface).
        //   20:46:52.301 surfaces; 20:46:52.449 and 20:47:23.643 (31 s) inside.
        //   20:49:44.315 surfaces (172 s after 20:46:52.301); everything to
        //     20:50:43.448 (59.13 s after it) inside.
        //   20:50:45.633 surfaces (61.32 s after 20:49:44.315).
        assert_eq!(BURST_2026_10_27_MS.len(), 46);
        let out = surfaced(&BURST_2026_10_27_MS, SpawnOrigin::GateContinuation);
        assert_eq!(out, vec![0, 109_136, 987_368, 1_159_382, 1_220_700]);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn nineteen_requests_in_seventeen_seconds_surface_exactly_one() {
        let first_19 = &BURST_2026_10_27_MS[..19];
        assert_eq!(first_19[18], 16_345, "the densest burst spans 17 s");
        let out = surfaced(first_19, SpawnOrigin::GateContinuation);
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn operator_requests_always_surface() {
        for origin in [SpawnOrigin::OperatorTerminal, SpawnOrigin::OperatorChat] {
            let out = surfaced(&BURST_2026_10_27_MS, origin);
            assert_eq!(out.len(), 46, "{origin:?}");
        }
        // ...even inside a window an unattended request just opened, and
        // without consuming it.
        let base = Instant::now();
        let mut c = Coalescer::default();
        let w = FOCUS_COALESCE_WINDOW;
        assert!(
            c.decide(base, chrono::Utc::now(), SpawnOrigin::GateContinuation, w)
                .0
        );
        let t1 = base + Duration::from_secs(1);
        assert!(
            c.decide(t1, chrono::Utc::now(), SpawnOrigin::OperatorTerminal, w)
                .0
        );
        assert_eq!(c.last_surface, Some(base));
        assert!(!c.decide(t1, chrono::Utc::now(), SpawnOrigin::Steward, w).0);
    }

    #[test]
    fn should_surface_boundary_is_inclusive_at_the_window() {
        let base = Instant::now();
        let w = Duration::from_secs(60);
        let o = SpawnOrigin::LoopingAgent;
        assert!(should_surface(base, None, o, w));
        assert!(!should_surface(
            base + Duration::from_millis(59_999),
            Some(base),
            o,
            w
        ));
        assert!(should_surface(base + w, Some(base), o, w));
    }

    #[test]
    fn suppressions_are_counted_and_reported_once_per_closed_window() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        let o = SpawnOrigin::GateContinuation;
        let at = |s: u64| base + Duration::from_secs(s);
        assert_eq!(c.decide(at(0), chrono::Utc::now(), o, w), (true, None));
        assert_eq!(c.decide(at(10), chrono::Utc::now(), o, w), (false, None));
        assert_eq!(c.decide(at(20), chrono::Utc::now(), o, w), (false, None));
        assert_eq!(c.suppressed_total, 2);
        // Still inside the window: nothing to report yet.
        assert_eq!(c.take_closed_window(at(59), w), None);
        // The window closed with 2 suppressions: reported exactly once.
        assert_eq!(c.take_closed_window(at(61), w), Some(2));
        assert_eq!(c.take_closed_window(at(62), w), None);
        // A window with no suppressions reports nothing.
        assert_eq!(c.decide(at(130), chrono::Utc::now(), o, w), (true, None));
        assert_eq!(c.decide(at(200), chrono::Utc::now(), o, w), (true, None));
        assert_eq!(c.suppressed_total, 2);
    }

    #[test]
    fn a_focus_request_takes_its_create_decision_once() {
        let base = Instant::now();
        let mut c = Coalescer::default();
        c.record_pending("t-1", true, base);
        c.record_pending("t-2", false, base);
        assert_eq!(c.take_pending("t-2", base), Some(false));
        assert_eq!(c.take_pending("t-1", base), Some(true));
        // Taken: a later request for the same id is decided fresh.
        assert_eq!(c.take_pending("t-1", base), None);
        // An untaken decision ages out.
        c.record_pending("t-3", true, base);
        assert_eq!(c.take_pending("t-3", base + PENDING_DECISION_TTL), None);
    }

    #[test]
    fn pending_decisions_are_bounded() {
        let base = Instant::now();
        let mut c = Coalescer::default();
        for i in 0..(PENDING_DECISION_CAP + 10) {
            c.record_pending(&format!("t-{i}"), false, base);
        }
        assert_eq!(c.pending.len(), PENDING_DECISION_CAP);
        assert_eq!(c.take_pending("t-0", base), None);
        assert_eq!(
            c.take_pending(&format!("t-{}", PENDING_DECISION_CAP + 9), base),
            Some(false)
        );
    }

    #[test]
    fn health_json_of_reports_the_fixture_exactly() {
        let w = FOCUS_COALESCE_WINDOW;
        assert_eq!(
            health_json_of(&Coalescer::default()),
            serde_json::json!({
                "windowSecs": 60,
                "suppressedTotal": 0,
                "lastSurfaceAt": null,
                "lastTabOnlySurfaceAt": null,
            })
        );

        let rfc = |t: &str| {
            chrono::DateTime::parse_from_rfc3339(t)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let base = Instant::now();
        let mut c = Coalescer::default();
        let o = SpawnOrigin::GateContinuation;
        assert!(c.decide(base, rfc("2026-10-27T20:30:24.933Z"), o, w).0);
        for secs in [3, 5, 7] {
            assert!(
                !c.decide(base + Duration::from_secs(secs), chrono::Utc::now(), o, w)
                    .0
            );
        }
        // Close the window first, so the since-boot total and the in-window
        // count diverge (3 vs 0) and the literal below can tell them apart.
        assert_eq!(
            c.take_closed_window(base + Duration::from_secs(61), w),
            Some(3)
        );
        assert_eq!(c.suppressed_in_window, 0);
        // An `Unknown` surface after the shared window closed opens its own.
        let t = base + Duration::from_secs(62);
        assert!(
            c.decide(t, rfc("2026-10-27T20:31:26.933Z"), SpawnOrigin::Unknown, w)
                .0
        );
        assert_eq!(
            health_json_of(&c),
            serde_json::json!({
                "windowSecs": 60,
                "suppressedTotal": 3,
                "lastSurfaceAt": "2026-10-27T20:30:24.933+00:00",
                "lastTabOnlySurfaceAt": "2026-10-27T20:31:26.933+00:00",
            })
        );
    }

    #[test]
    fn health_json_wrapper_carries_every_key() {
        let v = health_json();
        assert_eq!(v["windowSecs"], 60);
        assert!(v["suppressedTotal"].is_u64(), "{v}");
        assert!(v.get("lastSurfaceAt").is_some(), "{v}");
        assert!(v.get("lastTabOnlySurfaceAt").is_some(), "{v}");
    }

    // ── The create -> focus handoff, at the Coalescer level (injected time) ──

    fn wall() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    #[test]
    fn handoff_autonomous_create_suppressed_then_focus_is_false() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let o = SpawnOrigin::GateContinuation;
        let mut c = Coalescer::default();
        assert!(c.decide_for_create(base, wall(), o, "first", w).0);
        let t1 = base + Duration::from_secs(5);
        assert!(!c.decide_for_create(t1, wall(), o, "second", w).0);
        // The focus request arrives AFTER the window closed: a fresh decision
        // would surface it; the recorded create-time decision must win.
        let t2 = base + Duration::from_secs(70);
        assert!(!c.decide_for_focus(t2, wall(), o, "second", w).0);
        // Taken, not left behind.
        assert!(c.pending.iter().all(|(id, _, _)| id != "second"));
    }

    #[test]
    fn handoff_autonomous_create_surfaced_then_focus_is_true() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let o = SpawnOrigin::GateContinuation;
        let mut c = Coalescer::default();
        assert!(c.decide_for_create(base, wall(), o, "first", w).0);
        // Inside the window its own create opened: a fresh decision would be
        // suppressed; the recorded one surfaces it.
        let t1 = base + Duration::from_secs(5);
        assert!(c.decide_for_focus(t1, wall(), o, "first", w).0);
        assert_eq!(c.suppressed_total, 0);
    }

    #[test]
    fn handoff_operator_create_records_nothing() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        for o in [SpawnOrigin::OperatorTerminal, SpawnOrigin::OperatorChat] {
            assert!(c.decide_for_create(base, wall(), o, "op", w).0);
        }
        assert!(c.pending.is_empty());
        assert_eq!(c.last_surface, None);
        assert_eq!(c.last_tab_only_surface, None);
    }

    #[test]
    fn a_duplicate_fold_does_not_steal_the_create_decision() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let o = SpawnOrigin::GateContinuation;
        let mut c = Coalescer::default();
        assert!(c.decide_for_create(base, wall(), o, "first", w).0);
        // `orig` is created inside that window: suppressed.
        let t1 = base + Duration::from_secs(2);
        assert!(!c.decide_for_create(t1, wall(), o, "orig", w).0);
        // A duplicate anchor folds onto `orig` after the window closed; it
        // must not consume `orig`'s pending decision...
        let t2 = base + Duration::from_secs(65);
        assert_eq!(c.suppressed_total, 1, "orig's suppressed create");
        assert!(!c.decide_for_refocus(t2, wall(), o, "orig", w).0);
        // The refocus is a request of its own, so its suppression is counted.
        assert_eq!(c.suppressed_total, 2);
        assert_eq!(c.suppressed_in_window, 1, "the window close took the 1st");
        // ...so `orig`'s own focus request still finds it and stays
        // suppressed (a fresh decision here would surface it). That is the
        // create's own decision, already counted.
        let t3 = base + Duration::from_secs(66);
        assert!(!c.decide_for_focus(t3, wall(), o, "orig", w).0);
        assert_eq!(c.suppressed_total, 2);
    }

    #[test]
    fn a_refocus_with_nothing_pending_is_decided_fresh() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let o = SpawnOrigin::GateContinuation;
        let mut c = Coalescer::default();
        assert!(c.decide_for_refocus(base, wall(), o, "live", w).0);
        let t1 = base + Duration::from_secs(1);
        assert!(!c.decide_for_refocus(t1, wall(), o, "live", w).0);
    }

    #[test]
    fn a_refocus_of_a_surfaced_pending_terminal_honours_the_open_window() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        // `a` (a migrated pane) surfaces at 0 s; its decision stays pending.
        assert!(
            c.decide_for_create(base, wall(), SpawnOrigin::Respawn, "a", w)
                .0
        );
        // `b` surfaces at 70 s and opens a new window.
        let t1 = base + Duration::from_secs(70);
        assert!(
            c.decide_for_create(t1, wall(), SpawnOrigin::GateContinuation, "b", w)
                .0
        );
        // A duplicate folded onto `a` at 75 s must not replay `a`'s old
        // surface inside `b`'s window.
        let t2 = base + Duration::from_secs(75);
        assert!(
            !c.decide_for_refocus(t2, wall(), SpawnOrigin::GateContinuation, "a", w)
                .0
        );
        // The peek left `a`'s own decision in place.
        assert_eq!(c.take_pending("a", t2), Some(true));
    }

    #[test]
    fn a_refocus_of_a_surfaced_pending_terminal_surfaces_when_no_window_is_open() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        assert!(
            c.decide_for_create(base, wall(), SpawnOrigin::Respawn, "a", w)
                .0
        );
        // `a`'s window has closed and no other is open, so the refocus is a
        // fresh request that surfaces, and opens the window itself.
        let t1 = base + w + Duration::from_secs(5);
        assert!(
            c.decide_for_refocus(t1, wall(), SpawnOrigin::GateContinuation, "a", w)
                .0
        );
        assert_eq!(c.last_surface, Some(t1));
        assert_eq!(c.suppressed_total, 0);
    }

    #[test]
    fn suppressions_wait_for_every_open_window_to_close() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        let at = |s: u64| base + Duration::from_secs(s);
        let gate = SpawnOrigin::GateContinuation;
        assert!(c.decide(at(0), wall(), SpawnOrigin::Unknown, w).0);
        assert!(c.decide(at(30), wall(), gate, w).0);
        assert!(!c.decide(at(40), wall(), gate, w).0);
        // The tab-only window closed at 60 s, but the shared one is open to 90 s.
        assert_eq!(c.take_closed_window(at(60), w), None);
        assert_eq!(c.take_closed_window(at(90), w), Some(1));
    }

    /// A spawn that only selects a tab must not take the window from a real
    /// continuation: the view switch it would suppress is never retried, so an
    /// operator on another panel would see nothing.
    #[test]
    fn a_tab_only_spawn_does_not_take_a_continuations_view_switch() {
        let w = FOCUS_COALESCE_WINDOW;
        for origin in [
            SpawnOrigin::BootResume,
            SpawnOrigin::Respawn,
            SpawnOrigin::LoopingAgent,
            SpawnOrigin::Steward,
            SpawnOrigin::Scheduler,
            SpawnOrigin::Orchestration,
            SpawnOrigin::Unknown,
        ] {
            let base = Instant::now();
            let mut c = Coalescer::default();
            assert!(c.decide(base, wall(), origin, w).0, "{origin:?}");
            assert_eq!(c.last_surface, None, "{origin:?}");
            let t1 = base + Duration::from_secs(20);
            assert!(
                c.decide(t1, wall(), SpawnOrigin::GateContinuation, w).0,
                "{origin:?} took the continuation's window"
            );
            // A burst of tab-only spawns is still held to one per window.
            let t2 = base + Duration::from_secs(21);
            assert!(!c.decide(t2, wall(), origin, w).0, "{origin:?}");
        }
    }

    /// The table `decide` splits on, as literals: which origins' spawns always
    /// send a `terminal-focus-request`.
    #[test]
    fn only_continuations_and_condition_checks_always_switch_the_view() {
        for origin in [
            SpawnOrigin::GateContinuation,
            SpawnOrigin::UnitContinuation,
            SpawnOrigin::CoordDispatch,
        ] {
            assert!(switches_the_view(origin), "{origin:?}");
        }
        for origin in [
            SpawnOrigin::LoopingAgent,
            SpawnOrigin::Steward,
            SpawnOrigin::Respawn,
            SpawnOrigin::Scheduler,
            SpawnOrigin::Orchestration,
            SpawnOrigin::BootResume,
            SpawnOrigin::OperatorTerminal,
            SpawnOrigin::OperatorChat,
            SpawnOrigin::Unknown,
        ] {
            assert!(!switches_the_view(origin), "{origin:?}");
        }
    }

    #[test]
    fn an_all_unknown_burst_surfaces_once_per_window() {
        let out = surfaced(&[0, 1_000, 59_999, 60_000], SpawnOrigin::Unknown);
        assert_eq!(out, vec![0, 60_000]);
    }

    #[test]
    fn an_unknown_request_is_suppressed_inside_the_named_window() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        assert!(c.decide(base, wall(), SpawnOrigin::GateContinuation, w).0);
        let t1 = base + Duration::from_secs(30);
        assert!(!c.decide(t1, wall(), SpawnOrigin::Unknown, w).0);
        assert_eq!(c.last_tab_only_surface, None);
        assert_eq!(c.suppressed_total, 1);
    }

    #[test]
    fn tab_only_suppressions_are_reported_when_the_tab_only_window_closes() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        let u = SpawnOrigin::Unknown;
        assert_eq!(c.decide(base, wall(), u, w), (true, None));
        assert_eq!(
            c.decide(base + Duration::from_secs(10), wall(), u, w),
            (false, None)
        );
        assert_eq!(
            c.take_closed_window(base + Duration::from_secs(59), w),
            None
        );
        assert_eq!(
            c.take_closed_window(base + Duration::from_secs(60), w),
            Some(1)
        );
    }

    #[test]
    fn unknown_origin_opens_only_the_tab_only_window() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        // An agent's HTTP create surfaces (no window is open) and opens only
        // the tab-only window...
        assert!(c.decide(base, wall(), SpawnOrigin::Unknown, w).0);
        assert_eq!(c.last_surface, None);
        assert_eq!(c.last_tab_only_surface, Some(base));
        // ...so a real gate continuation one second later still surfaces.
        let t1 = base + Duration::from_secs(1);
        assert!(c.decide(t1, wall(), SpawnOrigin::GateContinuation, w).0);
        // Inside THAT window an Unknown create is coalesced and counted.
        let t2 = base + Duration::from_secs(2);
        assert!(!c.decide(t2, wall(), SpawnOrigin::Unknown, w).0);
        assert_eq!(c.suppressed_total, 1);
        assert_eq!(c.last_surface, Some(t1));
    }
}
