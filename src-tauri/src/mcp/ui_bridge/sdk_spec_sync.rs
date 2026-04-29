//! SSE-backed spec-sync progress endpoints (P2 remediation).
//!
//! Two endpoints, both backed by the shared `SpecSyncStateBus` Tauri-managed
//! state defined in `commands::spec_sync_state`:
//!
//!   - `GET /ui-bridge/sdk/spec-sync/status` — snapshot of the latest known
//!     sync state (JSON).
//!   - `GET /ui-bridge/sdk/spec-sync/stream` — Server-Sent Events that emit
//!     `data: {...}\n\n` on every state change, plus an initial `data:`
//!     event with the current state on connect, plus `event: ping\ndata: {}`
//!     heartbeats every 30s so reverse proxies/intermediaries don't close
//!     the connection on idle.
//!
//! The frontend's `useSpecSync` hook pushes state updates into the bus by
//! invoking the `push_spec_sync_state` Tauri command after each `setState`
//! — see `src/hooks/useSpecSync.ts` and
//! `src-tauri/src/commands/spec_sync_state.rs`. Backpressure is handled
//! naturally by `tokio::sync::watch`: if the SSE drain is slower than the
//! frontend pushes, intermediate states are coalesced (subscribers always
//! observe the latest value on the next `changed()` poll).
//!
//! Routing prefix follows the existing `/ui-bridge/sdk/...` family registered
//! in `mcp::sdk_client` (e.g. `/ui-bridge/sdk/network-requests`).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use tracing::{debug, warn};

use crate::commands::spec_sync_state::SpecSyncStateBus;
use crate::mcp::types::ApiState;

/// Resolve the `SpecSyncStateBus` from Tauri-managed state.
///
/// Returns `None` if the bus hasn't been registered (which would only
/// happen if `main.rs` forgot to `.manage()` it — i.e. a wiring bug).
fn bus_from_state(state: &Arc<ApiState>) -> Option<Arc<SpecSyncStateBus>> {
    use tauri::Manager;
    state
        .app_handle
        .try_state::<Arc<SpecSyncStateBus>>()
        .map(|s| s.inner().clone())
}

/// `GET /ui-bridge/sdk/spec-sync/status` — latest snapshot.
///
/// Returns the raw JSON pushed by the frontend; if no state has been
/// pushed yet, returns the default empty object (a `null`-equivalent
/// value from `SpecSyncStateMirror::default`).
pub async fn handle_spec_sync_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let bus = bus_from_state(&state).ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "SpecSyncStateBus not registered".to_string(),
    ))?;

    // `borrow()` returns the latest value without waiting.
    let snapshot = bus.rx.borrow().0.clone();
    Ok(Json(snapshot))
}

/// `GET /ui-bridge/sdk/spec-sync/stream` — Server-Sent Events stream.
///
/// Emits an immediate `data:` event with the current snapshot on connect,
/// then one `data:` event per state change, plus a `ping` event every 30s
/// to keep proxies happy. The stream lives until the client disconnects
/// (axum drops the response) or the watch sender is dropped (runner
/// shutdown).
pub async fn handle_spec_sync_stream(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, String),
> {
    let bus = bus_from_state(&state).ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "SpecSyncStateBus not registered".to_string(),
    ))?;

    let mut rx = bus.rx.clone();

    let stream = async_stream::stream! {
        // 1. Emit the current snapshot immediately on connect so the
        //    consumer doesn't have to wait for the next state change to
        //    learn what's going on.
        let initial = rx.borrow_and_update().0.clone();
        match serde_json::to_string(&initial) {
            Ok(json_str) => {
                yield Ok::<_, std::convert::Infallible>(Event::default().data(json_str));
            }
            Err(e) => {
                warn!("spec-sync/stream: failed to serialize initial state: {}", e);
            }
        }

        // 2. Loop: race watch updates against a 30s heartbeat tick.
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        // Skip the immediate first tick — we just emitted real data.
        ping_interval.tick().await;

        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        // Sender dropped — runner shutdown. End the stream.
                        debug!("spec-sync/stream: watch sender dropped, ending stream");
                        break;
                    }
                    let snapshot = rx.borrow_and_update().0.clone();
                    match serde_json::to_string(&snapshot) {
                        Ok(json_str) => {
                            yield Ok(Event::default().data(json_str));
                        }
                        Err(e) => {
                            warn!("spec-sync/stream: failed to serialize state: {}", e);
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    yield Ok(Event::default().event("ping").data("{}"));
                }
            }
        }
    };

    // KeepAlive::default() injects ":\n\n" comments at axum's default
    // interval as a redundant safety net. Our explicit `event: ping`
    // is the canonical heartbeat per the endpoint contract.
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ============================================================================
// Route registration
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/ui-bridge/sdk/spec-sync/status",
            get(handle_spec_sync_status),
        )
        .route(
            "/ui-bridge/sdk/spec-sync/stream",
            get(handle_spec_sync_stream),
        )
}

pub fn route_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("GET", "/ui-bridge/sdk/spec-sync/status"),
        ("GET", "/ui-bridge/sdk/spec-sync/stream"),
    ]
}
