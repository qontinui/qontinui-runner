//! The Terminal page's structured session: an operator-launched stream-json
//! session that ASKS before each tool call, and the commands that answer it
//! (plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
//! Phase 9).
//!
//! A structured session is the same in-process `ClaudeSession` a Conductor
//! worker is — opened through the chat path ([`open_ai_session`]) and recorded
//! as a `lane: structured` lifecycle row carrying its task run id, so the grid
//! renders it through `StructuredSessionCell` exactly as it renders a worker.
//! What differs is the posture: [`PermissionMode::Prompt`], so each
//! `can_use_tool` request reaches the operator as a permission card instead of
//! being allowed at launch.
//!
//! **Interactive surfaces only.** `create_structured_session` is a Tauri
//! command — the webview's launch menu is its only caller — and no HTTP route,
//! relay arm or UI Bridge invoke entry reaches it, because a prompted session
//! with nobody watching stalls each tool call until the runner's fail-closed
//! deny (plan Risks, "A permission prompt nobody is watching"). The task run is
//! recorded as `workflow_type = "structured_session"`, NOT `"chat"`: boot
//! resume re-spawns only `"chat"` rows, and it re-spawns them in bypass.

use std::sync::Arc;

use qontinui_types::cli_session::{CapabilityState, CliProfile, StructuredLane};
use tauri::State;

use crate::claude_session::permission::{PermissionDecision, PermissionRequestNotice};
use crate::claude_session::session::PermissionResponseOutcome;
use crate::claude_session::SessionManager;
use crate::commands::ai_session::{open_ai_session, AiSessionLaunch};
use crate::commands::compartments::StorageCompartment;
use crate::commands::CommandResponse;
use crate::session::launch_spec::PermissionMode;
use crate::session::session_lifecycle_store::{
    SessionLane, SessionLifecycleStore, TerminalSessionRecord, ORIGIN_AUTHORITATIVE,
};

/// The task run `workflow_type` of a structured session. Deliberately not
/// `"chat"` — see the module docs.
pub const STRUCTURED_SESSION_WORKFLOW_TYPE: &str = "structured_session";

/// The profile a structured launch of `provider` runs under, or why there is
/// none. Offered only for a lane the RUNNER implements (Claude stream-json —
/// Codex's `app-server` is a follow-up), whose typed permission requests are
/// verified, with known prompt-routing arguments. Pure.
pub fn structured_launch_profile(provider: &str) -> Result<&'static CliProfile, String> {
    let profile = qontinui_runner_lib::cli_profile::profile_for(provider)
        .ok_or_else(|| format!("unknown provider {provider:?}"))?;
    if profile.structured_lane != StructuredLane::ClaudeStreamJson {
        return Err(format!(
            "{} has no structured lane this runner implements (its profile says {:?})",
            profile.display_name, profile.structured_lane
        ));
    }
    if profile.typed_permission != CapabilityState::Supported {
        return Err(format!(
            "{}'s typed permission requests are not verified ({:?})",
            profile.display_name, profile.typed_permission
        ));
    }
    if profile.permission_prompt_args.is_empty() {
        return Err(format!(
            "{}'s profile names no argument that routes permission requests to the runner",
            profile.display_name
        ));
    }
    Ok(profile)
}

/// The lifecycle row a structured session is recorded under: keyed by its task
/// run id (also the pinned CLI session id and the SessionManager key), on the
/// launching page and zone, `lane: structured`.
fn structured_record(
    task_run_id: &str,
    provider: &str,
    page_id: Option<String>,
    zone_index: i32,
    title: Option<String>,
    working_dir: Option<String>,
) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: task_run_id.to_string(),
        config_dir: None,
        working_dir,
        page_id: page_id
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
        zone_index,
        title,
        terminal_id: task_run_id.to_string(),
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: provider.to_string(),
        lane: SessionLane::Structured,
        // Authoritative: the CLI session id is pinned to the task run id
        // (`--session-id`) by the chat path.
        origin: Some(ORIGIN_AUTHORITATIVE.to_string()),
        restore_pending_at: None,
        confirmed_at: None,
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
        name_source: None,
        tenant_id: None,
        // The id the grid's structured cell attaches to.
        task_run_id: Some(task_run_id.to_string()),
        // Safety-relevant and known: this session asks before each tool.
        bypass_permissions: Some(false),
        restored_from_boot_at: None,
        restore_tier: None,
        finished_at: None,
        wind_down_outcome: None,
        wind_down_at: None,
        finish_reason: None,
        finish_synced: false,
        spawn_device_default: None,
    }
}

/// Launch a structured session of `provider` from the Terminal page, in
/// [`PermissionMode::Prompt`], and record it on `page_id`/`zone_index`. The
/// response's `record` is the lifecycle row (camelCase, the shape
/// `terminal_session_list_open` returns) the grid adopts as a structured tab.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn create_structured_session(
    app_handle: tauri::AppHandle,
    session_manager: State<'_, Arc<SessionManager>>,
    ai_coord_registrar: State<'_, Arc<crate::claude_session::coord_register::AiCoordRegistrar>>,
    app_state: State<'_, StorageCompartment>,
    lifecycle: State<'_, Arc<SessionLifecycleStore>>,
    provider: String,
    page_id: Option<String>,
    zone_index: i32,
    working_dir: Option<String>,
    title: Option<String>,
) -> Result<CommandResponse, String> {
    let profile = match structured_launch_profile(&provider) {
        Ok(p) => p,
        Err(reason) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("No structured session for {provider}: {reason}")),
                data: None,
            })
        }
    };
    let working_dir = working_dir.filter(|d| !d.trim().is_empty());
    if let Some(dir) = &working_dir {
        if !std::path::Path::new(dir).is_dir() {
            return Ok(CommandResponse {
                success: false,
                message: Some(format!("Working directory {dir:?} is not a directory")),
                data: None,
            });
        }
    }
    let title = title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("{} (structured)", profile.display_name));

    let opened = open_ai_session(
        app_handle,
        &session_manager,
        &ai_coord_registrar,
        &app_state,
        Some(title.clone()),
        AiSessionLaunch {
            permission: PermissionMode::Prompt,
            working_dir: working_dir.clone(),
            workflow_type: STRUCTURED_SESSION_WORKFLOW_TYPE,
        },
    )
    .await?;
    if !opened.success {
        return Ok(opened);
    }
    let Some(task_run_id) = opened
        .data
        .as_ref()
        .and_then(|d| d.get("task_run_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return Ok(CommandResponse {
            success: false,
            message: Some("The session opened but reported no task_run_id".to_string()),
            data: None,
        });
    };

    lifecycle.record_open(structured_record(
        &task_run_id,
        &profile.id,
        page_id,
        zone_index,
        Some(title),
        working_dir,
    ));
    // Read the row back: the response must be what the store holds, not what
    // this command meant to write.
    let Some(record) = lifecycle.get(&task_run_id) else {
        // No record ⇒ no grid cell ⇒ nobody could answer its permission
        // requests. A prompting session nobody can see is closed, not left
        // to stall every tool call until its deadline.
        if let Some(session) = session_manager.remove(&task_run_id) {
            let _ = session.close();
        }
        return Ok(CommandResponse {
            success: false,
            message: Some(format!(
                "Structured session {task_run_id} started, but its lifecycle record could not be read back, so it was closed"
            )),
            data: Some(serde_json::json!({ "task_run_id": task_run_id })),
        });
    };
    Ok(CommandResponse {
        success: true,
        message: Some("Structured session ready".to_string()),
        data: Some(serde_json::json!({
            "task_run_id": task_run_id,
            "record": record,
        })),
    })
}

/// The decision a `respond_session_permission` call names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionChoice {
    Allow,
    Deny,
}

/// Build the broker's decision from the command's arguments. Pure.
pub fn decision_from(
    decision: PermissionChoice,
    message: Option<String>,
    interrupt: Option<bool>,
    updated_input: Option<serde_json::Value>,
) -> Result<PermissionDecision, String> {
    match decision {
        PermissionChoice::Allow => {
            if interrupt == Some(true) {
                return Err("interrupt applies to a deny only".to_string());
            }
            Ok(PermissionDecision::Allow { updated_input })
        }
        PermissionChoice::Deny => {
            if updated_input.is_some() {
                return Err("updatedInput applies to an allow only".to_string());
            }
            Ok(PermissionDecision::Deny {
                message,
                interrupt: interrupt.unwrap_or(false),
            })
        }
    }
}

/// Answer the parked permission request `request_id` of session `session_id`
/// (its SessionManager key / task run id). `allow` runs the tool with its own
/// input, or with `updated_input` when the operator edited it; `deny` sends
/// `message` (a default when absent) to the model, and with `interrupt` then
/// interrupts the turn.
///
/// Async, with the answer written on the blocking pool: it is a stdin pipe
/// write (plus an interrupt for "Deny & interrupt"), which must not run on the
/// main thread — the same posture as `send_user_message`.
#[tauri::command]
pub async fn respond_session_permission(
    session_manager: State<'_, Arc<SessionManager>>,
    session_id: String,
    request_id: String,
    decision: PermissionChoice,
    message: Option<String>,
    interrupt: Option<bool>,
    updated_input: Option<serde_json::Value>,
) -> Result<PermissionResponseOutcome, String> {
    let decision = decision_from(decision, message, interrupt, updated_input)?;
    let session = session_manager
        .get(&session_id)
        .ok_or_else(|| format!("no live session {session_id}"))?;
    tokio::task::spawn_blocking(move || session.respond_permission(&request_id, decision))
        .await
        .map_err(|e| format!("permission answer task failed: {e}"))?
}

/// The permission requests session `session_id` is waiting on, oldest first —
/// read at mount, since the `session-permission-request` event only carries
/// requests raised after a listener subscribed. An error (not an empty list)
/// when the session is not live, so absence never reads as "nothing pending".
#[tauri::command]
pub fn session_pending_permissions(
    session_manager: State<'_, Arc<SessionManager>>,
    session_id: String,
) -> Result<Vec<PermissionRequestNotice>, String> {
    session_manager
        .get(&session_id)
        .map(|s| s.pending_permissions())
        .ok_or_else(|| format!("no live session {session_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_claude_stream_json_gets_a_structured_launch() {
        let claude = structured_launch_profile("claude").expect("claude");
        assert_eq!(
            claude.permission_prompt_args,
            vec!["--permission-mode", "default", "--permission-prompt-tool", "stdio"]
        );
        let codex = structured_launch_profile("codex").unwrap_err();
        assert!(codex.contains("no structured lane this runner implements"), "{codex}");
        assert!(structured_launch_profile("gemini").unwrap_err().contains("unknown provider"));
    }

    #[test]
    fn the_record_is_a_structured_non_bypass_row_keyed_by_the_task_run() {
        let r = structured_record("trid-1", "claude", Some("page-7".into()), 2, Some("t".into()), None);
        assert_eq!(r.claude_session_id, "trid-1");
        assert_eq!(r.terminal_id, "trid-1");
        assert_eq!(r.task_run_id.as_deref(), Some("trid-1"));
        assert_eq!(r.lane, SessionLane::Structured);
        assert_eq!(r.bypass_permissions, Some(false));
        assert_eq!(r.page_id, "page-7");
        assert_eq!(r.zone_index, 2);
        let wire = serde_json::to_value(&r).unwrap();
        assert_eq!(wire["lane"], "structured");
        assert_eq!(wire["taskRunId"], "trid-1");
        // No page ⇒ the default page, never "".
        assert_eq!(structured_record("x", "claude", Some(" ".into()), 0, None, None).page_id, "default");
    }

    #[test]
    fn decisions_are_validated() {
        assert_eq!(
            decision_from(PermissionChoice::Allow, None, None, None).unwrap(),
            PermissionDecision::Allow { updated_input: None }
        );
        assert_eq!(
            decision_from(PermissionChoice::Deny, Some("no".into()), Some(true), None).unwrap(),
            PermissionDecision::Deny {
                message: Some("no".into()),
                interrupt: true
            }
        );
        assert!(decision_from(PermissionChoice::Allow, None, Some(true), None).is_err());
        assert!(decision_from(PermissionChoice::Deny, None, None, Some(serde_json::json!({}))).is_err());
        let wire: PermissionChoice = serde_json::from_str("\"deny\"").unwrap();
        assert_eq!(wire, PermissionChoice::Deny);
    }

    #[test]
    fn a_structured_session_is_never_boot_resumed_as_a_chat() {
        assert_ne!(STRUCTURED_SESSION_WORKFLOW_TYPE, AiSessionLaunch::chat().workflow_type);
        assert_eq!(AiSessionLaunch::chat().permission, PermissionMode::BypassPermissions);
    }
}
