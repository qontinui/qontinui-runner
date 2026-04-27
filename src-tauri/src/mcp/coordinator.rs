//! HTTP surface for the productivity-stack Coordinator.
//!
//! Two endpoints, mounted at `mcp::coordinator::routes()`:
//!
//! - `GET /coordinator/state` — composite snapshot the `/coordinate` slash
//!   command pulls every iteration. Bundles non-terminal tasks, the
//!   active+upcoming file registries, live sessions, and open escalations
//!   into a single payload so the agent doesn't fan-out individual
//!   requests.
//! - `POST /coordinator/act` — action dispatcher. Validates the action
//!   shape, applies the auto-act vs advise-only boundary defined in the
//!   plan, persists a `coordinator_decisions` row, and (for auto-act
//!   actions) executes the side effect. For advisory actions the row is
//!   stored with `auto_acted = false` and a `coordinator-escalation`
//!   Tauri event is emitted so the dashboard can surface a card.
//!
//! Per productivity-stack §4 ("auto-act vs advise-and-ask"): the boundary
//! is enforced server-side. Even if the agent asks to auto-act on a
//! `kill-session` or `force-promote-to-worktree`, the endpoint demotes it
//! to advisory.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tracing::{info, warn};

use crate::database::pg::coordinator_decisions::{
    CoordinatorDecisionRow, InsertCoordinatorDecisionInput,
};
use crate::database::pg::reviews::ReviewRow;
use crate::database::pg::tasks::{TaskRow, NON_TERMINAL_STATUSES};
use crate::executor::file_registry::FileRegistryInfo;
use crate::executor::upcoming_file_registry::UpcomingClaim;
use crate::mcp::types::ApiState;

// =============================================================================
// /coordinator/state
// =============================================================================

/// Snapshot of one live session — what `/coordinate` Rule A and Rule B
/// reason about.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionSummary {
    pub task_run_id: String,
    pub state: String,
}

/// Composite payload returned by `GET /coordinator/state`. Field shape
/// matches what the slash command consumes; renaming is a breaking change.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorStateSnapshot {
    pub tasks: Vec<TaskRow>,
    pub active_file_registry: Vec<FileRegistryInfo>,
    pub upcoming_file_registry: Vec<UpcomingClaim>,
    pub live_sessions: Vec<LiveSessionSummary>,
    /// Reviews from the last hour (Phase 3) so `/coordinate` Rule D can
    /// scan a single iteration's worth of verdicts in one call.
    pub recent_reviews: Vec<ReviewRow>,
    pub open_escalations: Vec<CoordinatorDecisionRow>,
}

async fn get_coordinator_state(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<CoordinatorStateSnapshot>, (StatusCode, String)> {
    let app = state.app_state.clone();

    let tasks = app
        .pg_db
        .list_tasks_by_status(NON_TERMINAL_STATUSES)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list_tasks_by_status: {}", e),
            )
        })?;

    let active_file_registry = app.file_registry_manager.info().await;
    let upcoming_file_registry = app.upcoming_file_registry.snapshot().await;

    // Pull live session ids from the SessionManager Tauri-state. The
    // Coordinator dashboard renders state per session; we surface only the
    // `Processing|Ready|Closed` discriminator string here so the JSON is
    // small. Detailed per-session metadata stays on the dashboard via the
    // dedicated terminal endpoints.
    let live_sessions = match state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm
            .inner()
            .list_active()
            .into_iter()
            .map(|task_run_id| LiveSessionSummary {
                task_run_id,
                state: "active".to_string(),
            })
            .collect(),
        None => Vec::new(),
    };

    let open_escalations = app.pg_db.list_open_escalations().await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("list_open_escalations: {}", e),
        )
    })?;

    // One hour lookback gives Rule D sufficient slack across slow Coordinator
    // iterations without flooding the agent with stale verdicts. Cap at 50
    // rows (matches /reviews/recent default).
    let recent_reviews = app.pg_db.list_recent_reviews(3600, 50).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("list_recent_reviews: {}", e),
        )
    })?;

    Ok(Json(CoordinatorStateSnapshot {
        tasks,
        active_file_registry,
        upcoming_file_registry,
        live_sessions,
        recent_reviews,
        open_escalations,
    }))
}

// =============================================================================
// /coordinator/act
// =============================================================================

/// Discriminated union mirroring the TS contract in productivity-stack §8.
/// Names are tagged with `kebab-case` so the wire format reads as
/// `{"type": "assign-task", "taskId": "...", "sessionId": "..."}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CoordinatorAction {
    AssignTask {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        reasoning: Option<String>,
    },
    PauseSession {
        #[serde(rename = "sessionId")]
        session_id: String,
        reason: String,
    },
    MergeTask {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(default)]
        reasoning: Option<String>,
    },
    // Plan §4 spells this `re-assign-needs-fix` in prose; serde's
    // kebab-case rename produces `reassign-needs-fix`. Accept both via the
    // explicit alias so the agent can copy either form from the doc body
    // without a 422.
    #[serde(alias = "re-assign-needs-fix")]
    ReassignNeedsFix {
        #[serde(rename = "taskId")]
        task_id: String,
        reasoning: String,
    },
    Escalate {
        #[serde(rename = "targetId")]
        target_id: String,
        reasoning: String,
    },
    KillSession {
        #[serde(rename = "sessionId")]
        session_id: String,
        reasoning: String,
    },
    ForcePromoteToWorktree {
        #[serde(rename = "taskId")]
        task_id: String,
        reasoning: String,
    },
    CancelTask {
        #[serde(rename = "taskId")]
        task_id: String,
        reasoning: String,
    },
    /// LLM-driven advisory: persist the reasoning, mark `auto_acted=true`
    /// (the row IS the action — surfacing to the user via the dashboard's
    /// Advisories panel), and emit `coordinator-advice`. Non-destructive.
    AdviseWithText {
        #[serde(rename = "targetId", default)]
        target_id: Option<String>,
        reasoning: String,
    },
    /// LLM-driven escalation: persist the reasoning with `auto_acted=false`
    /// so the dashboard's Escalations panel surfaces it for user approval.
    EscalateWithText {
        #[serde(rename = "targetId", default)]
        target_id: Option<String>,
        reasoning: String,
    },
    IdleNoAction,
}

/// Top-level body for `POST /coordinator/act`. Carries iteration metadata
/// for the decision log plus the action itself.
#[derive(Debug, Clone, Deserialize)]
pub struct CoordinatorActRequest {
    pub session_id: String,
    pub iteration: i64,
    /// One of `A`–`E` for cheap rules, `LLM` for LLM-driven (future), or
    /// `idle` for no-op cycles.
    pub rule: String,
    pub action: CoordinatorAction,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorActResponse {
    pub decision: CoordinatorDecisionRow,
    /// `true` when the side-effect was actually applied; `false` when the
    /// row was logged advisory-only (escalation path).
    pub auto_acted: bool,
}

/// Returns the action's discriminator name (matches the `type` tag from
/// the wire format) so it can be persisted and queried alongside `rule`.
fn action_name(action: &CoordinatorAction) -> &'static str {
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
fn must_advise_only(action: &CoordinatorAction) -> bool {
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
fn action_target(action: &CoordinatorAction) -> Option<String> {
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

fn action_reasoning(action: &CoordinatorAction) -> String {
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
async fn apply_auto_act(
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
            // These never reach apply_auto_act (must_advise_only returns
            // true), but the match arm satisfies exhaustiveness without a
            // catch-all that could mask new variants.
        }
    }
    Ok(())
}

/// Best-effort `send_user_message` to a worker session. Errors are logged
/// (the action is auto-act, the decision row records intent) but do not
/// abort the calling action — the user can re-issue from the dashboard if
/// the message didn't land.
async fn send_message_to_worker(state: &Arc<ApiState>, session_id: &str, message: &str) {
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

async fn coordinator_act(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CoordinatorActRequest>,
) -> Result<Json<CoordinatorActResponse>, (StatusCode, String)> {
    if req.session_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "session_id is required".to_string(),
        ));
    }
    if req.rule.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "rule is required".to_string()));
    }

    let action_name_str = action_name(&req.action);
    let advise_only = must_advise_only(&req.action);
    let auto_acted = !advise_only;
    let target_id = action_target(&req.action);
    let reasoning = action_reasoning(&req.action);

    if auto_acted {
        apply_auto_act(&state, &req.action).await?;
    }

    // Persist the decision row regardless of auto-act outcome. This is the
    // single audit log the dashboard renders.
    let row = state
        .app_state
        .pg_db
        .insert_coordinator_decision(&InsertCoordinatorDecisionInput {
            session_id: &req.session_id,
            iteration: req.iteration,
            rule: &req.rule,
            action: action_name_str,
            target_id: target_id.as_deref(),
            reasoning: &reasoning,
            auto_acted,
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Advisory actions emit a `coordinator-escalation` event so the
    // dashboard's Escalations panel can render a card without polling.
    // Auto-acted advisories (the LLM-driven `advise-with-text` action)
    // emit `coordinator-advice` to a sibling Advisories panel — both
    // events carry the same row payload.
    let event_name = if advise_only {
        "coordinator-escalation"
    } else if action_name_str == "advise-with-text" {
        "coordinator-advice"
    } else {
        ""
    };
    if !event_name.is_empty() {
        let payload = serde_json::json!({
            "decision_id": row.id,
            "session_id": row.session_id,
            "rule": row.rule,
            "action": row.action,
            "target_id": row.target_id,
            "reasoning": row.reasoning,
        });
        if let Err(e) = state.app_handle.emit(event_name, &payload) {
            warn!("Failed to emit {} event: {}", event_name, e);
        }
    }

    if advise_only {
        info!(
            "Coordinator escalation logged: rule={} action={} target={:?}",
            row.rule, row.action, row.target_id
        );
    } else {
        info!(
            "Coordinator action auto-acted: rule={} action={} target={:?}",
            row.rule, row.action, row.target_id
        );
    }

    Ok(Json(CoordinatorActResponse {
        decision: row,
        auto_acted,
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/coordinator/state", get(get_coordinator_state))
        .route("/coordinator/act", post(coordinator_act))
}
