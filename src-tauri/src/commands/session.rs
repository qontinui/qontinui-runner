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
    /// The work unit this session is working on. A plain `#[serde(alias)]`
    /// here, unlike [`Intent`]'s two real fields (see `session/intent.rs`).
    ///
    /// The alias hazard is not "is this struct serialized" — it is "can a body
    /// carrying BOTH spellings arrive", since serde treats an alias and the
    /// canonical name as the same field and rejects such a body with
    /// `duplicate field`. It cannot arrive here: `session_start`'s only caller
    /// is this runner's own frontend (`src/contexts/SessionContext.tsx`, whose
    /// TS `Intent` declares no slug field at all), and the command is exposed
    /// by neither `mcp::tauri_proxy`'s allow-list nor `mcp::backend_relay`'s
    /// dispatch table. If a door is ever opened onto `session_start` from
    /// coord — which dual-emits — this alias must become two real fields.
    #[serde(default, alias = "plan_slug")]
    pub work_unit_slug: Option<String>,
    #[serde(default)]
    pub correlation_topic: Option<String>,
    #[serde(default)]
    pub declared_paths: Vec<PathBuf>,
    #[serde(default)]
    pub share_output: bool,
    #[serde(default)]
    pub redact_secrets: Option<bool>,
    /// Phase 8b — explicit tenant binding for this session (the spawn
    /// input). Omitted → the registry stamps the device default
    /// (`machine.json::active_tenant_id`, default-for-new-sessions).
    #[serde(default)]
    pub tenant_id: Option<Uuid>,
}

impl From<StartSessionArgs> for Intent {
    fn from(a: StartSessionArgs) -> Self {
        Intent {
            kind: a.kind,
            purpose: a.purpose,
            repo: a.repo,
            branch: a.branch,
            work_unit_slug: a.work_unit_slug,
            // Accept-only on `Intent`; the runner never emits the legacy key.
            plan_slug: None,
            correlation_topic: a.correlation_topic,
            // Not exposed on `session_start` — page placement is only
            // meaningful on the gate-continuation terminal create path.
            page_id: None,
            declared_paths: a.declared_paths,
            share_output: a.share_output,
            redact_secrets: a.redact_secrets,
            tenant_id: a.tenant_id,
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

/// Plan §Phase 7 — "Continue elsewhere". Publish a handoff request that
/// moves `session_id` to `target_device_id`. The target runner's
/// receiver loop materializes the child session and closes this one.
///
/// This is the runner-initiated trigger; the dashboard's "Continue
/// elsewhere" button reaches the same coord endpoint through the web
/// backend proxy. Async because it POSTs to coord.
#[tauri::command]
pub async fn session_handoff(
    registry: tauri::State<'_, Arc<SessionRegistry>>,
    session_id: Uuid,
    target_device_id: Uuid,
) -> Result<CommandResponse, String> {
    let (http, coord_url) = {
        let cs = registry.inner().coord_sync();
        (cs.http_client(), cs.coord_url().to_string())
    };
    crate::session::handoff::trigger_handoff(&http, &coord_url, session_id, target_device_id)
        .await
        .map_err(|e| format!("session:handoff_failed: {e}"))?;
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
            session_handoff,
        ])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `session_start` accept side, wire-key retirement (plan
    /// `2026-07-30-coord-web-plan-slug-wire-key-retirement` Phase 1 step 6).
    /// A body from a pre-retirement caller carries the deprecated spelling;
    /// the alias must still bind it. Without this, dropping the alias would
    /// fail silently — `#[serde(default)]` yields `None` and the session is
    /// simply recorded with no work unit.
    #[test]
    fn start_session_args_accepts_the_legacy_plan_slug_key() {
        let args: StartSessionArgs = serde_json::from_value(serde_json::json!({
            "kind": "terminal_shell",
            "purpose": "fix the thing",
            "plan_slug": "2026-07-28-some-unit",
        }))
        .unwrap();
        assert_eq!(args.work_unit_slug.as_deref(), Some("2026-07-28-some-unit"));
    }

    #[test]
    fn start_session_args_accepts_the_canonical_key() {
        let args: StartSessionArgs = serde_json::from_value(serde_json::json!({
            "kind": "terminal_shell",
            "purpose": "fix the thing",
            "work_unit_slug": "2026-07-28-some-unit",
        }))
        .unwrap();
        assert_eq!(args.work_unit_slug.as_deref(), Some("2026-07-28-some-unit"));
    }

    /// The EMIT side, through a real production constructor. Whichever
    /// spelling came in, the `Intent` this command builds must serialize the
    /// canonical key ALONE — re-emitting `plan_slug` would re-seed the very
    /// key this plan retires, and coord's accept side already binds the new
    /// one.
    #[test]
    fn start_session_intent_emits_only_the_canonical_key() {
        for incoming in ["plan_slug", "work_unit_slug"] {
            let mut body = serde_json::json!({
                "kind": "terminal_shell",
                "purpose": "fix the thing",
            });
            body.as_object_mut().unwrap().insert(
                incoming.to_string(),
                serde_json::json!("2026-07-28-some-unit"),
            );
            let args: StartSessionArgs = serde_json::from_value(body).unwrap();
            let intent = Intent::from(args);
            assert_eq!(
                intent.work_unit_slug.as_deref(),
                Some("2026-07-28-some-unit"),
                "incoming key {incoming} must land on the canonical field"
            );
            assert_eq!(
                intent.plan_slug, None,
                "the deprecated field is accept-only on `Intent`"
            );
            let json = serde_json::to_value(&intent).unwrap();
            assert_eq!(
                json.get("work_unit_slug").and_then(|v| v.as_str()),
                Some("2026-07-28-some-unit")
            );
            assert!(
                json.get("plan_slug").is_none(),
                "incoming key {incoming} produced a legacy key on the wire: {json}"
            );
        }
    }

    /// A caller that sends no slug still produces a slug-less intent — the
    /// alias must not manufacture one.
    #[test]
    fn start_session_args_without_a_slug_stays_none() {
        let args: StartSessionArgs = serde_json::from_value(serde_json::json!({
            "kind": "terminal_shell",
            "purpose": "fix the thing",
        }))
        .unwrap();
        assert_eq!(args.work_unit_slug, None);
        let json = serde_json::to_value(Intent::from(args)).unwrap();
        assert!(json.get("work_unit_slug").is_none(), "{json}");
        assert!(json.get("plan_slug").is_none(), "{json}");
    }
}
