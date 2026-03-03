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
}

/// Start the backend relay client.
///
/// Connects outbound to the backend WebSocket and:
/// - Forwards inbound chat commands to local session handlers
/// - Forwards outbound ai-output/session-state events to the backend
/// - Auto-reconnects with exponential backoff
/// - Refreshes auth token on each reconnection attempt
pub async fn start_relay(api_state: Arc<ApiState>, backend_url: &str) -> Arc<BackendRelayState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let ws_base_url = format!(
        "{}/api/v1/automation/ws/automation/runner",
        backend_url
            .replace("https://", "wss://")
            .replace("http://", "ws://"),
    );
    let state = api_state.clone();

    let task_handle = tokio::spawn(async move {
        relay_loop(state, &ws_base_url, shutdown_rx).await;
    });

    Arc::new(BackendRelayState {
        shutdown_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

async fn relay_loop(
    api_state: Arc<ApiState>,
    ws_base_url: &str,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    use crate::auth::AuthManager;

    let mut backoff_ms: u64 = 2000;
    let max_backoff_ms: u64 = 60000;
    let mut consecutive_quick_disconnects: u32 = 0;

    // Bug #5 fix: Subscribe to event broadcast BEFORE the reconnection loop
    // so events are buffered during backoff/reconnection delays.
    let mut event_rx = api_state.app_state.event_broadcast.subscribe();

    loop {
        if *shutdown_rx.borrow() {
            info!("Backend relay shutting down");
            return;
        }

        // After many consecutive quick disconnects, the backend is persistently
        // rejecting us (e.g., streaming not enabled, session limits). Back off
        // aggressively to avoid hammering the server.
        if consecutive_quick_disconnects >= 5 {
            let extended_backoff = max_backoff_ms.max(120_000); // 2 minutes
            warn!(
                "Backend relay: {} consecutive quick disconnects. \
                 Check automation_streaming_enabled, session limits, and auth. \
                 Backing off for {}s.",
                consecutive_quick_disconnects,
                extended_backoff / 1000
            );
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(extended_backoff)) => {}
                _ = shutdown_rx.changed() => {
                    info!("Backend relay shutting down during extended backoff");
                    return;
                }
            }
            // Reset counter after extended backoff so we try again
            consecutive_quick_disconnects = 0;
            backoff_ms = 2000;
        }

        // Bug #4 fix: Refresh auth token on each reconnection attempt
        let token = match AuthManager::new().get_access_token() {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to refresh auth token for relay: {}", e);
                // Backoff and retry — token may become available after re-login
                info!("Reconnecting in {}ms...", backoff_ms);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
                    _ = shutdown_rx.changed() => {
                        info!("Backend relay shutting down during backoff");
                        return;
                    }
                }
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                continue;
            }
        };

        let url = format!("{}?token={}", ws_base_url, token);

        info!("Connecting to backend relay: {}", ws_base_url);

        match connect_async(&url).await {
            Ok((ws_stream, _response)) => {
                info!("Connected to backend relay");
                let connected_at = std::time::Instant::now();

                let (write, read) = ws_stream.split();
                let write = Arc::new(Mutex::new(write));

                // Send runner_info immediately so the backend knows our API port
                {
                    let port = api_state
                        .app_state
                        .api_port
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let instance_name = std::env::var("QONTINUI_INSTANCE_NAME").ok();
                    let runner_info = serde_json::json!({
                        "type": "runner_info",
                        "data": {
                            "runner_name": instance_name.unwrap_or_else(|| "Desktop Runner".to_string()),
                            "runner_port": port,
                            "runner_hostname": hostname::get().ok().map(|h| h.to_string_lossy().to_string()),
                            "runner_os": std::env::consts::OS,
                            "runner_version": env!("CARGO_PKG_VERSION"),
                        }
                    });
                    let info_text = serde_json::to_string(&runner_info).unwrap_or_default();
                    let mut w = write.lock().await;
                    if let Err(e) = w.send(Message::Text(info_text)).await {
                        warn!("Failed to send runner_info to backend relay: {}", e);
                    } else {
                        info!("Sent runner_info to backend relay (port={})", port);
                    }
                }

                // Run inbound and outbound handlers concurrently
                let write_clone = write.clone();
                let write_ping = write.clone();
                let state_clone = api_state.clone();
                let mut shutdown_clone = shutdown_rx.clone();

                tokio::select! {
                    _ = handle_inbound(read, state_clone, write_clone) => {
                        warn!("Backend relay inbound handler ended");
                    }
                    _ = handle_outbound(&mut event_rx, write.clone()) => {
                        warn!("Backend relay outbound handler ended");
                    }
                    _ = shutdown_clone.changed() => {
                        info!("Backend relay received shutdown signal");
                        return;
                    }
                    // Keepalive: detect dead/half-open connections
                    _ = async {
                        let mut interval = tokio::time::interval(Duration::from_secs(30));
                        interval.tick().await; // Skip immediate first tick
                        loop {
                            interval.tick().await;
                            let mut w = write_ping.lock().await;
                            if let Err(e) = w.send(Message::Ping(vec![])).await {
                                warn!("Relay keepalive ping failed: {}", e);
                                return;
                            }
                        }
                    } => {
                        warn!("Backend relay keepalive detected dead connection");
                    }
                }

                // Check if connection was stable (lasted > 10 seconds)
                let connection_duration = connected_at.elapsed();
                if connection_duration > Duration::from_secs(10) {
                    // Connection was stable — reset backoff
                    backoff_ms = 2000;
                    consecutive_quick_disconnects = 0;
                } else {
                    // Connection dropped quickly — likely rejected by backend
                    consecutive_quick_disconnects += 1;
                    warn!(
                        "Backend relay connection lasted only {:.1}s (quick disconnect #{})",
                        connection_duration.as_secs_f64(),
                        consecutive_quick_disconnects
                    );
                }
            }
            Err(e) => {
                warn!("Failed to connect to backend relay: {}", e);
                consecutive_quick_disconnects += 1;
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
                    info!("Relay inbound message: type={}", msg_type);
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
            Ok(Message::Close(frame)) => {
                if let Some(ref f) = frame {
                    warn!(
                        "Backend relay WebSocket closed by server: code={}, reason={}",
                        f.code, f.reason
                    );
                } else {
                    info!("Backend relay WebSocket closed (no close frame)");
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

/// Handle outbound events (forward local events to backend).
/// Takes a mutable reference to the broadcast receiver so it persists across reconnections.
async fn handle_outbound<S>(
    event_rx: &mut broadcast::Receiver<Value>,
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

            // 1. Create task run record in DB (fast, synchronous)
            let db = api_state.app_state.checkpoint_db.clone();
            let id_clone = task_run_id.clone();
            let name_clone = task_name.to_string();
            let create_result = tokio::task::spawn_blocking(move || {
                db.create_task_run(
                    &crate::database::CreateTaskRunInput::new(id_clone, name_clone)
                        .with_prompt("Remote chat session")
                        .with_workflow_type("chat"),
                )
            })
            .await;

            if create_result.is_err() || create_result.as_ref().unwrap().is_err() {
                return Some(serde_json::json!({
                    "type": "chat_created",
                    "error": "Failed to create task run"
                }));
            }

            // 2. Spawn session setup in background to avoid blocking the
            //    inbound handler (Claude CLI init can take 30+ seconds,
            //    during which we can't respond to WebSocket pings).
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

                let system_prompt = "You are an AI assistant in a chat session initiated from the \
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
                ) {
                    Ok(session) => {
                        let session = Arc::new(session);

                        if let Err(e) = session_manager.register(&bg_task_run_id, session.clone()) {
                            warn!("Failed to register relay chat session: {}", e);
                            return;
                        }

                        // Emit ready state
                        crate::commands::ai_chat::emit_session_state(
                            &bg_state.app_handle,
                            &bg_task_run_id,
                            &bg_task_run_id,
                            session.state(),
                        );

                        // Send initial prompt
                        let prompt_to_send = initial_prompt.as_deref().unwrap_or(&system_prompt);
                        if let Err(e) = session.send_initial_prompt(prompt_to_send) {
                            warn!("Failed to send initial prompt for relay chat: {}", e);
                            return;
                        }

                        // Emit processing state
                        crate::commands::ai_chat::emit_session_state(
                            &bg_state.app_handle,
                            &bg_task_run_id,
                            &bg_task_run_id,
                            session.state(),
                        );

                        info!(
                            "Relay chat session ready: task_run_id={}, task_name={}",
                            bg_task_run_id, bg_task_name
                        );
                    }
                    Err(e) => {
                        warn!("Failed to spawn relay chat session: {}", e);
                    }
                }
            });

            // Return immediately so handle_inbound stays responsive to pings
            info!(
                "Relay chat session initializing: task_run_id={}",
                task_run_id
            );
            Some(serde_json::json!({
                "type": "chat_created",
                "id": task_run_id,
                "task_name": task_name,
                "state": "initializing"
            }))
        }

        "chat_interrupt" => {
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
                            crate::commands::ai_chat::emit_session_state(
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

        "chat_close" => {
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

            // Update status in DB
            let db = api_state.app_state.checkpoint_db.clone();
            let id_clone = task_run_id.clone();
            let _ = tokio::task::spawn_blocking(move || {
                db.update_task_run_status(&id_clone, "stopped")
            })
            .await;

            Some(serde_json::json!({
                "type": "chat_session_state",
                "task_run_id": task_run_id,
                "state": "closed",
                "can_send": false,
                "can_interrupt": false
            }))
        }

        "chat_generate_workflow" => {
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

            // Get conversation from DB output_log
            let db = api_state.app_state.checkpoint_db.clone();
            let id_clone = task_run_id.clone();
            let output_log = match tokio::task::spawn_blocking(move || {
                db.get_task_run_output(&id_clone)
            })
            .await
            {
                Ok(Ok(log)) => log.unwrap_or_default(),
                _ => String::new(),
            };

            if output_log.is_empty() {
                return Some(serde_json::json!({
                    "type": "chat_workflow_generated",
                    "task_run_id": task_run_id,
                    "success": false,
                    "error": "No conversation history available"
                }));
            }

            // Build generation request with conversation as inline context
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
            };

            let doctor_handle = api_state.doctor_handle.clone();
            let db2 = api_state.app_state.checkpoint_db.clone();
            let trid = task_run_id.clone();
            let artifact_task_run_id = task_run_id.clone();

            let gen_result = tokio::task::spawn_blocking(move || {
                let gen_result = db2.with_conn(|conn| {
                    let (response, mut artifact) = crate::workflow_generation::generate_workflow(
                        request,
                        doctor_handle.as_ref(),
                        Some(conn),
                        None,
                    );
                    artifact.task_run_id = Some(artifact_task_run_id.clone());
                    if let Err(e) = db2.save_pipeline_artifact(&artifact) {
                        tracing::warn!("Failed to save pipeline artifact: {}", e);
                    }
                    Ok(response)
                });
                match gen_result {
                    Ok(response) => response,
                    Err(e) => {
                        warn!("DB access failed for chat workflow generation: {}", e);
                        crate::workflow_generation::GenerateWorkflowResponse {
                            workflow: None,
                            validation_errors: vec![],
                            success: false,
                            error: Some(format!("Database error during generation: {}", e)),
                            model_used: None,
                            verification_iterations: vec![],
                            hardening_summary: None,
                            discovery_calls: vec![],
                        }
                    }
                }
            })
            .await;

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

        "chat_get_output" => {
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

            let db = api_state.app_state.checkpoint_db.clone();
            let id_clone = task_run_id.clone();
            match tokio::task::spawn_blocking(move || db.get_task_run_output(&id_clone)).await {
                Ok(Ok(Some(output))) => Some(serde_json::json!({
                    "type": "chat_output",
                    "task_run_id": task_run_id,
                    "output_log": output
                })),
                Ok(Ok(None)) => Some(serde_json::json!({
                    "type": "chat_output",
                    "task_run_id": task_run_id,
                    "output_log": ""
                })),
                _ => Some(serde_json::json!({
                    "type": "chat_output",
                    "task_run_id": task_run_id,
                    "output_log": "",
                    "error": "Failed to retrieve output"
                })),
            }
        }

        "chat_rename" => {
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

            let db = api_state.app_state.checkpoint_db.clone();
            let id_clone = task_run_id.clone();
            let name_clone = new_name.clone();
            let rename_result = tokio::task::spawn_blocking(move || {
                db.update_task_run_name(&id_clone, &name_clone)
            })
            .await;

            match rename_result {
                Ok(Ok(())) => Some(serde_json::json!({
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
    use std::sync::OnceLock;

    /// Global relay state (managed outside Tauri state for simplicity)
    static RELAY_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<BackendRelayState>>>> =
        OnceLock::new();

    fn get_relay_holder() -> &'static tokio::sync::Mutex<Option<Arc<BackendRelayState>>> {
        RELAY_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Auto-start the cloud relay if enabled and auto_connect is set.
    /// Called from mcp_api.rs where ApiState is available.
    pub async fn auto_start_cloud_relay(api_state: Arc<ApiState>) {
        let settings = settings::load_settings();
        if !settings.cloud_relay.enabled || !settings.cloud_relay.auto_connect {
            return;
        }

        let mut guard = get_relay_holder().lock().await;

        // Check if existing relay is still alive
        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);

            if is_alive {
                return; // Relay is running, nothing to do
            }

            // Relay task has finished (dead connection) — stop it and restart
            info!("Cloud relay task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        let backend_url = &settings.cloud_relay.backend_url;
        info!("Auto-starting cloud relay to {}", backend_url);

        let relay = start_relay(api_state, backend_url).await;
        *guard = Some(relay);
    }

    #[tauri::command]
    pub async fn start_cloud_relay(
        state: tauri::State<'_, Arc<ApiState>>,
    ) -> Result<String, String> {
        let settings = settings::load_settings();
        if !settings.cloud_relay.enabled {
            return Err("Cloud relay is not enabled in settings".to_string());
        }

        // Bug #11 fix: Hold the lock for the entire start operation to prevent
        // TOCTOU race where rapid double-invocations could spawn two relays.
        let mut guard = get_relay_holder().lock().await;
        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);

            if is_alive {
                return Ok("Cloud relay is already running".to_string());
            }

            // Dead relay — stop and restart
            existing.stop().await;
            *guard = None;
        }

        let backend_url = &settings.cloud_relay.backend_url;
        info!("Starting cloud relay to {}", backend_url);

        let relay = start_relay(state.inner().clone(), backend_url).await;
        *guard = Some(relay);
        // guard drops naturally here

        Ok(format!(
            "Cloud relay started, connecting to {}",
            backend_url
        ))
    }

    #[tauri::command]
    pub async fn stop_cloud_relay(
        _state: tauri::State<'_, Arc<ApiState>>,
    ) -> Result<String, String> {
        let mut holder = get_relay_holder().lock().await;
        if let Some(relay) = holder.take() {
            relay.stop().await;
            Ok("Cloud relay stopped".to_string())
        } else {
            Ok("Cloud relay was not running".to_string())
        }
    }

    /// Check relay status without Tauri state (for HTTP endpoint)
    pub async fn get_cloud_relay_status_internal() -> serde_json::Value {
        let settings = settings::load_settings();
        let holder = get_relay_holder().lock().await;

        let is_running = if let Some(ref relay) = *holder {
            let handle_guard = relay.task_handle.lock().await;
            handle_guard.as_ref().is_some_and(|h| !h.is_finished())
        } else {
            false
        };

        serde_json::json!({
            "enabled": settings.cloud_relay.enabled,
            "backend_url": settings.cloud_relay.backend_url,
            "auto_connect": settings.cloud_relay.auto_connect,
            "is_running": is_running
        })
    }

    /// Check if the cloud relay is currently running.
    /// Bug #12 fix: Also checks if the underlying task is still alive, not just
    /// whether the state struct exists.
    #[tauri::command]
    pub async fn get_cloud_relay_status(
        _state: tauri::State<'_, Arc<ApiState>>,
    ) -> Result<serde_json::Value, String> {
        let settings = settings::load_settings();
        let holder = get_relay_holder().lock().await;

        let is_running = if let Some(ref relay) = *holder {
            let handle_guard = relay.task_handle.lock().await;
            handle_guard.as_ref().is_some_and(|h| !h.is_finished())
        } else {
            false
        };

        Ok(serde_json::json!({
            "enabled": settings.cloud_relay.enabled,
            "backend_url": settings.cloud_relay.backend_url,
            "auto_connect": settings.cloud_relay.auto_connect,
            "is_running": is_running
        }))
    }

    #[tauri::command]
    pub async fn save_cloud_relay_settings(
        _state: tauri::State<'_, Arc<ApiState>>,
        enabled: bool,
        backend_url: String,
        auto_connect: bool,
    ) -> Result<String, String> {
        let mut settings = settings::load_settings();
        settings.cloud_relay.enabled = enabled;
        settings.cloud_relay.backend_url = if backend_url.is_empty() {
            "https://qontinui.io".to_string()
        } else {
            backend_url
        };
        settings.cloud_relay.auto_connect = auto_connect;
        settings::save_settings(&settings)
            .map_err(|e| format!("Failed to save settings: {}", e))?;
        Ok("Cloud relay settings saved".to_string())
    }

    #[tauri::command]
    pub async fn get_cloud_relay_settings(
        _state: tauri::State<'_, Arc<ApiState>>,
    ) -> Result<serde_json::Value, String> {
        let settings = settings::load_settings();
        Ok(serde_json::json!({
            "enabled": settings.cloud_relay.enabled,
            "backend_url": settings.cloud_relay.backend_url,
            "auto_connect": settings.cloud_relay.auto_connect
        }))
    }
}
