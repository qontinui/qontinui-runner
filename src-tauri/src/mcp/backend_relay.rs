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

    let mut backoff_ms: u64 = 1000;
    let max_backoff_ms: u64 = 30000;

    // Bug #5 fix: Subscribe to event broadcast BEFORE the reconnection loop
    // so events are buffered during backoff/reconnection delays.
    let mut event_rx = api_state.app_state.event_broadcast.subscribe();

    loop {
        if *shutdown_rx.borrow() {
            info!("Backend relay shutting down");
            return;
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
                backoff_ms = 1000; // Reset backoff on successful connection

                let (write, read) = ws_stream.split();
                let write = Arc::new(Mutex::new(write));

                // Run inbound and outbound handlers concurrently
                let write_clone = write.clone();
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

            // 2. Get SessionManager and spawn a Claude session
            let session_manager: Option<Arc<crate::claude_session::SessionManager>> = api_state
                .app_handle
                .try_state::<Arc<crate::claude_session::SessionManager>>()
                .map(|s| s.inner().clone());

            let Some(session_manager) = session_manager else {
                return Some(serde_json::json!({
                    "type": "chat_created",
                    "id": task_run_id,
                    "task_name": task_name,
                    "state": "error",
                    "error": "SessionManager not available"
                }));
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
                &task_run_id,
                &api_state.app_handle,
                None, // session_ctx
                None, // finding_ctx
                None, // progress_ctx
                None, // pid_tracker
            ) {
                Ok(session) => {
                    let session = Arc::new(session);

                    // 3. Register with session manager
                    if let Err(e) = session_manager.register(&task_run_id, session.clone()) {
                        warn!("Failed to register relay chat session: {}", e);
                        return Some(serde_json::json!({
                            "type": "chat_created",
                            "id": task_run_id,
                            "task_name": task_name,
                            "state": "error",
                            "error": format!("Session registration failed: {}", e)
                        }));
                    }

                    // 4. Emit initial ready state
                    crate::commands::ai_chat::emit_session_state(
                        &api_state.app_handle,
                        &task_run_id,
                        &task_run_id,
                        session.state(),
                    );

                    // 5. Send initial prompt (system prompt or user-provided prompt)
                    let prompt_to_send = initial_prompt.as_deref().unwrap_or(&system_prompt);
                    if let Err(e) = session.send_initial_prompt(prompt_to_send) {
                        warn!("Failed to send initial prompt for relay chat: {}", e);
                        return Some(serde_json::json!({
                            "type": "chat_created",
                            "id": task_run_id,
                            "task_name": task_name,
                            "state": "error",
                            "error": format!("Failed to send initial prompt: {}", e)
                        }));
                    }

                    // Emit processing state
                    crate::commands::ai_chat::emit_session_state(
                        &api_state.app_handle,
                        &task_run_id,
                        &task_run_id,
                        session.state(),
                    );

                    info!("Relay chat session created: task_run_id={}", task_run_id);
                    Some(serde_json::json!({
                        "type": "chat_created",
                        "id": task_run_id,
                        "task_name": task_name,
                        "state": "ready"
                    }))
                }
                Err(e) => {
                    warn!("Failed to spawn relay chat session: {}", e);
                    Some(serde_json::json!({
                        "type": "chat_created",
                        "id": task_run_id,
                        "task_name": task_name,
                        "state": "error",
                        "error": format!("Session creation failed: {}", e)
                    }))
                }
            }
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
            };

            let doctor_handle = api_state.doctor_handle.clone();
            let db2 = api_state.app_state.checkpoint_db.clone();
            let trid = task_run_id.clone();

            let gen_result = tokio::task::spawn_blocking(move || {
                let gen_result = db2.with_conn(|conn| {
                    Ok(crate::workflow_generation::generate_workflow(
                        request,
                        doctor_handle.as_ref(),
                        Some(conn),
                        None,
                    ))
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
        if guard.is_some() {
            return Ok("Cloud relay is already running".to_string());
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
