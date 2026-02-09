//! Chrome extension handlers for MCP API
//!
//! Provides WebSocket handler for Chrome extension communication
//! and HTTP endpoints for extension status and commands.

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::{IntoResponse, Json},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::mcp::types::{ApiResponse, ApiState};

// =============================================================================
// Chrome Extension WebSocket Handlers
// =============================================================================

/// WebSocket handler for Chrome extension connection
///
/// The Chrome extension connects to /ws/extension to enable bidirectional
/// communication for UI Bridge exploration. The runner can send exploration
/// commands to the extension, which forwards them to the active browser tab.
pub async fn ws_extension_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_extension_ws(socket, state))
}

/// Handle WebSocket connection from Chrome extension (or offscreen document).
///
/// Uses a channel-based architecture to eliminate lock contention:
/// - This task exclusively owns the WebSocket SplitSink
/// - All other tasks (HTTP handlers, ping/pong) send messages through an mpsc channel
/// - The select loop drains the channel and forwards to the WebSocket without any mutex
///
/// This prevents the previous issue where HTTP handlers holding ws_sender mutex
/// could delay keepalive pings/pongs, causing the extension to detect a stale connection.
async fn handle_extension_ws(socket: WebSocket, state: Arc<ApiState>) {
    use std::sync::atomic::Ordering;

    let (mut sender, mut receiver) = socket.split();

    // Create an unbounded channel for outgoing messages.
    // The channel sender is stored in shared state for HTTP handlers to use.
    // This task exclusively owns the SplitSink and reads from the channel.
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Message>();

    // Store the channel sender (not the SplitSink) in shared state
    {
        let mut ws_sender = state.extension.ws_sender.lock().await;
        *ws_sender = Some(outbound_tx.clone());
    }

    // Update connection tracking
    let now_ms = chrono::Utc::now().timestamp_millis();
    state.extension.connected.store(true, Ordering::SeqCst);
    state.extension.last_pong.store(now_ms, Ordering::SeqCst);
    state
        .extension
        .connected_since
        .store(now_ms, Ordering::SeqCst);
    let reconnect_num = state
        .extension
        .reconnect_count
        .fetch_add(1, Ordering::SeqCst);

    info!(
        "Chrome extension WebSocket connected (connection #{}, previous last_pong_age=N/A)",
        reconnect_num + 1
    );

    // Per-connection counters for detailed disconnect logging
    let mut messages_received: u64 = 0;
    let mut pings_sent: u64 = 0;
    let mut pongs_received: u64 = 0;
    let mut text_messages_received: u64 = 0;
    let connection_start = std::time::Instant::now();

    // Server-side ping interval: sends PING every 20 seconds
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(20));
    ping_interval.tick().await; // First tick completes immediately

    // Track disconnect reason (initial value is fallback, all break paths set it)
    #[allow(unused_assignments)]
    let mut disconnect_reason = "unknown";

    // Main event loop: process incoming messages, outgoing channel messages, and pings
    loop {
        tokio::select! {
            // Handle incoming messages from extension
            result = receiver.next() => {
                messages_received += 1;
                match result {
                    Some(Ok(Message::Text(text))) => {
                        text_messages_received += 1;
                        match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(msg) => {
                                // Pass the channel sender so message handlers can
                                // send responses without any mutex contention
                                handle_extension_message(msg, state.clone(), &outbound_tx).await;
                            }
                            Err(e) => {
                                warn!("Failed to parse extension message: {} (len={})", e, text.len());
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        debug!("Received WebSocket ping from extension ({}B)", data.len());
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Update last pong timestamp for health tracking
                        pongs_received += 1;
                        state.extension.last_pong.store(
                            chrono::Utc::now().timestamp_millis(),
                            Ordering::SeqCst,
                        );
                        debug!("Received pong from extension (pong #{}, connection age={}s)",
                            pongs_received, connection_start.elapsed().as_secs());
                    }
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(ref cf) = frame {
                            info!("Extension WebSocket CLOSE frame: code={}, reason=\"{}\"",
                                cf.code, cf.reason);
                            disconnect_reason = "close_frame";
                        } else {
                            info!("Extension WebSocket CLOSE (no frame)");
                            disconnect_reason = "close_no_frame";
                        }
                        break;
                    }
                    Some(Ok(msg)) => {
                        debug!("Extension WebSocket received other message type: {:?}", msg);
                    }
                    Some(Err(e)) => {
                        warn!("Extension WebSocket ERROR: {} (after {}s, {} msgs)",
                            e, connection_start.elapsed().as_secs(), messages_received);
                        disconnect_reason = "error";
                        break;
                    }
                    None => {
                        // Stream ended — connection was dropped without close frame
                        let elapsed = connection_start.elapsed().as_secs();
                        let last_pong_ms = state.extension.last_pong.load(Ordering::SeqCst);
                        let pong_age_ms = chrono::Utc::now().timestamp_millis() - last_pong_ms;
                        info!(
                            "Extension WebSocket stream ended (no close frame). \
                             connection_age={}s, last_pong_age={}ms, pings_sent={}, pongs_received={}, \
                             text_msgs={}, total_msgs={}",
                            elapsed, pong_age_ms, pings_sent, pongs_received,
                            text_messages_received, messages_received
                        );
                        disconnect_reason = "stream_ended";
                        break;
                    }
                }
            }
            // Forward outgoing messages from the channel to the WebSocket
            Some(msg) = outbound_rx.recv() => {
                if let Err(e) = sender.send(msg).await {
                    warn!("Failed to send outbound message to extension: {} (after {}s)",
                        e, connection_start.elapsed().as_secs());
                    disconnect_reason = "outbound_send_failed";
                    break;
                }
            }
            // Send periodic PING frames to detect dead connections
            _ = ping_interval.tick() => {
                pings_sent += 1;
                if let Err(e) = sender.send(Message::Ping(vec![])).await {
                    warn!("Failed to send ping #{} to extension: {} (after {}s)",
                        pings_sent, e, connection_start.elapsed().as_secs());
                    disconnect_reason = "ping_send_failed";
                    break;
                }
                debug!("Sent ping #{} to extension (connection age={}s)",
                    pings_sent, connection_start.elapsed().as_secs());
            }
        }
    }

    // Clean up on disconnect
    let connection_duration = connection_start.elapsed();
    let pending_count;
    {
        // Remove the channel sender so new callers get an error immediately
        let mut ws_sender = state.extension.ws_sender.lock().await;
        *ws_sender = None;
    }
    state.extension.connected.store(false, Ordering::SeqCst);
    state.extension.connected_since.store(0, Ordering::SeqCst);

    // Reject all pending requests
    {
        let mut pending = state.extension.pending_requests.lock().await;
        pending_count = pending.len();
        for (request_id, sender) in pending.drain() {
            let _ = sender.send(serde_json::json!({
                "success": false,
                "error": "Extension disconnected"
            }));
            debug!(
                "Rejected pending extension request {} due to disconnect",
                request_id
            );
        }
    }

    info!(
        "Chrome extension WebSocket DISCONNECTED: reason={}, duration={}s, \
         pings_sent={}, pongs_received={}, text_msgs={}, total_msgs={}, \
         pending_rejected={}",
        disconnect_reason,
        connection_duration.as_secs(),
        pings_sent,
        pongs_received,
        text_messages_received,
        messages_received,
        pending_count
    );
}

/// Handle a message from the Chrome extension.
///
/// The `outbound_tx` channel sender allows sending responses back through the
/// WebSocket without any mutex contention — messages are pushed into the channel
/// and the WebSocket handler task (which owns the SplitSink) forwards them.
async fn handle_extension_message(
    msg: serde_json::Value,
    state: Arc<ApiState>,
    outbound_tx: &mpsc::UnboundedSender<Message>,
) {
    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let request_id = msg.get("requestId").and_then(|r| r.as_str()).unwrap_or("");

    match msg_type {
        "EXPLORATION_RESPONSE" => {
            // Response to a command we sent to the extension
            let mut pending = state.extension.pending_requests.lock().await;
            if let Some(sender) = pending.remove(request_id) {
                let _ = sender.send(msg.clone());
                debug!("Delivered extension response for request {}", request_id);
            } else {
                warn!("No pending request found for response {}", request_id);
            }
        }
        "RECORDING_SNAPSHOT" => {
            // Snapshot from the recorder during a recording session
            handle_recording_snapshot(msg, state).await;
        }
        "PING" => {
            // Extension sent an application-level ping — respond with PONG
            // so the extension knows the connection is alive.
            // Uses channel send (non-blocking, no mutex) instead of locking ws_sender.
            let pong = serde_json::json!({ "type": "PONG" });
            if let Ok(json_str) = serde_json::to_string(&pong) {
                let _ = outbound_tx.send(Message::Text(json_str));
            }
            // Also update last pong tracking (bidirectional health)
            state.extension.last_pong.store(
                chrono::Utc::now().timestamp_millis(),
                std::sync::atomic::Ordering::SeqCst,
            );
            debug!("Responded to application-level ping from extension");
        }
        "PONG" => {
            // Update last pong timestamp (application-level pong)
            state.extension.last_pong.store(
                chrono::Utc::now().timestamp_millis(),
                std::sync::atomic::Ordering::SeqCst,
            );
            debug!("Received application-level pong from extension");
        }
        "EXTENSION_REQUEST" => {
            // Extension is sending a request to the runner (future use)
            debug!("Received extension request: {:?}", msg);
        }
        "FULL_PAGE_CAPTURE_PROGRESS" => {
            // Progress update from full-page screenshot capture
            if let Some(progress) = msg.get("progress") {
                debug!("Full-page capture progress: {:?}", progress);
                // Note: Tauri event emission removed - AppState doesn't have app_handle
                // Progress is logged for debugging purposes
            }
        }
        _ => {
            debug!("Unknown extension message type: {}", msg_type);
        }
    }
}

/// Handle a recording snapshot from the browser extension
async fn handle_recording_snapshot(msg: serde_json::Value, state: Arc<ApiState>) {
    // Extract snapshot data
    let snapshot = match msg.get("snapshot") {
        Some(s) => s,
        None => {
            warn!("RECORDING_SNAPSHOT missing snapshot field");
            return;
        }
    };

    // Parse the snapshot
    let parsed: Result<crate::recording::RecordingSnapshot, _> =
        serde_json::from_value(snapshot.clone());

    match parsed {
        Ok(snapshot_data) => {
            debug!(
                "Received recording snapshot: trigger={}, url={}, elements={}",
                snapshot_data.trigger, snapshot_data.url, snapshot_data.element_count
            );

            // If the snapshot has enhanced action capture data, we can auto-add to active recording
            if let Some(action) = &snapshot_data.action {
                // Look for an active recording that matches this tab
                let tab_id = msg
                    .get("sessionTabId")
                    .and_then(|t| t.as_i64())
                    .map(|t| t as i32);

                if let Some(tab_id) = tab_id {
                    // Try to find an active recording for this tab
                    let storage = crate::recording::RecordingStorage::new(
                        state.app_state.checkpoint_db.clone(),
                    );

                    match storage.list_recordings(
                        Some(crate::recording::RecordingStatus::Recording),
                        Some(10),
                    ) {
                        Ok(recordings) => {
                            // Find recording for this tab
                            if let Some(recording) =
                                recordings.iter().find(|r| r.tab_id == Some(tab_id))
                            {
                                // Parse action type
                                let action_type: Result<crate::recording::ActionType, _> =
                                    action.action_type.parse();

                                if let Ok(action_type) = action_type {
                                    // Build action data based on type
                                    let action_data = match action_type {
                                        crate::recording::ActionType::Click => {
                                            action.click.as_ref().map(|c| {
                                                serde_json::to_value(c)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Type => {
                                            action.type_data.as_ref().map(|t| {
                                                serde_json::to_value(t)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Navigate => {
                                            action.navigate.as_ref().map(|n| {
                                                serde_json::to_value(n)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Select => {
                                            action.select.as_ref().map(|s| {
                                                serde_json::to_value(s)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Scroll => {
                                            action.scroll.as_ref().map(|s| {
                                                serde_json::to_value(s)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Keypress => {
                                            action.keypress.as_ref().map(|k| {
                                                serde_json::to_value(k)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Hover => None,
                                    };

                                    let input = crate::recording::AddActionInput {
                                        action_type,
                                        url: action.url.clone(),
                                        page_title: snapshot_data.title.clone(),
                                        target: action.target.clone(),
                                        action_data,
                                        timestamp: action.timestamp.clone(),
                                        duration_ms: None,
                                    };

                                    match storage.add_action(&recording.id, input) {
                                        Ok(recorded_action) => {
                                            info!(
                                                "Auto-recorded action {} for recording {}",
                                                recorded_action.sequence_number, recording.id
                                            );

                                            // Emit event to frontend
                                            if let Err(e) = state.app_handle.emit(
                                                "recording-action-added",
                                                serde_json::json!({
                                                    "recording_id": recording.id,
                                                    "action": recorded_action,
                                                }),
                                            ) {
                                                warn!("Failed to emit recording-action-added event: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Failed to auto-record action: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to list active recordings: {}", e);
                        }
                    }
                }
            }

            // Broadcast the snapshot to connected clients (for real-time monitoring)
            let _ = state.app_state.event_broadcast.send(serde_json::json!({
                "type": "recording_snapshot",
                "snapshot": snapshot_data,
                "tab_id": msg.get("sessionTabId"),
                "total_snapshots": msg.get("totalSnapshots"),
            }));
        }
        Err(e) => {
            warn!("Failed to parse recording snapshot: {}", e);
            debug!("Raw snapshot: {:?}", snapshot);
        }
    }
}

/// Send a command to the extension and wait for response.
///
/// Uses the mpsc channel to send messages — this is non-blocking and never
/// contends with the WebSocket handler's ping/pong loop.
pub async fn send_extension_command(
    state: Arc<ApiState>,
    action: &str,
    params: serde_json::Value,
    timeout_secs: u64,
) -> Result<serde_json::Value, String> {
    use std::sync::atomic::Ordering;

    // Check if extension is connected
    if !state.extension.connected.load(Ordering::SeqCst) {
        return Err("Chrome extension not connected".to_string());
    }

    // Generate request ID
    let request_id = format!(
        "runner-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    );

    // Create command message
    let command = serde_json::json!({
        "type": "EXPLORATION_COMMAND",
        "requestId": request_id,
        "action": action,
        "params": params
    });

    // Create oneshot channel for response
    let (tx, rx) = tokio::sync::oneshot::channel();

    // Register pending request
    {
        let mut pending = state.extension.pending_requests.lock().await;
        pending.insert(request_id.clone(), tx);
    }

    // Send command to extension via the channel (non-blocking, no mutex on SplitSink)
    {
        let ws_sender = state.extension.ws_sender.lock().await;
        if let Some(ref sender) = *ws_sender {
            match serde_json::to_string(&command) {
                Ok(json_str) => {
                    if let Err(e) = sender.send(Message::Text(json_str)) {
                        // Clean up pending request
                        let mut pending = state.extension.pending_requests.lock().await;
                        pending.remove(&request_id);
                        return Err(format!("Failed to send command to extension: {}", e));
                    }
                }
                Err(e) => {
                    let mut pending = state.extension.pending_requests.lock().await;
                    pending.remove(&request_id);
                    return Err(format!("Failed to serialize command: {}", e));
                }
            }
        } else {
            let mut pending = state.extension.pending_requests.lock().await;
            pending.remove(&request_id);
            return Err("Extension WebSocket sender not available".to_string());
        }
    }

    // Wait for response with timeout (0 = default 30s, not infinite)
    let effective_timeout = if timeout_secs == 0 { 30 } else { timeout_secs };
    match tokio::time::timeout(std::time::Duration::from_secs(effective_timeout), rx).await {
        Ok(Ok(response)) => {
            let success = response
                .get("success")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            if success {
                Ok(response
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            } else {
                let error = response
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                Err(error.to_string())
            }
        }
        Ok(Err(_)) => Err("Response channel closed".to_string()),
        Err(_) => {
            // Timeout - clean up pending request
            let mut pending = state.extension.pending_requests.lock().await;
            pending.remove(&request_id);
            Err(format!(
                "Extension command timed out after {}s",
                effective_timeout
            ))
        }
    }
}

// =============================================================================
// Extension HTTP Endpoints (for Python bridge)
// =============================================================================

/// Request body for extension command
#[derive(Debug, Deserialize)]
pub struct ExtensionCommandRequest {
    action: String,
    #[serde(default)]
    params: serde_json::Value,
    #[serde(default = "default_extension_timeout")]
    timeout_secs: u64,
}

/// Default timeout for extension commands.
/// Returns 0 to indicate no timeout (run until completion).
fn default_extension_timeout() -> u64 {
    0 // No timeout - run until completion
}

/// Get extension connection status with health details
pub async fn get_extension_status(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    use std::sync::atomic::Ordering;

    let connected = state.extension.connected.load(Ordering::SeqCst);
    let last_pong_ms = state.extension.last_pong.load(Ordering::SeqCst);
    let connected_since_ms = state.extension.connected_since.load(Ordering::SeqCst);
    let reconnect_count = state.extension.reconnect_count.load(Ordering::SeqCst);

    let now_ms = chrono::Utc::now().timestamp_millis();
    let last_pong_ago_sec = if last_pong_ms > 0 {
        Some((now_ms - last_pong_ms) / 1000)
    } else {
        None
    };
    let connection_age_sec = if connected_since_ms > 0 {
        Some((now_ms - connected_since_ms) / 1000)
    } else {
        None
    };

    let pending_count = state.extension.pending_requests.lock().await.len();

    let health = if connected {
        if let Some(pong_age) = last_pong_ago_sec {
            if pong_age > 45 {
                "stale"
            } else if pong_age > 25 {
                "degraded"
            } else {
                "healthy"
            }
        } else {
            "unknown"
        }
    } else {
        "disconnected"
    };

    Json(ApiResponse::success(serde_json::json!({
        "connected": connected,
        "websocket_url": "ws://localhost:9876/ws/extension",
        "last_pong_ago_sec": last_pong_ago_sec,
        "connection_age_sec": connection_age_sec,
        "reconnect_count": reconnect_count,
        "pending_requests": pending_count,
        "health": health
    })))
}

/// Send a command to the extension and wait for response
pub async fn send_extension_command_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExtensionCommandRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!(
        "Extension command request: action={}, params={:?}",
        request.action, request.params
    );

    match send_extension_command(state, &request.action, request.params, request.timeout_secs).await
    {
        Ok(data) => Json(ApiResponse::success(data)),
        Err(e) => {
            warn!("Extension command failed: {}", e);
            Json(ApiResponse {
                success: false,
                data: None::<serde_json::Value>,
                error: Some(e.to_string()),
            })
        }
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/ws/extension", get(ws_extension_handler))
        .route("/extension/status", get(get_extension_status))
        .route("/extension/command", post(send_extension_command_handler))
}
