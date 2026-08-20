//! Debug-only affordance for deliberately blocking the native UI thread.
//!
//! # Why this exists
//!
//! Phases 2-5 of `2026-08-19-runner-blocked-ui-thread-cannot-be-closed` all
//! have runtime gates whose precondition is *a runner whose tao/UI thread is
//! not pumping*. Every natural way to produce that state is a real bug, so
//! there was no way to exercise the wedge-detection rung, the honest `/health`
//! degradation, or the `close-request` 503 / `force-close` doors without one.
//!
//! This module supplies it: an HTTP route that enqueues a plain
//! `std::thread::sleep` onto the main thread via
//! [`tauri::AppHandle::run_on_main_thread`]. While that closure runs, the tao
//! event loop is genuinely blocked — `SendMessageTimeoutW` gets no round trip,
//! `IsHungAppWindow` goes true, unbounded window getters park, and Windows
//! reports `Responding: False`. It is the real condition, not a simulation of
//! it, which is the whole point: a mocked flag would prove nothing about the
//! code paths under test.
//!
//! # It cannot ship
//!
//! The module is declared `#[cfg(debug_assertions)]` in `mcp/mod.rs` and its
//! router is merged under the same gate in `mcp_api::start_server`, following
//! the precedent set by the relay's forced-panic trip switch
//! (`mcp/backend_relay.rs`). In a release build neither the handler nor the
//! route exists — there is nothing to reach.
//!
//! # Safety rails
//!
//! - The sleep is clamped to [`MAX_WEDGE_MS`]. A typo cannot park the runner
//!   forever; the loop always comes back on its own.
//! - The wedge is *self-releasing*, so a test that fails part-way does not
//!   leave an unkillable process behind.
//! - The route reports the enqueue time and, once the closure lands, the time
//!   the main thread actually entered the sleep — so a test can distinguish
//!   "the wedge is armed" from "the wedge is in effect" instead of guessing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::mcp::types::ApiState;

/// Hard ceiling on a requested wedge. Longer requests are clamped, and the
/// response says so.
const MAX_WEDGE_MS: u64 = 180_000;

/// Default wedge length: comfortably longer than the health monitor's
/// worst-case detection latency (`3 × (5 s cadence + 3 s probe)` = 24 s), so a
/// default-length wedge is guaranteed to cross the breadcrumb threshold.
const DEFAULT_WEDGE_MS: u64 = 40_000;

/// Unix-ms at which the *main thread* entered the sleep. Zero until it does.
static WEDGE_ENTERED_AT_MS: AtomicU64 = AtomicU64::new(0);
/// Unix-ms at which the main thread left the sleep. Zero until it does.
static WEDGE_RELEASED_AT_MS: AtomicU64 = AtomicU64::new(0);
/// Unix-ms at which the most recent wedge was requested (HTTP thread).
static WEDGE_REQUESTED_AT_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Default, Deserialize)]
struct WedgeQuery {
    /// Milliseconds to block the UI thread for. Clamped to [`MAX_WEDGE_MS`].
    ms: Option<u64>,
}

/// `POST /__debug/wedge-ui-thread?ms=40000` (debug builds only).
///
/// Enqueues a blocking sleep onto the tao main thread and returns immediately.
/// The response is the *enqueue* acknowledgement — poll the `GET` form of the
/// same route (which is served by the HTTP stack, not the event loop, and so
/// keeps answering during the wedge) to see when the main thread actually
/// entered it.
async fn wedge_ui_thread_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<WedgeQuery>,
) -> Json<Value> {
    let requested = query.ms.unwrap_or(DEFAULT_WEDGE_MS);
    let ms = requested.min(MAX_WEDGE_MS);
    let clamped = ms != requested;

    let requested_at = now_ms();
    WEDGE_REQUESTED_AT_MS.store(requested_at, Ordering::SeqCst);
    WEDGE_ENTERED_AT_MS.store(0, Ordering::SeqCst);
    WEDGE_RELEASED_AT_MS.store(0, Ordering::SeqCst);

    warn!(
        wedge_ms = ms,
        "DEBUG: deliberately blocking the native UI thread (debug builds only)"
    );

    let enqueued = state.app_handle.run_on_main_thread(move || {
        WEDGE_ENTERED_AT_MS.store(now_ms(), Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(ms));
        WEDGE_RELEASED_AT_MS.store(now_ms(), Ordering::SeqCst);
    });

    match enqueued {
        Ok(()) => {
            info!("DEBUG: UI-thread wedge enqueued for {ms} ms");
            Json(json!({
                "success": true,
                "enqueued": true,
                "wedgeMs": ms,
                "clamped": clamped,
                "requestedMs": requested,
                "requestedAtMs": requested_at,
                "maxWedgeMs": MAX_WEDGE_MS,
                "note": "Sleep enqueued onto the tao main thread. GET this route \
                         for the entered/released stamps; the wedge always self-releases."
            }))
        }
        Err(e) => Json(json!({
            "success": false,
            "enqueued": false,
            "error": format!("run_on_main_thread failed: {e}"),
        })),
    }
}

/// `GET /__debug/wedge-ui-thread` (debug builds only) — observe the wedge.
///
/// Deliberately touches nothing that needs the event loop, so it keeps
/// answering while the wedge it reports on is in effect.
async fn wedge_status_handler() -> Json<Value> {
    let requested_at = WEDGE_REQUESTED_AT_MS.load(Ordering::SeqCst);
    let entered_at = WEDGE_ENTERED_AT_MS.load(Ordering::SeqCst);
    let released_at = WEDGE_RELEASED_AT_MS.load(Ordering::SeqCst);
    Json(json!({
        "success": true,
        "requestedAtMs": requested_at,
        "enteredAtMs": entered_at,
        "releasedAtMs": released_at,
        "inEffect": entered_at != 0 && released_at == 0,
        "enqueueToEntryMs": if entered_at != 0 && requested_at != 0 {
            Some(entered_at.saturating_sub(requested_at))
        } else {
            None
        },
        "nowMs": now_ms(),
    }))
}

/// Router for the debug-only wedge affordance. Merged in
/// `mcp_api::start_server` under `#[cfg(debug_assertions)]`.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new().route(
        "/__debug/wedge-ui-thread",
        axum::routing::post(wedge_ui_thread_handler).get(wedge_status_handler),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp is what makes this safe to leave in a debug build: no request
    /// can park the UI thread past the ceiling.
    #[test]
    fn wedge_length_is_clamped() {
        let requested: u64 = 10_000_000;
        assert_eq!(requested.min(MAX_WEDGE_MS), MAX_WEDGE_MS);
        assert_eq!(1_000u64.min(MAX_WEDGE_MS), 1_000);
    }

    /// The default must exceed the health monitor's worst-case detection
    /// latency, or a default-length wedge would race the breadcrumb it exists
    /// to provoke.
    #[test]
    fn default_wedge_outlasts_worst_case_detection_latency() {
        let worst_case_ms = 3 * (5_000 + 3_000);
        assert!(
            DEFAULT_WEDGE_MS > worst_case_ms,
            "default wedge {DEFAULT_WEDGE_MS}ms must outlast worst-case detection {worst_case_ms}ms"
        );
    }
}
