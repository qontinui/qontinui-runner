//! Tauri commands for interactive AI chat sessions.
//!
//! These commands allow the frontend to send messages to active Claude CLI sessions,
//! interrupt processing, query session state, and manage standalone chat sessions.

use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;
use tracing::{info, warn};

use crate::claude_session::manager::SessionManager;
use crate::claude_session::state::SessionState;
use crate::commands::{AppState, CommandResponse};
use crate::database::{CheckpointDb, CreateTaskRunInput};
use crate::execution_context::AiSessionContext;
use crate::mcp::shared::AiOutputEvent;

/// Session state event payload (emitted on state transitions).
#[derive(Debug, Clone, Serialize)]
pub struct SessionStateEvent {
    #[serde(rename = "taskRunId")]
    pub task_run_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub state: String,
}

/// Emit a session state change event to the frontend.
pub fn emit_session_state(
    app_handle: &tauri::AppHandle,
    task_run_id: &str,
    session_id: &str,
    state: SessionState,
) {
    let event = SessionStateEvent {
        task_run_id: task_run_id.to_string(),
        session_id: session_id.to_string(),
        state: state.as_event_str().to_string(),
    };
    if let Err(e) = app_handle.emit("claude-session-state", &event) {
        warn!("Failed to emit claude-session-state event: {}", e);
    }
}

/// Send a user message to an active AI session.
///
/// If the session is Ready, the message is sent immediately.
/// If the session is Processing, the message is queued for delivery after the current turn.
///
/// Returns success with `data.queued` indicating whether the message was queued.
#[tauri::command]
pub async fn send_user_message(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
    message: String,
) -> Result<CommandResponse, String> {
    info!(
        "send_user_message: task_run_id={}, message_len={}",
        task_run_id,
        message.len()
    );

    let session = session_manager
        .get(&task_run_id)
        .ok_or_else(|| format!("No active session found for task_run_id: {}", task_run_id))?;

    // Emit the user's message as an ai-output event so it appears in the conversation.
    // We emit directly (not via emit_ai_output) so we can include task_run_id
    // without needing a full AiSessionContext.
    {
        let now = chrono::Utc::now().timestamp_millis();
        let event = AiOutputEvent {
            id: format!("ai-{}-{}", now, rand::random::<u32>()),
            timestamp: now,
            line: message.clone(),
            source: "user_message".to_string(),
            action_id: None,
            task_run_id: Some(task_run_id.clone()),
            session_id: None,
            session_name: None,
            phase: None,
            phase_iteration: None,
        };
        if let Err(e) = app_handle.emit("ai-output", &event) {
            warn!("Failed to emit user message ai-output event: {}", e);
        }
    }

    // Persist user message to output_log for recap/summary generation
    if let Ok(db) = CheckpointDb::new() {
        let formatted = format!("\n[USER_MESSAGE]\n{}\n[/USER_MESSAGE]\n", message);
        if let Err(e) = db.append_task_output_ex(&task_run_id, &formatted, false, false) {
            warn!("Failed to persist user message to output_log: {}", e);
        }
    }

    // If this is the first user interaction, prepend a context note so the AI
    // switches from terse autonomous output to conversational responses.
    let effective_message = if !session.has_user_interacted() {
        info!(
            "First user interaction detected for task_run_id={}, injecting context switch note",
            task_run_id
        );
        format!(
            "[SYSTEM NOTE: A user is now watching and interacting with this session. \
             Please acknowledge their message and respond conversationally while continuing \
             your work. The user's message follows.]\n\n{}",
            message
        )
    } else {
        message.clone()
    };

    match session.send_user_message(&effective_message) {
        Ok(sent_immediately) => {
            let queued = !sent_immediately;

            // Emit state change
            let new_state = session.state();
            emit_session_state(&app_handle, &task_run_id, session.session_id(), new_state);

            Ok(CommandResponse {
                success: true,
                message: Some(if queued {
                    "Message queued - will be sent after current turn completes".to_string()
                } else {
                    "Message sent".to_string()
                }),
                data: Some(serde_json::json!({
                    "queued": queued,
                    "state": new_state.as_event_str(),
                })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to send message: {}", e)),
            data: None,
        }),
    }
}

/// Interrupt the currently processing AI session.
///
/// Sends an interrupt control request to the Claude CLI process.
/// The CLI will stop its current turn and return to Ready state.
#[tauri::command]
pub async fn interrupt_ai_session(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("interrupt_ai_session: task_run_id={}", task_run_id);

    let session = session_manager
        .get(&task_run_id)
        .ok_or_else(|| format!("No active session found for task_run_id: {}", task_run_id))?;

    match session.interrupt() {
        Ok(()) => {
            let new_state = session.state();
            emit_session_state(&app_handle, &task_run_id, session.session_id(), new_state);

            Ok(CommandResponse {
                success: true,
                message: Some("Interrupt requested".to_string()),
                data: Some(serde_json::json!({
                    "state": new_state.as_event_str(),
                })),
            })
        }
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Failed to interrupt: {}", e)),
            data: None,
        }),
    }
}

/// Get the current state of an AI session.
///
/// Returns the session state and metadata.
#[tauri::command]
pub async fn get_ai_session_state(
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    match session_manager.get(&task_run_id) {
        Some(session) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "state": session.state().as_event_str(),
                "sessionId": session.session_id(),
                "userInteracted": session.has_user_interacted(),
                "pid": session.pid(),
            })),
        }),
        None => Ok(CommandResponse {
            success: true,
            message: Some("No active session".to_string()),
            data: Some(serde_json::json!({
                "state": null,
            })),
        }),
    }
}

/// Create a standalone chat session.
///
/// Creates a task run record in the database, spawns a Claude CLI session
/// in the background, and returns the task_run_id immediately.
#[tauri::command]
pub async fn create_chat_session(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    app_state: tauri::State<'_, Arc<AppState>>,
    task_name: Option<String>,
) -> Result<CommandResponse, String> {
    let task_run_id = uuid::Uuid::new_v4().to_string();
    let name = task_name.unwrap_or_else(|| "New Chat".to_string());

    info!(
        "create_chat_session: task_run_id={}, name={}",
        task_run_id, name
    );

    // 1. Create task run record in DB
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let name_clone = name.clone();
    let create_result = tokio::task::spawn_blocking(move || {
        db.create_task_run(
            &CreateTaskRunInput::new(id_clone, name_clone)
                .with_prompt("Chat session")
                .with_workflow_type("chat"),
        )
    })
    .await;

    if create_result.is_err() || create_result.as_ref().unwrap().is_err() {
        return Ok(CommandResponse {
            success: false,
            message: Some("Failed to create task run record".to_string()),
            data: None,
        });
    }

    // 2. Spawn session setup in background
    let sm = session_manager.inner().clone();
    let handle = app_handle.clone();
    let trid = task_run_id.clone();
    let name_for_ctx = name.clone();
    tokio::spawn(async move {
        let working_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Create an AiSessionContext so output events include the taskRunId.
        // Without this, emit_ai_output would emit events with taskRunId=null
        // and the frontend would be unable to filter them.
        let session_ctx = AiSessionContext::setup(&trid, &name_for_ctx);

        match crate::claude_session::ClaudeSession::spawn(
            &working_dir,
            &trid,
            &handle,
            Some(session_ctx), // Tags output events with taskRunId
            None,              // finding_ctx
            None,              // progress_ctx
            None,              // pid_tracker
        ) {
            Ok(session) => {
                let session = Arc::new(session);

                if let Err(e) = sm.register(&trid, session.clone()) {
                    warn!("Failed to register chat session: {}", e);
                    return;
                }

                // Emit ready state — session is ready for the user's first message.
                // No initial prompt is sent; the first user message will include
                // a context switch note via send_user_message's first-interaction logic.
                emit_session_state(&handle, &trid, &trid, session.state());

                info!("Chat session ready: task_run_id={}", trid);
            }
            Err(e) => {
                warn!("Failed to spawn chat session: {}", e);
            }
        }
    });

    // 3. Return immediately with task_run_id
    Ok(CommandResponse {
        success: true,
        message: Some("Chat session creating".to_string()),
        data: Some(serde_json::json!({
            "task_run_id": task_run_id,
            "state": "initializing",
        })),
    })
}

/// Close a chat session.
///
/// Gracefully closes the Claude CLI session and updates the task run status.
#[tauri::command]
pub async fn close_chat_session(
    app_handle: tauri::AppHandle,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    app_state: tauri::State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    info!("close_chat_session: task_run_id={}", task_run_id);

    // Remove and close the session
    if let Some(session) = session_manager.remove(&task_run_id) {
        let _ = session.close();
    }

    // Update DB status
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let _ =
        tokio::task::spawn_blocking(move || db.update_task_run_status(&id_clone, "stopped")).await;

    // Emit closed state
    emit_session_state(
        &app_handle,
        &task_run_id,
        &task_run_id,
        SessionState::Closed,
    );

    Ok(CommandResponse {
        success: true,
        message: Some("Chat session closed".to_string()),
        data: Some(serde_json::json!({
            "state": "closed",
        })),
    })
}

/// Rename a chat session.
///
/// Updates the task_name for the task run in the database.
#[tauri::command]
pub async fn rename_chat_session(
    app_state: tauri::State<'_, Arc<AppState>>,
    task_run_id: String,
    name: String,
) -> Result<CommandResponse, String> {
    info!(
        "rename_chat_session: task_run_id={}, name={}",
        task_run_id, name
    );

    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let name_clone = name.clone();
    let result =
        tokio::task::spawn_blocking(move || db.update_task_run_name(&id_clone, &name_clone)).await;

    match result {
        Ok(Ok(())) => Ok(CommandResponse {
            success: true,
            message: Some("Session renamed".to_string()),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "name": name,
            })),
        }),
        _ => Ok(CommandResponse {
            success: false,
            message: Some("Failed to rename session".to_string()),
            data: None,
        }),
    }
}

/// Get the output log for a chat session.
///
/// Returns the full conversation output from the database.
#[tauri::command]
pub async fn get_chat_output(
    app_state: tauri::State<'_, Arc<AppState>>,
    task_run_id: String,
) -> Result<CommandResponse, String> {
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let result = tokio::task::spawn_blocking(move || db.get_task_run_output(&id_clone)).await;

    match result {
        Ok(Ok(Some(output))) => Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "output_log": output,
            })),
        }),
        Ok(Ok(None)) => Ok(CommandResponse {
            success: true,
            message: Some("No output available".to_string()),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "output_log": "",
            })),
        }),
        _ => Ok(CommandResponse {
            success: false,
            message: Some("Failed to get output".to_string()),
            data: None,
        }),
    }
}

/// Generate a workflow from a chat conversation.
///
/// Reads the conversation output log and uses the workflow generator
/// to create a UnifiedWorkflow from the conversation context.
#[tauri::command]
pub async fn generate_workflow_from_chat(
    app_state: tauri::State<'_, Arc<AppState>>,
    task_run_id: String,
    description: Option<String>,
    include_ui_bridge: Option<bool>,
) -> Result<CommandResponse, String> {
    info!("generate_workflow_from_chat: task_run_id={}", task_run_id);

    // Get conversation from DB output_log
    let db = app_state.checkpoint_db.clone();
    let id_clone = task_run_id.clone();
    let output_log =
        match tokio::task::spawn_blocking(move || db.get_task_run_output(&id_clone)).await {
            Ok(Ok(Some(log))) => log,
            Ok(Ok(None)) => {
                return Ok(CommandResponse {
                    success: false,
                    message: Some("No conversation history available".to_string()),
                    data: None,
                });
            }
            _ => {
                return Ok(CommandResponse {
                    success: false,
                    message: Some("Failed to read conversation".to_string()),
                    data: None,
                });
            }
        };

    // Build generation request with conversation as inline context
    let request = crate::workflow_generation::GenerateWorkflowRequest {
        description: description
            .unwrap_or_else(|| "Generate workflow from chat conversation".to_string()),
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
    };

    // Get doctor handle for health monitoring
    let doctor_handle = app_state.doctor_handle.lock().await.clone();
    let db2 = app_state.checkpoint_db.clone();

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
        Ok(response) => Ok(CommandResponse {
            success: response.success,
            message: response.error.clone(),
            data: Some(serde_json::json!({
                "task_run_id": task_run_id,
                "success": response.success,
                "workflow": response.workflow,
                "error": response.error,
                "validation_errors": response.validation_errors,
                "model_used": response.model_used,
            })),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(format!("Generation task failed: {}", e)),
            data: None,
        }),
    }
}
