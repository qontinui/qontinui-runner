//! Workflow event emission to the qontinui-web backend.
//!
//! Runners emit events at key lifecycle points (run start, complete, fail, HITL, etc.)
//! to the backend via `POST /api/v1/events/workflow`. These events drive push
//! notifications to the user's mobile devices and the mobile event feed.

use crate::auth::AuthManager;
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{info, warn};

use crate::api_config::get_api_base_url;

/// Workflow event types matching the backend WorkflowEventType enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEventType {
    RunStarted,
    RunCompleted,
    RunFailed,
    SessionCompleted,
    TerminalExited,
    StepCompleted,
    HitlQuestionPending,
    RunnerCrashed,
    RunnerRecovered,
    BuildFailed,
    VerificationFailed,
    /// Emitted once per phase (setup/verification/agentic/completion) with
    /// the serialized `PhaseResult`. Ingested by the web backend's
    /// `POST /api/v1/events/phase-completed` endpoint.
    PhaseCompleted,
    // ---- New-project create funnel (plan 2026-09-22 PR-F) -----------------
    // Telemetry, not user-facing: the web backend suppresses push + feed for
    // these. All rows of one create attempt share one `run_id` (the flow id).
    /// `create_new_project` was entered (the user committed to creating).
    NewProjectStarted,
    /// The `validate` step passed (local template/name/location rules).
    NewProjectNameOk,
    /// The `create_remote` step created (or re-verified) the GitHub repo.
    NewProjectRepoCreated,
    /// The `push` step pushed `main` to GitHub.
    NewProjectPushed,
    /// The `enroll` step enrolled the repo with the Qontinui GitHub Apps.
    NewProjectEnrolled,
    /// Terminal event of every attempt, success or failure: carries `ok`,
    /// `failed_step`, the typed error code and `skipped_stages[]`.
    NewProjectFinished,
    /// The onboarding doctor reported `ready_to_land` (emitted by the UI in
    /// Phase 3 through [`emit_workflow_event`]; no runner-side emitter).
    NewProjectLive,
}

/// Payload sent to `POST /api/v1/events/workflow`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowEventPayload {
    pub event_type: WorkflowEventType,
    pub device_id: String,
    pub runner_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub timestamp: String,
}

/// Post a workflow event to the backend (fire-and-forget).
///
/// This function is designed to be called from `tokio::spawn` so it never
/// blocks the caller. Failures are logged but do not propagate.
pub async fn post_workflow_event(event: WorkflowEventPayload) {
    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        warn!("Cannot emit workflow event: not authenticated");
        return;
    }

    let access_token = match auth_manager.get_access_token() {
        Ok(token) => token,
        Err(e) => {
            warn!("Cannot emit workflow event: failed to get token: {}", e);
            return;
        }
    };

    let api_url = get_api_base_url();
    let event_type_str = format!("{:?}", event.event_type);

    info!(
        "Emitting workflow event: {} for device {} (run: {:?})",
        event_type_str, event.device_id, event.run_id
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match client
        .post(format!("{}/api/v1/events/workflow", api_url))
        .bearer_auth(&access_token)
        .json(&event)
        .send()
        .await
    {
        Ok(response) => {
            if response.status().is_success() {
                info!("Workflow event emitted successfully: {}", event_type_str);
            } else {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unknown".to_string());
                warn!("Workflow event emission failed ({}): {}", status, body);
            }
        }
        Err(e) => {
            warn!("Workflow event emission network error: {}", e);
        }
    }
}

/// Build runner name from hostname and platform.
fn get_runner_name() -> String {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let platform = std::env::consts::OS;
    format!("{} ({})", hostname, platform)
}

/// Get current ISO-8601 timestamp.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Emit a "run_started" event. Call after creating an execution run.
pub fn emit_run_started(run_id: &str, run_name: &str) {
    let auth_manager = AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) => id,
        Err(_) => return,
    };

    let event = WorkflowEventPayload {
        event_type: WorkflowEventType::RunStarted,
        device_id,
        runner_name: get_runner_name(),
        run_id: Some(run_id.to_string()),
        summary: format!("Run '{}' started", run_name),
        payload: None,
        timestamp: now_iso(),
    };

    tokio::spawn(post_workflow_event(event));
}

/// Emit a "run_completed" event. Call after completing an execution run successfully.
pub fn emit_run_completed(run_id: &str, summary: &str, stats: Option<serde_json::Value>) {
    let auth_manager = AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) => id,
        Err(_) => return,
    };

    let event = WorkflowEventPayload {
        event_type: WorkflowEventType::RunCompleted,
        device_id,
        runner_name: get_runner_name(),
        run_id: Some(run_id.to_string()),
        summary: summary.to_string(),
        payload: stats,
        timestamp: now_iso(),
    };

    tokio::spawn(post_workflow_event(event));
}

/// Emit a "run_failed" event. Call after an execution run fails.
pub fn emit_run_failed(run_id: &str, error_message: &str) {
    let auth_manager = AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) => id,
        Err(_) => return,
    };

    let event = WorkflowEventPayload {
        event_type: WorkflowEventType::RunFailed,
        device_id,
        runner_name: get_runner_name(),
        run_id: Some(run_id.to_string()),
        summary: format!("Run failed: {}", truncate(error_message, 200)),
        payload: Some(serde_json::json!({ "error": error_message })),
        timestamp: now_iso(),
    };

    tokio::spawn(post_workflow_event(event));
}

/// Emit a "session_completed" event. Call after an AI session finishes within a run.
pub fn emit_session_completed(
    run_id: &str,
    session_number: u32,
    duration_seconds: i64,
    workflow_name: &str,
) {
    let auth_manager = AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) => id,
        Err(_) => return,
    };

    let duration_str = if duration_seconds < 60 {
        format!("{}s", duration_seconds)
    } else {
        let mins = duration_seconds / 60;
        let secs = duration_seconds % 60;
        format!("{}m {}s", mins, secs)
    };

    let event = WorkflowEventPayload {
        event_type: WorkflowEventType::SessionCompleted,
        device_id,
        runner_name: get_runner_name(),
        run_id: Some(run_id.to_string()),
        summary: format!(
            "Session {} of '{}' completed in {}",
            session_number, workflow_name, duration_str
        ),
        payload: Some(serde_json::json!({
            "session_number": session_number,
            "duration_seconds": duration_seconds,
            "workflow_name": workflow_name,
        })),
        timestamp: now_iso(),
    };

    tokio::spawn(post_workflow_event(event));
}

/// Emit a "terminal_exited" event. Call from the terminal waiter thread when a PTY exits.
///
/// Uses `tauri::async_runtime::spawn` instead of `tokio::spawn` because the
/// terminal waiter runs on a plain `std::thread`, not a tokio task.
pub fn emit_terminal_exited(terminal_id: &str, title: &str, exit_code: Option<i32>) {
    let auth_manager = AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) => id,
        Err(_) => return,
    };

    let exit_str = match exit_code {
        Some(0) => "exited successfully".to_string(),
        Some(c) => format!("exited with code {}", c),
        None => "exited".to_string(),
    };

    let event = WorkflowEventPayload {
        event_type: WorkflowEventType::TerminalExited,
        device_id,
        runner_name: get_runner_name(),
        run_id: None,
        summary: format!("Terminal '{}' {}", title, exit_str),
        payload: Some(serde_json::json!({
            "terminal_id": terminal_id,
            "title": title,
            "exit_code": exit_code,
        })),
        timestamp: now_iso(),
    };

    tauri::async_runtime::spawn(post_workflow_event(event));
}

/// Emit one new-project create-funnel event (plan 2026-09-22 PR-F).
///
/// `flow_id` is minted once per `create_new_project` invocation and stamped
/// as `run_id` so every stage of one attempt joins. Fire-and-forget: a missing
/// device id, a missing token or a failed post is logged (or silently dropped)
/// and never reaches the caller — telemetry must never fail a project creation.
///
/// Uses `tauri::async_runtime::spawn` (as [`emit_terminal_exited`] does) so it
/// is safe from any calling context.
pub fn emit_new_project_stage(
    flow_id: &str,
    event_type: WorkflowEventType,
    summary: String,
    payload: serde_json::Value,
) {
    let auth_manager = AuthManager::new();
    let device_id = match auth_manager.get_device_id() {
        Ok(id) => id,
        Err(_) => return,
    };

    let event = WorkflowEventPayload {
        event_type,
        device_id,
        runner_name: get_runner_name(),
        run_id: Some(flow_id.to_string()),
        summary,
        payload: Some(payload),
        timestamp: now_iso(),
    };

    tauri::async_runtime::spawn(post_workflow_event(event));
}

/// Emit a generic workflow event via Tauri command.
///
/// This allows the frontend to trigger events for lifecycle points it manages
/// (e.g., HITL questions, verification failures).
#[tauri::command]
pub async fn emit_workflow_event(
    event_type: String,
    run_id: Option<String>,
    summary: String,
    payload: Option<serde_json::Value>,
) -> Result<(), String> {
    let auth_manager = AuthManager::new();

    let device_id = auth_manager
        .get_device_id()
        .map_err(|e| format!("Failed to get device ID: {}", e))?;

    // Parse event type
    let event_type_enum = match event_type.as_str() {
        "run_started" => WorkflowEventType::RunStarted,
        "run_completed" => WorkflowEventType::RunCompleted,
        "run_failed" => WorkflowEventType::RunFailed,
        "session_completed" => WorkflowEventType::SessionCompleted,
        "terminal_exited" => WorkflowEventType::TerminalExited,
        "step_completed" => WorkflowEventType::StepCompleted,
        "hitl_question_pending" => WorkflowEventType::HitlQuestionPending,
        "runner_crashed" => WorkflowEventType::RunnerCrashed,
        "runner_recovered" => WorkflowEventType::RunnerRecovered,
        "build_failed" => WorkflowEventType::BuildFailed,
        "verification_failed" => WorkflowEventType::VerificationFailed,
        "phase_completed" => WorkflowEventType::PhaseCompleted,
        "new_project_started" => WorkflowEventType::NewProjectStarted,
        "new_project_name_ok" => WorkflowEventType::NewProjectNameOk,
        "new_project_repo_created" => WorkflowEventType::NewProjectRepoCreated,
        "new_project_pushed" => WorkflowEventType::NewProjectPushed,
        "new_project_enrolled" => WorkflowEventType::NewProjectEnrolled,
        "new_project_finished" => WorkflowEventType::NewProjectFinished,
        "new_project_live" => WorkflowEventType::NewProjectLive,
        _ => return Err(format!("Invalid event_type: {}", event_type)),
    };

    let event = WorkflowEventPayload {
        event_type: event_type_enum,
        device_id,
        runner_name: get_runner_name(),
        run_id,
        summary,
        payload,
        timestamp: now_iso(),
    };

    tokio::spawn(post_workflow_event(event));

    Ok(())
}

/// Truncate a string to approximately max_len bytes (char-boundary safe).
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find the last char boundary at or before max_len
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_workflow_events")
        .invoke_handler(tauri::generate_handler![emit_workflow_event,])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire name of every funnel variant must be exactly the string the
    /// web backend's `WorkflowEventType` accepts (an unknown type 400s).
    #[test]
    fn new_project_variants_serialize_to_backend_names() {
        let cases = [
            (WorkflowEventType::NewProjectStarted, "new_project_started"),
            (WorkflowEventType::NewProjectNameOk, "new_project_name_ok"),
            (
                WorkflowEventType::NewProjectRepoCreated,
                "new_project_repo_created",
            ),
            (WorkflowEventType::NewProjectPushed, "new_project_pushed"),
            (
                WorkflowEventType::NewProjectEnrolled,
                "new_project_enrolled",
            ),
            (
                WorkflowEventType::NewProjectFinished,
                "new_project_finished",
            ),
            (WorkflowEventType::NewProjectLive, "new_project_live"),
        ];
        for (variant, name) in cases {
            assert_eq!(
                serde_json::to_value(&variant).expect("serialize"),
                serde_json::Value::String(name.to_string())
            );
        }
    }
}
