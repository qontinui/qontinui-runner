//! WebSocket handlers for MCP API
//!
//! Provides live event streaming over WebSocket connections.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::types::ApiState;

/// WebSocket handler for live execution events
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
                            if sender.send(Message::Text(json_str)).await.is_err() {
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
