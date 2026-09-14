//! Operator twins of the runner's HTTP spawn doors (plan
//! `2026-09-13-drained-runner-never-reaches-idle`, review round 1, D3).
//!
//! The runner UI starts prompts, workflows and task resumes by calling the
//! runner's own HTTP routes. Those routes cannot tell the UI from a script or a
//! UI-Bridge-driven automation — and an HTTP header saying "operator" would be
//! spoofable by anything that can reach the port — so under coord's device
//! drain every HTTP caller is autonomous (`unknown`) and is refused with 409.
//!
//! These Tauri commands are the same doors called in-process with an OPERATOR
//! origin. Tauri commands are reachable only from this runner's own webview:
//! they are NOT on the UI Bridge invoke allowlist (`ui_bridge_invoke.rs`) and
//! not on the `/tauri/invoke` proxy, so an automation cannot borrow them. Each
//! returns the door's HTTP status and JSON body unchanged, so the frontend
//! handles a twin's answer exactly as it handled the route's.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use tauri::Manager;

use crate::coord_drain_state::SpawnOrigin;
use crate::mcp::types::ApiState;

/// The origin every twin passes: an operator at this runner's own UI. The drain
/// never defers it; the draining banner tells the operator what is paused.
const OPERATOR: SpawnOrigin = SpawnOrigin::OperatorChat;

/// A door's answer: its HTTP status and JSON body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DoorReply {
    pub status: u16,
    pub body: serde_json::Value,
}

fn to_json<T: Serialize>(value: T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|e| {
        serde_json::json!({ "success": false, "error": format!("response did not serialize: {e}") })
    })
}

/// A door that answers `Json` on both arms.
pub fn door_reply<T: Serialize, E: Serialize>(
    result: Result<Json<T>, (StatusCode, Json<E>)>,
) -> DoorReply {
    match result {
        Ok(Json(body)) => DoorReply {
            status: StatusCode::OK.as_u16(),
            body: to_json(body),
        },
        Err((status, Json(body))) => DoorReply {
            status: status.as_u16(),
            body: to_json(body),
        },
    }
}

/// A door whose error arm is a bare string: rendered in the `ApiResponse`
/// error shape the frontend reads.
pub fn door_reply_text<T: Serialize>(result: Result<Json<T>, (StatusCode, String)>) -> DoorReply {
    match result {
        Ok(Json(body)) => DoorReply {
            status: StatusCode::OK.as_u16(),
            body: to_json(body),
        },
        Err((status, message)) => DoorReply {
            status: status.as_u16(),
            body: serde_json::json!({ "success": false, "error": message }),
        },
    }
}

fn api_state(app_handle: &tauri::AppHandle) -> Result<Arc<ApiState>, String> {
    app_handle
        .try_state::<Arc<ApiState>>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| "the runner API is not up yet — try again in a moment".to_string())
}

/// Call a spawn door in-process with `origin`, from a path and JSON body — for
/// the scheduler's operator "Run now", whose launchers address doors by path.
pub async fn call_door_in_process(
    path: &str,
    body: serde_json::Value,
    origin: SpawnOrigin,
) -> Result<DoorReply, String> {
    let app_handle = crate::tauri_app_handle::current()
        .ok_or_else(|| "no Tauri app handle — cannot call the door in-process".to_string())?;
    let state = api_state(&app_handle)?;
    if path == "/prompts/run" {
        let request = serde_json::from_value(body).map_err(|e| format!("bad request: {e}"))?;
        return Ok(door_reply(
            crate::mcp::ai_session::run_prompt(state, request, origin).await,
        ));
    }
    if let Some(id) = unified_workflow_run_id(path) {
        let request = serde_json::from_value(body).map_err(|e| format!("bad request: {e}"))?;
        return Ok(door_reply(
            crate::mcp::unified_workflows::run_unified_workflow(
                state,
                id.to_string(),
                request,
                origin,
            )
            .await,
        ));
    }
    Err(format!("no in-process door for {path}"))
}

/// PURE: the workflow id in `/unified-workflows/{id}/run`, if `path` is one.
fn unified_workflow_run_id(path: &str) -> Option<&str> {
    path.strip_prefix("/unified-workflows/")
        .and_then(|rest| rest.strip_suffix("/run"))
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

/// `POST /prompts/run`, as the runner UI.
#[tauri::command]
pub async fn operator_run_prompt(
    app_handle: tauri::AppHandle,
    request: crate::mcp::ai_session::RunPromptRequest,
) -> Result<DoorReply, String> {
    let state = api_state(&app_handle)?;
    Ok(door_reply(
        crate::mcp::ai_session::run_prompt(state, request, OPERATOR).await,
    ))
}

/// `POST /unified-workflows/{id}/run`, as the runner UI.
#[tauri::command]
pub async fn operator_run_unified_workflow(
    app_handle: tauri::AppHandle,
    id: String,
    request: crate::mcp::unified_workflows::RunUnifiedWorkflowRequest,
) -> Result<DoorReply, String> {
    let state = api_state(&app_handle)?;
    Ok(door_reply(
        crate::mcp::unified_workflows::run_unified_workflow(state, id, request, OPERATOR).await,
    ))
}

/// `POST /unified-workflows/execute-inline`, as the runner UI.
#[tauri::command]
pub async fn operator_execute_inline_workflow(
    app_handle: tauri::AppHandle,
    request: crate::mcp::unified_workflows::ExecuteInlineWorkflowRequest,
) -> Result<DoorReply, String> {
    let state = api_state(&app_handle)?;
    Ok(door_reply(
        crate::mcp::unified_workflows::execute_inline_workflow(state, request, OPERATOR).await,
    ))
}

/// `POST /unified-workflows/run-composed`, as the runner UI.
#[tauri::command]
pub async fn operator_run_composed_workflow(
    app_handle: tauri::AppHandle,
    request: crate::mcp::unified_workflows::RunComposedWorkflowRequest,
) -> Result<DoorReply, String> {
    let state = api_state(&app_handle)?;
    Ok(door_reply(
        crate::mcp::unified_workflows::run_composed_workflow(state, request, OPERATOR).await,
    ))
}

/// `POST /unified-workflows/generate-async`, as the runner UI.
#[tauri::command]
pub async fn operator_generate_unified_workflow_async(
    app_handle: tauri::AppHandle,
    request: crate::workflow_generation::generator::GenerateWorkflowRequest,
) -> Result<DoorReply, String> {
    let state = api_state(&app_handle)?;
    Ok(door_reply(
        crate::mcp::unified_workflows::generate_unified_workflow_async(state, request, OPERATOR)
            .await,
    ))
}

/// `POST /task-runs/{id}/resume`, as the runner UI.
#[tauri::command]
pub async fn operator_resume_task_run(
    app_handle: tauri::AppHandle,
    id: String,
    request: crate::mcp::task_runs::ResumeTaskRunRequest,
) -> Result<DoorReply, String> {
    let state = api_state(&app_handle)?;
    Ok(door_reply_text(
        crate::mcp::task_runs::resume_task_run(state, id, request, OPERATOR).await,
    ))
}

/// The scheduler's "Run now", as the runner UI. `POST /scheduler/tasks/{id}/run`
/// stays an autonomous (`unknown`) caller.
#[tauri::command]
pub async fn scheduler_run_task_now(id: String) -> Result<(), String> {
    crate::scheduler_service::run_task_now(&id, OPERATOR).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_door_reply_keeps_the_status_and_body_on_both_arms() {
        let ok: Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> =
            Ok(Json(serde_json::json!({ "success": true })));
        assert_eq!(
            door_reply(ok),
            DoorReply {
                status: 200,
                body: serde_json::json!({ "success": true })
            }
        );
        let refused: Result<Json<serde_json::Value>, (StatusCode, Json<_>)> = Err((
            StatusCode::CONFLICT,
            Json(crate::coord_drain_state::api_refusal(
                "drained",
                crate::coord_drain_state::DeferClass::Drained,
            )),
        ));
        let reply = door_reply(refused);
        assert_eq!(reply.status, 409);
        assert_eq!(reply.body["code"], "device_drained");
        assert_eq!(reply.body["success"], false);

        let text: Result<Json<serde_json::Value>, (StatusCode, String)> =
            Err((StatusCode::CONFLICT, "drain_unreadable: x".into()));
        let reply = door_reply_text(text);
        assert_eq!(reply.status, 409);
        assert_eq!(reply.body["error"], "drain_unreadable: x");
    }

    #[test]
    fn the_operator_origin_is_never_deferred() {
        assert!(!OPERATOR.is_autonomous());
    }

    #[test]
    fn in_process_workflow_paths_are_recognised_exactly() {
        assert_eq!(
            unified_workflow_run_id("/unified-workflows/abc/run"),
            Some("abc")
        );
        assert_eq!(unified_workflow_run_id("/unified-workflows//run"), None);
        assert_eq!(unified_workflow_run_id("/unified-workflows/a/b/run"), None);
        assert_eq!(unified_workflow_run_id("/prompts/run"), None);
    }

    #[test]
    fn the_twins_are_not_reachable_through_the_ui_bridge_allowlist() {
        for name in [
            "operator_run_prompt",
            "operator_run_unified_workflow",
            "operator_execute_inline_workflow",
            "operator_run_composed_workflow",
            "operator_generate_unified_workflow_async",
            "operator_resume_task_run",
            "scheduler_run_task_now",
            "steward_start",
        ] {
            assert!(
                !crate::ui_bridge_invoke::is_allowlisted(name),
                "{name} is an OPERATOR door — a UI-Bridge-driven start must stay autonomous"
            );
        }
    }
}
