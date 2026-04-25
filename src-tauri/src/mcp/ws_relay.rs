//! WebSocket relay for UI Bridge wrapper apps (Phase 1).
//!
//! Wrappers (third-party browser tabs that can't host an HTTP server, headless
//! Playwright shims, extensions) upgrade on `GET /ui-bridge/ws`, send a
//! `{type:"register", ...}` handshake frame, then receive
//! `{type:"command", commandId, action, payload}` frames and reply with
//! `{type:"response", commandId, success, result?, error?}`.
//!
//! This module owns:
//! - `WsConnectionManager` — the outbound-channel registry keyed by a monotonic
//!   `conn_id`, plus a reverse `app_id → conn_id` map for last-tab-wins routing.
//! - `ws_upgrade_handler` — the axum handler that accepts the upgrade, drives
//!   the register/command/response protocol, and wires response frames to the
//!   `CommandRelay`.
//!
//! Heartbeat: axum ws auto-handles ping frames, but we additionally send a
//! ping every 20s and close the socket if two consecutive pings go
//! un-ponged. Multi-tab grace period / primary-tab election are deferred to
//! Phase 2 — "last tab wins" is acceptable for v1.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::any,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use super::app_registry::{AppRegistry, AppTransport};
use super::app_discovery::DiscoveredApp;
use super::command_relay::{CommandRelay, CommandResponse};
use super::types::ApiState;

/// Heartbeat configuration. Sent every `HEARTBEAT_INTERVAL`; if we don't see a
/// pong (or any inbound frame, which also resets liveness) for
/// `HEARTBEAT_TIMEOUT`, we close the connection.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

/// Outbound mpsc buffer depth. Large enough that bursty command dispatch
/// doesn't block the sender; small enough to notice a stuck wrapper.
const OUTBOUND_BUFFER: usize = 64;

// ============================================================================
// WsConnectionManager
// ============================================================================

/// Errors returned when the manager cannot deliver an outbound frame.
#[derive(Debug, Clone, Copy)]
pub enum WsOutboundError {
    /// No entry for the given `conn_id` (connection closed or never existed).
    NotConnected,
    /// The mpsc send failed (receiver dropped). Treated the same as
    /// `NotConnected` by callers but kept distinct for logging.
    Send,
}

/// Per-connection outbound state. The sender pushes serialized frames to the
/// task that owns the WebSocket write half.
struct ConnectionEntry {
    app_id: String,
    outbound: mpsc::Sender<String>,
}

/// Tracks every active `/ui-bridge/ws` connection.
///
/// Concurrency model:
/// - `next_conn_id` is atomic, so connection ids are assigned without a lock.
/// - The two maps sit behind a single `Mutex` to keep the `conn_id ↔ app_id`
///   relation consistent across register / remove. The critical sections are
///   all constant-time and short.
pub struct WsConnectionManager {
    next_conn_id: AtomicU64,
    inner: Mutex<Inner>,
}

struct Inner {
    by_conn: HashMap<u64, ConnectionEntry>,
    /// Last-tab-wins: if the same `app_id` registers a new connection, the
    /// previous `conn_id` loses its slot here (but stays in `by_conn` until
    /// its own close drops it — otherwise we'd leak the outbound sender).
    by_app: HashMap<String, u64>,
}

impl WsConnectionManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_conn_id: AtomicU64::new(0),
            inner: Mutex::new(Inner {
                by_conn: HashMap::new(),
                by_app: HashMap::new(),
            }),
        })
    }

    /// Allocate a fresh `conn_id`. Monotonically increasing per-process.
    fn allocate_conn_id(&self) -> u64 {
        // Start at 1 so callers can treat 0 as "unset" if needed.
        self.next_conn_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Register a newly-connected WebSocket. Returns the `conn_id`.
    /// Displaces any prior connection for the same `app_id` (last-tab-wins).
    async fn register(
        &self,
        app_id: String,
        outbound: mpsc::Sender<String>,
    ) -> (u64, Option<u64>) {
        let conn_id = self.allocate_conn_id();
        let mut inner = self.inner.lock().await;
        let displaced = inner.by_app.insert(app_id.clone(), conn_id);
        inner
            .by_conn
            .insert(conn_id, ConnectionEntry { app_id, outbound });
        (conn_id, displaced)
    }

    /// Remove a connection by id. Only clears the `by_app` entry if it still
    /// points at this `conn_id` (guards against a stale remove clobbering a
    /// freshly-registered replacement connection for the same app).
    async fn remove(&self, conn_id: u64) -> Option<String> {
        let mut inner = self.inner.lock().await;
        let entry = inner.by_conn.remove(&conn_id)?;
        if inner.by_app.get(&entry.app_id) == Some(&conn_id) {
            inner.by_app.remove(&entry.app_id);
        }
        Some(entry.app_id)
    }

    /// Look up the current `conn_id` for an `app_id`. `None` if the app has
    /// no registered WS connection or its connection was displaced.
    pub async fn conn_for_app(&self, app_id: &str) -> Option<u64> {
        let inner = self.inner.lock().await;
        inner.by_app.get(app_id).copied()
    }

    /// Send a text frame to the specified connection.
    pub async fn send_text(&self, conn_id: u64, text: String) -> Result<(), WsOutboundError> {
        let sender = {
            let inner = self.inner.lock().await;
            inner
                .by_conn
                .get(&conn_id)
                .map(|e| e.outbound.clone())
                .ok_or(WsOutboundError::NotConnected)?
        };
        sender
            .send(text)
            .await
            .map_err(|_| WsOutboundError::Send)
    }

    /// Test helper: register a fake outbound channel so unit tests can
    /// exercise CommandRelay without spinning up a real WebSocket. Returns
    /// `(conn_id, outbound_rx)` — the caller plays the role of the wrapper
    /// by pulling frames off `outbound_rx`.
    #[cfg(test)]
    pub async fn test_register(&self, app_id: &str) -> (u64, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(16);
        let (conn_id, _displaced) = self.register(app_id.to_string(), tx).await;
        (conn_id, rx)
    }
}

// ============================================================================
// Wire protocol
// ============================================================================

/// First frame the wrapper sends after the upgrade — identifies the app and
/// provides the metadata the registry needs to mirror an HTTP phone-home.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterFrame {
    r#type: String, // must equal "register"
    app_id: String,
    app_name: String,
    app_type: String,
    /// Must be "websocket" if present; accepted for symmetry with the HTTP
    /// register payload, but we treat missing/absent as websocket here since
    /// the wrapper is literally speaking over a WS.
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    page_url: Option<String>,
    #[serde(default)]
    framework: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    version: Option<String>,
}

/// Any frame other than the handshake.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum InboundFrame {
    /// Response to a previously-dispatched command.
    #[serde(rename_all = "camelCase")]
    Response {
        command_id: String,
        #[serde(default)]
        success: Option<bool>,
        #[serde(default)]
        result: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<String>,
    },
    /// Push-based change event (DOM mutations etc.). For v1 we log + discard.
    #[serde(rename_all = "camelCase")]
    ChangeEvent {
        #[serde(default)]
        event: Option<serde_json::Value>,
    },
    /// Optional heartbeat frame from the wrapper. We already handle WS
    /// protocol-level pings/pongs; this variant just keeps us tolerant of
    /// wrappers that send an application-level ping.
    #[serde(other)]
    Other,
}

/// Reply sent to the wrapper once the handshake has been accepted.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisterAck<'a> {
    r#type: &'a str,
    app_id: &'a str,
    conn_id: u64,
    accepted_at: i64,
}

// ============================================================================
// Upgrade handler
// ============================================================================

/// `any /ui-bridge/ws` — axum upgrade handler.
pub async fn ws_upgrade_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| drive_connection(socket, state))
}

/// Split the socket, wait for the register frame, then run the send/recv
/// tasks until either side closes. Always cleans up registry + connection
/// manager state on exit.
async fn drive_connection(socket: WebSocket, state: Arc<ApiState>) {
    let ws_manager = state.ws_connection_manager.clone();
    let relay = state.ws_command_relay.clone();
    let registry = state.app_registry.clone();

    let (mut sink, mut stream) = socket.split();

    // 1. Wait for the register frame. Bail if the first message isn't valid.
    let register = match read_register_frame(&mut stream).await {
        Ok(r) => r,
        Err(reason) => {
            warn!("[ws-relay] rejected handshake: {}", reason);
            let _ = sink.send(Message::Close(None)).await;
            return;
        }
    };

    if let Some(declared) = register.transport.as_deref() {
        if !declared.eq_ignore_ascii_case("websocket") {
            warn!(
                "[ws-relay] rejected handshake: transport '{}' != 'websocket'",
                declared
            );
            let _ = sink.send(Message::Close(None)).await;
            return;
        }
    }

    // 2. Allocate the connection + register with the manager.
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<String>(OUTBOUND_BUFFER);
    let (conn_id, displaced) = ws_manager
        .register(register.app_id.clone(), outbound_tx)
        .await;

    if let Some(prev) = displaced {
        debug!(
            "[ws-relay] last-tab-wins: app '{}' displaced conn {} with conn {}",
            register.app_id, prev, conn_id
        );
    }

    // 3. Mirror into the app registry so discovery / list endpoints see it.
    let discovered = DiscoveredApp {
        app_id: register.app_id.clone(),
        app_name: register.app_name.clone(),
        app_type: register.app_type.clone(),
        framework: register.framework.clone(),
        url: register
            .page_url
            .clone()
            .unwrap_or_else(|| "ws://runner/ui-bridge/ws".to_string()),
        port: 0,
        base_path: String::new(),
        version: register.version.clone(),
        capabilities: register.capabilities.clone().unwrap_or_default(),
        element_count: None,
        component_count: None,
        discovered_at: chrono::Utc::now().timestamp_millis(),
    };
    registry
        .upsert(
            discovered,
            register.origin.clone(),
            AppTransport::Websocket,
            Some(conn_id),
        )
        .await;

    info!(
        "[ws-relay] app '{}' connected (conn_id={}, framework={:?})",
        register.app_id, conn_id, register.framework
    );

    // 4. Send the register ack so the wrapper knows its conn_id.
    let ack = RegisterAck {
        r#type: "registered",
        app_id: &register.app_id,
        conn_id,
        accepted_at: chrono::Utc::now().timestamp_millis(),
    };
    if let Ok(ack_json) = serde_json::to_string(&ack) {
        let _ = sink.send(Message::Text(ack_json.into())).await;
    }

    // 5. Fan out: one task drains outbound_rx → sink with periodic pings,
    //    the current task reads inbound frames.
    let send_task = tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                frame = outbound_rx.recv() => {
                    match frame {
                        Some(text) => {
                            if sink.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = heartbeat.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    });

    let recv_app_id = register.app_id.clone();
    let recv_relay = relay.clone();
    let recv_outcome = tokio::time::timeout(HEARTBEAT_TIMEOUT, async {
        // We manually gate on HEARTBEAT_TIMEOUT between frames — reset the
        // timer whenever the wrapper says *anything* (data, pong, ping).
        loop {
            let next = tokio::time::timeout(HEARTBEAT_TIMEOUT, stream.next()).await;
            let msg = match next {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => {
                    debug!("[ws-relay] recv error for '{}': {}", recv_app_id, e);
                    break;
                }
                Ok(None) => break,
                Err(_) => {
                    warn!(
                        "[ws-relay] app '{}' silent for >{}s — closing",
                        recv_app_id,
                        HEARTBEAT_TIMEOUT.as_secs()
                    );
                    break;
                }
            };

            match msg {
                Message::Text(text) => {
                    handle_inbound_text(&recv_app_id, &recv_relay, text.as_str()).await;
                }
                Message::Binary(_) => {
                    debug!("[ws-relay] ignoring binary frame from '{}'", recv_app_id);
                }
                Message::Ping(_) | Message::Pong(_) => {
                    // axum auto-responds to pings; pongs just keep the timer fresh.
                }
                Message::Close(_) => break,
            }
        }
    })
    .await;
    // Outer timeout is a belt-and-suspenders guard around the inner per-frame
    // timeout; either triggering means the loop is done.
    let _ = recv_outcome;

    // 6. Clean up. We reject all pending commands on disconnect so their
    //    awaiters don't have to wait for the 30s default timeout; this
    //    mirrors the TS relay's behavior on tab close.
    ws_manager.remove(conn_id).await;
    registry.remove(&register.app_id).await;
    relay.reject_all_on_disconnect().await;
    send_task.abort();
    info!(
        "[ws-relay] app '{}' disconnected (conn_id={})",
        register.app_id, conn_id
    );
}

/// Read exactly one text frame and parse it as a `RegisterFrame`. Anything
/// else (including close/binary frames) is rejected.
async fn read_register_frame(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Result<RegisterFrame, String> {
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .map_err(|_| "timed out waiting for register frame".to_string())?;
    let msg = match first {
        Some(Ok(m)) => m,
        Some(Err(e)) => return Err(format!("ws error before handshake: {}", e)),
        None => return Err("socket closed before handshake".to_string()),
    };
    let text = match msg {
        Message::Text(t) => t,
        Message::Binary(_) => return Err("expected text register frame, got binary".to_string()),
        Message::Close(_) => return Err("wrapper sent Close before handshake".to_string()),
        Message::Ping(_) | Message::Pong(_) => {
            return Err("expected register frame, got ping/pong".to_string());
        }
    };
    let frame: RegisterFrame = serde_json::from_str(text.as_str())
        .map_err(|e| format!("register frame not valid JSON: {}", e))?;
    if frame.r#type != "register" {
        return Err(format!(
            "first frame type must be 'register', got '{}'",
            frame.r#type
        ));
    }
    if frame.app_id.trim().is_empty() {
        return Err("register frame missing appId".to_string());
    }
    Ok(frame)
}

async fn handle_inbound_text(app_id: &str, relay: &Arc<CommandRelay>, text: &str) {
    let parsed: Result<InboundFrame, _> = serde_json::from_str(text);
    match parsed {
        Ok(InboundFrame::Response {
            command_id,
            success,
            result,
            error,
        }) => {
            let matched = relay
                .resolve(CommandResponse {
                    command_id: command_id.clone(),
                    success: success.unwrap_or_else(|| error.is_none()),
                    result,
                    error,
                })
                .await;
            if !matched {
                debug!(
                    "[ws-relay] orphan response from '{}' for command_id={}",
                    app_id, command_id
                );
            }
        }
        Ok(InboundFrame::ChangeEvent { event }) => {
            // Phase 1: log and discard. Phase 2 will wire this into the
            // existing change-event subscriber machinery.
            debug!("[ws-relay] changeEvent from '{}': {:?}", app_id, event);
        }
        Ok(InboundFrame::Other) => {
            debug!("[ws-relay] ignoring unknown frame type from '{}'", app_id);
        }
        Err(e) => {
            warn!("[ws-relay] invalid inbound frame from '{}': {}", app_id, e);
        }
    }
}

// ============================================================================
// Router
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/ui-bridge/ws", any(ws_upgrade_handler))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_allocates_monotonic_ids() {
        let m = WsConnectionManager::new();
        let (c1, _rx1) = m.test_register("a").await;
        let (c2, _rx2) = m.test_register("b").await;
        assert!(c2 > c1);
        assert_eq!(m.conn_for_app("a").await, Some(c1));
        assert_eq!(m.conn_for_app("b").await, Some(c2));
    }

    #[tokio::test]
    async fn last_tab_wins() {
        let m = WsConnectionManager::new();
        let (c1, _rx1) = m.test_register("app").await;
        let (c2, _rx2) = m.test_register("app").await;
        assert_ne!(c1, c2);
        assert_eq!(m.conn_for_app("app").await, Some(c2));
    }

    #[tokio::test]
    async fn remove_only_clears_app_mapping_if_current() {
        let m = WsConnectionManager::new();
        let (c1, _rx1) = m.test_register("app").await;
        let (c2, _rx2) = m.test_register("app").await;
        // Remove the stale (displaced) connection — should NOT clobber c2's
        // app mapping.
        let app_id = m.remove(c1).await;
        assert_eq!(app_id.as_deref(), Some("app"));
        assert_eq!(m.conn_for_app("app").await, Some(c2));
        // Remove the current connection — should clear the app mapping.
        let app_id = m.remove(c2).await;
        assert_eq!(app_id.as_deref(), Some("app"));
        assert_eq!(m.conn_for_app("app").await, None);
    }

    #[tokio::test]
    async fn send_text_not_connected_for_unknown_conn() {
        let m = WsConnectionManager::new();
        let err = m.send_text(999, "hi".into()).await.unwrap_err();
        assert!(matches!(err, WsOutboundError::NotConnected));
    }

    #[tokio::test]
    async fn send_text_send_error_when_receiver_dropped() {
        let m = WsConnectionManager::new();
        let (conn_id, rx) = m.test_register("a").await;
        drop(rx);
        let err = m.send_text(conn_id, "hi".into()).await.unwrap_err();
        assert!(matches!(err, WsOutboundError::Send));
    }
}
