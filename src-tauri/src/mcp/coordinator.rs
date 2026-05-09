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

use axum::extract::{Query, State};
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
use crate::database::pg::coordinator_leader::CoordinatorLeaderRow;
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
///
/// `state` is the session's actual lifecycle discriminator
/// (`created|initializing|ready|processing|interrupting|promoting|closing|closed`)
/// looked up via `SessionManager::get_state`. If a session disappears
/// between `list_active` and `get_state` (race), the entry is preserved
/// with `state = "closed"` and `is_active = false` so consumers see the
/// id rather than silently dropping it.
///
/// `is_active` mirrors `SessionState::is_active()` (true for any state
/// other than `closing` / `closed`) — kept as a dedicated field so
/// frontends that already discriminate on a boolean don't need to
/// re-implement the predicate.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionSummary {
    pub task_run_id: String,
    pub state: String,
    pub is_active: bool,
    /// Pty terminal id when this session is a pty-backed Worker (Phase 6).
    /// `None` for ClaudeSessions which run their own subprocess directly.
    /// The dashboard's Workers panel uses this to wire up the
    /// "View terminal" button without re-querying `/terminals` and
    /// title-prefix-matching against pty tabs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// User-facing title (e.g. "Worker 3"). `None` for ClaudeSessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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

    // Pull live session ids from the SessionManager Tauri-state and
    // resolve each id back to its actual lifecycle state. `get_state`
    // walks both the ClaudeSession and worker_sessions maps (post-Phase-6
    // worker registration), so this works uniformly for skill sessions
    // and pty workers. If a session disappears between `list_active` and
    // `get_state` (race), surface it as `closed`/`is_active = false`
    // rather than dropping the entry — the consumer can decide whether
    // to filter.
    let live_sessions = match state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => {
            let manager = sm.inner();
            manager
                .list_active()
                .into_iter()
                .map(|task_run_id| {
                    // Pty-backed workers expose `title()` and
                    // `terminal_id()` so the dashboard can map a
                    // liveSessions row to its terminal tab. ClaudeSessions
                    // (subprocess-backed) have neither — leave both fields
                    // `None` so the JSON omits them.
                    let (terminal_id, title) = match manager.get_worker(&task_run_id) {
                        Some(w) => (
                            Some(w.terminal_id().to_string()),
                            Some(w.title().to_string()),
                        ),
                        None => (None, None),
                    };
                    match manager.get_state(&task_run_id) {
                        Some(s) => LiveSessionSummary {
                            task_run_id,
                            state: s.as_event_str().to_string(),
                            is_active: s.is_active(),
                            terminal_id,
                            title,
                        },
                        None => LiveSessionSummary {
                            task_run_id,
                            state: "closed".to_string(),
                            is_active: false,
                            terminal_id,
                            title,
                        },
                    }
                })
                .collect()
        }
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
    /// User-driven override: flip a `pending` task to `ready` even when
    /// upstream completion reports flag a blocking follow-up. Per
    /// productivity-coordinator-completion-reports §4 "Auto-act boundary"
    /// this action is FORCED advisory — the Coordinator agent cannot
    /// auto-act on it; only the user's explicit click on the dashboard
    /// (which fires `/coordinator/act` with `auto_acted=false`) executes the
    /// flip. The HTTP path in `coordinator_act` runs the `apply` branch
    /// regardless of `must_advise_only` because the inbound POST itself IS
    /// the user-confirmed escalation.
    ForceFlipReadyDespiteBlocker {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(default)]
        reasoning: Option<String>,
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

    // The cheap-rules scheduler routes through `auto_acted` — destructive
    // actions are advisory-only at that layer. The HTTP path additionally
    // carves out user-fire actions (e.g. force-flip-ready-despite-blocker)
    // so a user-confirmed POST executes the side effect even when the row
    // is logged as advisory. See `is_user_fire_only_action` in act.rs.
    let should_apply = auto_acted || crate::coordinator::act::is_user_fire_only_action(&req.action);
    if should_apply {
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
            observation_hash: "",
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
// /coordinator/tasks/reset-stale
// =============================================================================

/// Query string for `POST /coordinator/tasks/reset-stale`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetStaleTasksQuery {
    #[serde(default)]
    pub dry_run: bool,
}

/// One task that was examined but not flipped back to `ready`. The
/// `reason` strings are stable so callers (the Productivity dashboard, the
/// `/coordinate` slash command) can group/render them without matching on
/// free-form text.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTask {
    pub task_id: String,
    pub status: String,
    pub assigned_session_id: Option<String>,
    pub reason: String,
}

/// Response for `POST /coordinator/tasks/reset-stale`. `reset` is the set
/// of task ids that were (or, in dry-run mode, would be) flipped back to
/// `ready`; `skipped` is the audit trail of tasks that were inspected but
/// not eligible.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetStaleTasksResponse {
    pub reset: Vec<String>,
    pub skipped: Vec<SkippedTask>,
    pub dry_run: bool,
}

/// Flip stale `assigned`/`needs_fix` tasks back to `ready` when their
/// `assigned_session_id` is no longer present in the live SessionManager
/// set. Useful between test runs against pre-decomposed plan fixtures —
/// the prior runner's worker session ids are dead weight in
/// `coord.tasks.assigned_session_id` and prevent the next coordinator
/// iteration from picking the task back up.
///
/// Idempotent: a second concurrent call will see the task already in
/// `ready` and skip it with reason `status not assigned/needs_fix`.
async fn reset_stale_tasks(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ResetStaleTasksQuery>,
) -> Result<Json<ResetStaleTasksResponse>, (StatusCode, String)> {
    let dry_run = query.dry_run;
    let app = state.app_state.clone();

    // 1. Pull every non-terminal task — same source the dashboard uses.
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

    // 2. Resolve the live session set. If SessionManager isn't registered
    //    (test harness, or runner shutting down), treat the live set as
    //    empty — every assigned task counts as stale.
    let live: std::collections::HashSet<String> = match state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm.inner().list_active().into_iter().collect(),
        None => std::collections::HashSet::new(),
    };

    // 3. Classify each task into eligible vs skipped.
    //    Carry the original status + assigned_session_id alongside the id
    //    so the non-dry-run path can re-emit them into `skipped` if the
    //    PG flip itself errors (R4 — without this, failed flips would
    //    silently disappear from the response).
    struct EligibleTask {
        task_id: String,
        status: String,
        assigned_session_id: Option<String>,
    }
    let mut eligible: Vec<EligibleTask> = Vec::new();
    let mut skipped: Vec<SkippedTask> = Vec::new();
    for task in &tasks {
        let status = task.status.as_str();
        if status != "assigned" && status != "needs_fix" {
            skipped.push(SkippedTask {
                task_id: task.id.clone(),
                status: task.status.clone(),
                assigned_session_id: task.assigned_session_id.clone(),
                reason: "status not assigned/needs_fix".to_string(),
            });
            continue;
        }
        match &task.assigned_session_id {
            None => {
                // Defensive: shouldn't happen for assigned/needs_fix in
                // normal operation, but possible after a manual DB poke.
                skipped.push(SkippedTask {
                    task_id: task.id.clone(),
                    status: task.status.clone(),
                    assigned_session_id: None,
                    reason: "no assigned_session_id".to_string(),
                });
            }
            Some(sid) if live.contains(sid) => {
                skipped.push(SkippedTask {
                    task_id: task.id.clone(),
                    status: task.status.clone(),
                    assigned_session_id: Some(sid.clone()),
                    reason: "session still alive".to_string(),
                });
            }
            Some(sid) => {
                eligible.push(EligibleTask {
                    task_id: task.id.clone(),
                    status: task.status.clone(),
                    assigned_session_id: Some(sid.clone()),
                });
            }
        }
    }

    // 4. In non-dry-run mode, apply the flip via `force_reset_task_to_ready`
    //    — the recovery primitive that bypasses the canonical state-machine
    //    guard (R3). The primitive's WHERE clause keeps the call race-safe:
    //    if the row has already moved to `running`/`review`/etc., the UPDATE
    //    affects zero rows and we surface that as a skip rather than a flip.
    let mut reset_ids: Vec<String> = Vec::new();
    if !dry_run {
        for et in &eligible {
            match app.pg_db.force_reset_task_to_ready(&et.task_id).await {
                Ok(true) => {
                    reset_ids.push(et.task_id.clone());
                }
                Ok(false) => {
                    // Row didn't match the WHERE guard — concurrent caller
                    // already moved it past assigned/needs_fix. Surface so
                    // the caller can audit instead of silently dropping.
                    skipped.push(SkippedTask {
                        task_id: et.task_id.clone(),
                        status: et.status.clone(),
                        assigned_session_id: et.assigned_session_id.clone(),
                        reason: "concurrent transition (no row matched)".to_string(),
                    });
                }
                Err(e) => {
                    warn!(
                        "reset_stale_tasks: force_reset_task_to_ready({}) failed: {}",
                        et.task_id, e
                    );
                    skipped.push(SkippedTask {
                        task_id: et.task_id.clone(),
                        status: et.status.clone(),
                        assigned_session_id: et.assigned_session_id.clone(),
                        reason: format!("flip failed: {}", e),
                    });
                }
            }
        }
        info!(
            "reset_stale_tasks: flipped {} stale task(s) back to ready (skipped={})",
            reset_ids.len(),
            skipped.len()
        );
    } else {
        // Dry-run: report what we *would* flip without touching PG.
        reset_ids = eligible.iter().map(|et| et.task_id.clone()).collect();
        info!(
            "reset_stale_tasks (dry-run): would flip {} stale task(s); skipped={}",
            reset_ids.len(),
            skipped.len()
        );
    }

    Ok(Json(ResetStaleTasksResponse {
        reset: reset_ids,
        skipped,
        dry_run,
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
// /coordinator/decisions
// =============================================================================

/// Query string for `GET /coordinator/decisions`. `limit` defaults to 50
/// and is clamped to `[1, 200]` to match the Decision Log's pagination
/// budget. `rule` and `action` are optional exact-match filters passed
/// through verbatim to `list_recent_coordinator_decisions`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorDecisionsQuery {
    #[serde(default = "default_decisions_limit")]
    pub limit: i64,
    #[serde(default)]
    pub rule: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
}

fn default_decisions_limit() -> i64 {
    50
}

/// Response shape for `GET /coordinator/decisions`. `total` is a
/// convenience for clients that don't want to compute `decisions.len()`
/// themselves; it always equals `decisions.len()` because the helper
/// returns the full result set (no separate count query).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorDecisionsResponse {
    pub decisions: Vec<CoordinatorDecisionRow>,
    pub total: usize,
}

async fn get_coordinator_decisions(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<CoordinatorDecisionsQuery>,
) -> Result<Json<CoordinatorDecisionsResponse>, (StatusCode, String)> {
    let limit = query.limit.clamp(1, 200);
    let rule_filter = query.rule.as_deref().filter(|s| !s.is_empty());
    let action_filter = query.action.as_deref().filter(|s| !s.is_empty());

    let decisions = state
        .app_state
        .pg_db
        .list_recent_coordinator_decisions(limit, rule_filter, action_filter)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list_recent_coordinator_decisions: {}", e),
            )
        })?;

    let total = decisions.len();
    Ok(Json(CoordinatorDecisionsResponse { decisions, total }))
}

// =============================================================================
// /coordinator/leader/break-lease
// =============================================================================

/// Response for `POST /coordinator/leader/break-lease`. The endpoint is
/// idempotent + race-safe: the underlying `release_stale_coordinator_lease`
/// helper only deletes a row whose `leased_until` is at least 60s in the
/// past, so calling it on a healthy lease is a no-op (`broken = false`).
/// `current` and `lease_status` reflect the post-action state so the UI
/// can refresh the "current leader" badge from one round-trip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakLeaseResponse {
    pub broken: bool,
    pub current: Option<CoordinatorLeaderRow>,
    pub lease_status: String,
}

async fn break_coordinator_lease(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<BreakLeaseResponse>, (StatusCode, String)> {
    let broken = state
        .app_state
        .pg_db
        .release_stale_coordinator_lease()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("release_stale_coordinator_lease: {}", e),
            )
        })?;

    let current = state
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
    let lease_status = crate::commands::productivity::compute_lease_status_for_http(&current);

    if broken {
        info!(
            "Coordinator stale lease cleared; post-action lease_status={}",
            lease_status
        );
    }

    Ok(Json(BreakLeaseResponse {
        broken,
        current,
        lease_status,
    }))
}

// =============================================================================
// Shadow diff (sd01_coord_coordinator_shadow_decisions)
// =============================================================================

/// Query string for `GET /coordinator/shadow-diff`. `window_seconds`
/// defaults to 86400 (24 hours). `sample_limit` controls how many
/// per-disagreement rows the response embeds for human inspection;
/// defaults to 20, capped at 200.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowDiffQuery {
    #[serde(default)]
    pub window_seconds: Option<i64>,
    #[serde(default)]
    pub sample_limit: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowDiffResponse {
    pub window_seconds: i64,
    pub buckets: Vec<crate::database::pg::coordinator_shadow_decisions::ShadowDiffBucket>,
    /// Aggregate agreement rate over rows where shadow + live both saw
    /// the same observation_hash. `None` when no matched observations
    /// exist in the window — e.g. when only the Rust shadow scheduler
    /// is running (no live `/coordinate` invocations to compare against).
    pub agreement_rate_overall: Option<f64>,
    pub matched_observations_total: i64,
    pub agreements_total: i64,
    pub disagreement_samples: Vec<ShadowDisagreementSample>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowDisagreementSample {
    pub shadow: crate::database::pg::coordinator_shadow_decisions::CoordinatorShadowDecisionRow,
    pub live_rule: String,
    pub live_action: String,
}

/// `GET /coordinator/shadow-diff?windowSeconds=86400&sampleLimit=20`
async fn shadow_diff(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ShadowDiffQuery>,
) -> Result<Json<ShadowDiffResponse>, (StatusCode, String)> {
    let window_seconds = query.window_seconds.unwrap_or(86_400).max(60);
    let sample_limit = query.sample_limit.unwrap_or(20).clamp(1, 200);

    let buckets = state
        .app_state
        .pg_db
        .shadow_diff_aggregate(window_seconds)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let matched_observations_total: i64 = buckets.iter().map(|b| b.matched_observations).sum();
    let agreements_total: i64 = buckets.iter().map(|b| b.agreements_on_match).sum();
    let agreement_rate_overall = if matched_observations_total > 0 {
        Some(agreements_total as f64 / matched_observations_total as f64)
    } else {
        None
    };

    let disagreements = state
        .app_state
        .pg_db
        .shadow_diff_disagreements(window_seconds, sample_limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let disagreement_samples = disagreements
        .into_iter()
        .map(
            |(shadow, live_rule, live_action)| ShadowDisagreementSample {
                shadow,
                live_rule,
                live_action,
            },
        )
        .collect();

    Ok(Json(ShadowDiffResponse {
        window_seconds,
        buckets,
        agreement_rate_overall,
        matched_observations_total,
        agreements_total,
        disagreement_samples,
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
        .route("/coordinator/decisions", get(get_coordinator_decisions))
        .route("/coordinator/shadow-diff", get(shadow_diff))
        .route(
            "/coordinator/leader/break-lease",
            post(break_coordinator_lease),
        )
        .route("/coordinator/tasks/reset-stale", post(reset_stale_tasks))
}
