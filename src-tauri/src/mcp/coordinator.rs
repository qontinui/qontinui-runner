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

    let action_name_str = crate::coordinator::act::action_name(&req.action);
    let advise_only = crate::coordinator::act::must_advise_only(&req.action);
    let auto_acted = !advise_only;
    let target_id = crate::coordinator::act::action_target(&req.action);
    let reasoning = crate::coordinator::act::action_reasoning(&req.action);

    if auto_acted {
        crate::coordinator::act::apply(&state, &req.action).await?;
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
// /coordinator/leader
// =============================================================================

/// HTTP mirror of the `get_coordinator_leader` Tauri command. Returns the
/// same `LeaderResponse` shape (camelCase JSON) so external agents can
/// poll the lease status without going through Tauri IPC.
async fn get_coordinator_leader(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<crate::commands::productivity::LeaderResponse>, (StatusCode, String)> {
    let leader = state
        .app_state
        .pg_db
        .current_coordinator_leader()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("current_coordinator_leader: {}", e),
            )
        })?;
    let lease_status = crate::commands::productivity::compute_lease_status_for_http(&leader);
    Ok(Json(crate::commands::productivity::LeaderResponse {
        leader,
        lease_status,
    }))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/coordinator/state", get(get_coordinator_state))
        .route("/coordinator/act", post(coordinator_act))
        .route("/coordinator/leader", get(get_coordinator_leader))
}
