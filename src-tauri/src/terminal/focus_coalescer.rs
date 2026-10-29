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
//! The rule here: across all NAMED unattended origins
//! ([`SpawnOrigin::is_autonomous`], every one except [`SpawnOrigin::Unknown`])
//! at most ONE view-switching decision surfaces per [`FOCUS_COALESCE_WINDOW`].
//! The first spawn of a burst still surfaces (the reason the focus request
//! exists — plan `2026-06-07-gate-continuation-auto-surface-and-focus`); the
//! rest appear as tabs without stealing the view. Operator origins always
//! surface and open no window.
//!
//! [`SpawnOrigin::Unknown`] — a caller this runner cannot name (an agent's
//! HTTP `POST /terminals`, a UI-Bridge or relay create) — has a window of its
//! OWN. An `Unknown` request surfaces only when neither the named window nor
//! the `Unknown` window is open, and its surface opens only the `Unknown`
//! window. So an all-`Unknown` burst is still held to one surface per
//! window, while an agent's own terminal create never suppresses the next
//! real gate continuation's surface.
//!
//! One decision per spawn. The tab-selection half rides `terminal-created`,
//! which the [`TerminalManager`](super::TerminalManager) emits at PTY create;
//! the main-view half is the later `terminal-focus-request`. The create-time
//! decision is recorded against the terminal id and the first focus request
//! for that id TAKES it, so the two halves can never disagree (a create that
//! surfaced is followed by its view switch; a suppressed create never gets
//! one). A REFOCUS (a duplicate anchor folded onto a live tab) only PEEKS the
//! recorded decision, so it can never consume the original's and hand that
//! terminal a fresh decision later. A peeked suppression stays suppressed; a
//! peeked surface is NOT replayed — the refocus is a new request and is
//! decided against the window as it stands now, as is one with nothing
//! recorded.
//!
//! A suppression is never silent: it is a `debug!`, a `/health`
//! `focusCoalescer.suppressedTotal` count, and — once per window that had
//! any — one `info!` `focus: coalesced N unattended focus requests`, logged at
//! the first decision or `/health` read after that window closed.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, info};

use crate::coord_drain_state::SpawnOrigin;

/// At most one unattended view switch per this window (D3).
pub const FOCUS_COALESCE_WINDOW: Duration = Duration::from_secs(60);

/// How long a create-time decision waits for its focus request. A
/// continuation's create -> start verdict was measured at up to 38 s for the
/// create alone; ten minutes covers any start wait, and an entry that is
/// never taken (a site that emits no focus request) ages out.
const PENDING_DECISION_TTL: Duration = Duration::from_secs(600);

/// Hard bound on recorded create-time decisions, oldest dropped first.
const PENDING_DECISION_CAP: usize = 512;

/// The pure decision: does a view-switching request from `origin` at `now`
/// surface, given the last unattended surface at `last_surface`?
///
/// Operator origins always surface. An unattended origin surfaces when no
/// unattended request has surfaced yet, or when at least `window` has elapsed
/// since the last one did.
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

/// The process-global coalescer state. Time is injected into every method so
/// the state machine is tested without a clock.
#[derive(Debug, Default)]
struct Coalescer {
    /// The instant the last NAMED unattended request surfaced (the named
    /// window; [`SpawnOrigin::Unknown`] never sets it).
    last_surface: Option<Instant>,
    /// Wall-clock twin of `last_surface`, for `/health`.
    last_surface_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The instant the last [`SpawnOrigin::Unknown`] request surfaced (the
    /// `Unknown` window).
    last_unknown_surface: Option<Instant>,
    /// Wall-clock twin of `last_unknown_surface`, for `/health`.
    last_unknown_surface_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Suppressed requests since boot.
    suppressed_total: u64,
    /// Suppressed requests while a window was open, not yet reported by the
    /// window-close `info!`.
    suppressed_in_window: u64,
    /// Create-time decisions awaiting their focus request: (terminal id,
    /// surfaced, decided at).
    pending: VecDeque<(String, bool, Instant)>,
}

impl Coalescer {
    /// If every open window (named and `Unknown`) has closed with
    /// suppressions in it, return that count and reset it — the caller logs
    /// it once.
    fn take_closed_window(&mut self, now: Instant, window: Duration) -> Option<u64> {
        let last = self.last_surface.max(self.last_unknown_surface)?;
        if self.suppressed_in_window == 0 || now.saturating_duration_since(last) < window {
            return None;
        }
        Some(std::mem::take(&mut self.suppressed_in_window))
    }

    /// Decide one request and update the state. Returns (surfaced, the
    /// closed-window count to log, if any).
    ///
    /// A NAMED unattended origin is checked against, and opens, the named
    /// window. [`SpawnOrigin::Unknown`] is checked against both windows and
    /// opens only its own (module doc). A suppressed request of either kind
    /// is counted.
    fn decide(
        &mut self,
        now: Instant,
        wall: chrono::DateTime<chrono::Utc>,
        origin: SpawnOrigin,
        window: Duration,
    ) -> (bool, Option<u64>) {
        let closed = self.take_closed_window(now, window);
        let unknown = origin == SpawnOrigin::Unknown;
        let surface = should_surface(now, self.last_surface, origin, window)
            && (!unknown || should_surface(now, self.last_unknown_surface, origin, window));
        if origin.is_autonomous() {
            if !surface {
                self.suppressed_total += 1;
                self.suppressed_in_window += 1;
            } else if unknown {
                self.last_unknown_surface = Some(now);
                self.last_unknown_surface_at = Some(wall);
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
    /// surface past the window that is open now.
    fn decide_for_refocus(
        &mut self,
        now: Instant,
        wall: chrono::DateTime<chrono::Utc>,
        origin: SpawnOrigin,
        terminal_id: &str,
        window: Duration,
    ) -> (bool, Option<u64>) {
        match self.peek_pending(terminal_id, now) {
            Some(false) => (false, self.take_closed_window(now, window)),
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
        "lastUnknownSurfaceAt": c.last_unknown_surface_at.map(|t| t.to_rfc3339()),
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
/// onto it) switches the view: the terminal's recorded create-time decision
/// while one is still pending (peeked, never taken), else a fresh one.
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

/// `/health` `focusCoalescer`. `lastSurfaceAt` is the named window's last
/// surface and `lastUnknownSurfaceAt` the `Unknown` window's; `null` means no
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
                "lastUnknownSurfaceAt": null,
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
        // An `Unknown` surface after the named window closed opens its own.
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
                "lastUnknownSurfaceAt": "2026-10-27T20:31:26.933+00:00",
            })
        );
    }

    #[test]
    fn health_json_wrapper_carries_every_key() {
        let v = health_json();
        assert_eq!(v["windowSecs"], 60);
        assert!(v["suppressedTotal"].is_u64(), "{v}");
        assert!(v.get("lastSurfaceAt").is_some(), "{v}");
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
        assert!(!c.decide_for_refocus(t2, wall(), o, "orig", w).0);
        // ...so `orig`'s own focus request still finds it and stays
        // suppressed (a fresh decision here would surface it).
        let t3 = base + Duration::from_secs(66);
        assert!(!c.decide_for_focus(t3, wall(), o, "orig", w).0);
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
        assert_eq!(c.last_unknown_surface, None);
        assert_eq!(c.suppressed_total, 1);
    }

    #[test]
    fn unknown_only_suppressions_are_reported_when_the_unknown_window_closes() {
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
    fn unknown_origin_is_coalesced_but_never_opens_a_window() {
        let base = Instant::now();
        let w = FOCUS_COALESCE_WINDOW;
        let mut c = Coalescer::default();
        // An agent's HTTP create surfaces (no window is open) but opens none...
        assert!(c.decide(base, wall(), SpawnOrigin::Unknown, w).0);
        assert_eq!(c.last_surface, None);
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
