//! Backend relay client for remote mobile-to-runner communication.
//!
//! Connects outbound to the qontinui-web backend via WebSocket,
//! enabling remote control from mobile devices when not on LAN.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tauri::Manager;
use tokio::sync::{broadcast, watch, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

use crate::mcp::types::ApiState;

/// State for the backend relay client
pub struct BackendRelayState {
    /// Shutdown signal
    shutdown_tx: watch::Sender<bool>,
    /// Handle to the relay task
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl BackendRelayState {
    /// Stop the relay client
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
        info!("Backend relay stopped");
    }
}

/// Start the backend relay client.
///
/// Connects outbound to the backend WebSocket and:
/// - Forwards inbound chat commands to local session handlers
/// - Forwards outbound ai-output/session-state events to the backend
/// - Auto-reconnects with exponential backoff
pub async fn start_relay(
    api_state: Arc<ApiState>,
    backend_url: &str,
    auth_token: &str,
) -> Arc<BackendRelayState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let url = format!(
        "{}/api/v1/automation/ws/automation/runner?token={}",
        backend_url
            .replace("https://", "wss://")
            .replace("http://", "ws://"),
        auth_token
    );
    let url_clone = url.clone();
    let state = api_state.clone();

    let task_handle = tokio::spawn(async move {
        relay_loop(state, &url_clone, shutdown_rx).await;
    });

    Arc::new(BackendRelayState {
        shutdown_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

async fn relay_loop(api_state: Arc<ApiState>, url: &str, mut shutdown_rx: watch::Receiver<bool>) {
    let mut backoff_ms: u64 = 1000;
    let max_backoff_ms: u64 = 30000;

    loop {
        if *shutdown_rx.borrow() {
            info!("Backend relay shutting down");
            return;
        }

        info!(
            "Connecting to backend relay: {}",
            url.split('?').next().unwrap_or(url)
        );

        match connect_async(url).await {
            Ok((ws_stream, _response)) => {
                info!("Connected to backend relay");
                backoff_ms = 1000; // Reset backoff on successful connection

                let (write, read) = ws_stream.split();
                let write = Arc::new(Mutex::new(write));

                // Subscribe to local event broadcast for forwarding
                let event_rx = api_state.app_state.event_broadcast.subscribe();

                // Run inbound and outbound handlers concurrently
                let write_clone = write.clone();
                let state_clone = api_state.clone();
                let mut shutdown_clone = shutdown_rx.clone();

                tokio::select! {
                    _ = handle_inbound(read, state_clone, write_clone) => {
                        warn!("Backend relay inbound handler ended");
                    }
                    _ = handle_outbound(event_rx, write.clone()) => {
                        warn!("Backend relay outbound handler ended");
                    }
                    _ = shutdown_clone.changed() => {
                        info!("Backend relay received shutdown signal");
                        return;
                    }
                }
            }
            Err(e) => {
                warn!("Failed to connect to backend relay: {}", e);
            }
        }

        // Exponential backoff before reconnecting
        info!("Reconnecting in {}ms...", backoff_ms);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
            _ = shutdown_rx.changed() => {
                info!("Backend relay shutting down during backoff");
                return;
            }
        }
        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
    }
}

/// Handle inbound messages from the backend (commands from mobile)
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
                    let response = handle_relay_command(&api_state, msg_type, &data).await;
                    if let Some(response) = response {
                        let response_text = serde_json::to_string(&response).unwrap_or_default();
                        let mut w = write.lock().await;
                        if let Err(e) = w.send(Message::Text(response_text)).await {
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
            Ok(Message::Close(_)) => {
                info!("Backend relay WebSocket closed");
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

/// Handle outbound events (forward local events to backend)
async fn handle_outbound<S>(
    mut event_rx: broadcast::Receiver<Value>,
    write: Arc<
        Mutex<futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>>,
    >,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match event_rx.recv().await {
            Ok(event) => {
                let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");

                // Only forward ai-output and session-state events
                if channel != "ai-output" && channel != "session-state" {
                    continue;
                }

                let relay_msg = if channel == "ai-output" {
                    serde_json::json!({
                        "type": "chat_response",
                        "data": event.get("payload"),
                    })
                } else {
                    serde_json::json!({
                        "type": "chat_session_state",
                        "data": event.get("payload"),
                    })
                };

                let text = serde_json::to_string(&relay_msg).unwrap_or_default();
                let mut w = write.lock().await;
                if let Err(e) = w.send(Message::Text(text)).await {
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

/// Handle a command from the backend/mobile client
async fn handle_relay_command(
    api_state: &Arc<ApiState>,
    msg_type: &str,
    data: &Value,
) -> Option<Value> {
    match msg_type {
        "chat_message" => {
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

            // Use the same logic as the HTTP send_message_to_session handler
            let session_manager: Option<Arc<crate::claude_session::SessionManager>> = api_state
                .app_handle
                .try_state::<Arc<crate::claude_session::SessionManager>>()
                .map(|s| s.inner().clone());

            if let Some(sm) = session_manager {
                if let Some(session) = sm.get(task_run_id) {
                    // Emit user message event
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::mcp::shared::emit_ai_output(
                            &api_state.app_handle,
                            content,
                            "user_message",
                            None,
                            None,
                        );
                    }));

                    // Persist to output log
                    let _ = api_state.app_state.checkpoint_db.append_task_output_ex(
                        task_run_id,
                        &format!("\n[USER_MESSAGE]\n{}\n[/USER_MESSAGE]\n", content),
                        false,
                        false,
                    );

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

        "chat_list_running" => {
            let db = api_state.app_state.checkpoint_db.clone();
            match tokio::task::spawn_blocking(move || db.get_running_task_runs()).await {
                Ok(Ok(runs)) => {
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

        "chat_session_state" => {
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

        "chat_create" => {
            let task_name = data
                .get("task_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Remote Chat");
            let task_run_id = uuid::Uuid::new_v4().to_string();

            let db = api_state.app_state.checkpoint_db.clone();
            let id_clone = task_run_id.clone();
            let name_clone = task_name.to_string();
            let create_result = tokio::task::spawn_blocking(move || {
                db.create_task_run(&crate::database::CreateTaskRunInput {
                    id: id_clone,
                    task_name: name_clone,
                    prompt: Some("Remote chat session".to_string()),
                    task_type: Some("task".to_string()),
                    config_id: None,
                    workflow_name: None,
                    max_sessions: None,
                    auto_continue: None,
                    execution_steps_json: None,
                    log_sources_json: None,
                    workflow_type: Some("chat".to_string()),
                    parent_task_run_id: None,
                    root_task_run_id: None,
                    depth: 0,
                    workspace_id: None,
                    triggered_by: None,
                    bridge_id: None,
                    is_reflection: false,
                    reflection_source_task_run_id: None,
                })
            })
            .await;

            match create_result {
                Ok(Ok(_)) => Some(serde_json::json!({
                    "type": "chat_created",
                    "id": task_run_id,
                    "task_name": task_name,
                    "state": "created"
                })),
                _ => Some(serde_json::json!({
                    "type": "chat_created",
                    "error": "Failed to create task run"
                })),
            }
        }

        "heartbeat" => Some(serde_json::json!({"type": "heartbeat_ack"})),

        _ => {
            warn!("Unknown relay command type: {}", msg_type);
            None
        }
    }
}

/// Tauri commands for cloud relay control
pub mod commands {
    use super::*;
    use crate::settings;

    #[tauri::command]
    pub async fn start_cloud_relay(
        _state: tauri::State<'_, Arc<ApiState>>,
    ) -> Result<String, String> {
        let settings = settings::load_settings();
        if !settings.cloud_relay.enabled {
            return Err("Cloud relay is not enabled in settings".to_string());
        }

        // TODO: Get auth token from stored credentials
        // For now, return an error explaining the requirement
        Err("Cloud relay requires authentication. Configure credentials in settings.".to_string())
    }

    #[tauri::command]
    pub async fn stop_cloud_relay(
        _state: tauri::State<'_, Arc<ApiState>>,
    ) -> Result<String, String> {
        // The relay state would need to be stored in AppState or managed state
        // For now, this is a placeholder
        Ok("Cloud relay stopped".to_string())
    }
}
