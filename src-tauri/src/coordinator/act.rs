//! Auto-act dispatcher and helpers for the productivity-stack Coordinator.
//!
//! Lifted from `crate::mcp::coordinator` (Phase 1a of the
//! `productivity-coordinator-rust-promotion` plan) so that both the HTTP
//! handler `POST /coordinator/act` and the in-process Rust scheduler
//! drive side effects through one code path. The
//! `CoordinatorAction`/`CoordinatorActRequest`/`CoordinatorActResponse`
//! wire types stay in `mcp::coordinator` next to the HTTP routes.
//!
//! Per productivity-stack §4 the auto-act vs advise-only boundary is
//! enforced server-side. `must_advise_only` lists the destructive set;
//! everything else runs through `apply` and ends up with `auto_acted =
//! true` on the persisted decision row.

use std::sync::Arc;

use axum::http::StatusCode;
use tauri::Manager;
use tracing::warn;

use crate::mcp::coordinator::CoordinatorAction;
use crate::mcp::types::ApiState;

/// Returns the action's discriminator name (matches the `type` tag from
/// the wire format) so it can be persisted and queried alongside `rule`.
pub(crate) fn action_name(action: &CoordinatorAction) -> &'static str {
    match action {
        CoordinatorAction::AssignTask { .. } => "assign-task",
        CoordinatorAction::PauseSession { .. } => "pause-session",
        CoordinatorAction::MergeTask { .. } => "merge-task",
        CoordinatorAction::ReassignNeedsFix { .. } => "reassign-needs-fix",
        CoordinatorAction::Escalate { .. } => "escalate",
        CoordinatorAction::KillSession { .. } => "kill-session",
        CoordinatorAction::ForcePromoteToWorktree { .. } => "force-promote-to-worktree",
        CoordinatorAction::CancelTask { .. } => "cancel-task",
        CoordinatorAction::AdviseWithText { .. } => "advise-with-text",
        CoordinatorAction::EscalateWithText { .. } => "escalate-with-text",
        CoordinatorAction::IdleNoAction => "idle-no-action",
    }
}

/// Per the auto-act boundary in productivity-stack §4, only the listed
/// destructive actions force an advisory-only outcome. Everything else is
/// cheap+reversible and runs auto-acted. `cancel-task` is in the
/// destructive set because v1 has no signal for "user-requested
/// cancellation" — the slash command escalates first.
pub(crate) fn must_advise_only(action: &CoordinatorAction) -> bool {
    matches!(
        action,
        CoordinatorAction::KillSession { .. }
            | CoordinatorAction::ForcePromoteToWorktree { .. }
            | CoordinatorAction::CancelTask { .. }
            | CoordinatorAction::Escalate { .. }
            | CoordinatorAction::EscalateWithText { .. }
            | CoordinatorAction::IdleNoAction
    )
}

/// Returns the action's `target_id` — what the action operates on. Used
/// for the persisted decision row and for the escalation event payload.
pub(crate) fn action_target(action: &CoordinatorAction) -> Option<String> {
    match action {
        CoordinatorAction::AssignTask { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::PauseSession { session_id, .. } => Some(session_id.clone()),
        CoordinatorAction::MergeTask { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::ReassignNeedsFix { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::Escalate { target_id, .. } => Some(target_id.clone()),
        CoordinatorAction::KillSession { session_id, .. } => Some(session_id.clone()),
        CoordinatorAction::ForcePromoteToWorktree { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::CancelTask { task_id, .. } => Some(task_id.clone()),
        CoordinatorAction::AdviseWithText { target_id, .. } => target_id.clone(),
        CoordinatorAction::EscalateWithText { target_id, .. } => target_id.clone(),
        CoordinatorAction::IdleNoAction => None,
    }
}

pub(crate) fn action_reasoning(action: &CoordinatorAction) -> String {
    match action {
        CoordinatorAction::AssignTask {
            reasoning,
            task_id,
            session_id,
        } => reasoning
            .clone()
            .unwrap_or_else(|| format!("assign task {} → session {}", task_id, session_id)),
        CoordinatorAction::PauseSession { reason, .. } => reason.clone(),
        CoordinatorAction::MergeTask {
            reasoning, task_id, ..
        } => reasoning
            .clone()
            .unwrap_or_else(|| format!("merge task {}", task_id)),
        CoordinatorAction::ReassignNeedsFix { reasoning, .. }
        | CoordinatorAction::Escalate { reasoning, .. }
        | CoordinatorAction::KillSession { reasoning, .. }
        | CoordinatorAction::ForcePromoteToWorktree { reasoning, .. }
        | CoordinatorAction::CancelTask { reasoning, .. }
        | CoordinatorAction::AdviseWithText { reasoning, .. }
        | CoordinatorAction::EscalateWithText { reasoning, .. } => reasoning.clone(),
        CoordinatorAction::IdleNoAction => "no cheap rule fired this iteration".to_string(),
    }
}

/// Apply a cheap-and-reversible action. Returns `Err` on irrecoverable
/// failure — that triggers an HTTP 500. Soft failures (e.g. session not
/// found) are logged and the row still persists with `auto_acted = true`,
/// since the *intent* was to auto-act.
pub(crate) async fn apply(
    state: &Arc<ApiState>,
    action: &CoordinatorAction,
) -> Result<(), (StatusCode, String)> {
    let pg = &state.app_state.pg_db;

    match action {
        CoordinatorAction::AssignTask {
            task_id,
            session_id,
            ..
        } => {
            // Idempotent — assign_task_to_session checks for `ready`/`needs_fix`
            // before flipping. We don't fail on `Ok(false)` because the agent
            // may be retrying; `Err` is reserved for real DB problems.
            pg.assign_task_to_session(task_id, session_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        CoordinatorAction::PauseSession { .. } => {
            // The pause side effect lives in the slash command (it has HTTP
            // affinity to the worker session). v1 logs the decision row and
            // returns; the agent issues the follow-up `/sessions/<id>/message`
            // POST. The decision row remains the source of truth.
        }
        CoordinatorAction::MergeTask { task_id, reasoning } => {
            // Per plan §4 Rule D: merge-task is the per-tab commit trigger.
            // We send the canonical commit prompt to the worker and flip the
            // task to `done`. The actual `git commit` runs in the worker's
            // own session (it owns the working tree); this endpoint just
            // requests it and records intent.
            //
            // Resolution path: latest review for the task → reviewed
            // session id → /sessions/<id>/message wrapper. We tolerate
            // "no review row" (returns Ok with no message sent — that's a
            // misuse the decision row will surface to the user).
            let latest_review = pg
                .get_reviews_for_task(task_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
                .into_iter()
                .next();

            // Flip the task's status to done. This is best-effort — the
            // worker may not yet have transitioned to `review`, in which
            // case we leave the row alone and the worker's own commit flow
            // picks up later.
            if let Err(e) = pg.transition_task_status(task_id, "review", "done").await {
                warn!(
                    "merge-task: transition_task_status review->done failed for {}: {}",
                    task_id, e
                );
            }

            if let Some(review) = latest_review {
                let prompt = format!(
                    "Please commit your work for task {} now. The review verdict is {} \
                     (confidence {:.2}). {}\n\nUse `git add -A && git commit -m \"<msg>\"` \
                     with a concise commit message that references the task.",
                    task_id,
                    review.verdict,
                    review.confidence,
                    reasoning.as_deref().unwrap_or("")
                );
                send_message_to_worker(state, &review.reviewed_session_id, &prompt).await;
            } else {
                warn!(
                    "merge-task for {} has no review rows; skipping commit prompt",
                    task_id
                );
            }
        }
        CoordinatorAction::ReassignNeedsFix { task_id, reasoning } => {
            // Per plan §4 Rule D: re-assign to the same worker with the
            // reviewer's `reasoning` as the new user message, prefixed
            // by the action's free-form `reasoning` (typically "retry N
            // of 3").
            let latest_review = pg
                .get_reviews_for_task(task_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
                .into_iter()
                .next();

            // Flip the task back to `assigned` so the worker's tooling
            // recognises it as live work. `review -> needs_fix -> assigned`
            // is the canonical retry path.
            if let Err(e) = pg
                .transition_task_status(task_id, "review", "needs_fix")
                .await
            {
                warn!(
                    "re-assign-needs-fix: transition review->needs_fix failed for {}: {}",
                    task_id, e
                );
            }
            if let Err(e) = pg
                .transition_task_status(task_id, "needs_fix", "assigned")
                .await
            {
                warn!(
                    "re-assign-needs-fix: transition needs_fix->assigned failed for {}: {}",
                    task_id, e
                );
            }

            if let Some(review) = latest_review {
                let prompt = format!(
                    "Reviewer feedback for task {} (verdict={}, confidence={:.2}):\n\n{}\n\n\
                     Coordinator note: {}\n\nPlease address the items above and resume work.",
                    task_id, review.verdict, review.confidence, review.reasoning, reasoning,
                );
                send_message_to_worker(state, &review.reviewed_session_id, &prompt).await;
            } else {
                warn!(
                    "re-assign-needs-fix for {} has no review rows; skipping reviewer feedback",
                    task_id
                );
            }
        }
        CoordinatorAction::AdviseWithText { .. } => {
            // The advisory IS the action. Persistence + the
            // `coordinator-advice` event the dispatcher emits below are the
            // only side effects; nothing on disk changes. Keeps `auto_acted
            // = true` so the row reads as a completed move in the audit
            // trail (per Phase 5 spec).
        }
        CoordinatorAction::IdleNoAction
        | CoordinatorAction::Escalate { .. }
        | CoordinatorAction::KillSession { .. }
        | CoordinatorAction::ForcePromoteToWorktree { .. }
        | CoordinatorAction::CancelTask { .. }
        | CoordinatorAction::EscalateWithText { .. } => {
            // These never reach apply (must_advise_only returns true), but
            // the match arm satisfies exhaustiveness without a catch-all
            // that could mask new variants.
        }
    }
    Ok(())
}

/// Best-effort `send_user_message` to a worker session. Errors are logged
/// (the action is auto-act, the decision row records intent) but do not
/// abort the calling action — the user can re-issue from the dashboard if
/// the message didn't land.
pub(crate) async fn send_message_to_worker(state: &Arc<ApiState>, session_id: &str, message: &str) {
    let session_manager = match state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm.inner().clone(),
        None => {
            warn!("send_message_to_worker: SessionManager not available");
            return;
        }
    };

    match session_manager.get(session_id) {
        Some(session) => {
            if let Err(e) = session.send_user_message(message) {
                warn!(
                    "send_message_to_worker: send_user_message failed for {}: {}",
                    session_id, e
                );
            }
        }
        None => {
            warn!(
                "send_message_to_worker: no active session for task_run_id {}",
                session_id
            );
        }
    }
}
