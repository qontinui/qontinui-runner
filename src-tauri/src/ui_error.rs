//! UI error reporting surface for the runner frontend.
//!
//! Tracks the latest unhandled error observed by the React `ErrorBoundary`
//! at the top of the component tree (see `src/ErrorBoundary.tsx`). The
//! state is exposed to ops via two sinks:
//!
//! 1. The existing [`/health`](crate::mcp_api) endpoint, which now carries
//!    a `derived_status: "healthy" | "degraded" | "errored"` field alongside
//!    a nullable `ui_error` object. Supervisors and the qontinui-web fleet
//!    view poll this to flag runners whose Rust backend is up but whose UI
//!    is broken (errored) or whose embedding subsystem is unreachable
//!    (degraded).
//! 2. The runner's heartbeats (both the operations heartbeat in
//!    [`crate::heartbeat`] and the runner-fleet heartbeat in
//!    [`crate::server_mode`]). Each heartbeat payload includes the
//!    `derived_status` and `ui_error` fields so receivers can react
//!    without polling `/health` separately.
//!
//! Storage is in-memory only (no persistence). A single [`UiErrorState`]
//! lives on [`crate::commands::AppState`]. Reads go through a `RwLock`;
//! writes coalesce repeat occurrences of the same `message`/`digest` into
//! a single record with an incrementing `count` and a sliding
//! `reported_at` timestamp while preserving `first_seen`.
//!
//! # There is deliberately NO backend writer into this state
//!
//! **Decision recorded 2026-08-19**, plan
//! `2026-08-10-popout-webview2-creation-failure` Phase 3, after review of a
//! first cut that added one (`report_from_backend`, since deleted). The
//! function is gone; this is why, so nobody re-adds it as an obvious
//! convenience. `ui_error_has_exactly_one_writer` pins it as a test.
//!
//! `ui_error` is not a general "something went wrong in the UI" channel. It
//! means one specific thing — *the React error boundary at the top of the main
//! window's component tree caught an unhandled error* — and three properties
//! follow from that meaning, all of which a backend writer breaks:
//!
//! 1. **It is a latch with exactly one clear path, and that path is the
//!    frontend's.** The only clearer is [`clear_ui_error`], invoked from
//!    `src/ErrorBoundary.tsx`'s `componentDidUpdate` behind
//!    `prevState.hasError && !this.state.hasError`. A fresh mount starts
//!    `hasError === false`, so that transition can *never* fire for an error
//!    the boundary did not itself raise. Anything the backend writes here is
//!    therefore latched for the **lifetime of the process**.
//! 2. **Every consumer reads it as "the main window's tree is dead."**
//!    `mcp_api.rs`'s `/health` maps any `ui_error` to
//!    `derived_status: "errored"` (via [`compute_derived_status`]) and
//!    `mcp::ui_bridge::request` maps it to `FrontendState::TreeCrashed`, i.e.
//!    `frontendReady: false`. Downstream of that, qontinui-web's dispatcher
//!    refuses to auto-select the runner (503 `runner_unhealthy`), the runner
//!    disappears from every picker, and the supervisor attributes later dev
//!    actions as `D3Category::Contradiction`.
//! 3. **So a non-main webview failure written here is both a permanent latch
//!    and a lie** — it takes an otherwise-healthy runner out of the fleet's
//!    dispatch pool, forever, because one pop-out window failed. On the pop-out
//!    defect this plan is about, that would have broken the very UI-Bridge
//!    readiness the plan exists to restore.
//!
//! What the backend uses instead, per failure class:
//!
//! * **A window that was built without a webview** — the builder returns `Err`
//!   (`webview_recovery::verify_window_has_a_webview`), which is loud where it
//!   belongs: the interactive pop-out path logs `tracing::error!` and fails the
//!   `open_terminal_window` invoke back to the caller that asked for the
//!   window; boot-restore logs and skips; the *main* window's builder makes it
//!   fatal to startup and to the recovery ladder's terminal rung. D4's
//!   "loud, never silently successful" is satisfied by the `Err` + the log, not
//!   by a health latch.
//! * **A non-main webview that later dies** — an `error!` log line naming the
//!   window (`webview_recovery::attach_non_main_process_failed_handler`).
//! * **The MAIN window's UI dying** — already covered, and by a signal with the
//!   properties a health signal needs: [`ui_dead_now`] over
//!   `ui_bridge_last_pong`. It is **self-clearing** (the next pong retracts it)
//!   and it is **attributed to the right window** (only the main window's
//!   frontend pongs). That is the shape a backend-observed health signal has to
//!   have; `ui_error` is not it.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tokio::sync::RwLock;

/// A single unhandled frontend error.
///
/// Serialized as part of the `/health` response and every heartbeat payload
/// when `UiErrorState::get()` returns `Some`. `first_seen` is pinned to the
/// first report; `reported_at` slides forward on coalesced repeats. `count`
/// counts the number of `report()` calls that collapsed into this record.
#[derive(Debug, Clone, Serialize)]
pub struct UiError {
    /// `error.message` from the React error boundary. Always present.
    pub message: String,
    /// `error.stack` if available. Minified builds may omit this.
    pub stack: Option<String>,
    /// React's `ErrorInfo.componentStack`, if available.
    pub component_stack: Option<String>,
    /// React 18+ production error digest (when the render error was from a
    /// minified bundle). Used as the coalescing key when both sides have one.
    pub digest: Option<String>,
    /// When the error first fired (pinned — not updated by repeat reports).
    pub first_seen: DateTime<Utc>,
    /// When the most recent matching report fired. Updated each coalesce.
    pub reported_at: DateTime<Utc>,
    /// Number of reports that collapsed into this record (>= 1).
    pub count: u32,
}

/// Shared in-memory holder for the current UI error, if any.
///
/// Wraps a single-slot `Option<UiError>`. Concurrent reads are cheap
/// (`RwLock::read`); writes serialize through `RwLock::write`. Construct
/// exactly one instance on [`crate::commands::AppState`].
#[derive(Debug, Default)]
pub struct UiErrorState {
    inner: Arc<RwLock<Option<UiError>>>,
}

impl UiErrorState {
    /// Create an empty state. Equivalent to `Default::default()`; provided
    /// so callers can be explicit at the AppState construction site.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Record a UI error.
    ///
    /// If an existing record matches (same `digest` when both records have
    /// one, otherwise same `message`), increments `count` and updates
    /// `reported_at` + the latest `stack`/`component_stack` snapshot, but
    /// keeps `first_seen`. Otherwise replaces the slot with a fresh
    /// record whose `first_seen = reported_at = now` and `count = 1`.
    pub async fn report(
        &self,
        message: String,
        stack: Option<String>,
        component_stack: Option<String>,
        digest: Option<String>,
    ) {
        let now = Utc::now();
        let mut guard = self.inner.write().await;

        let matches = guard
            .as_ref()
            .map(|existing| matches_existing(existing, &message, digest.as_deref()))
            .unwrap_or(false);

        if matches {
            if let Some(existing) = guard.as_mut() {
                existing.count = existing.count.saturating_add(1);
                existing.reported_at = now;
                // Keep first_seen pinned. Refresh the freshest snapshot of
                // optional fields in case the new report carries more
                // context than the first one did.
                if stack.is_some() {
                    existing.stack = stack;
                }
                if component_stack.is_some() {
                    existing.component_stack = component_stack;
                }
                if digest.is_some() {
                    existing.digest = digest;
                }
                // message intentionally not overwritten — matching key.
            }
        } else {
            *guard = Some(UiError {
                message,
                stack,
                component_stack,
                digest,
                first_seen: now,
                reported_at: now,
                count: 1,
            });
        }
    }

    /// Wipe the current record. Called by the frontend error boundary when
    /// it recovers from an error state.
    pub async fn clear(&self) {
        let mut guard = self.inner.write().await;
        *guard = None;
    }

    /// Read a clone of the current record, if any. Cheap for hot paths
    /// like `/health` and heartbeats (clone is a few small fields).
    pub async fn get(&self) -> Option<UiError> {
        self.inner.read().await.clone()
    }
}

/// Determine whether an incoming report should coalesce into `existing`.
///
/// Priority: if both sides carry a non-empty `digest`, match on that
/// (React 18+ production builds). Otherwise fall back to `message`
/// equality. An empty digest on either side falls back to message match.
fn matches_existing(existing: &UiError, message: &str, digest: Option<&str>) -> bool {
    match (existing.digest.as_deref(), digest) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => a == b,
        _ => existing.message == message,
    }
}

// ---------------------------------------------------------------------------
// UI liveness predicate
// ---------------------------------------------------------------------------

/// Diagnostics rung: how long since the last frontend pong before the
/// UI-Bridge diagnostics surfaces call the frontend `Stale`.
///
/// This is the pre-existing 30s threshold that
/// `crate::mcp::ui_bridge::request::classify_frontend_state` has always
/// applied — extracted here so there is exactly one definition of it.
pub const UI_STALE_AFTER_MS: u64 = 30_000;

/// Status rung: how long since the last frontend pong before
/// [`compute_derived_status`] calls the UI dead and reports `errored`.
///
/// Deliberately slacker than [`UI_STALE_AFTER_MS`] because this rung can
/// trigger recovery, and a status that flaps would drive a recovery loop.
/// Rust emits `ui-bridge-ping` unconditionally every 3s (`mcp_api.rs`
/// startup wiring) and the frontend answers `ui-bridge-pong`, so `last_pong`
/// advances on its own whenever any UI is alive. 90s is therefore **30
/// consecutive missed pings** — far outside anything a GC pause or a busy
/// main thread can produce.
///
/// The ordering `UI_STALE_AFTER_MS < UI_DEAD_AFTER_MS` is load-bearing: the
/// two calibrations may only ever escalate in one direction, so diagnostics
/// say `Stale` well before status says dead. A drift-guard test locks it in.
pub const UI_DEAD_AFTER_MS: u64 = 90_000;

/// Was a UI alive, and has it stopped checking in?
///
/// The single shared staleness predicate over the `ui_bridge_last_pong`
/// atomic. Both the diagnostics ladder (`classify_frontend_state`, at
/// [`UI_STALE_AFTER_MS`]) and the status ladder ([`compute_derived_status`],
/// at [`UI_DEAD_AFTER_MS`]) call it, so the two surfaces cannot drift into
/// disagreeing about whether the same frontend is alive.
///
/// # The `last_pong == 0` guard
///
/// `last_pong == 0` means **no UI has ever checked in**, which is not the
/// same thing as a UI that died — so it returns `false` (not stale). Three
/// real cases live in that window:
///
/// 1. **Server mode.** `QONTINUI_SERVER_MODE` (`launch_env.rs`,
///    `LaunchEnv::server_mode`) makes `main.rs:3319` log "Skipping main
///    window creation (server mode)" and skip the `WebviewWindowBuilder`
///    branch entirely. Such a runner never mounts a webview, never pongs,
///    and holds `last_pong == 0` for its whole process lifetime. This is the
///    primary real-world reason the guard exists: without it every
///    server-mode instance would report `errored` forever.
/// 2. **Boot.** Every windowed runner spends its first seconds here, before
///    React mounts and the SDK answers the first ping.
/// 3. **Failed window creation.** If the window never came up there is
///    nothing to declare dead; the `WindowMissing` / `NeverPonged` rungs of
///    `classify_frontend_state` describe that case far better than a
///    staleness verdict would.
///
/// `after_ms` picks the calibration; pass one of the two constants above
/// rather than a literal.
pub fn ui_stale(last_pong: u64, pong_age_ms: u64, after_ms: u64) -> bool {
    last_pong > 0 && pong_age_ms > after_ms
}

/// [`ui_stale`] at [`UI_DEAD_AFTER_MS`], reading the pong stamp straight off
/// `AppState::ui_bridge_last_pong` and the age off the wall clock.
///
/// For the status sinks (the heartbeat loops) that do not already have a
/// `pong_age_ms` in scope, so the NTP-safe age arithmetic is written once.
/// `last_pong` is a wall-clock stamp, so a backwards clock step (NTP
/// correction, sleep/resume) can leave it AHEAD of now; `saturating_sub` reads
/// a pong from the "future" as age 0 — maximally fresh, the honest answer —
/// where a plain subtraction would underflow and panic.
pub fn ui_dead_now(last_pong: &std::sync::atomic::AtomicU64) -> bool {
    let last_pong = last_pong.load(std::sync::atomic::Ordering::Relaxed);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let pong_age_ms = if last_pong > 0 {
        now_ms.saturating_sub(last_pong)
    } else {
        0
    };
    ui_stale(last_pong, pong_age_ms, UI_DEAD_AFTER_MS)
}

// ---------------------------------------------------------------------------
// Pong provenance + native UI-thread liveness
// ---------------------------------------------------------------------------

/// How long since the last **event-provenance** pong (see
/// [`record_event_pong`]) before the native event loop is judged to have
/// stopped delivering events to a renderer that is still demonstrably alive.
///
/// Deliberately the same number as [`UI_DEAD_AFTER_MS`], and deliberately a
/// separate constant: it measures a *different fact* (the loop stopped
/// delivering) off a *different stamp*, and the two calibrations must be free
/// to move apart without one silently dragging the other. The 3s
/// `ui-bridge-ping` cadence makes this 30 consecutive undelivered pings.
pub const UI_EVENT_DEAD_AFTER_MS: u64 = UI_DEAD_AFTER_MS;

/// Wall-clock ms of the last pong whose arrival PROVES the native event loop
/// pumped — 0 until the first one.
///
/// # Why this atom exists (the 2026-08-19 finding)
///
/// `AppState::ui_bridge_last_pong` serves **two independent liveness facts
/// through one slot**, and that is the defect. The frontend answers
/// `ui-bridge-ping` over a Tauri event, but it ALSO runs an unconditional 3s
/// HTTP pong as a safety net against a WebView2 JS→Rust IPC failure
/// (`src/hooks/useUIBridgeEventHandler.ts`). WebView2 services `fetch` in the
/// browser/network process, **not** on the host's UI thread — so during a
/// native message-loop hang with a live renderer the HTTP pong keeps landing,
/// `ui_bridge_last_pong` stays fresh, [`ui_stale`] never fires and
/// `derived_status` stays `healthy` forever. There was no detection floor at
/// all for that failure, not a 90s one.
///
/// This stamp advances **only** on evidence that the loop delivered something
/// to the renderer:
///
/// * the `ui-bridge-pong` Tauri event listener (round trip both ways);
/// * `POST /ui-bridge/pong?source=event` — the ping WAS delivered, only the
///   JS→Rust return leg fell back to HTTP (the very failure the safety net
///   exists for, so its provenance must survive the fallback);
/// * `ui-bridge-response` / `POST /ui-bridge/ipc-response` — a
///   `ui-bridge-request` emitted through the loop reached the renderer and
///   was answered.
///
/// The unconditional safety-net pong (`?source=safety-net`, and any unlabeled
/// `POST /ui-bridge/pong` from an older frontend or an external caller) does
/// **not** advance it: arriving over HTTP proves the renderer is alive and
/// nothing at all about the native loop.
///
/// # Reading the two stamps together
///
/// * native loop wedged, renderer alive → **event pong stale, any-pong
///   fresh** → this bug (2026-08-19). Only the split can express it.
/// * renderer / browser process dead (the 2026-08-01 mode) → **both stale** →
///   [`ui_stale`] fires exactly as it already did; provenance changes nothing.
/// * loop healthy → both fresh.
///
/// A process-global rather than an `AppState` field on purpose: the one
/// detector proved to keep running through a wedge is
/// [`crate::health_monitor`]'s dedicated OS thread, which holds no
/// `AppState` and no runtime. A liveness fact only an `AppState` holder can
/// read is unreadable exactly when it matters.
static LAST_EVENT_PONG_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Stamp an **event-provenance** pong. See [`LAST_EVENT_PONG_MS`] for which
/// call sites qualify — and, more importantly, which do not.
pub fn record_event_pong() {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    LAST_EVENT_PONG_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
}

/// The last event-provenance pong stamp (ms since epoch), or 0 if none has
/// ever arrived. 0 is UNKNOWN — never "the loop is dead"; see [`ui_stale`]'s
/// `last_pong == 0` guard, which this reuses verbatim.
pub fn last_event_pong() -> u64 {
    LAST_EVENT_PONG_MS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Age in ms of the last event-provenance pong, read off the wall clock.
///
/// `0` when none has ever landed — the caller pairs it with
/// [`last_event_pong`] so [`ui_stale`]'s never-seen guard can distinguish
/// UNKNOWN from fresh. `saturating_sub` for the same NTP reason as
/// [`ui_dead_now`].
pub fn last_event_pong_age_ms() -> u64 {
    let stamp = last_event_pong();
    if stamp == 0 {
        return 0;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now_ms.saturating_sub(stamp)
}

/// Everything the native-UI-thread verdict is computed from. A struct, like
/// `crate::mcp::ui_bridge::request::FrontendStateInputs`, so four sinks
/// cannot transpose same-typed positional arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeUiInputs {
    /// The Win32 `SendMessageTimeoutW` rung, read from
    /// [`crate::health_monitor::ui_thread_wedged`] — a plain cached atomic,
    /// safe from an async handler. `None` = the probe cannot run here
    /// (non-Windows, or no main-window HWND cached yet) and is UNKNOWN, never
    /// "fine". Use [`native_ui_probe_verdict`] to read it.
    pub probe_wedged: Option<bool>,
    /// This caller's OWN bounded window-getter round-trip timed out just now
    /// (`window_probe::WINDOW_GETTER_TIMEOUT`). Only `/health` sets it: having
    /// just watched its own liveness getter time out, reporting `healthy`
    /// would be a fresh lie of exactly the kind this change removes.
    pub window_getter_unresponsive: bool,
    /// `AppState::ui_bridge_last_pong` — a pong of ANY provenance. Proves the
    /// renderer is alive; proves nothing about the native loop.
    pub last_pong: u64,
    /// Age of `last_pong`. Meaningless when `last_pong == 0`.
    pub pong_age_ms: u64,
    /// [`last_event_pong`] — the provenance-bearing stamp.
    pub last_event_pong: u64,
    /// Age of `last_event_pong`. Meaningless when it is 0.
    pub event_pong_age_ms: u64,
}

/// One verdict on the native message loop, with every component preserved so
/// the surfaces can report *why* rather than just *that*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeUiLiveness {
    /// [`NativeUiInputs::probe_wedged`], passed through for reporting.
    pub probe_wedged: Option<bool>,
    /// [`NativeUiInputs::window_getter_unresponsive`], passed through.
    pub window_getter_unresponsive: bool,
    /// The renderer is demonstrably alive AND no event-provenance pong has
    /// landed inside [`UI_EVENT_DEAD_AFTER_MS`]. This is the pong-provenance
    /// reading of the 2026-08-19 failure, and it is cross-platform — the
    /// Win32 probe is not.
    pub events_undelivered: bool,
    /// Age of the last event-provenance pong; `None` = none ever seen
    /// (boot, server mode, or a frontend that has never been pinged).
    pub event_pong_age_ms: Option<u64>,
    /// The verdict fed to [`compute_derived_status`] as its `native_ui_wedged`
    /// input.
    pub wedged: bool,
    /// Stable machine-readable reason. `"pumping"` and `"unknown"` are the two
    /// non-wedged values; do not reword these, fleet consumers match on them.
    pub reason: &'static str,
}

/// Classify the native message loop. Pure — no clock, no atomics, no runtime.
///
/// Ordering of the wedged reasons is by directness of evidence: the Win32
/// round-trip asks the loop itself, the caller's own timed-out getter is a
/// single sample of the same thing, and event-pong staleness is the
/// downstream, cross-platform inference.
pub fn classify_native_ui(i: NativeUiInputs) -> NativeUiLiveness {
    // The renderer answered recently over SOME path, so it is alive. Only
    // then is a missing event-pong evidence about the LOOP rather than about
    // the frontend being gone (which `ui_dead` already covers, and which must
    // keep reporting as the 2026-08-01 mode, not as this one).
    let renderer_alive = i.last_pong > 0 && !ui_stale(i.last_pong, i.pong_age_ms, UI_DEAD_AFTER_MS);
    // `ui_stale`'s `> 0` guard is doing real work here: a frontend that has
    // never produced an event-provenance pong is UNKNOWN (booting, server
    // mode, no ping loop), never wedged.
    let events_stale = ui_stale(
        i.last_event_pong,
        i.event_pong_age_ms,
        UI_EVENT_DEAD_AFTER_MS,
    );
    let events_undelivered = renderer_alive && events_stale;

    let reason = if i.probe_wedged == Some(true) {
        "native_probe_wedged"
    } else if i.window_getter_unresponsive {
        "window_getter_unresponsive"
    } else if events_undelivered {
        "events_undelivered"
    } else if i.probe_wedged == Some(false) {
        "pumping"
    } else {
        "unknown"
    };

    NativeUiLiveness {
        probe_wedged: i.probe_wedged,
        window_getter_unresponsive: i.window_getter_unresponsive,
        events_undelivered,
        event_pong_age_ms: (i.last_event_pong > 0).then_some(i.event_pong_age_ms),
        wedged: i.probe_wedged == Some(true) || i.window_getter_unresponsive || events_undelivered,
        reason,
    }
}

/// Read the Win32 rung without blocking: the cached
/// [`crate::health_monitor::ui_thread_wedged`] atomic, gated on the probe
/// being able to run at all.
///
/// **Never** call `health_monitor::ui_thread_pumping()` from an async sink —
/// it is a blocking `SendMessageTimeoutW` with a 3s ceiling, so an `async fn`
/// calling it parks a tokio worker for 3s during exactly the hang it is
/// reporting. That is the bug this phase removes from `/health`, not one to
/// re-introduce. The 3-consecutive-sample escalation behind the atom also
/// keeps a single long repaint from flapping the fleet's status.
pub fn native_ui_probe_verdict() -> Option<bool> {
    // No cached HWND means nothing to ask (non-Windows stub, server mode, or
    // pre-window startup). UNKNOWN, not healthy.
    crate::ui_thread_probe::main_hwnd()?;
    // A stopped monitor never clears the atom either, so `false` from it would
    // be a claim nobody made. UNKNOWN is the honest reading — same rule as the
    // missing handle above.
    if !crate::health_monitor::is_running() {
        return None;
    }
    Some(crate::health_monitor::ui_thread_wedged())
}

/// [`classify_native_ui`] against the live atomics and the wall clock, for the
/// sinks that do not already hold a `last_pong` / `pong_age_ms` pair.
///
/// `saturating_sub` for the same reason [`ui_dead_now`] uses it: a backwards
/// clock step can leave a stamp ahead of `now`, and age 0 (maximally fresh) is
/// the honest reading where a plain subtraction would panic.
pub fn native_ui_liveness_now(last_pong: &std::sync::atomic::AtomicU64) -> NativeUiLiveness {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let last_pong = last_pong.load(std::sync::atomic::Ordering::Relaxed);
    let last_event_pong = last_event_pong();
    classify_native_ui(NativeUiInputs {
        probe_wedged: native_ui_probe_verdict(),
        window_getter_unresponsive: false,
        last_pong,
        pong_age_ms: now_ms.saturating_sub(last_pong),
        last_event_pong,
        event_pong_age_ms: now_ms.saturating_sub(last_event_pong),
    })
}

// ---------------------------------------------------------------------------
// Derived-status helper
// ---------------------------------------------------------------------------

/// Compute the runner's overall `derived_status` from its sub-signals.
///
/// Priority (highest wins): any `errored` signal → "errored"; otherwise any
/// `degraded` signal → "degraded"; otherwise "healthy".
///
/// Inputs:
/// * `has_ui_error` — true when a React `ErrorBoundary` report is outstanding.
/// * `has_recent_crash` — true when a fresh Rust crash dump was surfaced at
///   startup (non-unwinding panics abort before React sees them).
/// * `ui_dead` — the frontend was alive and has stopped checking in, i.e.
///   [`ui_stale`] at [`UI_DEAD_AFTER_MS`]. `None` = this sink cannot tell
///   (treated as unknown, never as dead). `Some(true)` = errored.
///
///   This is `errored`, not `degraded`, on purpose. `degraded` is already the
///   occupied meaning for "a subsystem is out but the app still works"
///   (embedding service, PG) — a desktop app whose window is dead is not that
///   case, and overloading the class would make `degraded` mean two unrelated
///   things. `errored` is also the only class that routes to a
///   recovery-eligible state, so it is what makes an automated response
///   expressible at all. Consumers that care whether the automation paths
///   survived can still read `ui_error` / `recent_crash` / `responsive`,
///   which are all published alongside this field.
///
///   The failure this input exists for: a WebView2 browser-process crash
///   kills the UI host while the Rust backend keeps serving, so `ui_error`
///   (set by the React error boundary) is structurally unavailable and
///   `/health` reported `healthy` with a dead window for 19 hours.
/// * `native_ui_wedged` — the native message loop has stopped pumping, from
///   [`native_ui_liveness_now`] / [`classify_native_ui`]. `None` = this sink
///   cannot tell; `Some(true)` = errored.
///
///   **A DISTINCT input from `ui_dead`, and it must stay distinct.** `ui_dead`
///   reads `ui_bridge_last_pong`, which the frontend keeps fresh over an
///   unconditional 3s HTTP `fetch` that WebView2 services in its browser
///   process — off the host's UI thread entirely. So during the 2026-08-19
///   failure (native loop hung, renderer alive, `Responding: False`, `/health`
///   still `200`) `ui_dead` is `false` for as long as the hang lasts, and was
///   the ONLY UI-liveness input this function had. Folding the native signal
///   into `ui_dead` would have made a wedged loop indistinguishable from a
///   dead webview and left it hostage to the same pong; as its own input,
///   `derived_status` can go `errored` on the native evidence **without
///   depending on the pong at all**.
/// * `embedding_reachable` — `None` until the first probe has run (treated as
///   unknown, not degraded — avoids false positives during boot). `Some(true)`
///   = healthy. `Some(false)` = degraded.
/// * `relay_connected` — whether the backend WS relay currently holds an
///   open, post-handshake connection to qontinui-web. `None` when the relay
///   is legitimately idle (tier below `qontinui_account`, or
///   `web_integration.enabled = false`, or settings unreadable): a runner
///   deliberately configured local-only must NOT read as degraded, and an
///   UNKNOWN must not be reported as a fault. `Some(false)` = the relay is
///   gated ON but not connected → degraded, never `errored`: local
///   execution is unaffected, so this is a subsystem out while the app still
///   works — exactly the class `degraded` already means.
///
///   The failure this input exists for: a relay rejected at registration
///   reconnects forever on its backoff without ever changing any published
///   status, so `/health` reported `healthy` for THREE DAYS while the runner
///   was unreachable from every cloud client and its workflow events were
///   being dropped. `ws_connected` existed the whole time — on
///   `/web-integration/status`, which nothing polls. Plan
///   `2026-08-25-coord-jwt-kid-collides-across-environments`.
/// * `pg_reachable` — bounded PG liveness (a `SELECT 1` with the deadpool
///   `get()` timeout). `None` when not probed (e.g. the heartbeat sinks that
///   don't run a DB round-trip, or a runner with no PG configured) — treated
///   as unknown, not degraded. `Some(false)` = the data layer is unreachable →
///   degraded, so `/health` stops reporting "healthy" while every PG-backed
///   panel is dead (iter4 B-5).
///
/// All four sinks (`/health`, the operations heartbeat, the web-backend
/// heartbeat relay, and the UI-Bridge diagnostics surfaces) call this so their
/// `derived_status` stays in lockstep. Only the `/health` handler passes a
/// probed `pg_reachable`; the others pass `None`. All four DO pass a real
/// `ui_dead` — the heartbeat sinks especially, since nothing polls `/health`
/// on an end user's machine and the heartbeats are the only path by which a
/// dead UI is ever visible off-box. All four also pass a real
/// `native_ui_wedged`, for the same reason and with more force: that failure
/// has no pong-based floor under it at all.
/// The sub-signals [`compute_derived_status`] weighs.
///
/// A struct rather than a positional argument list because the inputs are
/// now five consecutive, mutually interchangeable `Option<bool>`s:
/// transposing two of them compiles silently and yields a wrong health
/// verdict, on the one function every fleet probe trusts. Named fields make
/// that a compile error instead.
///
/// `Default` is all-`None`/`false` — "this sink cannot tell" — which is also
/// the correct value for any signal a caller does not probe, so a sink adds
/// only the fields it actually measures and a new signal does not touch
/// every existing call site.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HealthInputs {
    /// The React error boundary reported an error.
    pub has_ui_error: bool,
    /// A crash dump was found at startup.
    pub has_recent_crash: bool,
    /// UI liveness from `last_pong`. See [`ui_stale`].
    pub ui_dead: Option<bool>,
    /// The native message loop stopped pumping. A DISTINCT input from
    /// `ui_dead`: the frontend's unconditional 3s HTTP pong is serviced by
    /// WebView2's browser process, so `ui_dead` stays `Some(false)` for the
    /// whole hang. `None` where the sink cannot probe it.
    pub native_ui_wedged: Option<bool>,
    /// Embedding-service reachability; `None` until the first probe.
    pub embedding_reachable: Option<bool>,
    /// Bounded PG liveness; `None` when not probed.
    pub pg_reachable: Option<bool>,
    /// Backend WS relay liveness; `None` when no relay is expected.
    pub relay_connected: Option<bool>,
}

pub fn compute_derived_status(i: &HealthInputs) -> &'static str {
    if i.has_ui_error
        || i.has_recent_crash
        || matches!(i.ui_dead, Some(true))
        || matches!(i.native_ui_wedged, Some(true))
    {
        "errored"
    } else if matches!(i.embedding_reachable, Some(false))
        || matches!(i.pg_reachable, Some(false))
        || matches!(i.relay_connected, Some(false))
    {
        "degraded"
    } else {
        "healthy"
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// Report a UI error observed by the React error boundary.
///
/// JS call shape (Tauri converts camelCase at the top-arg level):
/// `invoke("report_ui_error", { message, stack, componentStack, digest })`
#[tauri::command]
pub async fn report_ui_error(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
    message: String,
    stack: Option<String>,
    component_stack: Option<String>,
    digest: Option<String>,
) -> Result<(), String> {
    app_state
        .ui_error
        .report(message, stack, component_stack, digest)
        .await;
    Ok(())
}

/// Clear the current UI error state (called when the boundary recovers).
#[tauri::command]
pub async fn clear_ui_error(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<(), String> {
    app_state.ui_error.clear().await;
    Ok(())
}

/// Read the current UI error state. Useful for debugging and as an
/// allowlisted target for the Phase 3I UI Bridge invoke proxy.
#[tauri::command]
pub async fn get_ui_error(
    app_state: tauri::State<'_, Arc<crate::commands::AppState>>,
) -> Result<Option<UiError>, String> {
    Ok(app_state.ui_error.get().await)
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_ui_error")
        .invoke_handler(tauri::generate_handler![
            report_ui_error,
            clear_ui_error,
            get_ui_error,
        ])
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_report_sets_count_and_timestamps() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        let got = state.get().await.expect("state should be populated");
        assert_eq!(got.message, "boom");
        assert_eq!(got.count, 1);
        assert_eq!(got.first_seen, got.reported_at);
    }

    #[tokio::test]
    async fn repeat_same_message_coalesces() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        let first_seen = state.get().await.unwrap().first_seen;

        // Small gap so reported_at moves forward measurably.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        state
            .report("boom".to_string(), Some("stack".into()), None, None)
            .await;

        let got = state.get().await.unwrap();
        assert_eq!(got.count, 2);
        assert_eq!(got.first_seen, first_seen);
        assert!(got.reported_at >= first_seen);
        assert_eq!(got.stack.as_deref(), Some("stack"));
    }

    #[tokio::test]
    async fn different_message_replaces_record() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        state.report("other".to_string(), None, None, None).await;
        let got = state.get().await.unwrap();
        assert_eq!(got.message, "other");
        assert_eq!(got.count, 1);
    }

    #[tokio::test]
    async fn digest_is_preferred_coalescing_key() {
        let state = UiErrorState::new();
        state
            .report(
                "minified error #185".to_string(),
                None,
                None,
                Some("185".into()),
            )
            .await;
        // Different message but same digest -> coalesce.
        state
            .report(
                "different minified message".to_string(),
                None,
                None,
                Some("185".into()),
            )
            .await;
        let got = state.get().await.unwrap();
        assert_eq!(got.count, 2);
        // Original message is kept (matching key).
        assert_eq!(got.message, "minified error #185");
    }

    #[tokio::test]
    async fn clear_wipes_state() {
        let state = UiErrorState::new();
        state.report("boom".to_string(), None, None, None).await;
        state.clear().await;
        assert!(state.get().await.is_none());
    }

    #[test]
    fn derived_status_errored_wins_over_everything() {
        assert_eq!(
            compute_derived_status(&HealthInputs {
                has_ui_error: true,
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
        assert_eq!(
            compute_derived_status(&HealthInputs {
                has_ui_error: true,
                has_recent_crash: true,
                ui_dead: Some(false),
                embedding_reachable: Some(false),
                pg_reachable: Some(false),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
        assert_eq!(
            compute_derived_status(&HealthInputs {
                has_recent_crash: true,
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
    }

    #[test]
    fn derived_status_degraded_when_embedding_unreachable() {
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(false),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "degraded"
        );
    }

    #[test]
    fn derived_status_degraded_when_pg_unreachable() {
        // iter4 B-5: PG down while everything else is fine → degraded, so
        // /health can no longer report "healthy" over a dead data layer.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(false),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "degraded"
        );
    }

    #[test]
    fn derived_status_healthy_when_embedding_reachable() {
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy"
        );
    }

    #[test]
    fn derived_status_unknown_embedding_is_healthy_not_degraded() {
        // Boot-time: probe hasn't run yet. Avoid false-positive degraded.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy"
        );
    }

    #[test]
    fn derived_status_unknown_pg_is_healthy_not_degraded() {
        // No PG probe (heartbeat sinks, or no PG configured) must not
        // false-positive to degraded.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy"
        );
    }

    // -----------------------------------------------------------------------
    // UI liveness (`ui_stale` + the `ui_dead` input)
    // -----------------------------------------------------------------------

    /// Wall-clock stamp standing in for a frontend that has ponged at least
    /// once. Only its non-zero-ness matters to [`ui_stale`].
    const SOME_PONG: u64 = 1_700_000_000_000;

    #[test]
    fn ui_stale_never_seen_is_not_stale_headless_server_mode_guard() {
        // `last_pong == 0` means no UI has EVER checked in. The highest-
        // consequence case is `QONTINUI_SERVER_MODE`: `main.rs:3319` skips
        // main-window creation entirely, so such a runner never mounts a
        // webview and holds `last_pong == 0` for its whole process lifetime.
        // Without this guard EVERY server-mode instance would be classified
        // `errored` forever — the worst regression this change could cause.
        // Boot (pre-first-pong) and failed window creation land here too.
        for age in [0, 1, UI_STALE_AFTER_MS + 1, UI_DEAD_AFTER_MS + 1, u64::MAX] {
            assert!(
                !ui_stale(0, age, UI_STALE_AFTER_MS),
                "last_pong == 0 must never read as stale (age {age})"
            );
            assert!(
                !ui_stale(0, age, UI_DEAD_AFTER_MS),
                "last_pong == 0 must never read as dead (age {age})"
            );
        }
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy",
            "a runner that never mounted a webview is healthy, not errored"
        );
    }

    #[test]
    fn derived_status_healthy_when_ui_alive_and_fresh() {
        assert!(!ui_stale(SOME_PONG, 500, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy"
        );
    }

    #[test]
    fn derived_status_errored_when_ui_was_alive_then_went_stale() {
        assert!(ui_stale(SOME_PONG, UI_DEAD_AFTER_MS + 1, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(true),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
    }

    #[test]
    fn derived_status_dead_ui_outranks_degraded_pg() {
        // A dead window is not "a subsystem is out but the app works".
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(true),
                embedding_reachable: Some(true),
                pg_reachable: Some(false),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(true),
                embedding_reachable: Some(false),
                pg_reachable: Some(false),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
    }

    #[test]
    fn derived_status_unknown_ui_liveness_is_unchanged_from_before() {
        // `None` = this sink cannot tell. Guards the pre-change behavior for
        // any non-probing caller: identical verdicts to `Some(false)`.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy"
        );
        assert_eq!(
            compute_derived_status(&HealthInputs {
                embedding_reachable: Some(false),
                ..Default::default()
            }),
            "degraded"
        );
        assert_eq!(
            compute_derived_status(&HealthInputs {
                has_ui_error: true,
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored"
        );
    }

    /// The 2026-08-01 incident payload, replayed verbatim. This case reads
    /// `healthy` on `origin/main` — that is the whole defect.
    ///
    /// WebView2's browser process died (`msedgewebview2.exe`, `msedge.dll`
    /// `STATUS_BREAKPOINT`). The window went blank while the Rust backend kept
    /// serving, so `ui_error` was null (a crashed browser process cannot run
    /// the React error boundary) and `recent_crash` was null (the startup-only
    /// Rust dump scanner cannot see a mid-run WebView2 Crashpad dump). The one
    /// signal that stayed correct was `last_pong`: set, then ~16 minutes stale.
    #[test]
    fn derived_status_regression_2026_08_01_dead_webview_read_healthy() {
        let pong_age_ms = 16 * 60 * 1_000; // ~16 min, as observed
        let ui_dead = ui_stale(SOME_PONG, pong_age_ms, UI_DEAD_AFTER_MS);
        assert!(ui_dead, "a 16-minute-old pong is well past the dead rung");
        assert_eq!(
            compute_derived_status(&HealthInputs {
                // ui_error and recent_crash were both null -- a crashed
                // browser process cannot run the React error boundary, and the
                // startup-only dump scanner cannot see a mid-run WebView2
                // Crashpad dump. Left at their `false` defaults.
                ui_dead: Some(ui_dead), // the only signal that stayed correct
                // The native probe is UNKNOWN in this replay, and that is the
                // point: the 2026-08-01 mode is caught by the pong rung alone,
                // exactly as it already was. Provenance changes nothing here —
                // a dead browser process pongs on NEITHER path. Left at its
                // `None` default.
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "errored",
            "the dead-webview incident must no longer report healthy"
        );
    }

    /// The 2026-08-25 incident, replayed. This case reads `healthy` on
    /// `origin/main` -- that is the whole defect.
    ///
    /// The backend relay was rejected at registration and reconnected on its
    /// backoff for THREE DAYS. Nothing else was wrong: the UI was alive, the
    /// embedding service and PG were reachable, no crash, no boundary error.
    /// So `/health` said `healthy` while the runner was unreachable from
    /// every cloud client and was silently dropping its workflow events.
    #[test]
    fn derived_status_regression_2026_08_25_dead_relay_read_healthy() {
        assert_eq!(
            compute_derived_status(&HealthInputs {
                // Nothing else was wrong: no boundary error, no crash, UI
                // alive, embedding and PG reachable.
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(false), // the only signal that was wrong
                ..Default::default()
            }),
            "degraded",
            "a relay gated ON but not connected must not report healthy"
        );
    }

    #[test]
    fn a_relay_that_is_off_by_configuration_is_not_a_fault() {
        // `None` = no relay expected (tier below qontinui_account, or
        // web_integration disabled, or the runner simply not paired yet). A
        // runner deliberately configured local-only must never read degraded
        // for a relay it was never meant to have -- which matters because
        // qontinui-web gates dispatch on `derived_status == "healthy"`, so a
        // false `degraded` silently removes it from the auto-dispatch pool.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: None,
                ..Default::default()
            }),
            "healthy"
        );
    }
    #[test]
    fn a_dead_relay_degrades_but_never_errors() {
        // `degraded` is the right class, but NOT because it keeps the runner
        // dispatchable -- it does not. qontinui-web gates dispatch on
        // `derived_status == "healthy"` (workflow_dispatcher `_pick_auto_runner`
        // and `RunOnRunnerButton`), so `degraded` already removes it from the
        // auto-dispatch pool and the picker. That is CORRECT here: dispatch
        // rides the very relay that is down.
        //
        // The distinction `degraded` buys is against `errored`, which is the
        // recovery-eligible class -- it routes to automated recovery and reads
        // as "this runner is broken". A runner whose relay is down still
        // executes local work correctly, so it is a subsystem out while the app
        // works: exactly what `degraded` already means for the embedding
        // service and PG.
        let with_relay_down = compute_derived_status(&HealthInputs {
            ui_dead: Some(false),
            embedding_reachable: Some(true),
            pg_reachable: Some(true),
            relay_connected: Some(false),
            ..Default::default()
        });
        assert_ne!(with_relay_down, "errored");
        assert_eq!(with_relay_down, "degraded");

        // ...and it must not MASK a real error either.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                has_ui_error: true,
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(false),
                ..Default::default()
            }),
            "errored",
            "a genuine UI error still wins over relay degradation"
        );
    }

    #[test]
    fn ui_stale_thresholds_cannot_drift_past_each_other() {
        // The invariant the single-predicate refactor exists to protect: the
        // diagnostics rung must always fire BEFORE the status rung, so the two
        // surfaces can only escalate in one direction. The paired
        // `FrontendState::Stale`-vs-`derived_status`-healthy half of this
        // guard lives in `mcp::ui_bridge::request`'s tests, where
        // `classify_frontend_state` is in scope.
        assert!(
            UI_STALE_AFTER_MS < UI_DEAD_AFTER_MS,
            "diagnostics must escalate before status"
        );
        let between = (UI_STALE_AFTER_MS + UI_DEAD_AFTER_MS) / 2;
        assert!(ui_stale(SOME_PONG, between, UI_STALE_AFTER_MS));
        assert!(!ui_stale(SOME_PONG, between, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                relay_connected: Some(true),
                ..Default::default()
            }),
            "healthy",
            "an age between the two rungs is stale for diagnostics, not dead"
        );
    }

    /// Wire-contract snapshot: serialized `UiError` must carry the exact
    /// snake_case field names the supervisor (`qontinui-supervisor::health_cache::UiErrorSummary`)
    /// and the web backend (`app/schemas/runner_fleet.py::UiErrorPayload`)
    /// deserialize. Drift here silently breaks fleet-level aggregation.
    #[test]
    fn ui_error_json_shape_matches_consumer_contract() {
        let now = Utc::now();
        let err = UiError {
            message: "boom".to_string(),
            stack: Some("stack-trace".to_string()),
            component_stack: Some("component-stack".to_string()),
            digest: Some("185".to_string()),
            first_seen: now,
            reported_at: now,
            count: 3,
        };
        let v = serde_json::to_value(&err).expect("serialize UiError");
        let obj = v.as_object().expect("UiError serializes to a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> = [
            "message",
            "stack",
            "component_stack",
            "digest",
            "first_seen",
            "reported_at",
            "count",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            keys, expected,
            "UiError wire keys drifted; consumers will silently fail to parse"
        );
        // Sanity-check types the supervisor's serde structs rely on.
        assert!(obj["message"].is_string());
        assert!(obj["count"].is_u64());
        assert!(
            obj["first_seen"].is_string(),
            "DateTime serializes as ISO8601 string"
        );
    }

    /// **`ui_error` has exactly one writer, and it is the frontend.**
    ///
    /// A tripwire for the decision recorded in this module's "There is
    /// deliberately NO backend writer" section. The convenience of routing a
    /// Rust-side UI failure into this state is obvious and the cost is not —
    /// it is a permanent, mis-attributed latch that takes a healthy runner out
    /// of the fleet's dispatch pool — so the ban is asserted rather than
    /// commented.
    ///
    /// Scope, stated honestly. [`ui_error_writer_hits`] matches on the
    /// **verb prefix** (`report…` / `clear…`) after any of the three ways this
    /// state is reachable — `x.ui_error.…`, `x.ui_error().…` (the accessor at
    /// `commands::compartments`) and `crate::ui_error::…` — so every *named*
    /// wrapper is caught, not just the bare call. That matters: the writer
    /// this guard was written to keep out was called `report_from_backend`,
    /// and the first cut of this test banned the literal `"ui_error.report("`,
    /// whose mandatory `(` let `report_from_backend` through under two of its
    /// three spellings. Symmetry matters for the same reason — a backend
    /// *clear* silently un-latches a genuine ErrorBoundary crash, which is
    /// worse than a spurious report, so `clear` is banned exactly as hard.
    ///
    /// What it still does NOT catch, so nobody mistakes it for a proof: a
    /// writer that first binds the `Arc` to a differently-named local
    /// (`let s = st.ui_error(); s.report(…)`), or one that reaches
    /// `UiErrorState` through a re-export under another module path. It is a
    /// tripwire; the module doc is the real authority.
    #[test]
    fn ui_error_has_exactly_one_writer() {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push((
                        path.display().to_string(),
                        std::fs::read_to_string(&path).expect("read source"),
                    ));
                }
            }
        }

        // From CARGO_MANIFEST_DIR, never the CWD: a test binary run from the
        // wrong directory would otherwise scan nothing and pass vacuously.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        assert!(
            files.len() > 20,
            "source walk found only {} files — the walk is broken, not the crate",
            files.len()
        );

        let mut scanned = 0usize;
        // Collected, not asserted per-line: a run that fails must name EVERY
        // writer it found, so one mutation replay answers "which spellings are
        // caught?" in a single run instead of one recompile per spelling.
        let mut writers: Vec<String> = Vec::new();
        for (name, body) in &files {
            if name.ends_with("ui_error.rs") {
                continue;
            }
            scanned += 1;
            for (i, line) in body.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for hit in ui_error_writer_hits(code) {
                    writers.push(format!("{name}:{}: {hit}", i + 1));
                }
            }
        }
        assert!(
            writers.is_empty(),
            "backend writers of ui_error found:\n  {}\n\n`ui_error` means \"the React error \
             boundary caught an unhandled error in the MAIN window's tree\": it is a \
             process-lifetime latch whose only clear path is the frontend's own \
             `clear_ui_error`, and every consumer turns it into `derived_status: errored` + \
             `frontendReady: false`, which removes the runner from qontinui-web's dispatch \
             pool. A backend *clear* is worse still — it silently un-latches a real crash. A \
             backend-observed failure needs a self-clearing, correctly-attributed signal \
             instead — see this module's \"There is deliberately NO backend writer\" section.",
            writers.join("\n  ")
        );
        assert!(
            scanned > 20,
            "scanned only {scanned} files besides ui_error.rs"
        );
    }

    /// Every `ui_error` **writer** spelling on one line of source.
    ///
    /// Matches `…ui_error.<fn>`, `…ui_error().<fn>` and `…ui_error::<fn>`
    /// (including a `use …::{a, b}` group) and reports a hit when `<fn>`
    /// starts with `report` or `clear` — the two mutating verbs on
    /// [`UiErrorState`]. **Prefix, never a trailing `(`**: see
    /// `ui_error_has_exactly_one_writer`'s doc for why.
    ///
    /// The anchor must be a whole identifier, so `has_ui_error`,
    /// `ui_error_snapshot` and `gather_ui_error_signals` are not matches. The
    /// only exemption is `ui_error::report_ui_error` / `ui_error::clear_ui_error`
    /// — the FRONTEND's own `#[tauri::command]`s, named once in `main.rs`'s
    /// `invoke_handler` list. Those are the sanctioned writer, not a bypass.
    fn ui_error_writer_hits(code: &str) -> Vec<String> {
        const ANCHOR: &str = "ui_error";
        const WRITE_VERBS: [&str; 2] = ["report", "clear"];
        const FRONTEND_COMMANDS: [&str; 2] = ["report_ui_error", "clear_ui_error"];

        fn is_ident(c: char) -> bool {
            c.is_ascii_alphanumeric() || c == '_'
        }

        let mut hits = Vec::new();
        let mut from = 0usize;
        while let Some(rel) = code[from..].find(ANCHOR) {
            let start = from + rel;
            let after = start + ANCHOR.len();
            from = after;

            if code[..start].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let rest = &code[after..];
            if rest.starts_with(is_ident) {
                continue;
            }

            let (rest, accessor) = match rest.strip_prefix("()") {
                Some(r) => (r, "()"),
                None => (rest, ""),
            };
            let (rest, sep) = if let Some(r) = rest.strip_prefix("::") {
                (r, "::")
            } else if let Some(r) = rest.strip_prefix('.') {
                (r, ".")
            } else {
                continue;
            };

            // `use crate::ui_error::{report_from_backend, UiError};`
            let names: Vec<&str> = match rest.strip_prefix('{') {
                Some(group) => group.split([',', '}']).map(str::trim).collect(),
                None => vec![rest],
            };
            for name in names {
                let ident: String = name.chars().take_while(|c| is_ident(*c)).collect();
                if ident.is_empty() || !WRITE_VERBS.iter().any(|v| ident.starts_with(v)) {
                    continue;
                }
                if sep == "::" && FRONTEND_COMMANDS.contains(&ident.as_str()) {
                    continue;
                }
                hits.push(format!("{ANCHOR}{accessor}{sep}{ident}"));
            }
        }
        hits
    }

    /// The matcher's own truth table — the replay that
    /// `ui_error_has_exactly_one_writer` is only as good as.
    ///
    /// The first four rows are the exact spellings a pre-PR review replayed
    /// against the `"ui_error.report("` version of the guard, where rows 2-4
    /// were **missed**; rows 2 and 3 name `report_from_backend`, the very
    /// function the guard exists to keep out. Row 5 is the backend *clear*,
    /// which is worse than a backend report and which the old guard also
    /// missed. Do not delete a row to make a change pass.
    #[test]
    fn the_writer_guard_catches_every_known_bypass() {
        for banned in [
            "        app_state.ui_error.report(m).await;",
            "        app_state.ui_error.report_from_backend(m).await;",
            "        crate::ui_error::report_from_backend(&app, m).await;",
            "        app_state.ui_error().clear().await;",
            "        app_state.ui_error.clear_from_backend().await;",
            "        state.app_state.ui_error().report_from_backend(m).await;",
            "    use crate::ui_error::{report_from_backend, UiError};",
        ] {
            assert!(
                !ui_error_writer_hits(banned.trim_start()).is_empty(),
                "writer slipped past the guard: {banned}"
            );
        }

        // Reads, unrelated identifiers and the frontend's own commands are NOT
        // writers. A guard that fires on these gets deleted by the next person
        // who trips it, which is how a real guard dies.
        for allowed in [
            "let ui_error_snapshot = app_state.ui_error.get().await;",
            "let has_ui_error = state.app_state.ui_error.get().await.is_some();",
            "let ui_error = gather_ui_error_signals(&state, last_pong, age).await;",
            "ui_error::clear_ui_error,",
            "ui_error::report_ui_error,",
            "ui_error::get_ui_error,",
            "use crate::ui_error::UiError;",
            "use crate::ui_error::{compute_derived_status, ui_stale};",
            "let expression = \"invoke(\\\"report_ui_error\\\", {stack: \\\"at A\\\"})\";",
            "pub ui_error: Arc<crate::ui_error::UiErrorState>,",
            "\"ui_error\": ui_error_json,",
        ] {
            assert!(
                ui_error_writer_hits(allowed).is_empty(),
                "false positive — this is not a writer: {allowed} => {:?}",
                ui_error_writer_hits(allowed)
            );
        }
    }

    // -----------------------------------------------------------------------
    // Pong provenance + the native UI-thread input (plan
    // 2026-08-19-runner-blocked-ui-thread-cannot-be-closed, Phase 5)
    // -----------------------------------------------------------------------

    /// Build inputs for the healthy case; each test perturbs one field.
    fn native_inputs() -> NativeUiInputs {
        NativeUiInputs {
            probe_wedged: Some(false),
            window_getter_unresponsive: false,
            last_pong: SOME_PONG,
            pong_age_ms: 1_000,
            last_event_pong: SOME_PONG,
            event_pong_age_ms: 1_000,
        }
    }

    #[test]
    fn native_ui_healthy_when_loop_pumps_and_both_pongs_are_fresh() {
        let v = classify_native_ui(native_inputs());
        assert!(!v.wedged);
        assert!(!v.events_undelivered);
        assert_eq!(v.reason, "pumping");
        assert_eq!(v.event_pong_age_ms, Some(1_000));
    }

    /// **The 2026-08-19 bug, in one assertion.**
    ///
    /// Native loop wedged, renderer alive: the frontend's unconditional 3s
    /// HTTP pong is serviced by WebView2's browser process, so
    /// `ui_bridge_last_pong` stays fresh and `ui_stale` reads `false` forever.
    /// Only the event-provenance stamp goes stale, and only because the loop
    /// stopped delivering `ui-bridge-ping`. Before the split, the two facts
    /// shared one slot and this state was inexpressible.
    #[test]
    fn native_ui_event_pong_stale_while_http_pong_fresh_is_this_bug() {
        let v = classify_native_ui(NativeUiInputs {
            // The Win32 rung is deliberately UNKNOWN here so the assertion
            // rests on provenance ALONE — this is the cross-platform arm.
            probe_wedged: None,
            last_event_pong: SOME_PONG,
            event_pong_age_ms: UI_EVENT_DEAD_AFTER_MS + 1,
            ..native_inputs()
        });
        assert!(v.events_undelivered, "renderer alive + events undelivered");
        assert!(v.wedged);
        assert_eq!(v.reason, "events_undelivered");

        // And the shipped pong predicate still reads the runner as fine —
        // which is precisely why a distinct input was needed.
        assert!(!ui_stale(SOME_PONG, 1_000, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                native_ui_wedged: Some(v.wedged),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                ..Default::default()
            }),
            "errored",
            "a wedged native loop must not report healthy just because the renderer pongs"
        );
    }

    /// The mirror-image mode must NOT be reclassified. When the browser
    /// process dies (2026-08-01) neither path pongs, so `ui_dead` fires as it
    /// always did and the native verdict stays out of it — the two incidents
    /// keep distinct reasons.
    #[test]
    fn native_ui_both_pongs_stale_is_the_dead_webview_mode_not_a_native_hang() {
        let stale = UI_DEAD_AFTER_MS + 1;
        let v = classify_native_ui(NativeUiInputs {
            probe_wedged: None,
            pong_age_ms: stale,
            event_pong_age_ms: stale,
            ..native_inputs()
        });
        assert!(
            !v.events_undelivered,
            "the renderer is gone, so a missing event pong says nothing about the loop"
        );
        assert!(!v.wedged);
        assert_eq!(v.reason, "unknown");
        assert!(ui_stale(SOME_PONG, stale, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(true),
                native_ui_wedged: Some(v.wedged),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                ..Default::default()
            }),
            "errored",
            "the 2026-08-01 arm still fires, through `ui_dead`"
        );
    }

    /// Never-ponged is UNKNOWN on the event stamp too. Server mode
    /// (`QONTINUI_SERVER_MODE` mounts no webview) would otherwise report a
    /// permanent native hang — the same regression `ui_stale`'s `> 0` guard
    /// exists to prevent, which is why this reuses it rather than re-deriving.
    #[test]
    fn native_ui_never_event_ponged_is_unknown_not_wedged() {
        for age in [0, 1, UI_EVENT_DEAD_AFTER_MS + 1, u64::MAX] {
            let v = classify_native_ui(NativeUiInputs {
                probe_wedged: None,
                last_event_pong: 0,
                event_pong_age_ms: age,
                ..native_inputs()
            });
            assert!(!v.wedged, "never-event-ponged must not read as wedged (age {age})");
            assert_eq!(v.event_pong_age_ms, None, "no stamp ⇒ no age (age {age})");
            assert_eq!(v.reason, "unknown");
        }
    }

    #[test]
    fn native_ui_probe_outranks_the_inferred_reasons() {
        let v = classify_native_ui(NativeUiInputs {
            probe_wedged: Some(true),
            window_getter_unresponsive: true,
            event_pong_age_ms: UI_EVENT_DEAD_AFTER_MS + 1,
            ..native_inputs()
        });
        assert!(v.wedged);
        assert_eq!(
            v.reason, "native_probe_wedged",
            "the Win32 round-trip asks the loop itself; it wins the reason"
        );
    }

    /// `/health`'s own bounded getter timing out is direct evidence about the
    /// loop. Reporting `healthy` right after watching it time out would be a
    /// new lie in place of the one this phase removes.
    #[test]
    fn native_ui_own_window_getter_timeout_is_a_wedged_verdict() {
        let v = classify_native_ui(NativeUiInputs {
            probe_wedged: None,
            window_getter_unresponsive: true,
            ..native_inputs()
        });
        assert!(v.wedged);
        assert_eq!(v.reason, "window_getter_unresponsive");
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                native_ui_wedged: Some(v.wedged),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                ..Default::default()
            }),
            "errored"
        );
    }

    /// The native input must be able to carry the verdict on its own — no
    /// pong help, no crash dump, no error boundary. That is the whole point of
    /// making it a distinct parameter.
    #[test]
    fn derived_status_errored_on_the_native_signal_alone() {
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                native_ui_wedged: Some(true),
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                ..Default::default()
            }),
            "errored"
        );
        // …and it outranks a merely-degraded subsystem, like every other
        // `errored` input.
        assert_eq!(
            compute_derived_status(&HealthInputs {
                ui_dead: Some(false),
                native_ui_wedged: Some(true),
                embedding_reachable: Some(false),
                pg_reachable: Some(false),
                ..Default::default()
            }),
            "errored"
        );
    }

    #[test]
    fn derived_status_unknown_native_signal_is_unchanged_from_before() {
        // `None` = this sink cannot tell (non-Windows, no HWND yet). Identical
        // verdicts to `Some(false)`, so no sink is downgraded by adding the
        // input before it can answer.
        for native in [None, Some(false)] {
            assert_eq!(
                compute_derived_status(&HealthInputs {
                    ui_dead: Some(false),
                    native_ui_wedged: native,
                    embedding_reachable: Some(true),
                    pg_reachable: Some(true),
                    ..Default::default()
                }),
                "healthy"
            );
            assert_eq!(
                compute_derived_status(&HealthInputs {
                    ui_dead: Some(false),
                    native_ui_wedged: native,
                    embedding_reachable: Some(false),
                    ..Default::default()
                }),
                "degraded"
            );
            assert_eq!(
                compute_derived_status(&HealthInputs {
                    has_ui_error: true,
                    ui_dead: Some(false),
                    native_ui_wedged: native,
                    embedding_reachable: Some(true),
                    pg_reachable: Some(true),
                    ..Default::default()
                }),
                "errored"
            );
        }
    }

    /// The 2026-08-19 incident payload, replayed. On `origin/main` this reads
    /// `healthy` — `Responding: False` on the process while `/health` answered
    /// `200` and the pong loop never missed a beat.
    #[test]
    fn derived_status_regression_2026_08_19_wedged_ui_thread_read_healthy() {
        let pong_age_ms = 2_000; // HTTP pong, serviced off the UI thread
        let ui_dead = ui_stale(SOME_PONG, pong_age_ms, UI_DEAD_AFTER_MS);
        assert!(!ui_dead, "the shipped pong rung is blind to this failure");
        let native = classify_native_ui(NativeUiInputs {
            probe_wedged: Some(true), // SendMessageTimeoutW(WM_NULL) timed out
            window_getter_unresponsive: true, // /health's own getter timed out
            pong_age_ms,
            event_pong_age_ms: 25 * 60 * 1_000, // no ping delivered in 25 min
            ..native_inputs()
        });
        assert!(native.wedged);
        assert_eq!(
            compute_derived_status(&HealthInputs {
                // ui_error: null — the React tree is fine
                // recent_crash: null — nothing died
                ui_dead: Some(ui_dead),
                // false: the pong keeps arriving over HTTP
                native_ui_wedged: Some(native.wedged),
                // the only signal that can see this
                embedding_reachable: Some(true),
                pg_reachable: Some(true),
                ..Default::default()
            }),
            "errored",
            "a runner whose window cannot be closed must not report healthy"
        );
    }

    #[test]
    fn event_pong_stamp_advances_only_when_recorded() {
        // Process-global, so this test asserts the transition it causes rather
        // than any starting value — a sibling test may have stamped it first.
        record_event_pong();
        let stamped = last_event_pong();
        assert!(stamped > 0, "recording must leave a non-zero stamp");
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(
            now_ms.saturating_sub(stamped) < 60_000,
            "the stamp must be a wall-clock ms epoch, not a monotonic tick"
        );
    }

    #[test]
    fn event_dead_rung_tracks_the_pong_dead_rung() {
        // They are equal today and free to diverge; what may never happen is
        // the event rung firing BEFORE the diagnostics rung, which would make
        // a booting frontend look like a wedged loop.
        assert!(UI_STALE_AFTER_MS <= UI_EVENT_DEAD_AFTER_MS);
        assert_eq!(UI_EVENT_DEAD_AFTER_MS, UI_DEAD_AFTER_MS);
    }
}
