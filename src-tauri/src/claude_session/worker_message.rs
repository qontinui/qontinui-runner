//! In-process message injection into a live session — the ONE primitive
//! every "say this to that session" path goes through.
//!
//! `send_message_to_worker` injects through the SDK queue
//! (`ClaudeSession::send_user_message`, safe by construction when the session
//! is `Processing`). Caller: the coord session-bus poller
//! (`mcp::session_message_poller`). The pty-backed `WorkerSession`
//! fallthrough it used to carry went with the worker plane in Phase 4.
//!
//! Rehomed from the Productivity scheduler's `coordinator::act` by Phase 3 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`; Phase 4 then
//! deleted the scheduler and the completion-report route that were its other
//! two callers.

use std::sync::Arc;

use tauri::Manager;
use tracing::warn;

use crate::mcp::types::ApiState;

/// Best-effort `send_user_message` to a session. Errors are logged but do
/// not abort the caller — the sender can re-issue if the message didn't
/// land.
pub(crate) async fn send_message_to_worker(state: &Arc<ApiState>, session_id: &str, message: &str) {
    // Best-effort at this call site: the failure is already warn-logged inside.
    let _ = send_message_to_worker_via_handle(&state.app_handle, session_id, message).await;
}

/// The `AppHandle`-only variant of [`send_message_to_worker`]. `send_user_message`
/// only ever needs the `AppHandle` (to reach the `SessionManager` state), so
/// callers that hold an `AppHandle` but no `Arc<ApiState>` — e.g. the PR
/// shepherd's device-local author-notify (plan `2026-07-04-runner-pr-shepherd`
/// Phase 4), which runs inside the PR watcher with only `PrWatcherDeps` —
/// inject through here without constructing an `ApiState`. `send_message_to_worker`
/// delegates to this so there is exactly ONE injection primitive.
///
/// Returns whether the injection actually landed: failures are warn-logged AND
/// surfaced as `Err`, because some callers (the PR shepherd's one-per-head
/// notify claim) must not treat a swallowed failure as a delivered message.
pub(crate) async fn send_message_to_worker_via_handle(
    app_handle: &tauri::AppHandle,
    session_id: &str,
    message: &str,
) -> Result<(), String> {
    let session_manager = match app_handle.try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm.inner().clone(),
        None => {
            warn!("send_message_to_worker: SessionManager not available");
            return Err("SessionManager not available".to_string());
        }
    };

    if let Some(session) = session_manager.get(session_id) {
        // Ok(true) = sent immediately, Ok(false) = queued — both delivered.
        return match session.send_user_message(message) {
            Ok(_) => Ok(()),
            Err(e) => {
                warn!(
                    "send_message_to_worker: send_user_message failed for {}: {}",
                    session_id, e
                );
                Err(format!("send_user_message failed for {session_id}: {e}"))
            }
        };
    }

    warn!(
        "send_message_to_worker: no active session for task_run_id {}",
        session_id
    );
    Err(format!("no active session for task_run_id {session_id}"))
}
