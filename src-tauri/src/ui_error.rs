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
/// * `embedding_reachable` — `None` until the first probe has run (treated as
///   unknown, not degraded — avoids false positives during boot). `Some(true)`
///   = healthy. `Some(false)` = degraded.
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
/// dead UI is ever visible off-box.
pub fn compute_derived_status(
    has_ui_error: bool,
    has_recent_crash: bool,
    ui_dead: Option<bool>,
    embedding_reachable: Option<bool>,
    pg_reachable: Option<bool>,
) -> &'static str {
    if has_ui_error || has_recent_crash || matches!(ui_dead, Some(true)) {
        "errored"
    } else if matches!(embedding_reachable, Some(false)) || matches!(pg_reachable, Some(false)) {
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
            compute_derived_status(true, false, Some(false), Some(true), Some(true)),
            "errored"
        );
        assert_eq!(
            compute_derived_status(true, true, Some(false), Some(false), Some(false)),
            "errored"
        );
        assert_eq!(
            compute_derived_status(false, true, Some(false), Some(true), Some(true)),
            "errored"
        );
    }

    #[test]
    fn derived_status_degraded_when_embedding_unreachable() {
        assert_eq!(
            compute_derived_status(false, false, Some(false), Some(false), Some(true)),
            "degraded"
        );
    }

    #[test]
    fn derived_status_degraded_when_pg_unreachable() {
        // iter4 B-5: PG down while everything else is fine → degraded, so
        // /health can no longer report "healthy" over a dead data layer.
        assert_eq!(
            compute_derived_status(false, false, Some(false), Some(true), Some(false)),
            "degraded"
        );
    }

    #[test]
    fn derived_status_healthy_when_embedding_reachable() {
        assert_eq!(
            compute_derived_status(false, false, Some(false), Some(true), Some(true)),
            "healthy"
        );
    }

    #[test]
    fn derived_status_unknown_embedding_is_healthy_not_degraded() {
        // Boot-time: probe hasn't run yet. Avoid false-positive degraded.
        assert_eq!(
            compute_derived_status(false, false, None, None, None),
            "healthy"
        );
    }

    #[test]
    fn derived_status_unknown_pg_is_healthy_not_degraded() {
        // No PG probe (heartbeat sinks, or no PG configured) must not
        // false-positive to degraded.
        assert_eq!(
            compute_derived_status(false, false, Some(false), Some(true), None),
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
            compute_derived_status(false, false, Some(false), Some(true), Some(true)),
            "healthy",
            "a runner that never mounted a webview is healthy, not errored"
        );
    }

    #[test]
    fn derived_status_healthy_when_ui_alive_and_fresh() {
        assert!(!ui_stale(SOME_PONG, 500, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(false, false, Some(false), Some(true), Some(true)),
            "healthy"
        );
    }

    #[test]
    fn derived_status_errored_when_ui_was_alive_then_went_stale() {
        assert!(ui_stale(SOME_PONG, UI_DEAD_AFTER_MS + 1, UI_DEAD_AFTER_MS));
        assert_eq!(
            compute_derived_status(false, false, Some(true), Some(true), Some(true)),
            "errored"
        );
    }

    #[test]
    fn derived_status_dead_ui_outranks_degraded_pg() {
        // A dead window is not "a subsystem is out but the app works".
        assert_eq!(
            compute_derived_status(false, false, Some(true), Some(true), Some(false)),
            "errored"
        );
        assert_eq!(
            compute_derived_status(false, false, Some(true), Some(false), Some(false)),
            "errored"
        );
    }

    #[test]
    fn derived_status_unknown_ui_liveness_is_unchanged_from_before() {
        // `None` = this sink cannot tell. Guards the pre-change behavior for
        // any non-probing caller: identical verdicts to `Some(false)`.
        assert_eq!(
            compute_derived_status(false, false, None, Some(true), Some(true)),
            "healthy"
        );
        assert_eq!(
            compute_derived_status(false, false, None, Some(false), None),
            "degraded"
        );
        assert_eq!(
            compute_derived_status(true, false, None, Some(true), Some(true)),
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
            compute_derived_status(
                false,         // ui_error: null
                false,         // recent_crash: null
                Some(ui_dead), // the only signal that stayed correct
                Some(true),    // embedding service fine
                Some(true),    // PG fine
            ),
            "errored",
            "the dead-webview incident must no longer report healthy"
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
            compute_derived_status(false, false, Some(false), Some(true), Some(true)),
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
}
