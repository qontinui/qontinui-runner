//! HTTP surface for session lifecycle from agents (Coordinator,
//! `/auto-review`, `/summarize-session`).
//!
//! Productivity-stack §4 specifies four endpoints under
//! `mcp::sessions::routes()`:
//!
//! - `POST /sessions/spawn` — like `mcp::task_runs::create_ai_session` but
//!   accepts an optional `role` field. Recognised roles dispatch the
//!   matching slash-command body as the new session's first user message,
//!   so the Coordinator can spawn an `auto-review` worker, a fresh
//!   `coordinate` instance, etc., without a parallel HTTP route per role.
//! - `POST /sessions/<id>/message` — HTTP wrapper around the
//!   `send_user_message` Tauri command at
//!   `commands::ai_session::send_user_message`.
//! - `GET /sessions/<id>/touched-files` — wraps
//!   `pg::session_touched_files::get_files_touched`.
//! - `GET /sessions/<id>/transcript` — returns the session's stored
//!   transcript JSON. Reuses the workspace-scanning logic from
//!   `commands::transcript`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use tracing::{info, warn};

use crate::database::CreateTaskRunInput;
use crate::mcp::shared::AiSessionContext;
use crate::mcp::types::ApiState;
use crate::terminal::transcript;

// =============================================================================
// /sessions/spawn
// =============================================================================

/// Slash-command bodies the runner can dispatch automatically. Naming
/// matches the `.claude/commands/<role>.md` filenames so `/coordinate`
/// and friends can route by role string without a translation map.
fn role_slash_command(role: &str) -> Option<&'static str> {
    match role {
        "auto-review" => Some("/auto-review"),
        "coordinate" => Some("/coordinate"),
        "decompose-plan" => Some("/decompose-plan"),
        "summarize-session" => Some("/summarize-session"),
        "implement-plan" => Some("/implement-plan"),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnSessionRequest {
    /// Display name for the new session's tab.
    #[serde(default = "default_session_name")]
    pub task_name: String,
    /// Optional role discriminator. When `Some(role)` and the role is in
    /// the allowlist, the session's first user message is the matching
    /// `/<role>` slash command followed by `args` (if any).
    #[serde(default)]
    pub role: Option<String>,
    /// Optional argument string appended to the slash command. Only used
    /// when `role` is set.
    #[serde(default)]
    pub args: Option<String>,
}

fn default_session_name() -> String {
    "Coordinator-spawned session".to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnSessionResponse {
    pub task_run_id: String,
    pub task_name: String,
    pub state: String,
    pub role: Option<String>,
    /// True when the role was recognised and the slash command was
    /// dispatched as the initial prompt; false for plain ad-hoc sessions.
    pub dispatched_slash_command: bool,
}

async fn spawn_session(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SpawnSessionRequest>,
) -> Result<Json<SpawnSessionResponse>, (StatusCode, String)> {
    // Validate role early so a typo doesn't silently fall back to a plain
    // session — the agent likely meant a specific role.
    let slash_command = match req.role.as_deref() {
        Some(role) if !role.is_empty() => match role_slash_command(role) {
            Some(cmd) => Some(cmd),
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "Unknown role '{}'. Allowed: auto-review, coordinate, \
                         decompose-plan, summarize-session, implement-plan",
                        role
                    ),
                ));
            }
        },
        _ => None,
    };

    let task_run_id = uuid::Uuid::new_v4().to_string();

    let input = CreateTaskRunInput::new(&task_run_id, &req.task_name)
        .with_prompt("Coordinator-spawned session")
        .with_workflow_type("chat");
    state
        .app_state
        .pg_db
        .create_task_run(&input)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    let working_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Build the initial prompt. For role-driven spawns the prompt IS the
    // slash command line — Claude Code resolves `/coordinate` against the
    // .claude/commands/coordinate.md body at session start. For plain
    // spawns we fall back to a generic system prompt to match the existing
    // ad-hoc create_ai_session behaviour.
    let initial_prompt = match (slash_command, req.args.as_deref()) {
        (Some(cmd), Some(args)) if !args.trim().is_empty() => format!("{} {}", cmd, args.trim()),
        (Some(cmd), _) => cmd.to_string(),
        (None, _) => "You are an AI assistant in a session initiated from the Coordinator. \
                      Respond helpfully and conversationally."
            .to_string(),
    };

    let session_ctx = AiSessionContext::setup(&task_run_id, &req.task_name);

    let dispatched = slash_command.is_some();
    let role_for_response = req.role.clone();

    // Wrap the spawn+register+initial-prompt sequence in spawn_blocking so the
    // CLI init handshake can't race SessionManager::register — otherwise output
    // events emitted during the handshake land before the session is reachable
    // by id and the chunk writer drops them.
    let sm = session_manager.clone();
    let handle = state.app_handle.clone();
    let trid = task_run_id.clone();
    let working_dir_for_closure = working_dir.clone();
    let initial_prompt_for_closure = initial_prompt.clone();
    let spawn_result = tokio::task::spawn_blocking(move || {
        let session = match crate::claude_session::ClaudeSession::spawn(
            &working_dir_for_closure,
            &trid,
            &handle,
            Some(session_ctx),
            None,
            None,
            None,
            None,
            None,
        ) {
            Ok(s) => Arc::new(s),
            Err(e) => return Err(format!("spawn failed: {}", e)),
        };

        if let Err(e) = sm.register(&trid, session.clone()) {
            return Err(format!("register failed: {}", e));
        }

        crate::commands::ai_session::emit_session_state(&handle, &trid, &trid, session.state());

        if let Err(e) = session.send_initial_prompt(&initial_prompt_for_closure) {
            return Err(format!("initial prompt failed: {}", e));
        }

        crate::commands::ai_session::emit_session_state(&handle, &trid, &trid, session.state());

        Ok(())
    })
    .await;

    match spawn_result {
        Ok(Ok(())) => {
            info!(
                "Spawned session task_run_id={} role={:?} dispatched={}",
                task_run_id, req.role, dispatched
            );
            Ok(Json(SpawnSessionResponse {
                task_run_id,
                task_name: req.task_name,
                state: "ready".to_string(),
                role: role_for_response,
                dispatched_slash_command: dispatched,
            }))
        }
        Ok(Err(e)) => {
            warn!(
                "Failed to spawn role={:?} session {}: {}",
                req.role, task_run_id, e
            );
            Ok(Json(SpawnSessionResponse {
                task_run_id,
                task_name: req.task_name,
                state: "error".to_string(),
                role: role_for_response,
                dispatched_slash_command: false,
            }))
        }
        Err(join_err) => {
            warn!(
                "spawn_blocking join error for session {}: {}",
                task_run_id, join_err
            );
            Ok(Json(SpawnSessionResponse {
                task_run_id,
                task_name: req.task_name,
                state: "error".to_string(),
                role: role_for_response,
                dispatched_slash_command: false,
            }))
        }
    }
}

// =============================================================================
// /sessions/<id>/message
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub task_run_id: String,
    pub queued: bool,
}

async fn send_message(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, (StatusCode, String)> {
    if req.message.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "message must not be empty".to_string(),
        ));
    }

    let session_manager: Arc<crate::claude_session::SessionManager> = state
        .app_handle
        .state::<Arc<crate::claude_session::SessionManager>>()
        .inner()
        .clone();

    let session = session_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("No active session found for task_run_id: {}", id),
        )
    })?;

    // ClaudeSession::send_user_message returns true when the message went
    // out immediately, false when the worker was Processing and the
    // message was queued. Surface that as `queued` so callers can match
    // the Tauri command's behaviour.
    let sent_immediately = session.send_user_message(&req.message).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("send_user_message failed: {}", e),
        )
    })?;

    Ok(Json(SendMessageResponse {
        task_run_id: id,
        queued: !sent_immediately,
    }))
}

// =============================================================================
// /sessions/<id>/touched-files
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchedFilesResponse {
    pub task_run_id: String,
    pub files: Vec<String>,
}

async fn get_touched_files(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<TouchedFilesResponse>, (StatusCode, String)> {
    let files = state
        .app_state
        .pg_db
        .get_files_touched(&id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(TouchedFilesResponse {
        task_run_id: id,
        files,
    }))
}

// =============================================================================
// /sessions/<id>/transcript
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResponse {
    pub task_run_id: String,
    pub messages: Vec<serde_json::Value>,
}

async fn get_transcript(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<TranscriptResponse>, (StatusCode, String)> {
    // The runner's task_run_id is not necessarily the Claude
    // session_id used by the on-disk JSONL transcripts. The DB stores
    // the worker's persisted output_log by task_run_id, which is the
    // canonical "what the agent did" record an agentic reviewer needs.
    let output = state
        .app_state
        .pg_db
        .get_task_output(&id)
        .await
        .unwrap_or_default();

    if !output.trim().is_empty() {
        // Wrap the output_log in a single-message envelope so the response
        // shape is uniform across the DB-backed and JSONL-backed paths.
        let messages = vec![serde_json::json!({
            "role": "transcript",
            "content": output,
        })];
        return Ok(Json(TranscriptResponse {
            task_run_id: id,
            messages,
        }));
    }

    // Fall back to scanning the on-disk Claude Code config dirs by
    // session_id. This is the same code path
    // `commands::transcript::transcript_read_session` uses; we re-run it
    // for HTTP callers.
    let project = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_default();

    let config_dirs = transcript::find_claude_config_dirs();
    for dir in &config_dirs {
        if let Ok(messages) = transcript::read_session(dir, &project, &id) {
            let json_messages: Vec<serde_json::Value> = messages
                .into_iter()
                .filter_map(|m| serde_json::to_value(&m).ok())
                .collect();
            return Ok(Json(TranscriptResponse {
                task_run_id: id,
                messages: json_messages,
            }));
        }
    }

    Ok(Json(TranscriptResponse {
        task_run_id: id,
        messages: Vec::new(),
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/sessions/spawn", post(spawn_session))
        .route("/sessions/{id}/message", post(send_message))
        .route("/sessions/{id}/touched-files", get(get_touched_files))
        .route("/sessions/{id}/transcript", get(get_transcript))
}
