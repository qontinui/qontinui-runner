//! Backend relay client for remote mobile-to-runner communication.
//!
//! Connects outbound to the qontinui-web backend via WebSocket,
//! enabling remote control from mobile devices when not on LAN.

use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
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
    /// Kick counter: incremented to interrupt backoff sleeps and force an
    /// immediate reconnect with freshly-read settings + tokens. Used after
    /// login or backend_url changes so the running task picks up new state
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

    /// Kick the relay: interrupt any in-progress backoff sleep and force the
    /// loop to restart its next iteration immediately, re-reading settings
    /// and auth tokens from scratch.
    ///
    /// Call this after:
    /// - User re-logs in (fresh tokens stored in keyring)
    /// - `cloud_relay.backend_url` changes in settings (new tunnel, etc.)
    /// - Any other event that makes the relay's current backoff stale
    pub fn kick(&self) {
        let current = *self.kick_tx.borrow();
        let _ = self.kick_tx.send(current.wrapping_add(1));
    }
}

/// Start the backend relay client.
///
/// Connects outbound to the backend WebSocket and:
/// - Forwards inbound chat commands to local session handlers
/// - Forwards outbound ai-output/session-state events to the backend
/// - Auto-reconnects with exponential backoff
/// - Refreshes auth token on each reconnection attempt
/// - Re-reads `cloud_relay.backend_url` from settings on every attempt, so
///   settings changes (e.g., new tunnel URL) take effect on the next retry
///   without needing a full runner restart
///
/// Note: the `backend_url` argument is retained for call-site clarity and to
/// surface an error if cloud relay is disabled, but the loop itself always
/// reads the current settings value so URL changes are picked up live.
pub async fn start_relay(api_state: Arc<ApiState>, _backend_url: &str) -> Arc<BackendRelayState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // Kick channel: incremented by `.kick()` to interrupt backoff sleeps and
    // force an immediate retry with freshly-read settings + tokens.
    let (kick_tx, kick_rx) = watch::channel(0u64);
    let state = api_state.clone();

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

        // Re-read backend URL from settings on every iteration so that URL
        // changes (new tunnel, re-configured backend) are picked up live.
        let ws_base_url = {
            let settings = crate::settings::load_settings();
            format!(
                "{}/api/v1/automation/ws/automation/runner",
                settings
                    .cloud_relay
                    .backend_url
                    .replace("https://", "wss://")
                    .replace("http://", "ws://"),
            )
        };

        // After many consecutive quick disconnects, the backend is persistently
        // rejecting us (e.g., streaming not enabled, session limits). Back off
        // aggressively to avoid hammering the server.
        if consecutive_quick_disconnects >= 5 {
            let extended_backoff = max_backoff_ms.max(120_000); // 2 minutes
            warn!(
                "Backend relay: {} consecutive quick disconnects. \
                 Check automation_streaming_enabled, session limits, and auth. \
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
            // Reset counter after extended backoff (or kick) so we try again
            consecutive_quick_disconnects = 0;
            backoff_ms = 2000;
        }

        // Bug #4 fix: Refresh auth token on each reconnection attempt
        let token = match AuthManager::new().get_access_token() {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to refresh auth token for relay: {}", e);
                // Backoff and retry — token may become available after re-login
                info!(
                    "Reconnecting in {}ms... (send kick to retry sooner)",
                    backoff_ms
                );
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {
                        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Backend relay shutting down during backoff");
                        return;
                    }
                    _ = kick_rx.changed() => {
                        info!("Backend relay kicked during token backoff — retrying now");
                        backoff_ms = 2000;
                    }
                }
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
                    if let Err(e) = w.send(Message::Text(info_text.into())).await {
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
                            if let Err(e) = w.send(Message::Ping(vec![].into())).await {
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
        info!(
            "Reconnecting in {}ms... (send kick to retry sooner)",
            backoff_ms
        );
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
            }
            _ = shutdown_rx.changed() => {
                info!("Backend relay shutting down during backoff");
                return;
            }
            _ = kick_rx.changed() => {
                info!("Backend relay kicked during reconnect backoff — retrying now");
                backoff_ms = 2000;
            }
        }
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

                // Forward ai-output, session-state, and terminal events
                let relay_msg = if channel == "ai-output" {
                    serde_json::json!({
                        "type": "chat_response",
                        "data": event.get("payload"),
                    })
                } else if channel == "session-state" {
                    serde_json::json!({
                        "type": "chat_session_state",
                        "data": event.get("payload"),
                    })
                } else if channel == "terminal-output" {
                    serde_json::json!({
                        "type": "terminal_output",
                        "terminal_id": event.get("payload").and_then(|p| p.get("terminal_id")),
                        "data": event.get("payload").and_then(|p| p.get("data")),
                    })
                } else if channel == "terminal-exit" {
                    serde_json::json!({
                        "type": "terminal_exit",
                        "terminal_id": event.get("payload").and_then(|p| p.get("terminal_id")),
                        "exit_code": event.get("payload").and_then(|p| p.get("exit_code")),
                    })
                } else {
                    continue;
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

        "chat_list_running" => match api_state.app_state.pg_db.get_running_task_runs(None).await {
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
        },

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

            // 1. Create task run record in DB
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
                    None, // model_override
                ) {
                    Ok(session) => {
                        let session = Arc::new(session);

                        if let Err(e) = session_manager.register(&bg_task_run_id, session.clone()) {
                            warn!("Failed to register relay AI session: {}", e);
                            return;
                        }

                        // Emit ready state
                        crate::commands::ai_session::emit_session_state(
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

            // Return immediately so handle_inbound stays responsive to pings
            info!("Relay AI session initializing: task_run_id={}", task_run_id);
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
            // See transcript.rs rationale — thread AppState for brief-mode port.
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

            // Save pipeline artifact via PG (async, outside spawn_blocking)
            match &gen_result {
                Ok((_, artifact)) => {
                    if let Err(e) = pg_db.save_generation_artifact(artifact).await {
                        tracing::warn!("Failed to save pipeline artifact to PG: {}", e);
                    }
                }
                _ => {}
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

            let rename_result = api_state
                .app_state
                .pg_db
                .update_task_name(&task_run_id, &new_name)
                .await;

            match rename_result {
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

        // ====================================================================
        // Terminal relay commands (from mobile via backend)
        // ====================================================================
        "terminal_list" => {
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

        "terminal_create" => {
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
                    None, // page_id
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

        "terminal_input" => {
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
            // Fire-and-forget, no response
            None
        }

        "terminal_resize" => {
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
            // Fire-and-forget, no response
            None
        }

        "terminal_close" => {
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

        "terminal_buffer" => {
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

    /// Auto-start the cloud relay if enabled and auto_connect is set,
    /// or if a local backend is detected on localhost:8000.
    /// Called from mcp_api.rs where ApiState is available.
    pub async fn auto_start_cloud_relay(api_state: Arc<ApiState>) {
        let settings = settings::load_settings();

        // Determine backend URL: use explicit config, or auto-detect local backend
        let backend_url = if settings.cloud_relay.enabled && settings.cloud_relay.auto_connect {
            settings.cloud_relay.backend_url.clone()
        } else if !settings.cloud_relay.enabled {
            // Auto-detect local backend for local dev
            match detect_local_backend().await {
                Some(url) => {
                    info!(
                        "Local backend detected at {}, auto-connecting cloud relay",
                        url
                    );
                    url
                }
                None => return,
            }
        } else {
            return;
        };

        let mut guard = get_relay_holder().lock().await;

        // Check if existing relay is still alive
        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);

            if is_alive {
                // Relay task is alive — it may be stuck in a backoff loop with
                // stale tokens or pointed at a stale URL. Kick it so the next
                // iteration re-reads settings + tokens and retries immediately.
                info!("Cloud relay already running; kicking to re-read settings/tokens");
                existing.kick();
                return;
            }

            // Relay task has finished (dead connection) — stop it and restart
            info!("Cloud relay task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        info!("Auto-starting cloud relay to {}", backend_url);

        let relay = start_relay(api_state, &backend_url).await;
        *guard = Some(relay);
    }

    /// Probe localhost:8000 to see if a local backend is running.
    /// Returns the URL if reachable, None otherwise.
    async fn detect_local_backend() -> Option<String> {
        let url = "http://localhost:8000";
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
        {
            Ok(c) => c,
            Err(_) => return None,
        };
        match client.get(format!("{}/health", url)).send().await {
            Ok(resp) if resp.status().is_success() => Some(url.to_string()),
            _ => None,
        }
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
    pub async fn stop_cloud_relay() -> Result<String, String> {
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
    pub async fn get_cloud_relay_status() -> Result<serde_json::Value, String> {
        Ok(get_cloud_relay_status_internal().await)
    }

    #[tauri::command]
    pub async fn save_cloud_relay_settings(
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

        // If the relay is running, kick it so it re-reads settings (picks up
        // the new backend_url) on its next iteration — no restart needed.
        kick_cloud_relay().await;

        Ok("Cloud relay settings saved".to_string())
    }

    /// Kick the running cloud relay (if any) so it interrupts any backoff
    /// sleep and immediately retries with freshly-read settings and tokens.
    /// Call after login, settings changes, or any other event that makes
    /// the relay's current state stale.
    pub async fn kick_cloud_relay() {
        let guard = get_relay_holder().lock().await;
        if let Some(ref relay) = *guard {
            relay.kick();
        }
    }

    #[tauri::command]
    pub async fn get_cloud_relay_settings() -> Result<serde_json::Value, String> {
        // ApiState is not a Tauri-managed state (only axum uses it), so this
        // command reads directly from settings.json like any other Tauri
        // settings command. Previously declared `State<Arc<ApiState>>` which
        // caused Tauri to fail with "state not managed for field `state`".
        let settings = settings::load_settings();
        Ok(serde_json::json!({
            "enabled": settings.cloud_relay.enabled,
            "backend_url": settings.cloud_relay.backend_url,
            "auto_connect": settings.cloud_relay.auto_connect
        }))
    }
}
