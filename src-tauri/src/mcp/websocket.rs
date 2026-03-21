//! WebSocket and SSE event streaming handlers for MCP API
//!
//! Provides real-time event streaming via WebSocket and Server-Sent Events (SSE)
//! for frontend clients to receive live automation state updates.

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::sse::{Event, KeepAlive},
    response::{IntoResponse, Sse},
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::mcp::types::ApiState;

/// WebSocket handler for streaming execution events
///
/// Clients connect to /ws/events to receive real-time execution events including:
/// - Image recognition results with found coordinates
/// - Tree events (state activation/deactivation)
/// - Workflow execution progress
///
/// This enables the web frontend to display live perception of automation state.
pub async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_events(socket, state))
}

/// Handle WebSocket connection for event streaming
async fn handle_ws_events(socket: WebSocket, state: Arc<ApiState>) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to the broadcast channel
    let mut event_rx = state.app_state.event_broadcast.subscribe();

    info!("WebSocket client connected for event streaming");

    // Spawn task to forward broadcast events to WebSocket
    let send_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    // Serialize event to JSON string
                    match serde_json::to_string(&event) {
                        Ok(json_str) => {
                            if sender.send(Message::Text(json_str.into())).await.is_err() {
                                debug!("WebSocket client disconnected");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to serialize event for WebSocket: {}", e);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WebSocket client lagged, skipped {} events", n);
                    // Continue receiving - client can handle gaps
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("Event broadcast channel closed");
                    break;
                }
            }
        }
    });

    // Handle incoming messages (for ping/pong or future commands)
    while let Some(result) = receiver.next().await {
        match result {
            Ok(Message::Ping(data)) => {
                // Ping/pong is handled automatically by axum
                debug!("Received ping from WebSocket client");
                let _ = data; // Acknowledge we received it
            }
            Ok(Message::Close(_)) => {
                debug!("WebSocket client sent close");
                break;
            }
            Ok(_) => {
                // Ignore other message types for now
            }
            Err(e) => {
                warn!("WebSocket receive error: {}", e);
                break;
            }
        }
    }

    // Clean up send task
    send_task.abort();
    info!("WebSocket client disconnected from event streaming");
}

/// SSE events handler for MCP notification streaming
///
/// Provides Server-Sent Events (SSE) stream of runner events.
/// More compatible with HTTP-based MCP clients than WebSocket.
///
/// Events emitted:
/// - qontinui/execution_started - Workflow begins
/// - qontinui/execution_progress - Step completion
/// - qontinui/execution_completed - Workflow ends
/// - qontinui/test_started - Test begins
/// - qontinui/test_completed - Test ends
/// - qontinui/image_recognition - Match found/failed
/// - qontinui/error - Error occurs
pub async fn sse_events_handler(
    State(state): State<Arc<ApiState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    use futures_util::StreamExt as FuturesStreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    // Subscribe to the broadcast channel
    let event_rx = state.app_state.event_broadcast.subscribe();

    info!("SSE client connected for event streaming");

    // Convert broadcast receiver to SSE stream
    // Use futures_util::StreamExt::filter_map for compatibility with Sse
    let stream =
        FuturesStreamExt::filter_map(BroadcastStream::new(event_rx), |result| async move {
            match result {
                Ok(event) => {
                    // Serialize event to JSON and wrap in SSE Event
                    match serde_json::to_string(&event) {
                        Ok(json_str) => {
                            // Determine event type from the event data
                            let event_type = event
                                .get("event_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("message");

                            Some(Ok(Event::default()
                                .event(format!("qontinui/{}", event_type))
                                .data(json_str)))
                        }
                        Err(e) => {
                            warn!("Failed to serialize event for SSE: {}", e);
                            None
                        }
                    }
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    warn!("SSE client lagged, skipped {} events", n);
                    // Send a notification about skipped events
                    Some(Ok(Event::default().event("qontinui/warning").data(
                        format!("{{\"message\": \"Skipped {} events due to lag\"}}", n),
                    )))
                }
            }
        });

    // Return SSE response with keep-alive
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/ws/events", get(ws_events_handler))
        .route("/sse/events", get(sse_events_handler))
}
