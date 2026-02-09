//! Tauri commands for interactive AI chat sessions.
//!
//! These commands allow the frontend to send messages to active Claude CLI sessions,
//! interrupt processing, and query session state.

use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;
use tracing::{info, warn};

use crate::claude_session::manager::SessionManager;
use crate::claude_session::state::SessionState;
use crate::commands::CommandResponse;
use crate::database::CheckpointDb;
use crate::mcp::shared::emit_ai_output;

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

    // Emit the user's message as an ai-output event so it appears in the conversation
    let session_ctx = None; // User messages don't have session context
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_ai_output(&app_handle, &message, "user_message", None, session_ctx);
    }));

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
