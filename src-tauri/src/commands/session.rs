//! Tauri commands for the unified [`crate::session::Session`] primitive.
//!
//! Plan §Phase 2 / §Phase 4 — these are the canonical Tauri commands the
//! runner frontend will call once Phase 4 cuts over from the legacy
//! `terminal_*` / `ai_session_*` surface. Phase 2 ships them alongside
//! the legacy commands (the old ones stay registered until Phase 4 lands)
//! so the runner doesn't break between PRs.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tracing::error;
use uuid::Uuid;

use crate::commands::CommandResponse;
use crate::session::{description_to_json, Intent, SessionError, SessionKind, SessionRegistry};

/// JSON shape accepted by `session_start`. Mirrors [`Intent`] verbatim;
/// re-declared so the Tauri-side deserialization rejects unknown fields
/// without leaning on `Intent`'s `serde(default)` flexibility.
#[derive(Debug, Clone, Deserialize)]
pub struct StartSessionArgs {
    pub kind: SessionKind,
    pub purpose: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub declared_paths: Vec<PathBuf>,
    #[serde(default)]
    pub share_output: bool,
    #[serde(default)]
    pub redact_secrets: Option<bool>,
}

impl From<StartSessionArgs> for Intent {
    fn from(a: StartSessionArgs) -> Self {
        Intent {
            kind: a.kind,
            purpose: a.purpose,
            repo: a.repo,
            branch: a.branch,
            declared_paths: a.declared_paths,
            share_output: a.share_output,
            redact_secrets: a.redact_secrets,
        }
    }
}

/// Shape returned by `session_start` / `session_describe`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionStartResponse {
    pub id: Uuid,
}

fn map_err(e: SessionError) -> String {
    // Surface structured codes so the frontend can branch without parsing
    // text. The plain string form remains the Display impl.
    match &e {
        SessionError::Intent(_) => format!("session:intent_invalid: {}", e),
        SessionError::Transport(_) => format!("session:transport_error: {}", e),
        SessionError::StealReasonTooShort { .. } => format!("session:reason_too_short: {}", e),
        SessionError::NotFound(_) => format!("session:not_found: {}", e),
        SessionError::Outbox(_) => format!("session:outbox_error: {}", e),
    }
}

/// Start a new session. Returns its UUID; the caller can then drive
/// follow-up calls (focus/close/describe/steal) by id.
#[tauri::command]
pub fn session_start(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
    args: StartSessionArgs,
) -> Result<CommandResponse, String> {
    let intent: Intent = args.into();
    match registry.inner().start(intent) {
        Ok(handle) => {
            let id = handle.id();
            Ok(CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::json!({ "id": id })),
            })
        }
        Err(e) => {
            error!("session_start failed: {}", e);
            Err(map_err(e))
        }
    }
}

/// "Focus" the session — heartbeats `last_heartbeat_at` and records a
/// `state_change` event in the outbox so the dashboard knows the
/// operator is on this session right now.
#[tauri::command]
pub fn session_focus(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
    session_id: Uuid,
) -> Result<CommandResponse, String> {
    registry.inner().focus_by_id(session_id).map_err(map_err)?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Close the session. Idempotent — closing an already-closed session
/// returns success.
#[tauri::command]
pub fn session_close(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
    session_id: Uuid,
) -> Result<CommandResponse, String> {
    registry.inner().close_by_id(session_id).map_err(map_err)?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Return the [`crate::session::SessionDescription`] for the given session.
#[tauri::command]
pub fn session_describe(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
    session_id: Uuid,
) -> Result<CommandResponse, String> {
    let desc = registry
        .inner()
        .describe_by_id(session_id)
        .map_err(map_err)?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(description_to_json(&desc)),
    })
}

/// List every live session. Convenience for the runner frontend's session
/// panel; coord is the canonical source of truth across machines.
#[tauri::command]
pub fn session_list(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
) -> Result<CommandResponse, String> {
    let all = registry.inner().snapshot();
    let json = serde_json::to_value(&all).map_err(|e| e.to_string())?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "sessions": json })),
    })
}

/// Plan §D14 — steal a session's claim with a typed reason. Min 10
/// chars enforced by [`crate::session::MIN_STEAL_REASON_CHARS`].
#[tauri::command]
pub fn session_steal(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
    session_id: Uuid,
    reason: String,
) -> Result<CommandResponse, String> {
    registry
        .inner()
        .steal_by_id(session_id, &reason)
        .map_err(map_err)?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Tauri plugin bundle. main.rs adds one `.plugin(commands::session::plugin())`
/// to wire all five commands.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_session")
        .invoke_handler(tauri::generate_handler![
            session_start,
            session_focus,
            session_close,
            session_describe,
            session_list,
            session_steal,
        ])
        .build()
}
