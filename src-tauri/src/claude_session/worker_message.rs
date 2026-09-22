//! In-process message injection into a live session — the ONE primitive
//! every "say this to that session" path goes through.
//!
//! `send_message_to_worker_via_handle` injects through the SDK queue
//! (`ClaudeSession::send_user_message`, safe by construction when the session
//! is `Processing`). Callers: the coord session-bus poller
//! (`mcp::session_message_poller`) and the PR shepherd's author-notify. The
//! pty-backed `WorkerSession` fallthrough this module used to carry went with
//! the worker plane in Phase 4.
//!
//! Rehomed from the Productivity scheduler's `coordinator::act` by Phase 3 of
//! `2026-09-12-consolidate-local-orchestration-onto-conductor`. Phase 4 then
//! deleted the scheduler and the completion-report route, which were the two
//! callers of the former `ApiState`-taking `send_message_to_worker` wrapper —
//! so that wrapper is gone too, leaving exactly ONE injection primitive rather
//! than a delegating pair whose outer half had no callers left.

use std::sync::Arc;

use tauri::Manager;
use tracing::warn;

/// Inject a message into a live session, given only an `AppHandle`. `send_user_message`
/// only ever needs the `AppHandle` (to reach the `SessionManager` state), so
/// callers that hold an `AppHandle` but no `Arc<ApiState>` — e.g. the PR
/// shepherd's device-local author-notify (plan `2026-07-04-runner-pr-shepherd`
/// Phase 4), which runs inside the PR watcher with only `PrWatcherDeps` —
/// inject through here without constructing an `ApiState`.
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
