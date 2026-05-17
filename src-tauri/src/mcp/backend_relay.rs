//! Unified runner ↔ qontinui-web WebSocket relay (Phase 3).
//!
//! Opens a single outbound WebSocket to `WS /api/v1/runners/ws` on the
//! configured `WebIntegrationSettings.backend_url`, authenticated with a
//! `Authorization: Bearer <runner_token>` header. This connection replaces
//! the legacy split-brain architecture in which:
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
//! Trigger condition is purely `WebIntegrationSettings.enabled &&
//! !runner_token.is_empty()` — there is no longer a `cloud_relay` toggle
//! or a localhost auto-detect probe.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

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

/// Periodicity of WS heartbeat sends. The backend's `last_heartbeat`
/// timestamp updates from these.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Periodicity of WS keepalive ping frames. Lower than `HEARTBEAT_INTERVAL`
/// so we can detect dead/half-open connections faster than the heartbeat
/// cadence would.
const KEEPALIVE_PING_INTERVAL: Duration = Duration::from_secs(20);

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

        // Phase 3 trigger: web integration enabled and a token is set.
        // No `cloud_relay.enabled` gating, no localhost auto-detect — the
        // user's `WebIntegrationSettings` is the sole source of truth.
        if !web_integration.enabled || web_integration.runner_token.trim().is_empty() {
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
        let runner_token = web_integration.runner_token.trim().to_string();

        let ws_url = format!(
            "{}/api/v1/runners/ws",
            backend_url
                .replace("https://", "wss://")
                .replace("http://", "ws://"),
        );

        // After many consecutive quick disconnects, the backend is
        // persistently rejecting us (e.g. revoked token, runner-token
        // expired). Back off aggressively to avoid hammering the server.
        if consecutive_quick_disconnects >= 5 {
            let extended_backoff = max_backoff_ms.max(120_000);
            warn!(
                "Backend relay: {} consecutive quick disconnects. \
                 Check runner token validity and backend connectivity. \
                 Backing off for {}s (send kick to retry sooner).",
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
        let request_result: Result<HttpRequest, String> = build_ws_request(&ws_url, &runner_token);
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
                clear_connection_error(&api_state).await;

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

                tokio::select! {
                    _ = handle_inbound(read, state_inbound, write_inbound) => {
                        warn!("Backend relay inbound handler ended");
                    }
                    _ = handle_outbound(&mut event_rx, write_outbound, state_outbound) => {
                        warn!("Backend relay outbound handler ended");
                    }
                    _ = run_heartbeat_sender(state_heartbeat, write_heartbeat) => {
                        warn!("Backend relay heartbeat sender ended");
                    }
                    _ = run_keepalive_pinger(write_keepalive) => {
                        warn!("Backend relay keepalive detected dead connection");
                    }
                    _ = shutdown_clone.changed() => {
                        info!("Backend relay received shutdown signal");
                        mark_disconnected(&api_state).await;
                        return;
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
/// and then set our auth header on top.
fn build_ws_request(ws_url: &str, runner_token: &str) -> Result<HttpRequest, String> {
    let mut req = ws_url
        .into_client_request()
        .map_err(|e| format!("invalid ws URL: {}", e))?;
    let auth_value = format!("Bearer {}", runner_token);
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
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(msg_result) = read.next().await {
        match msg_result {
            Ok(Message::Text(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(data) => {
                    let msg_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    info!("Relay inbound message: type={}", msg_type);

                    // Special-case the `connected` handshake reply so we can
                    // capture runner_id on the shared state.
                    if msg_type == "connected" {
                        handle_connected_message(&api_state, &data).await;
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
                return;
            }
            Err(e) => {
                warn!("Backend relay read error: {}", e);
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
/// {"type": "connected", "runner_id": "<uuid>"}
/// ```
async fn handle_connected_message(api_state: &Arc<ApiState>, data: &Value) {
    let sm_state = match api_state.app_state.current_server_mode().await {
        Some(s) => s,
        None => {
            warn!(
                "Received `connected` message but no ServerModeState is installed; \
                 the relay will continue but state queries will return empty"
            );
            return;
        }
    };

    sm_state.set_ws_connected(true);

    if let Some(rid_str) = data.get("runner_id").and_then(|v| v.as_str()) {
        match Uuid::parse_str(rid_str) {
            Ok(rid) => {
                sm_state.set_runner_id(rid).await;
                info!("Backend assigned runner_id={}", rid);
            }
            Err(e) => {
                warn!(
                    "Backend `connected` message had unparseable runner_id={}: {}",
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
async fn run_keepalive_pinger<S>(
    write: Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut interval = tokio::time::interval(KEEPALIVE_PING_INTERVAL);
    interval.tick().await;
    loop {
        interval.tick().await;
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
        "terminal_create" => handle_terminal_create(api_state, data),
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

fn handle_terminal_create(api_state: &Arc<ApiState>, data: &Value) -> Option<Value> {
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

        match tm.create(
            title,
            working_dir,
            None,
            cols,
            rows,
            api_state.app_handle.clone(),
        ) {
            Ok(info) => Some(serde_json::json!({
                "type": "terminal_created",
                "terminal": info,
                "request_id": data.get("request_id"),
            })),
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
    /// Trigger condition is `enabled && !runner_token.is_empty()`. When the
    /// condition is unmet the relay still starts — its main loop sleeps
    /// awaiting a kick — so a subsequent settings save can wake it up
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
