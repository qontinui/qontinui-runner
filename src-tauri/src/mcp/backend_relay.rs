//! Unified runner ↔ qontinui-web WebSocket relay (Phase 3).
//!
//! Opens a single outbound WebSocket to `WS /api/v1/devices/ws` on the
//! configured `WebIntegrationSettings.backend_url`, authenticated with a
//! `Authorization: Bearer <device_jwt>` header. The device-JWT is minted
//! by coord (`/pair/cli/complete` → 4h TTL) and stored in `AuthManager`'s
//! `access_token` slot by `pair::persist_pairing`; the background
//! `mcp::device_jwt_refresher` task keeps it fresh. This connection
//! replaces the legacy split-brain architecture in which:
//!
//! - `server_mode/mod.rs` HTTP-registered the runner and heartbeated every
//!   30s via `POST /runners/{id}/heartbeat`.
//! - `python-bridge/websocket_client.py` opened a parallel WebSocket to the
//!   legacy `/api/v1/automation/ws/automation/runner` endpoint.
//! - `mcp/backend_relay.rs` (this file) opened a third WebSocket to the
//!   same legacy endpoint with query-string token auth.
//!
//! All three are gone. This module is the canonical channel for:
//!
//! - **Registration.** The first message after handshake is `runner_info`,
//!   which the backend uses to upsert the `runners` row and assign the
//!   `runner_id` (returned in the `connected` reply).
//! - **Heartbeats.** Every 30s the relay sends `{"type": "heartbeat"}`;
//!   the backend refreshes `last_heartbeat`.
//! - **Phase results.** When the workflow executor publishes a `phase-result`
//!   event onto `event_broadcast`, the relay forwards it as a
//!   `phase_completed` WS message.
//! - **UI errors / recent crashes.** The relay forwards `ui-error` and
//!   `recent-crash` event broadcasts as their respective WS messages.
//! - **Inbound dispatch / command / chat / terminal.** Backend pushes
//!   `dispatch`, `command`, `chat`, `terminal` messages, which the relay
//!   routes to local executors (chat → SessionManager, terminal →
//!   TerminalManager, dispatch → workflow auto_run, command → existing
//!   handlers). Responses are sent back as `dispatch_ack`,
//!   `command_response`, `chat_response`, `terminal_response`.
//!
//! Reconnect uses exponential backoff (2s → 60s) with kick-to-retry-now.
//! Trigger condition is `tier == QontinuiAccount && WebIntegrationSettings.
//! enabled && AuthManager has a non-empty device-JWT` — there is no longer
//! a `cloud_relay` toggle or a localhost auto-detect probe. The runner_token
//! field is no longer consulted by the relay (Phase 3 of the unified-devices
//! migration); it is owned by the refresher's outbound HTTP to coord's
//! pair-cli endpoint.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tauri::Manager;
use tokio::sync::{broadcast, watch, Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        handshake::client::Request as HttpRequest,
        http::{header, HeaderValue},
        Message,
    },
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::mcp::types::ApiState;

/// Maximum size (bytes) of a request or response body the generic
/// `http_request` relay arm will carry over the WS in a single
/// `command_response` frame. Requests with a decoded body larger than this
/// are rejected with 413 before issuing the local call; local responses
/// whose body exceeds this are likewise replaced with a 413 error reply so
/// the runner never emits a multi-megabyte WS frame (head-of-line blocking
/// / memory-blowup guard). See plan §3.1 (option (a) + cap).
const RELAY_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Pure idle predicate: given the three relay gates, should the loop
/// idle (await a kick) instead of connecting? True iff *any* gate is unmet:
///
/// 1. Runner tier is anything other than `QontinuiAccount` (Tier 0/1 has
///    no business reaching qontinui-web — Phase 4 of the tier-decoupling
///    plan).
/// 2. `web_integration.enabled` is false (user-facing master switch).
/// 3. AuthManager's `access_token` slot is empty — no device-JWT to
///    present, which would 401-spam the WS without this guard. Phase 3 of
///    the unified-devices migration replaced the old `runner_token` gate;
///    the runner_token is now only consumed by `device_jwt_refresher` for
///    outbound HTTP to coord's pair-cli endpoint.
///
/// Kept as a pure function (no I/O, no global state) so the Phase 9
/// tier-matrix calibration suite can exercise it deterministically. The
/// I/O-bound wrapper `should_relay_idle` reads from `AuthManager` and
/// calls into this inner predicate.
pub(crate) fn should_relay_idle_with(
    tier: crate::settings::RunnerTier,
    enabled: bool,
    has_device_jwt: bool,
) -> bool {
    if tier != crate::settings::RunnerTier::QontinuiAccount {
        return true;
    }
    if !enabled {
        return true;
    }
    !has_device_jwt
}

/// Live idle predicate consumed by `relay_loop`. Reads the device-JWT
/// slot from `AuthManager` (filesystem I/O on `~/.qontinui`) and delegates
/// to the pure inner [`should_relay_idle_with`]. Tests must use the inner
/// directly to avoid touching user data_local_dir.
pub(crate) fn should_relay_idle(settings: &crate::settings::Settings) -> bool {
    let has_device_jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) => !t.trim().is_empty(),
        Err(_) => false,
    };
    should_relay_idle_with(
        settings.tier,
        settings.web_integration.enabled,
        has_device_jwt,
    )
}

/// Diagnostic snapshot of the relay's idle-gating inputs + live connection
/// state, served at `GET /web-integration/status` on the runner's local API.
///
/// Sourced from exactly the same places the relay loop reads when deciding
/// whether to idle ([`should_relay_idle`]): `settings.tier`,
/// `settings.web_integration.enabled`, the AuthManager device-JWT slot, and
/// the live `ServerModeState` (WS connection + last connection error). This
/// lets an operator see *why* the runner never appears to the mobile app /
/// cloud — instead of every gate being invisible.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct WebIntegrationStatus {
    /// The runner tier (`local` / `local_provider` / `qontinui_account`).
    /// Anything other than `qontinui_account` idles the relay (gate 1).
    pub tier: crate::settings::RunnerTier,
    /// User-facing master switch `settings.web_integration.enabled` (gate 2).
    pub web_integration_enabled: bool,
    /// Whether AuthManager's `access_token` slot holds a non-empty
    /// device-JWT (gate 3). The actual token is never exposed.
    pub has_device_jwt: bool,
    /// Whether the relay currently holds an open, post-handshake WS
    /// connection. False whenever the relay is idling or reconnecting.
    pub ws_connected: bool,
    /// The most recent relay connection error (e.g. handshake 401, DNS
    /// failure), or `None` if the last attempt succeeded / none yet.
    pub last_error: Option<String>,
}

/// Build the diagnostic [`WebIntegrationStatus`] from the same settings +
/// relay state the idle gate reads. Pure-ish (only reads): used by both the
/// HTTP handler and any future caller that wants the gating snapshot.
pub(crate) async fn web_integration_status(api_state: &Arc<ApiState>) -> WebIntegrationStatus {
    let settings = crate::settings::load_settings();
    let has_device_jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) => !t.trim().is_empty(),
        Err(_) => false,
    };

    let (ws_connected, last_error) = match api_state.app_state.current_server_mode().await {
        Some(sm) => (sm.is_ws_connected(), sm.registration_error().await),
        None => (false, None),
    };

    WebIntegrationStatus {
        tier: settings.tier,
        web_integration_enabled: settings.web_integration.enabled,
        has_device_jwt,
        ws_connected,
        last_error,
    }
}

/// `GET /web-integration/status` — local-only diagnostic endpoint exposing
/// the relay's idle-gating inputs + live connection state. Unauthenticated,
/// consistent with the rest of the runner's localhost-bound API surface
/// (see the CORS rationale in `mcp_api::start_server`). Returns the JSON
/// `{ tier, web_integration_enabled, has_device_jwt, ws_connected,
/// last_error }`.
async fn web_integration_status_handler(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::response::Json<WebIntegrationStatus> {
    axum::response::Json(web_integration_status(&state).await)
}

/// `POST /web-integration/relay/kick` — force the WS relay to interrupt any
/// in-progress backoff sleep and immediately retry a connect with
/// freshly-read settings + tokens.
///
/// The relay reconnects on its own exponential backoff (2s → 120s), so after
/// a fresh pair (which DOES kick internally via `redeem_pair_code`) or any
/// out-of-band credential change there's otherwise no way to force an
/// immediate reconnect without a UI action or a runner restart. This endpoint
/// pokes the same kick channel the in-process callers use
/// ([`commands::kick_cloud_relay`]). It's a no-op (still returns ack) when no
/// relay task is running. Local-only + unauthenticated, consistent with the
/// rest of the runner's localhost-bound API surface.
async fn relay_kick_handler() -> axum::response::Json<Value> {
    commands::kick_cloud_relay().await;
    axum::response::Json(serde_json::json!({
        "ok": true,
        "kicked": true,
    }))
}

/// Router exposing the relay's web-integration diagnostic + control
/// endpoints. Merged into the base router in `mcp_api::start_server`.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new()
        .route(
            "/web-integration/status",
            axum::routing::get(web_integration_status_handler),
        )
        .route(
            "/web-integration/relay/kick",
            axum::routing::post(relay_kick_handler),
        )
}

/// Return true iff `e` is a tungstenite handshake error with HTTP 401
/// (Unauthorized). Used by [`relay_loop`] to detect a stale device-JWT
/// and kick the refresher BEFORE serving out the backoff sleep.
///
/// Factored out of the inline `Err(e)` arm so the Phase 5.3 unit tests
/// can construct synthetic errors and pin the discrimination:
/// - `Error::Http(401)` → true (the only "yes")
/// - `Error::Http(<anything else>)` → false (500 / 403 / 404 are NOT
///   triggers to kick the refresher — they signal coord-side bugs or
///   policy denials that a fresh JWT won't fix)
/// - `Error::Io(...)`, `Error::ConnectionClosed`, transport errors → false
///
/// Keeping this isolated from the I/O path means a refactor that moves
/// to a different WS client (or augments the detection with WWW-
/// Authenticate parsing) has a regression net here without needing a
/// live coord.
pub(crate) fn is_unauthorized(e: &tokio_tungstenite::tungstenite::Error) -> bool {
    if let tokio_tungstenite::tungstenite::Error::Http(ref resp) = e {
        return resp.status() == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
    }
    false
}

/// Periodicity of WS heartbeat sends. The backend's `last_heartbeat`
/// timestamp updates from these.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Periodicity of WS keepalive ping frames. Lower than `HEARTBEAT_INTERVAL`
/// so we can detect dead/half-open connections faster than the heartbeat
/// cadence would.
const KEEPALIVE_PING_INTERVAL: Duration = Duration::from_secs(20);

/// Maximum interval between inbound frames before we declare the WS dead
/// and force a reconnect. Set to ~2.25x `KEEPALIVE_PING_INTERVAL` so a
/// single missed pong is tolerated (network jitter) but two consecutive
/// silent pings trip the reconnect. Without this gate, a half-open TCP
/// session (server gone, runner still buffering Ping frames into a kernel
/// send queue that hasn't yet faulted) keeps `wsConnected=true` on the
/// runner side indefinitely while the backend's `device.ws_session_id`
/// has long since been cleared by the cleanup task — the exact
/// false-positive observed in 2026-05-22.
const STALE_INBOUND_TIMEOUT: Duration = Duration::from_secs(45);

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// State for the backend relay client.
pub struct BackendRelayState {
    /// Shutdown signal
    shutdown_tx: watch::Sender<bool>,
    /// Kick counter: incremented to interrupt backoff sleeps and force an
    /// immediate reconnect with freshly-read settings + tokens. Used after
    /// settings save / token refresh so the running task picks up new state
    /// without a full runner restart.
    kick_tx: watch::Sender<u64>,
    /// Handle to the relay task
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl BackendRelayState {
    /// Stop the relay client, giving it a chance to shut down gracefully.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            // Give graceful shutdown 3 seconds before aborting
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => {
                    info!("Backend relay stopped gracefully");
                }
                Err(_) => {
                    warn!("Backend relay did not stop in 3s; shutdown signal sent, moving on");
                }
            }
        }
        info!("Backend relay stopped");
    }

    /// Kick the relay: interrupt any in-progress backoff sleep and force
    /// the loop to restart its next iteration immediately, re-reading
    /// settings and the runner token from scratch.
    pub fn kick(&self) {
        let current = *self.kick_tx.borrow();
        let _ = self.kick_tx.send(current.wrapping_add(1));
    }
}

/// Start the WS relay.
///
/// Spawns a task that owns the connection lifecycle: connect, send
/// `runner_info`, run inbound + outbound + heartbeat handlers concurrently,
/// reconnect with backoff. Each iteration re-reads `WebIntegrationSettings`,
/// so settings changes (new backend URL, fresh token) take effect on the
/// next attempt without a runner restart.
pub async fn start_relay(api_state: Arc<ApiState>) -> Arc<BackendRelayState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (kick_tx, kick_rx) = watch::channel(0u64);
    let state = api_state;

    let task_handle = tokio::spawn(async move {
        relay_loop(state, shutdown_rx, kick_rx).await;
    });

    Arc::new(BackendRelayState {
        shutdown_tx,
        kick_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

async fn relay_loop(
    api_state: Arc<ApiState>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut kick_rx: watch::Receiver<u64>,
) {
    let mut backoff_ms: u64 = 2000;
    let max_backoff_ms: u64 = 60000;
    let mut consecutive_quick_disconnects: u32 = 0;

    // Subscribe to event broadcast BEFORE the reconnection loop so events
    // are buffered during backoff/reconnection delays (rather than dropped).
    let mut event_rx = api_state.app_state.event_broadcast.subscribe();

    loop {
        if *shutdown_rx.borrow() {
            info!("Backend relay shutting down");
            return;
        }

        // Re-read settings on every iteration so backend URL / token
        // changes take effect on the next reconnect.
        let settings = crate::settings::load_settings();
        let web_integration = &settings.web_integration;

        // Phase 3 unified-devices + tier-decoupling: relay idles unless the
        // user is in Tier 2 (signed into Qontinui) AND web integration is
        // enabled AND a device-JWT is present in AuthManager's access_token
        // slot. The tier gate stops the 403-spam loop for Tier 0/1 installs
        // that have no business reaching qontinui-web. The JWT gate replaces
        // the legacy runner_token gate — coord now mints a short-lived JWT
        // that's the canonical WS bearer. See
        // `plans/2026-05-21-runner-unified-devices-migration.md` Phase 3.
        // The predicate is split into a pure inner (`should_relay_idle_with`)
        // and the I/O-bound wrapper (`should_relay_idle`) so the Phase 9
        // calibration suite can exercise the gate without touching disk.
        if should_relay_idle(&settings) {
            // Wait for a kick or shutdown before re-checking — there's
            // nothing to do until settings change.
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    info!("Backend relay shutting down (was idle awaiting config)");
                    return;
                }
                _ = kick_rx.changed() => {
                    info!("Backend relay kicked — re-reading config");
                    continue;
                }
            }
        }

        let backend_url = web_integration
            .backend_url
            .trim()
            .trim_end_matches('/')
            .to_string();

        let ws_url = format!(
            "{}/api/v1/devices/ws",
            backend_url
                .replace("https://", "wss://")
                .replace("http://", "ws://"),
        );

        // Pull the device-JWT from AuthManager's access_token slot —
        // populated by `pair::persist_pairing` and kept fresh by
        // `mcp::device_jwt_refresher`. The upstream `should_relay_idle`
        // gate already verified the slot is non-empty, but a TOCTOU race
        // with `clear_tokens` (sign-out) or an in-flight rotation could
        // leave us empty here; in that case, kick the refresher and
        // sleep until the next iteration. (Phase 3 of the unified-devices
        // migration plan.)
        let device_jwt = match crate::auth::AuthManager::new().get_access_token() {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                info!(
                    "Backend relay: device-JWT slot empty at connect time — \
                     kicking refresher and backing off"
                );
                crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
                sleep_with_kick(
                    Duration::from_millis(backoff_ms),
                    &mut shutdown_rx,
                    &mut kick_rx,
                )
                .await;
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                continue;
            }
        };

        // After many consecutive quick disconnects, the backend is
        // persistently rejecting us (e.g. revoked device-JWT, coord
        // un-paired this device). Back off aggressively to avoid
        // hammering the server.
        if consecutive_quick_disconnects >= 5 {
            let extended_backoff = max_backoff_ms.max(120_000);
            warn!(
                "Backend relay: {} consecutive quick disconnects. \
                 Check device-JWT validity (sign-in state) and backend \
                 connectivity. Backing off for {}s (send kick to retry sooner).",
                consecutive_quick_disconnects,
                extended_backoff / 1000
            );
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(extended_backoff)) => {}
                _ = shutdown_rx.changed() => {
                    info!("Backend relay shutting down during extended backoff");
                    return;
                }
                _ = kick_rx.changed() => {
                    info!("Backend relay kicked during extended backoff — retrying now");
                }
            }
            consecutive_quick_disconnects = 0;
            backoff_ms = 2000;
        }

        info!("Connecting to runner WS: {}", ws_url);

        // Build a tungstenite Request with a Bearer auth HEADER.
        // tokio-tungstenite 0.29 accepts an `http::Request` via
        // `IntoClientRequest`, which lets us set custom headers (the WS
        // spec doesn't define a header-auth scheme, but Anthropic-style
        // backends accept Authorization: Bearer here).
        let request_result: Result<HttpRequest, String> = build_ws_request(&ws_url, &device_jwt);
        let request = match request_result {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("Failed to build WS request: {}", e);
                warn!("{}", msg);
                record_connection_error(&api_state, msg).await;
                consecutive_quick_disconnects += 1;
                sleep_with_kick(
                    Duration::from_millis(backoff_ms),
                    &mut shutdown_rx,
                    &mut kick_rx,
                )
                .await;
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                continue;
            }
        };

        match connect_async(request).await {
            Ok((ws_stream, _response)) => {
                info!("Connected to runner WS at {}", ws_url);
                let connected_at = std::time::Instant::now();
                // NOTE: the connection error is intentionally NOT cleared
                // here. A successful TCP/WS *handshake* only means the
                // server accepted the socket — it may still close/reject us
                // before the `connected` application-ack (e.g. the
                // device-bridge register path fails because Redis pairing is
                // down). Clearing here left a misleading `ws_connected=false`
                // + `last_error=null` state that cost real time to diagnose
                // (Redis-disabled pairing outage). The error is now cleared
                // inside `handle_connected_message`, i.e. only once the
                // backend's `connected` ack actually arrives. The shared
                // `connected_ack` flag lets the inbound handler record an
                // actionable close reason if the socket dies before the ack.
                let connected_ack = Arc::new(AtomicBool::new(false));

                let (write, read) = ws_stream.split();
                let write = Arc::new(Mutex::new(write));

                // Send runner_info immediately. The backend uses this to
                // upsert the `runners` row and replies with `connected`
                // carrying the runner_id.
                if let Err(e) = send_runner_info(&api_state, &write).await {
                    warn!("Failed to send runner_info: {}", e);
                    consecutive_quick_disconnects += 1;
                    sleep_with_kick(
                        Duration::from_millis(backoff_ms),
                        &mut shutdown_rx,
                        &mut kick_rx,
                    )
                    .await;
                    backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                    continue;
                }

                // Run inbound, outbound, heartbeat, and keepalive concurrently.
                let write_inbound = write.clone();
                let write_outbound = write.clone();
                let write_heartbeat = write.clone();
                let write_keepalive = write.clone();
                let state_inbound = api_state.clone();
                let state_outbound = api_state.clone();
                let state_heartbeat = api_state.clone();
                let mut shutdown_clone = shutdown_rx.clone();
                // Mark the loop-level `kick_rx` seen at the current version
                // BEFORE cloning it for the connected select. This is the
                // fix for the reconnect kick-loop: a `watch::Receiver` clone
                // inherits the SOURCE receiver's last-observed version, and
                // `changed()` resolves whenever the channel version is newer
                // than that. The connected arm below awaits `kick_clone`'s
                // `changed()`, which marks only the CLONE seen — the loop
                // owner `kick_rx` stays at the pre-kick version. Without this
                // `borrow_and_update`, a single kick (which ends the connected
                // select, tears down, and `continue`s) would re-enter here,
                // clone `kick_rx` *still at the pre-kick version*, and the new
                // `kick_clone.changed()` would resolve IMMEDIATELY on the same
                // already-handled kick → tear down → loop, seeding a tight
                // connect/teardown loop (~23 cycles in 20s observed) that
                // hammered the device-WS register path and pressured the web
                // backend's Redis pool. Marking `kick_rx` seen here means each
                // fresh `kick_clone` starts level with the channel, so exactly
                // ONE genuine kick triggers exactly ONE reconnect.
                kick_rx.borrow_and_update();

                // Clone the kick receiver so an established connection can be
                // torn down on a kick (settings change / tier demotion /
                // sign-out). Without this arm the connected select only ends
                // when the connection dies on its own, so a `kick_cloud_relay()`
                // from `set_runner_tier(local)` would NOT idle a live relay —
                // `ws_connected` would stay true long after tier dropped to
                // Local. Re-looping re-evaluates `should_relay_idle`.
                let mut kick_clone = kick_rx.clone();

                // Shared "last inbound frame at" epoch-ms, updated by the
                // inbound handler on every received frame (Text, Ping, Pong,
                // Binary, ...) and read by the keepalive pinger to detect a
                // half-open TCP where local sends keep buffering successfully
                // but nothing comes back from the server.
                let last_inbound_ms = Arc::new(AtomicU64::new(now_epoch_ms()));
                let last_inbound_inbound = last_inbound_ms.clone();
                let last_inbound_keepalive = last_inbound_ms.clone();
                let connected_ack_inbound = connected_ack.clone();

                tokio::select! {
                    _ = handle_inbound(read, state_inbound, write_inbound, last_inbound_inbound, connected_ack_inbound) => {
                        warn!("Backend relay inbound handler ended");
                    }
                    _ = handle_outbound(&mut event_rx, write_outbound, state_outbound) => {
                        warn!("Backend relay outbound handler ended");
                    }
                    _ = run_heartbeat_sender(state_heartbeat, write_heartbeat) => {
                        warn!("Backend relay heartbeat sender ended");
                    }
                    _ = run_keepalive_pinger(write_keepalive, last_inbound_keepalive) => {
                        warn!("Backend relay keepalive detected dead connection");
                    }
                    _ = shutdown_clone.changed() => {
                        info!("Backend relay received shutdown signal");
                        mark_disconnected(&api_state).await;
                        return;
                    }
                    _ = kick_clone.changed() => {
                        info!(
                            "Backend relay kicked while connected — tearing down \
                             connection to re-evaluate idle gate (tier/enabled/JWT)"
                        );
                        mark_disconnected(&api_state).await;
                        backoff_ms = 2000;
                        consecutive_quick_disconnects = 0;
                        continue;
                    }
                }

                mark_disconnected(&api_state).await;

                // Connection ended. Decide whether this was a "stable" run
                // (≥10s) for backoff reset purposes.
                let connection_duration = connected_at.elapsed();
                if connection_duration > Duration::from_secs(10) {
                    backoff_ms = 2000;
                    consecutive_quick_disconnects = 0;
                } else {
                    consecutive_quick_disconnects += 1;
                    warn!(
                        "Backend relay connection lasted only {:.1}s (quick disconnect #{})",
                        connection_duration.as_secs_f64(),
                        consecutive_quick_disconnects
                    );
                }
            }
            Err(e) => {
                // Detect HTTP 401 on the upgrade handshake — coord rejects
                // the device-JWT (expired or revoked). Kick the refresher
                // so it re-exchanges the runner_token for a fresh JWT
                // before the next reconnect attempt; otherwise the relay
                // would sleep through the entire backoff before the
                // refresher's 5-min interval fires. (Phase 3 of the
                // unified-devices migration plan.) Detection factored
                // into [`is_unauthorized`] so the Phase 5.3 tests can
                // exercise the branching without spinning up a WS server.
                if is_unauthorized(&e) {
                    info!(
                        "Backend relay got 401 on WS upgrade — kicking \
                         device-JWT refresher to re-exchange"
                    );
                    crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
                }
                let msg = format!("Failed to connect to runner WS: {}", e);
                warn!("{}", msg);
                record_connection_error(&api_state, msg).await;
                consecutive_quick_disconnects += 1;
            }
        }

        info!(
            "Reconnecting in {}ms... (send kick to retry sooner)",
            backoff_ms
        );
        sleep_with_kick(
            Duration::from_millis(backoff_ms),
            &mut shutdown_rx,
            &mut kick_rx,
        )
        .await;
        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
    }
}

/// Build a tungstenite client `Request` with an `Authorization: Bearer`
/// header. We start from the URL's parsed `IntoClientRequest` form (which
/// fills in `Host`, `Upgrade`, `Connection`, `Sec-WebSocket-*` headers)
/// and then set our auth header on top. The bearer is the device-JWT
/// minted by coord; see Phase 3 of the unified-devices migration plan.
fn build_ws_request(ws_url: &str, device_jwt: &str) -> Result<HttpRequest, String> {
    let mut req = ws_url
        .into_client_request()
        .map_err(|e| format!("invalid ws URL: {}", e))?;
    let auth_value = format!("Bearer {}", device_jwt);
    let header_value = HeaderValue::from_str(&auth_value)
        .map_err(|e| format!("invalid auth header value: {}", e))?;
    req.headers_mut()
        .insert(header::AUTHORIZATION, header_value);
    Ok(req)
}

/// Send the initial `runner_info` payload. The backend uses this to upsert
/// the `runners` row keyed by (user_id, name) and replies with `connected`.
async fn send_runner_info<S>(
    api_state: &Arc<ApiState>,
    write: &Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let port = api_state.app_state.api_port.load(Ordering::Relaxed);
    let instance_name = crate::instance::instance_name();
    let hostname = hostname::get()
        .ok()
        .map(|h| h.to_string_lossy().to_string());

    // Capabilities are static for now; the runner does GUI automation +
    // accessibility + Restate-style durable execution. Future work can
    // surface dynamic capabilities (e.g. presence of a Python bridge).
    let capabilities = vec![
        "gui_automation".to_string(),
        "accessibility".to_string(),
        "restate".to_string(),
    ];

    // Payload field names are camelCase to match the backend contract.
    let runner_info = serde_json::json!({
        "type": "runner_info",
        "name": instance_name.unwrap_or_else(|| "primary".to_string()),
        "hostname": hostname,
        "ipAddress": null,
        "port": port,
        "os": std::env::consts::OS,
        "osVersion": std::env::consts::ARCH,
        "capabilities": capabilities,
    });

    let info_text =
        serde_json::to_string(&runner_info).map_err(|e| format!("serialize runner_info: {}", e))?;
    let mut w = write.lock().await;
    w.send(Message::Text(info_text.into()))
        .await
        .map_err(|e| format!("send runner_info: {}", e))?;
    info!("Sent runner_info to backend (port={})", port);
    Ok(())
}

/// Inbound message handler. Reads from the WS, routes typed messages to
/// the appropriate local executor, and sends responses back over the same
/// connection.
async fn handle_inbound<S>(
    mut read: futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    api_state: Arc<ApiState>,
    write: Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
    last_inbound_ms: Arc<AtomicU64>,
    // Set to `true` once the backend's `connected` application-ack has been
    // processed. If the socket closes/errors while this is still `false`, the
    // server accepted the TCP/WS handshake but rejected us before completing
    // registration — record the close reason as an actionable `last_error`
    // instead of leaving `ws_connected=false` + `last_error=null`.
    connected_ack: Arc<AtomicBool>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(msg_result) = read.next().await {
        // Any inbound frame (including Pong, which we otherwise discard
        // below) is proof the server is reachable. Stamp the shared
        // timestamp before doing anything else so the keepalive pinger's
        // staleness check stays accurate even when the read side is
        // ahead of the message-routing code.
        if msg_result.is_ok() {
            last_inbound_ms.store(now_epoch_ms(), Ordering::Relaxed);
        }
        match msg_result {
            Ok(Message::Text(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(data) => {
                    let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    info!("Relay inbound message: type={}", msg_type);

                    // Special-case the `connected` handshake reply so we can
                    // capture runner_id on the shared state.
                    if msg_type == "connected" {
                        handle_connected_message(&api_state, &data).await;
                        // The registration ack arrived: the relay is fully
                        // up. Mark the ack so a later close isn't
                        // misclassified as close-before-ack.
                        connected_ack.store(true, Ordering::Relaxed);
                        continue;
                    }

                    let response = handle_relay_command(&api_state, msg_type, &data).await;
                    if let Some(response) = response {
                        let response_text = serde_json::to_string(&response).unwrap_or_default();
                        let mut w = write.lock().await;
                        if let Err(e) = w.send(Message::Text(response_text.into())).await {
                            warn!("Failed to send relay response: {}", e);
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to parse relay message: {}", e);
                }
            },
            Ok(Message::Ping(data)) => {
                let mut w = write.lock().await;
                let _ = w.send(Message::Pong(data)).await;
            }
            Ok(Message::Close(frame)) => {
                if let Some(ref f) = frame {
                    warn!(
                        "Backend relay WS closed by server: code={}, reason={}",
                        f.code, f.reason
                    );
                } else {
                    info!("Backend relay WS closed (no close frame)");
                }
                // If the server closed us before the `connected` ack, this
                // is a registration rejection (e.g. device-bridge register
                // failed because Redis pairing is down). Surface it as a
                // non-null `last_error` so status queries show an actionable
                // reason instead of a silent `ws_connected=false` /
                // `last_error=null`.
                if !connected_ack.load(Ordering::Relaxed) {
                    let detail = match frame {
                        Some(f) if !f.reason.is_empty() => {
                            format!("code={}, reason={}", f.code, f.reason)
                        }
                        Some(f) => format!("code={}", f.code),
                        None => "no close frame".to_string(),
                    };
                    record_connection_error(&api_state, close_before_ack_error(&detail)).await;
                }
                return;
            }
            Err(e) => {
                warn!("Backend relay read error: {}", e);
                if !connected_ack.load(Ordering::Relaxed) {
                    record_connection_error(
                        &api_state,
                        format!(
                            "Backend relay read error before the `connected` ack \
                             (registration rejected): {}",
                            e
                        ),
                    )
                    .await;
                }
                return;
            }
            _ => {}
        }
    }
}

/// Apply the backend's `connected` reply to the shared state. The reply
/// shape is:
///
/// ```json
/// {"type": "connected", "device_id": "<uuid>"}
/// ```
///
/// The backend's `/api/v1/devices/ws` handler sends `device_id` (post the
/// 2026-05 unify-runner-concepts migration); this module historically
/// only read `runner_id`, which left `runnerId: null` in
/// `get_web_integration_status` forever and stymied any caller relying on
/// it for routing. Accept either key so the relay is resilient to the
/// backend's verbiage drift.
async fn handle_connected_message(api_state: &Arc<ApiState>, data: &Value) {
    let sm_state = match api_state.app_state.current_server_mode().await {
        Some(s) => s,
        None => {
            // No ServerModeState was installed at boot — e.g. web integration
            // was disabled then, and a later Cognito/pair-code sign-in enabled
            // it via save_settings + a relay kick without routing through
            // apply_web_integration_settings. We're demonstrably connected, so
            // self-install from current settings; otherwise status queries
            // would report this runner disconnected until the next restart.
            match api_state.app_state.install_server_mode_if_absent().await {
                Some(s) => {
                    info!(
                        "Backend relay: self-installed ServerModeState on connect \
                         (none was present at boot)"
                    );
                    s
                }
                None => {
                    warn!(
                        "Received `connected` message but no ServerModeState is installed \
                         and current settings don't form a valid config; the relay will \
                         continue but state queries will return empty"
                    );
                    return;
                }
            }
        }
    };

    sm_state.set_ws_connected(true);
    // Clear any prior connection error ONLY now that the backend has
    // acknowledged our registration. Clearing on the bare handshake (the
    // old behavior) masked register-path rejections that close the socket
    // after accept but before this ack.
    clear_connection_error(api_state).await;

    let id_str = data
        .get("device_id")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("runner_id").and_then(|v| v.as_str()));

    if let Some(rid_str) = id_str {
        match Uuid::parse_str(rid_str) {
            Ok(rid) => {
                sm_state.set_runner_id(rid).await;
                info!("Backend assigned device/runner id={}", rid);
            }
            Err(e) => {
                warn!(
                    "Backend `connected` message had unparseable device/runner id={}: {}",
                    rid_str, e
                );
            }
        }
    }
}

/// Heartbeat sender. Every 30s, write a `{"type": "heartbeat"}` message.
/// The backend's `last_heartbeat` updates from this. On send failure the
/// task ends, which causes the parent `tokio::select!` to drop the
/// connection and reconnect.
async fn run_heartbeat_sender<S>(
    api_state: Arc<ApiState>,
    write: Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    interval.tick().await; // skip immediate first tick

    loop {
        interval.tick().await;

        // Pull the latest UI error / recent crash from local state so the
        // heartbeat carries a fresh derived health signal. The web backend
        // can derive its `derived_status` from this.
        let ui_error_snapshot = api_state.app_state.ui_error.get().await;
        let recent_crash_snapshot = api_state.app_state.crash_dumps.get().await;
        let derived_status = crate::ui_error::compute_derived_status(
            ui_error_snapshot.is_some(),
            recent_crash_snapshot.is_some(),
            crate::mcp_api::embedding_reachable_cached(),
        )
        .to_string();

        let payload = serde_json::json!({
            "type": "heartbeat",
            "derivedStatus": derived_status,
            "uiError": ui_error_snapshot,
            "recentCrash": recent_crash_snapshot,
        });
        let text = match serde_json::to_string(&payload) {
            Ok(t) => t,
            Err(e) => {
                warn!("Heartbeat serialize failed: {}", e);
                continue;
            }
        };

        let mut w = write.lock().await;
        if let Err(e) = w.send(Message::Text(text.into())).await {
            warn!("Heartbeat send failed: {}", e);
            return;
        }
        drop(w);

        // Update last-heartbeat-at on shared state for the Settings UI.
        if let Some(sm) = api_state.app_state.current_server_mode().await {
            sm.set_last_heartbeat_at(chrono::Utc::now().to_rfc3339())
                .await;
        }
    }
}

/// Keepalive pinger. Sends a low-level Ping frame every 20s so we can
/// detect dead/half-open TCP sessions even when the heartbeat send appears
/// to succeed locally but the server-side socket has gone away.
///
/// Also gates on `last_inbound_ms`: if no frame has arrived from the
/// server in `STALE_INBOUND_TIMEOUT`, declare the connection dead and
/// return. The outer `tokio::select!` arm then drops the WS and the
/// reconnect loop opens a fresh session. Without this check, a half-open
/// TCP can keep `wsConnected=true` on the runner indefinitely while the
/// backend has already cleared `device.ws_session_id` — the exact bug
/// observed in 2026-05-22.
async fn run_keepalive_pinger<S>(
    write: Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
    last_inbound_ms: Arc<AtomicU64>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut interval = tokio::time::interval(KEEPALIVE_PING_INTERVAL);
    interval.tick().await;
    loop {
        interval.tick().await;

        // Staleness check: if nothing has come from the server in
        // STALE_INBOUND_TIMEOUT, force a reconnect.
        let last = last_inbound_ms.load(Ordering::Relaxed);
        let now = now_epoch_ms();
        if now.saturating_sub(last) > STALE_INBOUND_TIMEOUT.as_millis() as u64 {
            warn!(
                "Backend relay keepalive: no inbound frame for {}ms (limit {}ms) — \
                 connection appears half-open, forcing reconnect",
                now.saturating_sub(last),
                STALE_INBOUND_TIMEOUT.as_millis()
            );
            return;
        }

        let mut w = write.lock().await;
        if let Err(e) = w.send(Message::Ping(vec![].into())).await {
            warn!("Keepalive ping failed: {}", e);
            return;
        }
    }
}

/// Outbound event forwarder. Subscribes to `event_broadcast` and forwards
/// matching events as typed WS messages. Channels handled:
///
/// - `phase-result` → `phase_completed`
/// - `ui-error` → `ui_error`
/// - `recent-crash` → `recent_crash`
/// - `ai-output` → `chat_response` (mobile chat sessions)
/// - `session-state` → `chat_session_state`
/// - `terminal-output` / `terminal-exit` → `terminal_output` / `terminal_exit`
async fn handle_outbound<S>(
    event_rx: &mut broadcast::Receiver<Value>,
    write: Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
    api_state: Arc<ApiState>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // The WS relay is per-connection and the ServerModeState clone is a
    // cheap Arc-backed handle that lives for the connection's lifetime, so
    // fetch it once up front. If it's absent at connection start (state not
    // yet installed) we re-fetch lazily below.
    let mut server_mode = api_state.app_state.current_server_mode().await;

    loop {
        match event_rx.recv().await {
            Ok(event) => {
                let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");

                // Flood control: only forward terminal-output / terminal-exit
                // frames when the backend has an active terminal subscriber.
                // Never gate phase-result / ui-error / recent-crash /
                // ai-output / session-state.
                if channel == "terminal-output" || channel == "terminal-exit" {
                    if server_mode.is_none() {
                        server_mode = api_state.app_state.current_server_mode().await;
                    }
                    let subscribed = server_mode
                        .as_ref()
                        .map(|sm| sm.terminal_subscriber_count() > 0)
                        .unwrap_or(false);
                    if !subscribed {
                        continue;
                    }
                }

                // Wire shapes are defined in `qontinui_runner_lib::relay_envelopes`
                // (lib-side so the schema_export aggregator can register them) —
                // see that module's docs for the discriminated-union contract
                // shared with web + mobile consumers via
                // `@qontinui/shared-types/tauri-events`.
                use qontinui_runner_lib::relay_envelopes::RunnerRelayMessage;
                let relay_msg = match channel {
                    "phase-result" => {
                        // event.payload = { execution_id, result }
                        let payload = event.get("payload").cloned().unwrap_or(Value::Null);
                        RunnerRelayMessage::PhaseCompleted { data: payload }
                    }
                    "ui-error" => RunnerRelayMessage::UiError {
                        data: event.get("payload").cloned().unwrap_or(Value::Null),
                    },
                    "recent-crash" => RunnerRelayMessage::RecentCrash {
                        data: event.get("payload").cloned().unwrap_or(Value::Null),
                    },
                    "ai-output" => RunnerRelayMessage::ChatResponse {
                        data: event.get("payload").cloned().unwrap_or(Value::Null),
                    },
                    "session-state" => RunnerRelayMessage::ChatSessionState {
                        data: event.get("payload").cloned().unwrap_or(Value::Null),
                    },
                    "terminal-output" => RunnerRelayMessage::TerminalOutput {
                        terminal_id: event
                            .get("payload")
                            .and_then(|p| p.get("terminal_id"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        data: event
                            .get("payload")
                            .and_then(|p| p.get("data"))
                            .cloned()
                            .unwrap_or(Value::Null),
                    },
                    "terminal-exit" => RunnerRelayMessage::TerminalExit {
                        terminal_id: event
                            .get("payload")
                            .and_then(|p| p.get("terminal_id"))
                            .cloned()
                            .unwrap_or(Value::Null),
                        exit_code: event
                            .get("payload")
                            .and_then(|p| p.get("exit_code"))
                            .cloned()
                            .unwrap_or(Value::Null),
                    },
                    _ => continue,
                };

                let text = serde_json::to_string(&relay_msg).unwrap_or_default();
                let mut w = write.lock().await;
                if let Err(e) = w.send(Message::Text(text.into())).await {
                    warn!("Failed to forward event to backend: {}", e);
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Backend relay lagged, skipped {} events", n);
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("Event broadcast closed, stopping outbound relay");
                return;
            }
        }
    }
}

/// Build the `last_error` text for a socket that closed *before* the
/// backend's `connected` application-ack. The server accepted the TCP/WS
/// handshake but rejected us during registration (e.g. the device-bridge
/// register path failed because Redis pairing is down). Surfacing this as a
/// non-null, actionable error avoids the misleading `ws_connected=false` /
/// `last_error=null` state that previously stymied diagnosis.
fn close_before_ack_error(detail: &str) -> String {
    format!(
        "Backend closed the WS before the `connected` ack (registration rejected): {}",
        detail
    )
}

async fn record_connection_error(api_state: &Arc<ApiState>, msg: String) {
    if let Some(sm) = api_state.app_state.current_server_mode().await {
        sm.set_registration_error(Some(msg)).await;
        sm.set_ws_connected(false);
    }
}

async fn clear_connection_error(api_state: &Arc<ApiState>) {
    if let Some(sm) = api_state.app_state.current_server_mode().await {
        sm.set_registration_error(None).await;
    }
}

async fn mark_disconnected(api_state: &Arc<ApiState>) {
    if let Some(sm) = api_state.app_state.current_server_mode().await {
        sm.set_ws_connected(false);
    }
}

async fn sleep_with_kick(
    delay: Duration,
    shutdown_rx: &mut watch::Receiver<bool>,
    kick_rx: &mut watch::Receiver<u64>,
) {
    tokio::select! {
        _ = tokio::time::sleep(delay) => {}
        _ = shutdown_rx.changed() => {}
        _ = kick_rx.changed() => {}
    }
}

/// Handle an inbound command from the backend.
///
/// Recognized message types:
///
/// - `dispatch` — start a workflow run. Replies with `dispatch_ack`.
/// - `command`, `chat`, `terminal` — currently routed to the legacy
///   chat/terminal handlers below for back-compat. Phase 4 will rename
///   the inbound types but the routing stays the same.
/// - `chat_*` — legacy chat session commands (kept for mobile relay).
/// - `terminal_*` — legacy terminal commands (kept for mobile relay).
/// - `heartbeat` — server pong; reply with `heartbeat_ack`.
async fn handle_relay_command(
    api_state: &Arc<ApiState>,
    msg_type: &str,
    data: &Value,
) -> Option<Value> {
    match msg_type {
        // --------------------------------------------------------------
        // Phase 3 protocol — typed dispatch / command / chat / terminal
        // --------------------------------------------------------------
        "dispatch" => handle_dispatch(api_state, data).await,

        "command" | "chat" | "terminal" => {
            // Frontend WS endpoints now relay through this single channel.
            // The inner `subtype` field carries the original command name
            // (e.g. "chat_message" inside a `chat` envelope). Unwrap and
            // dispatch through the legacy handler.
            let subtype = data.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            let inner = data.get("payload").cloned().unwrap_or_else(|| data.clone());
            // Box the recursive call to avoid an infinitely-sized future.
            return Box::pin(handle_relay_command(api_state, subtype, &inner)).await;
        }

        // --------------------------------------------------------------
        // Web -> runner WS bridge commands. The web side's
        // `dispatch_and_wait` helper publishes JSON payloads with a
        // top-level `type` matching the command name; we forward to
        // the Python dispatcher and return a `command_response` frame
        // carrying the same `request_id` so the web HTTP handler can
        // correlate the reply. Per
        // `plans/2026-05-17-web-runner-ws-bridge-plan-b.md` Phase 2,
        // this is the foundational arm — Phases 3-7 extend the match
        // (one new arm per new command type) and
        // `ws_bridge_dispatch::is_supported_command`.
        // --------------------------------------------------------------
        cmd if crate::mcp::ws_bridge_dispatch::is_supported_command(cmd) => {
            let response = crate::mcp::ws_bridge_dispatch::dispatch_command(cmd, data).await;
            Some(response)
        }

        // --------------------------------------------------------------
        // Generic HTTP relay (mobile remote-runner connection). The web
        // backend's `runner_proxy` handler relays a mobile client's HTTP
        // control call down this WS as a top-level `http_request` envelope
        // and waits (by `request_id`) for a `command_response`. We self-call
        // the runner's OWN local Axum server over loopback and return the
        // response. This relays *every* runner route transparently — see
        // `plans/2026-05-25-mobile-backend-remote-runner-relay.md` §1a/§2.
        // --------------------------------------------------------------
        "http_request" => handle_http_request(api_state, data).await,

        // --------------------------------------------------------------
        // Mobile chat session relay (carry-over from legacy relay)
        // --------------------------------------------------------------
        "chat_message" => handle_chat_message(api_state, data).await,
        "chat_list_running" => handle_chat_list_running(api_state).await,
        "chat_session_state" => handle_chat_session_state(api_state, data).await,
        "chat_create" => handle_chat_create(api_state, data).await,
        "chat_interrupt" => handle_chat_interrupt(api_state, data).await,
        "chat_close" => handle_chat_close(api_state, data).await,
        "chat_generate_workflow" => handle_chat_generate_workflow(api_state, data).await,
        "chat_get_output" => handle_chat_get_output(api_state, data).await,
        "chat_rename" => handle_chat_rename(api_state, data).await,

        // --------------------------------------------------------------
        // Mobile terminal session relay
        // --------------------------------------------------------------
        "terminal_list" => handle_terminal_list(api_state, data),
        "terminal_create" => handle_terminal_create(api_state, data).await,
        "terminal_input" => handle_terminal_input(api_state, data),
        "terminal_resize" => handle_terminal_resize(api_state, data),
        "terminal_close" => handle_terminal_close(api_state, data).await,
        "terminal_buffer" => handle_terminal_buffer(api_state, data),

        "heartbeat" => Some(serde_json::json!({"type": "heartbeat_ack"})),

        // --------------------------------------------------------------
        // Terminal-output flow control. The backend opens/closes interest
        // in terminal frames so the runner can suppress the
        // `terminal-output` / `terminal-exit` WS flood when nobody is
        // attached. Wire contract (other repos depend on it):
        //   {"type":"terminal_subscribe","runner_id":"<uuid>"}
        //   {"type":"terminal_unsubscribe","runner_id":"<uuid>"}
        // runner_id is informational — the WS is already runner-scoped.
        // Fire-and-forget: no response is sent.
        // --------------------------------------------------------------
        "terminal_subscribe" => {
            if let Some(sm) = api_state.app_state.current_server_mode().await {
                sm.incr_terminal_subscribers();
                info!(
                    "terminal_subscribe: terminal-output relay enabled \
                     (subscribers={})",
                    sm.terminal_subscriber_count()
                );
            } else {
                warn!(
                    "terminal_subscribe received but no ServerModeState is \
                     installed; terminal-output relay stays suppressed"
                );
            }
            None
        }

        "terminal_unsubscribe" => {
            if let Some(sm) = api_state.app_state.current_server_mode().await {
                sm.decr_terminal_subscribers();
                info!(
                    "terminal_unsubscribe: subscribers now {}",
                    sm.terminal_subscriber_count()
                );
            } else {
                warn!(
                    "terminal_unsubscribe received but no ServerModeState is \
                     installed"
                );
            }
            None
        }

        _ => {
            warn!("Unknown relay command type: {}", msg_type);
            None
        }
    }
}

/// Hop-by-hop request headers that must NOT be forwarded to the local
/// loopback call (they describe the WS-relayed transport, not the inner
/// request). Lowercased for case-insensitive comparison.
const RELAY_SKIP_REQUEST_HEADERS: &[&str] =
    &["host", "connection", "transfer-encoding", "content-length"];

/// Hop-by-hop / framing response headers that must NOT be echoed back over
/// the WS (reqwest already decoded the body; `content-length` would be
/// wrong post-base64 round-trip and `transfer-encoding` is meaningless off
/// the wire). Lowercased.
const RELAY_SKIP_RESPONSE_HEADERS: &[&str] = &[
    "transfer-encoding",
    "connection",
    "keep-alive",
    "content-length",
];

/// Build the `command_response` reply frame the backend's `dispatch_and_wait`
/// correlates on. `headers` is a JSON object of string→string; `body_b64` is
/// the (already base64-encoded) body. Carries the originating `request_id`
/// verbatim so the waiter wakes and maps the reply.
fn http_relay_response(request_id: &Value, status: u16, headers: Value, body_b64: String) -> Value {
    serde_json::json!({
        "type": "command_response",
        "request_id": request_id,
        "status": status,
        "headers": headers,
        "body_b64": body_b64,
    })
}

/// Build a small JSON-body error reply (`command_response`) with the given
/// status. The body is `{"error": <message>}` base64-encoded; the header set
/// advertises `content-type: application/json`.
fn http_relay_error(request_id: &Value, status: u16, message: &str) -> Value {
    let body = serde_json::json!({ "error": message }).to_string();
    let headers = serde_json::json!({ "content-type": "application/json" });
    http_relay_response(
        request_id,
        status,
        headers,
        STANDARD.encode(body.as_bytes()),
    )
}

/// Handle a generic `http_request` relay command (mobile remote-runner
/// connection). Self-calls the runner's own local Axum server over loopback
/// and returns the response as a `command_response` frame. See
/// `plans/2026-05-25-mobile-backend-remote-runner-relay.md` §2 / Phase 2.
///
/// Always returns `Some(command_response)` — even on error — so the backend's
/// `dispatch_and_wait` wakes and maps the reply rather than timing out.
async fn handle_http_request(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    // Build the loopback base URL from the runner's OWN bound port (never
    // hardcode 9876 — secondary/temp runners bind elsewhere), then delegate
    // to the transport-agnostic core (testable without an `ApiState`).
    let base = crate::mcp::types::get_self_base_url(&api_state.app_state);
    Some(relay_http_to_base(&base, data).await)
}

/// Transport-agnostic core of the `http_request` relay: given the loopback
/// base URL (e.g. `http://localhost:9876`) and the inbound envelope, issue
/// the local call and build the `command_response` reply frame. Split out so
/// it can be unit-tested against a real local server without constructing a
/// full `ApiState`. Always returns a `command_response` value.
async fn relay_http_to_base(base: &str, data: &Value) -> Value {
    // `request_id` is echoed verbatim (may be string/null); the backend
    // correlates the reply purely on it.
    let request_id = data.get("request_id").cloned().unwrap_or(Value::Null);

    let method_str = data
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let method = match reqwest::Method::from_bytes(method_str.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            warn!("http_request relay: invalid method {:?}", method_str);
            return http_relay_error(&request_id, 400, "invalid HTTP method");
        }
    };

    let path = data
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('/');
    let query = data.get("query").and_then(|v| v.as_str()).unwrap_or("");

    // Decode the request body (base64). Empty / absent => no body.
    let body_b64 = data.get("body_b64").and_then(|v| v.as_str()).unwrap_or("");
    let body_bytes: Vec<u8> = if body_b64.is_empty() {
        Vec::new()
    } else {
        match STANDARD.decode(body_b64) {
            Ok(b) => b,
            Err(e) => {
                warn!("http_request relay: invalid body_b64: {}", e);
                return http_relay_error(&request_id, 400, "invalid base64 request body");
            }
        }
    };

    if body_bytes.len() > RELAY_MAX_BODY_BYTES {
        warn!(
            "http_request relay: request body {} bytes exceeds cap {}",
            body_bytes.len(),
            RELAY_MAX_BODY_BYTES
        );
        return http_relay_error(&request_id, 413, "request body exceeds relay size limit");
    }

    // Build the loopback URL from the supplied base (caller derives it from
    // the runner's OWN bound port via `get_self_base_url`).
    let mut url = format!("{}/{}", base.trim_end_matches('/'), path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }

    // Forward request headers minus hop-by-hop.
    let mut header_map = reqwest::header::HeaderMap::new();
    if let Some(obj) = data.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            let lk = k.to_lowercase();
            if RELAY_SKIP_REQUEST_HEADERS.contains(&lk.as_str()) {
                continue;
            }
            let val = match v.as_str() {
                Some(s) => s.to_string(),
                None => v.to_string(),
            };
            match (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(&val),
            ) {
                (Ok(name), Ok(value)) => {
                    header_map.insert(name, value);
                }
                _ => {
                    warn!("http_request relay: skipping invalid header {:?}", k);
                }
            }
        }
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("http_request relay: failed to build client: {}", e);
            return http_relay_error(&request_id, 502, "failed to build relay client");
        }
    };

    let mut req = client.request(method, &url).headers(header_map);
    if !body_bytes.is_empty() {
        req = req.body(body_bytes);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("http_request relay: local call to {} failed: {}", url, e);
            return http_relay_error(&request_id, 502, "runner local HTTP call failed");
        }
    };

    let status = resp.status().as_u16();

    // Collect response headers minus hop-by-hop, as a JSON string->string map.
    let mut headers_obj = serde_json::Map::new();
    for (name, value) in resp.headers().iter() {
        let lname = name.as_str().to_lowercase();
        if RELAY_SKIP_RESPONSE_HEADERS.contains(&lname.as_str()) {
            continue;
        }
        if let Ok(s) = value.to_str() {
            headers_obj.insert(name.as_str().to_string(), Value::String(s.to_string()));
        }
    }

    let resp_body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!("http_request relay: reading response body failed: {}", e);
            return http_relay_error(&request_id, 502, "reading runner response body failed");
        }
    };

    if resp_body.len() > RELAY_MAX_BODY_BYTES {
        warn!(
            "http_request relay: response body {} bytes exceeds cap {}",
            resp_body.len(),
            RELAY_MAX_BODY_BYTES
        );
        return http_relay_error(&request_id, 413, "response body exceeds relay size limit");
    }

    let body_out = STANDARD.encode(&resp_body);
    http_relay_response(&request_id, status, Value::Object(headers_obj), body_out)
}

/// Handle an inbound `dispatch` message from the backend. Spawns the
/// workflow on the auto_run path and replies with `dispatch_ack`.
async fn handle_dispatch(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let workflow_id = data
        .get("workflow_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let parent_task_run_id = data
        .get("parent_task_run_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let dispatch_id = data
        .get("dispatch_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let workflow_id = match workflow_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            return Some(serde_json::json!({
                "type": "dispatch_ack",
                "dispatch_id": dispatch_id,
                "success": false,
                "error": "missing workflow_id",
            }));
        }
    };

    let deps = crate::unified_workflow_executor::auto_run::AutoRunDeps {
        app_state: api_state.app_state.clone(),
        config_storage: api_state.config_storage.clone(),
        app_handle: api_state.app_handle.clone(),
        pid_tracker: api_state.current_ai_pids.clone(),
    };

    let workflow_id_for_task = workflow_id.clone();
    let parent_owned = parent_task_run_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::unified_workflow_executor::auto_run::launch_workflow_by_id(
            deps,
            &workflow_id_for_task,
            parent_owned.as_deref(),
        )
    })
    .await;

    match result {
        Ok(Ok(execution_id)) => Some(serde_json::json!({
            "type": "dispatch_ack",
            "dispatch_id": dispatch_id,
            "success": true,
            "execution_id": execution_id,
            "workflow_id": workflow_id,
        })),
        Ok(Err(e)) => Some(serde_json::json!({
            "type": "dispatch_ack",
            "dispatch_id": dispatch_id,
            "success": false,
            "error": e,
        })),
        Err(join_err) => Some(serde_json::json!({
            "type": "dispatch_ack",
            "dispatch_id": dispatch_id,
            "success": false,
            "error": format!("dispatch task failed: {}", join_err),
        })),
    }
}

// ---------------------------------------------------------------------------
// Mobile chat handlers — preserved from the legacy relay
// ---------------------------------------------------------------------------

async fn handle_chat_message(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");

    if task_run_id.is_empty() || content.is_empty() {
        return Some(serde_json::json!({
            "type": "chat_message_ack",
            "success": false,
            "error": "Missing task_run_id or content"
        }));
    }

    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|s| s.inner().clone());

    if let Some(sm) = session_manager {
        if let Some(session) = sm.get(task_run_id) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::mcp::shared::emit_ai_output(
                    &api_state.app_handle,
                    content,
                    "user_message",
                    None,
                    None,
                );
            }));

            let msg = format!("\n[USER_MESSAGE]\n{}\n[/USER_MESSAGE]\n", content);
            let _ = api_state
                .app_state
                .pg_db
                .append_task_output_ex(task_run_id, &msg, false, false)
                .await;

            match session.send_user_message(content) {
                Ok(sent_immediately) => Some(serde_json::json!({
                    "type": "chat_message_ack",
                    "success": true,
                    "queued": !sent_immediately,
                    "task_run_id": task_run_id,
                    "state": session.state().as_event_str()
                })),
                Err(e) => Some(serde_json::json!({
                    "type": "chat_message_ack",
                    "success": false,
                    "error": format!("Send failed: {}", e),
                    "task_run_id": task_run_id
                })),
            }
        } else {
            Some(serde_json::json!({
                "type": "chat_message_ack",
                "success": false,
                "error": "No active session for task_run_id",
                "task_run_id": task_run_id
            }))
        }
    } else {
        Some(serde_json::json!({
            "type": "chat_message_ack",
            "success": false,
            "error": "SessionManager not available"
        }))
    }
}

async fn handle_chat_list_running(api_state: &Arc<ApiState>) -> Option<Value> {
    match api_state.app_state.pg_db.get_running_task_runs(None).await {
        Ok(runs) => {
            let run_list: Vec<Value> = runs
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "task_name": r.task_name,
                        "status": r.status,
                        "created_at": r.created_at,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "type": "chat_running_tasks",
                "tasks": run_list
            }))
        }
        _ => Some(serde_json::json!({
            "type": "chat_running_tasks",
            "tasks": [],
            "error": "Failed to query running tasks"
        })),
    }
}

async fn handle_chat_session_state(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if task_run_id.is_empty() {
        return Some(serde_json::json!({
            "type": "chat_session_state",
            "state": "not_found",
            "can_send": false,
            "can_interrupt": false
        }));
    }

    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|s| s.inner().clone());

    if let Some(sm) = session_manager {
        if let Some(session) = sm.get(task_run_id) {
            let current_state = session.state();
            Some(serde_json::json!({
                "type": "chat_session_state",
                "task_run_id": task_run_id,
                "state": current_state.as_event_str(),
                "can_send": current_state.can_send_message(),
                "can_interrupt": current_state.can_interrupt(),
                "session_id": session.session_id(),
                "user_interacted": session.has_user_interacted()
            }))
        } else {
            Some(serde_json::json!({
                "type": "chat_session_state",
                "task_run_id": task_run_id,
                "state": "not_found",
                "can_send": false,
                "can_interrupt": false
            }))
        }
    } else {
        Some(serde_json::json!({
            "type": "chat_session_state",
            "state": "not_found",
            "can_send": false,
            "can_interrupt": false
        }))
    }
}

async fn handle_chat_create(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    info!("Handling chat_create command");
    let task_name = data
        .get("task_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Remote Chat");
    let initial_prompt = data
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let task_run_id = uuid::Uuid::new_v4().to_string();

    let create_input = crate::database::CreateTaskRunInput::new(&task_run_id, task_name)
        .with_prompt("Remote AI session")
        .with_workflow_type("chat");
    let create_result = api_state
        .app_state
        .pg_db
        .create_task_run(&create_input)
        .await;

    if create_result.is_err() {
        return Some(serde_json::json!({
            "type": "chat_created",
            "error": "Failed to create task run"
        }));
    }

    let bg_state = api_state.clone();
    let bg_task_run_id = task_run_id.clone();
    let bg_task_name = task_name.to_string();
    tokio::spawn(async move {
        let session_manager: Option<Arc<crate::claude_session::SessionManager>> = bg_state
            .app_handle
            .try_state::<Arc<crate::claude_session::SessionManager>>()
            .map(|s| s.inner().clone());

        let Some(session_manager) = session_manager else {
            warn!("SessionManager not available for relay chat");
            return;
        };

        let working_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        let system_prompt = "You are an AI assistant in a session initiated from the \
            qontinui mobile app. Respond helpfully and conversationally. The user may ask \
            about anything — workflows, automation, code questions, or general topics."
            .to_string();

        match crate::claude_session::ClaudeSession::spawn(
            &working_dir,
            &bg_task_run_id,
            &bg_state.app_handle,
            None,
            None,
            None,
            None,
            None,
            None,
        ) {
            Ok(session) => {
                let session = Arc::new(session);

                if let Err(e) = session_manager.register(&bg_task_run_id, session.clone()) {
                    warn!("Failed to register relay AI session: {}", e);
                    return;
                }

                crate::commands::ai_session::emit_session_state(
                    &bg_state.app_handle,
                    &bg_task_run_id,
                    &bg_task_run_id,
                    session.state(),
                );

                let prompt_to_send = initial_prompt.as_deref().unwrap_or(&system_prompt);
                if let Err(e) = session.send_initial_prompt(prompt_to_send) {
                    warn!("Failed to send initial prompt for relay chat: {}", e);
                    return;
                }

                crate::commands::ai_session::emit_session_state(
                    &bg_state.app_handle,
                    &bg_task_run_id,
                    &bg_task_run_id,
                    session.state(),
                );

                info!(
                    "Relay AI session ready: task_run_id={}, task_name={}",
                    bg_task_run_id, bg_task_name
                );
            }
            Err(e) => {
                warn!("Failed to spawn relay AI session: {}", e);
            }
        }
    });

    info!("Relay AI session initializing: task_run_id={}", task_run_id);
    Some(serde_json::json!({
        "type": "chat_created",
        "id": task_run_id,
        "task_name": task_name,
        "state": "initializing"
    }))
}

async fn handle_chat_interrupt(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if task_run_id.is_empty() {
        return Some(serde_json::json!({
            "type": "error",
            "error": "Missing task_run_id"
        }));
    }

    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|s| s.inner().clone());

    if let Some(sm) = session_manager {
        if let Some(session) = sm.get(task_run_id) {
            match session.interrupt() {
                Ok(()) => {
                    crate::commands::ai_session::emit_session_state(
                        &api_state.app_handle,
                        task_run_id,
                        session.session_id(),
                        session.state(),
                    );
                    Some(serde_json::json!({
                        "type": "chat_session_state",
                        "task_run_id": task_run_id,
                        "state": session.state().as_event_str(),
                        "can_send": session.state().can_send_message(),
                        "can_interrupt": session.state().can_interrupt()
                    }))
                }
                Err(e) => Some(serde_json::json!({
                    "type": "error",
                    "task_run_id": task_run_id,
                    "error": format!("Interrupt failed: {}", e)
                })),
            }
        } else {
            Some(serde_json::json!({
                "type": "error",
                "task_run_id": task_run_id,
                "error": "No active session for task_run_id"
            }))
        }
    } else {
        Some(serde_json::json!({
            "type": "error",
            "error": "SessionManager not available"
        }))
    }
}

async fn handle_chat_close(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if task_run_id.is_empty() {
        return Some(serde_json::json!({
            "type": "error",
            "error": "Missing task_run_id"
        }));
    }

    let session_manager: Option<Arc<crate::claude_session::SessionManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|s| s.inner().clone());

    if let Some(sm) = session_manager {
        let _ = sm.remove(&task_run_id);
    }

    let _ = api_state
        .app_state
        .pg_db
        .update_task_run_status(&task_run_id, "stopped")
        .await;

    Some(serde_json::json!({
        "type": "chat_session_state",
        "task_run_id": task_run_id,
        "state": "closed",
        "can_send": false,
        "can_interrupt": false
    }))
}

async fn handle_chat_generate_workflow(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = data
        .get("params")
        .and_then(|p| p.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("Generate workflow from chat conversation")
        .to_string();
    let include_ui_bridge = data
        .get("params")
        .and_then(|p| p.get("include_ui_bridge_instructions"))
        .and_then(|v| v.as_bool());

    if task_run_id.is_empty() {
        return Some(serde_json::json!({
            "type": "error",
            "error": "Missing task_run_id"
        }));
    }

    let output_log = api_state
        .app_state
        .pg_db
        .get_task_output(&task_run_id)
        .await
        .unwrap_or_default();

    if output_log.is_empty() {
        return Some(serde_json::json!({
            "type": "chat_workflow_generated",
            "task_run_id": task_run_id,
            "success": false,
            "error": "No conversation history available"
        }));
    }

    let request = crate::workflow_generation::GenerateWorkflowRequest {
        // Stream C: backend-relay-launched workflows default to the runner app.
        app_id: crate::spec_api::storage::RUNNER_APP_ID.to_string(),
        description,
        inline_context: Some(format!(
            "The following is a conversation between a user and an AI assistant. \
             Use this conversation context to generate an appropriate workflow:\n\n{}",
            output_log
        )),
        category: None,
        tags: None,
        max_iterations: None,
        provider: None,
        model: None,
        skip_ai_summary: None,
        log_source_selection: None,
        prompt_template: None,
        auto_include_contexts: Some(true),
        context_ids: None,
        max_fix_iterations: Some(3),
        discovery_mode: None,
        include_ui_bridge_instructions: include_ui_bridge,
        reflection_mode: Some(true),
        investigate_codebase: Some(true),
        include_design_guidance: None,
        auto_run: None,
        model_overrides: None,
        generate_specification: Some(true),
        verification_depth: None,
        discover_ui_bridge_specs: None,
        simple_mode: None,
        pipeline_depth: None,
        tool_tags: None,
        exploration_settings: None,
        target_runner_port: None,
    };

    let doctor_handle = api_state.doctor_handle.clone();
    let pg_db = api_state.app_state.pg_db.clone();
    let pg_clone = pg_db.clone();
    let trid = task_run_id.clone();
    let artifact_task_run_id = task_run_id.clone();
    let app_state_for_gen = api_state.app_state.clone();

    let gen_result = tokio::task::spawn_blocking(move || {
        let (response, mut artifact) = crate::workflow_generation::generate_workflow(
            request,
            doctor_handle.as_ref(),
            Some(&pg_clone),
            None,
            Some(&*app_state_for_gen),
        );
        artifact.task_run_id = Some(artifact_task_run_id.clone());
        (response, artifact)
    })
    .await;

    if let Ok((_, artifact)) = &gen_result {
        if let Err(e) = pg_db.save_generation_artifact(artifact).await {
            tracing::warn!("Failed to save pipeline artifact to PG: {}", e);
        }
    }
    let gen_result = gen_result.map(|(response, _)| response);

    match gen_result {
        Ok(response) => Some(serde_json::json!({
            "type": "chat_workflow_generated",
            "task_run_id": trid,
            "success": response.success,
            "workflow": response.workflow,
            "error": response.error,
            "validation_errors": response.validation_errors,
            "model_used": response.model_used
        })),
        Err(e) => Some(serde_json::json!({
            "type": "chat_workflow_generated",
            "task_run_id": trid,
            "success": false,
            "error": format!("Generation task failed: {}", e)
        })),
    }
}

async fn handle_chat_get_output(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if task_run_id.is_empty() {
        return Some(serde_json::json!({
            "type": "error",
            "error": "Missing task_run_id"
        }));
    }

    let output = api_state
        .app_state
        .pg_db
        .get_task_output(&task_run_id)
        .await
        .unwrap_or_default();
    Some(serde_json::json!({
        "type": "chat_output",
        "task_run_id": task_run_id,
        "output_log": output
    }))
}

async fn handle_chat_rename(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let task_run_id = data
        .get("task_run_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let new_name = data
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if task_run_id.is_empty() || new_name.is_empty() {
        return Some(serde_json::json!({
            "type": "error",
            "error": "Missing task_run_id or name"
        }));
    }

    match api_state
        .app_state
        .pg_db
        .update_task_name(&task_run_id, &new_name)
        .await
    {
        Ok(()) => Some(serde_json::json!({
            "type": "chat_renamed",
            "task_run_id": task_run_id,
            "new_name": new_name
        })),
        _ => Some(serde_json::json!({
            "type": "error",
            "task_run_id": task_run_id,
            "error": "Failed to rename session"
        })),
    }
}

// ---------------------------------------------------------------------------
// Mobile terminal handlers — preserved from the legacy relay
// ---------------------------------------------------------------------------

fn handle_terminal_list(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let terminal_manager: Option<Arc<crate::terminal::TerminalManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());

    if let Some(tm) = terminal_manager {
        let terminals = tm.list();
        Some(serde_json::json!({
            "type": "terminal_sessions",
            "terminals": terminals,
            "request_id": data.get("request_id"),
        }))
    } else {
        Some(serde_json::json!({
            "type": "error",
            "message": "TerminalManager not available",
            "request_id": data.get("request_id"),
        }))
    }
}

async fn handle_terminal_create(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let terminal_manager: Option<Arc<crate::terminal::TerminalManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());

    if let Some(tm) = terminal_manager {
        let title = data.get("title").and_then(|v| v.as_str()).map(String::from);
        let working_dir = data
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(String::from);
        let cols = data
            .get("cols")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(u16::MAX as u64) as u16);
        let rows = data
            .get("rows")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(u16::MAX as u64) as u16);
        // Phase 2 of `plans/2026-05-28-isolate-session-edit-work-in-worktrees.md`.
        // Accept the same `intent_repo` opt-in the Tauri command + HTTP-SDK
        // proxy accept so backend-relay callers can declare edit intent on
        // a registered repo. Allocation happens via the shared helper; the
        // context (if any) is parked on the resulting TerminalSession.
        let intent_repo = data
            .get("intent_repo")
            .and_then(|v| v.as_str())
            .map(String::from);

        // L2 (shared-checkout coordination gap fix) — derive `intent_repo`
        // from `working_dir` when the caller didn't declare one (no-op
        // until `QONTINUI_AGENT_WORKTREE_MODE` is on).
        let effective_intent_repo: Option<String> = intent_repo.clone().or_else(|| {
            working_dir.as_deref().and_then(|wd| {
                let derived = crate::agent_worktree::canonical_paths::repo_slug_for_path(
                    std::path::Path::new(wd),
                );
                if let Some(ref repo) = derived {
                    tracing::debug!(
                        working_dir = %wd,
                        derived_intent_repo = %repo,
                        "backend_relay terminal_create: derived intent_repo from working_dir"
                    );
                }
                derived
            })
        });

        let (working_dir, isolated_ctx) =
            crate::agent_worktree::isolated_edit::acquire_for_terminal(
                effective_intent_repo.as_deref(),
                title.as_deref().unwrap_or("Terminal edit session"),
                working_dir,
            )
            .await;

        match tm.create(
            title,
            working_dir,
            None,
            cols,
            rows,
            api_state.app_handle.clone(),
        ) {
            Ok(info) => {
                if let Some(ctx) = isolated_ctx {
                    if let Some(session) = tm.get(&info.id) {
                        session.set_isolated_edit_ctx(ctx);
                    }
                }
                Some(serde_json::json!({
                    "type": "terminal_created",
                    "terminal": info,
                    "request_id": data.get("request_id"),
                }))
            }
            Err(e) => Some(serde_json::json!({
                "type": "error",
                "message": format!("Failed to create terminal: {}", e),
                "request_id": data.get("request_id"),
            })),
        }
    } else {
        Some(serde_json::json!({
            "type": "error",
            "message": "TerminalManager not available",
            "request_id": data.get("request_id"),
        }))
    }
}

fn handle_terminal_input(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let terminal_manager: Option<Arc<crate::terminal::TerminalManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());

    if let Some(tm) = terminal_manager {
        let terminal_id = data
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let input_data = data.get("data").and_then(|v| v.as_str()).unwrap_or("");

        if let Some(session) = tm.get(terminal_id) {
            match STANDARD.decode(input_data) {
                Ok(bytes) => {
                    if let Err(e) = session.write(&bytes) {
                        warn!("Relay: failed to write to terminal {}: {}", terminal_id, e);
                    }
                }
                Err(e) => {
                    warn!("Invalid base64 terminal input: {}", e);
                }
            }
        }
    }
    None
}

fn handle_terminal_resize(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let terminal_manager: Option<Arc<crate::terminal::TerminalManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());

    if let Some(tm) = terminal_manager {
        let terminal_id = data
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let cols = data
            .get("cols")
            .and_then(|v| v.as_u64())
            .unwrap_or(80)
            .min(u16::MAX as u64) as u16;
        let rows = data
            .get("rows")
            .and_then(|v| v.as_u64())
            .unwrap_or(24)
            .min(u16::MAX as u64) as u16;

        if let Some(session) = tm.get(terminal_id) {
            if let Err(e) = session.resize(cols, rows) {
                warn!("Relay: failed to resize terminal {}: {}", terminal_id, e);
            }
        }
    }
    None
}

async fn handle_terminal_close(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let terminal_manager: Option<Arc<crate::terminal::TerminalManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());

    if let Some(tm) = terminal_manager {
        let terminal_id = data
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let request_id = data.get("request_id").cloned();

        let tm_clone = tm.clone();
        let id_clone = terminal_id.clone();
        match tokio::task::spawn_blocking(move || tm_clone.close(&id_clone)).await {
            Ok(Ok(())) => Some(serde_json::json!({
                "type": "terminal_closed",
                "terminal_id": terminal_id,
                "request_id": request_id,
            })),
            Ok(Err(e)) => Some(serde_json::json!({
                "type": "error",
                "message": format!("Failed to close terminal: {}", e),
                "terminal_id": terminal_id,
                "request_id": request_id,
            })),
            Err(e) => Some(serde_json::json!({
                "type": "error",
                "message": format!("Join error: {}", e),
                "terminal_id": terminal_id,
                "request_id": request_id,
            })),
        }
    } else {
        Some(serde_json::json!({
            "type": "error",
            "message": "TerminalManager not available",
            "request_id": data.get("request_id"),
        }))
    }
}

fn handle_terminal_buffer(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
    let terminal_manager: Option<Arc<crate::terminal::TerminalManager>> = api_state
        .app_handle
        .try_state::<Arc<crate::terminal::TerminalManager>>()
        .map(|s| s.inner().clone());

    if let Some(tm) = terminal_manager {
        let terminal_id = data
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(session) = tm.get(terminal_id) {
            let (buf_data, start_offset) = session.get_scrollback_buffer();
            let total_bytes = session.info().total_bytes_produced;
            session.reset_flow_control();
            let encoded = STANDARD.encode(&buf_data);

            Some(serde_json::json!({
                "type": "terminal_buffer_response",
                "terminal_id": terminal_id,
                "data": encoded,
                "start_offset": start_offset,
                "total_bytes_produced": total_bytes,
                "request_id": data.get("request_id"),
            }))
        } else {
            Some(serde_json::json!({
                "type": "error",
                "message": format!("Terminal not found: {}", terminal_id),
                "request_id": data.get("request_id"),
            }))
        }
    } else {
        Some(serde_json::json!({
            "type": "error",
            "message": "TerminalManager not available",
            "request_id": data.get("request_id"),
        }))
    }
}

/// Internal helpers for managing the WS relay lifecycle.
///
/// The relay was historically called the "cloud relay"; the function names
/// preserve that prefix because they're used from `mcp_api::start_server`
/// and from the auth refresh flow. They drive the unified runner WS at
/// `/api/v1/runners/ws`, not a separate cloud-relay path.
pub mod commands {
    use super::*;
    use std::sync::OnceLock;

    /// Global relay state (managed outside Tauri state for simplicity).
    static RELAY_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<BackendRelayState>>>> =
        OnceLock::new();

    fn get_relay_holder() -> &'static tokio::sync::Mutex<Option<Arc<BackendRelayState>>> {
        RELAY_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Auto-start the WS relay if `WebIntegrationSettings` are configured.
    ///
    /// Trigger condition is `tier == QontinuiAccount && enabled && a
    /// device-JWT is present in AuthManager`. When the condition is unmet
    /// the relay still starts — its main loop sleeps awaiting a kick — so
    /// a subsequent settings save or refresher-completion can wake it up
    /// without restarting the runner.
    ///
    /// Called from `mcp_api::start_server` once `Arc<ApiState>` is available.
    pub async fn auto_start_cloud_relay(api_state: Arc<ApiState>) {
        let mut guard = get_relay_holder().lock().await;

        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);

            if is_alive {
                info!("Runner WS relay already running; kicking to re-read settings/tokens");
                existing.kick();
                return;
            }

            info!("Runner WS relay task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        info!("Starting runner WS relay");
        let relay = start_relay(api_state).await;
        *guard = Some(relay);
    }

    /// Kick the running relay (if any) so it interrupts any backoff sleep
    /// and immediately retries with freshly-read settings and tokens.
    pub async fn kick_cloud_relay() {
        let guard = get_relay_holder().lock().await;
        if let Some(ref relay) = *guard {
            relay.kick();
        }
    }
}

#[cfg(test)]
mod tests {
    //! Phase 5.3 unit tests — `is_unauthorized` discrimination.
    //!
    //! The relay's 401 → kick-refresher path is the hot-loop guard
    //! against a stale device-JWT: if the discriminator stops firing on
    //! a real 401 (e.g. tungstenite refactors how it represents HTTP
    //! errors in a future minor bump), the relay quietly sleeps through
    //! its full exponential backoff before the refresher's 5-minute
    //! interval fires. That manifests as "ran the runner, took 10
    //! minutes to come online after a JWT expiry." These tests pin the
    //! shape so a tungstenite bump can't silently break it.

    use super::*;
    use tokio_tungstenite::tungstenite::{
        http::{self, Response, StatusCode},
        Error,
    };

    fn http_err_with_status(status: StatusCode) -> Error {
        // Build a minimal http::Response<Option<Vec<u8>>> — that's the
        // shape tokio-tungstenite 0.29 wraps in Error::Http (boxed).
        let resp: Response<Option<Vec<u8>>> = http::Response::builder()
            .status(status)
            .body(None)
            .expect("build response");
        Error::Http(Box::new(resp))
    }

    #[test]
    fn detects_401_in_tungstenite_error() {
        let err = http_err_with_status(StatusCode::UNAUTHORIZED);
        assert!(
            is_unauthorized(&err),
            "tungstenite::Error::Http with status 401 MUST be recognized"
        );
    }

    #[test]
    fn does_not_detect_401_on_io_error() {
        // Synthesize an I/O error — std::io::Error wraps cleanly.
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no listener");
        let err = Error::Io(io_err);
        assert!(
            !is_unauthorized(&err),
            "I/O errors are NOT 401 — kicking the refresher on every \
             ECONNREFUSED would mask deeper transport problems"
        );
    }

    #[test]
    fn does_not_detect_401_on_http_500() {
        let err = http_err_with_status(StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !is_unauthorized(&err),
            "HTTP 500 is a coord-side bug — a fresh JWT will not fix it. \
             The refresher MUST NOT be kicked."
        );
    }

    #[test]
    fn does_not_detect_401_on_http_403() {
        // 403 = "your token is valid but you don't have permission" —
        // re-pairing produces the SAME runner_token and would still get
        // 403. Refresher must NOT fire.
        let err = http_err_with_status(StatusCode::FORBIDDEN);
        assert!(
            !is_unauthorized(&err),
            "HTTP 403 (policy denial) MUST NOT trigger a refresher kick"
        );
    }

    #[test]
    fn does_not_detect_401_on_connection_closed() {
        // ConnectionClosed = clean WS close — not an auth signal.
        let err = Error::ConnectionClosed;
        assert!(
            !is_unauthorized(&err),
            "ConnectionClosed MUST NOT trigger a refresher kick"
        );
    }

    // ------------------------------------------------------------------
    // close-before-ack diagnosability.
    //
    // The relay now defers `clear_connection_error` until the backend's
    // `connected` ack arrives (see `handle_connected_message`), and on a
    // close/error *before* that ack it records an actionable `last_error`
    // via `close_before_ack_error`. These pin the message shape so a
    // future refactor can't silently regress back to `last_error=null`
    // (the ambiguous state that cost real time during the Redis-disabled
    // pairing outage).

    #[test]
    fn close_before_ack_error_includes_code_and_reason() {
        let msg = close_before_ack_error("code=1011, reason=register failed");
        assert!(
            msg.contains("before the `connected` ack"),
            "must name the close-before-ack condition: {msg}"
        );
        assert!(
            msg.contains("registration rejected"),
            "must flag this as a registration rejection: {msg}"
        );
        assert!(
            msg.contains("code=1011") && msg.contains("register failed"),
            "must carry the actionable close code/reason: {msg}"
        );
    }

    #[test]
    fn close_before_ack_error_is_non_empty_without_close_frame() {
        // A close with no frame still yields a non-null, descriptive error
        // rather than the old silent `ws_connected=false` / `last_error=null`.
        let msg = close_before_ack_error("no close frame");
        assert!(
            !msg.is_empty() && msg.contains("no close frame"),
            "must remain actionable even with no close frame: {msg}"
        );
    }

    // ------------------------------------------------------------------
    // `http_request` relay arm (Phase 2 mobile remote-runner relay).
    //
    // These exercise `relay_http_to_base` — the transport-agnostic core of
    // the `http_request` arm — against a real ephemeral axum server, so the
    // full status/header/body round-trip (incl. binary base64 integrity) is
    // verified end-to-end without constructing a full `ApiState`. The pure
    // error-reply construction (400/413/502) is verified server-free.
    // ------------------------------------------------------------------

    use axum::{
        body::Bytes,
        extract::Query,
        http::{HeaderMap, StatusCode as AxumStatus},
        routing::{get, post},
        Router,
    };
    use serde_json::json;
    use std::collections::HashMap;

    /// Spawn a tiny axum server on an ephemeral loopback port and return its
    /// base URL (e.g. `http://127.0.0.1:54321`). The server stays alive for
    /// the duration of the test process (task is detached).
    async fn spawn_test_server(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        // Give the accept loop a beat to come up.
        tokio::time::sleep(Duration::from_millis(50)).await;
        format!("http://{}", addr)
    }

    fn b64_decode(s: &str) -> Vec<u8> {
        STANDARD.decode(s).expect("valid base64 in reply body_b64")
    }

    #[tokio::test]
    async fn relay_get_round_trips_status_headers_query_and_body() {
        // Echo handler: returns query param `foo` and a custom request header
        // in a JSON body, with a custom response header + 201 status.
        async fn handler(
            Query(params): Query<HashMap<String, String>>,
            headers: HeaderMap,
        ) -> impl axum::response::IntoResponse {
            let foo = params.get("foo").cloned().unwrap_or_default();
            let xv = headers
                .get("x-test")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = json!({ "foo": foo, "x_test": xv }).to_string();
            (
                AxumStatus::CREATED,
                [("content-type", "application/json"), ("x-reply", "yes")],
                body,
            )
        }
        let base = spawn_test_server(Router::new().route("/echo", get(handler))).await;

        let env = json!({
            "type": "http_request",
            "request_id": "req-1",
            "method": "GET",
            "path": "/echo",
            "query": "foo=bar",
            "headers": { "x-test": "hello", "host": "should-be-stripped" },
            "body_b64": ""
        });

        let reply = relay_http_to_base(&base, &env).await;

        assert_eq!(reply["type"], "command_response");
        assert_eq!(reply["request_id"], "req-1");
        assert_eq!(reply["status"], 201);
        // Response header forwarded; content-length stripped.
        assert_eq!(reply["headers"]["x-reply"], "yes");
        assert!(
            reply["headers"].get("content-length").is_none(),
            "content-length must be stripped from the response headers"
        );

        let body = String::from_utf8(b64_decode(reply["body_b64"].as_str().unwrap())).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["foo"], "bar", "query forwarded");
        assert_eq!(parsed["x_test"], "hello", "request header forwarded");
    }

    #[tokio::test]
    async fn relay_post_round_trips_binary_body_intact() {
        // Echo the raw request bytes straight back, unchanged.
        async fn handler(body: Bytes) -> impl axum::response::IntoResponse {
            (AxumStatus::OK, body)
        }
        let base = spawn_test_server(Router::new().route("/bin", post(handler))).await;

        // Bytes that are not valid UTF-8 — proves we carry raw binary, not text.
        let raw: Vec<u8> = vec![0x00, 0xff, 0x10, 0x80, 0x7f, 0xfe, 0x01];
        let env = json!({
            "type": "http_request",
            "request_id": "req-bin",
            "method": "POST",
            "path": "bin",
            "body_b64": STANDARD.encode(&raw),
        });

        let reply = relay_http_to_base(&base, &env).await;

        assert_eq!(reply["status"], 200);
        let returned = b64_decode(reply["body_b64"].as_str().unwrap());
        assert_eq!(returned, raw, "binary body must round-trip byte-for-byte");
    }

    #[tokio::test]
    async fn relay_request_body_over_cap_returns_413_without_calling() {
        // Point at an unroutable base; the 413 must fire BEFORE any call.
        let oversize = STANDARD.encode(vec![0u8; RELAY_MAX_BODY_BYTES + 1]);
        let env = json!({
            "type": "http_request",
            "request_id": "req-big",
            "method": "POST",
            "path": "anything",
            "body_b64": oversize,
        });

        let reply = relay_http_to_base("http://127.0.0.1:1", &env).await;

        assert_eq!(reply["type"], "command_response");
        assert_eq!(reply["request_id"], "req-big");
        assert_eq!(reply["status"], 413);
        let body = String::from_utf8(b64_decode(reply["body_b64"].as_str().unwrap())).unwrap();
        assert!(body.contains("exceeds relay size limit"), "got: {body}");
    }

    #[tokio::test]
    async fn relay_unreachable_local_server_returns_502() {
        // Port 1 has no listener -> reqwest connect error -> 502 reply.
        let env = json!({
            "type": "http_request",
            "request_id": "req-502",
            "method": "GET",
            "path": "health",
            "body_b64": "",
        });

        let reply = relay_http_to_base("http://127.0.0.1:1", &env).await;

        assert_eq!(reply["type"], "command_response");
        assert_eq!(reply["request_id"], "req-502");
        assert_eq!(reply["status"], 502);
    }

    #[test]
    fn http_relay_error_builds_command_response_with_json_body() {
        let reply = http_relay_error(&json!("rid-7"), 400, "invalid HTTP method");
        assert_eq!(reply["type"], "command_response");
        assert_eq!(reply["request_id"], "rid-7");
        assert_eq!(reply["status"], 400);
        assert_eq!(reply["headers"]["content-type"], "application/json");
        let body = String::from_utf8(b64_decode(reply["body_b64"].as_str().unwrap())).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["error"], "invalid HTTP method");
    }

    // ------------------------------------------------------------------
    // Reconnect kick-loop guard (watch-channel mark-seen invariant).
    //
    // The connected `tokio::select!` in `relay_loop` awaits a CLONED kick
    // receiver (`kick_clone`). A `watch::Receiver` clone inherits the
    // SOURCE receiver's last-observed version, and `changed()` resolves
    // whenever the channel version is newer than what the receiver has
    // seen — and marks only the receiver it's called on. The bug: the
    // connected arm marked `kick_clone` seen but left the loop owner
    // `kick_rx` at the pre-kick version, so on `continue` → reconnect the
    // freshly-cloned `kick_clone` (inheriting `kick_rx`'s STALE version)
    // saw the same already-handled kick as still-pending and re-fired
    // immediately, seeding a tight connect/teardown loop.
    //
    // The fix calls `kick_rx.borrow_and_update()` at connect time, right
    // before cloning. The async loop itself is impractical to unit-test,
    // so these pin the exact `watch` semantics the fix depends on:
    // `try_recv`/`has_changed` model `changed()`'s readiness.

    #[test]
    fn cloned_kick_receiver_inherits_source_seen_version() {
        // A clone taken AFTER the source observed the latest version starts
        // level with the channel — `has_changed()` is false until a NEW send.
        let (tx, mut rx) = watch::channel(0u64);
        let _ = tx.send(1); // a kick
        rx.borrow_and_update(); // loop owner marks it seen (the fix)

        let clone_after = rx.clone();
        assert!(
            !clone_after.has_changed().expect("sender alive"),
            "a clone taken after the source marked the kick seen MUST start \
             level — otherwise the connected select re-fires on the same kick"
        );
    }

    #[test]
    fn one_kick_triggers_one_teardown_then_quiesces() {
        // Model the connect → clone → kick → teardown → reconnect → clone
        // sequence. With the `borrow_and_update` fix, the SECOND clone (the
        // reconnect) sees no pending change, so a single kick = a single
        // teardown rather than a self-perpetuating loop.
        let (tx, mut kick_rx) = watch::channel(0u64);

        // ---- connect #1 ----
        kick_rx.borrow_and_update(); // the fix: mark seen at connect
        let mut kick_clone_1 = kick_rx.clone();
        // one genuine kick arrives while connected
        let _ = tx.send(1);
        assert!(
            kick_clone_1.has_changed().expect("sender alive"),
            "the connected arm MUST observe the (one) genuine kick"
        );
        kick_clone_1.borrow_and_update(); // connected arm consumes it, then `continue`

        // ---- reconnect / connect #2 (no new kick has been sent) ----
        kick_rx.borrow_and_update(); // the fix again, at the new connect
        let kick_clone_2 = kick_rx.clone();
        assert!(
            !kick_clone_2.has_changed().expect("sender alive"),
            "after handling one kick, the reconnect's fresh clone MUST NOT \
             see a pending change — this is the regression guard against the \
             tight connect/teardown loop"
        );

        // ---- a SECOND genuine kick still tears down (intent preserved) ----
        let mut kick_clone_2 = kick_clone_2;
        let _ = tx.send(2);
        assert!(
            kick_clone_2.has_changed().expect("sender alive"),
            "a NEW kick must still force exactly one reconnect"
        );
    }
}
