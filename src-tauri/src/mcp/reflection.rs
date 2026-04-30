//! HTTP surface for the productivity-stack reflection panel and the
//! plan-recommendations card (Phase 5).
//!
//! Two endpoints, mounted at `mcp::reflection::routes()`:
//!
//! - `GET /productivity/reflection/<plan_id>` — per-plan rollup of total /
//!   completed / escalated tasks, average review confidence across all
//!   reviews on the plan's tasks, top knowledge entries attributed to
//!   tasks in the plan, and the chronological review trail.
//! - `GET /productivity/plan-recommendations` — cheap-rule "Coordinator
//!   suggests next plan" payload. Server-side ranking only — no LLM.
//!   Surfaces top 3 candidate plans with a one-sentence rationale.
//!
//! These mirror the Tauri commands in `commands::productivity` so that
//! both Tauri callers and HTTP-only callers (the slash commands) get the
//! same shape. The Rust types here are camelCase via
//! `serde(rename_all = "camelCase")` so they line up with the TS contract
//! in productivity-stack §8 Phase 5 / "Type contract additions".

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::Manager;
use tracing::warn;

use crate::commands::AppState;
use crate::mcp::types::ApiState;

// =============================================================================
// Per-plan reflection payload
// =============================================================================

/// One row of the "Top knowledge" list — a knowledge entry attributed to
/// a task in the plan. `area` is the bucket key the
/// KnowledgeBrowser modal filters by.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopKnowledgeEntry {
    pub id: String,
    pub area: String,
    pub summary: String,
}

/// One row of the chronological "Review trail" — a single review verdict
/// on any task in the plan. `created_at` is ISO-8601 (PG `::text` cast).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTrailEntry {
    pub review_id: String,
    pub task_id: String,
    pub verdict: String,
    pub confidence: f64,
    pub created_at: String,
}

/// Full reflection payload — what the ReflectionPanel renders inline at
/// the top of the plan board's task list.
///
/// Empty plans render gracefully:
///   - `avgConfidence` is `None` when no reviews exist for any task.
///   - `topKnowledge` is empty when no knowledge entries cite the plan's
///     tasks (Phase 4 hasn't run on it yet).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reflection {
    pub plan_id: String,
    pub total_tasks: i64,
    pub completed_tasks: i64,
    pub escalated_tasks: i64,
    pub avg_confidence: Option<f64>,
    pub knowledge_count: i64,
    pub top_knowledge: Vec<TopKnowledgeEntry>,
    pub review_trail: Vec<ReviewTrailEntry>,
}

/// Compute every aggregate the ReflectionPanel needs in a single
/// HTTP-call's worth of PG queries. Each query is straight aggregation
/// against the `tasks` / `reviews` / `productivity_knowledge` tables —
/// no joins on the FE side.
pub async fn compute_reflection(
    app_state: &Arc<AppState>,
    plan_id: &str,
) -> Result<Reflection, String> {
    let pg = &app_state.pg_db;

    // 1. Task tallies — one COUNT pass over tasks scoped to the plan.
    let pool = pg.pool();
    let conn = pool
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    let task_counts_row = conn
        .query_one(
            r#"
            SELECT
                COUNT(*)::bigint                                                AS total,
                COUNT(*) FILTER (WHERE status = 'done')::bigint                 AS completed,
                COUNT(*) FILTER (WHERE status = 'escalated')::bigint            AS escalated
            FROM coord.tasks
            WHERE plan_id = $1::uuid
            "#,
            &[&plan_id],
        )
        .await
        .map_err(|e| format!("Failed to compute task tallies: {}", e))?;

    let total_tasks: i64 = task_counts_row.get(0);
    let completed_tasks: i64 = task_counts_row.get(1);
    let escalated_tasks: i64 = task_counts_row.get(2);

    // 2. Average confidence across every review row whose task belongs
    //    to the plan. Returns NULL when no reviews exist.
    let avg_row = conn
        .query_one(
            r#"
            SELECT AVG(r.confidence)::float8 AS avg_conf
            FROM coord.reviews r
            JOIN coord.tasks  t ON t.id = r.task_id
            WHERE t.plan_id = $1::uuid
            "#,
            &[&plan_id],
        )
        .await
        .map_err(|e| format!("Failed to compute avg confidence: {}", e))?;

    let avg_confidence: Option<f64> = avg_row.get(0);

    // 3. Knowledge count across every productivity_knowledge row whose
    //    `task_id` belongs to the plan.
    let knowledge_count_row = conn
        .query_one(
            r#"
            SELECT COUNT(*)::bigint
            FROM productivity_knowledge k
            JOIN coord.tasks t ON t.id = k.task_id
            WHERE t.plan_id = $1::uuid
            "#,
            &[&plan_id],
        )
        .await
        .map_err(|e| format!("Failed to count knowledge: {}", e))?;

    let knowledge_count: i64 = knowledge_count_row.get(0);

    // 4. Top knowledge entries (newest 5) — the panel's clickable list.
    let top_knowledge_rows = conn
        .query(
            r#"
            SELECT k.id::text, k.area, k.summary
            FROM productivity_knowledge k
            JOIN coord.tasks t ON t.id = k.task_id
            WHERE t.plan_id = $1::uuid
            ORDER BY k.created_at DESC
            LIMIT 5
            "#,
            &[&plan_id],
        )
        .await
        .map_err(|e| format!("Failed to list top knowledge: {}", e))?;

    let top_knowledge: Vec<TopKnowledgeEntry> = top_knowledge_rows
        .iter()
        .map(|r| TopKnowledgeEntry {
            id: r.get(0),
            area: r.get(1),
            summary: r.get(2),
        })
        .collect();

    // 5. Full review trail for every task in the plan, newest-first.
    let trail_rows = conn
        .query(
            r#"
            SELECT r.id::text, r.task_id::text, r.verdict, r.confidence, r.created_at::text
            FROM coord.reviews r
            JOIN coord.tasks t ON t.id = r.task_id
            WHERE t.plan_id = $1::uuid
            ORDER BY r.created_at DESC
            "#,
            &[&plan_id],
        )
        .await
        .map_err(|e| format!("Failed to list review trail: {}", e))?;

    let review_trail: Vec<ReviewTrailEntry> = trail_rows
        .iter()
        .map(|r| ReviewTrailEntry {
            review_id: r.get(0),
            task_id: r.get(1),
            verdict: r.get(2),
            confidence: r.get(3),
            created_at: r.get(4),
        })
        .collect();

    Ok(Reflection {
        plan_id: plan_id.to_string(),
        total_tasks,
        completed_tasks,
        escalated_tasks,
        avg_confidence,
        knowledge_count,
        top_knowledge,
        review_trail,
    })
}

async fn get_reflection_handler(
    State(state): State<Arc<ApiState>>,
    Path(plan_id): Path<String>,
) -> Result<Json<Reflection>, (StatusCode, String)> {
    compute_reflection(&state.app_state, &plan_id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

// =============================================================================
// Plan recommendations
// =============================================================================

/// One ranked plan candidate the Coordinator dashboard surfaces. The
/// reasoning is a server-generated cheap-rule string; no LLM here. The
/// LLM only consults inside the `/coordinate` loop itself.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRecommendation {
    pub plan_id: String,
    pub title: Option<String>,
    pub ready_task_count: i64,
    pub idle_session_count: i64,
    pub unconflicting_claim_count: i64,
    pub reasoning: String,
}

/// Compute the v1 plan-recommendation ranking. Order:
///
/// 1. Number of tasks ready / not assigned (descending).
/// 2. Tie-breaker: count of upcoming claims with no conflict against any
///    *active* file-registry holder (descending).
///
/// Returns the top 3. Empty when no plan has any ready / unassigned work.
pub async fn compute_plan_recommendations(
    app_state: &Arc<AppState>,
    session_manager: Option<Arc<crate::claude_session::SessionManager>>,
) -> Result<Vec<PlanRecommendation>, String> {
    let pg = &app_state.pg_db;

    // Active file-registry paths — anything currently being edited counts
    // as a conflict for upcoming claims on the same path.
    let active_info = app_state.file_registry_manager.info().await;
    let active_paths: HashSet<String> = active_info.into_iter().map(|i| i.file_path).collect();

    // Idle session count — sessions not currently assigned to any task.
    // Cheap pass over the SessionManager: every active session that the
    // task table doesn't list as `assigned_session_id` for an in-flight
    // task is idle.
    let live_sessions: Vec<String> = match &session_manager {
        Some(sm) => sm.list_active(),
        None => Vec::new(),
    };

    // Set of session ids that are currently busy (assigned to a non-terminal
    // task). One query, no join.
    let pool = pg.pool();
    let conn = pool
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;
    let busy_sessions_rows = conn
        .query(
            r#"
            SELECT DISTINCT assigned_session_id
            FROM coord.tasks
            WHERE assigned_session_id IS NOT NULL
              AND status IN ('assigned', 'running', 'review', 'needs_fix')
            "#,
            &[],
        )
        .await
        .map_err(|e| format!("Failed to load busy sessions: {}", e))?;
    let busy: HashSet<String> = busy_sessions_rows
        .into_iter()
        .filter_map(|r| r.get::<_, Option<String>>(0))
        .collect();
    let idle_session_count: i64 =
        live_sessions.iter().filter(|s| !busy.contains(*s)).count() as i64;

    // Live (non-archived) plans only — we never recommend a `done` or
    // `abandoned` plan as the next thing to start.
    let plans = pg.list_plans_filtered(false).await?;

    let mut candidates: Vec<PlanRecommendation> = Vec::new();
    for plan in plans {
        // Ready / not-yet-assigned tasks on this plan.
        let ready_row = conn
            .query_one(
                r#"
                SELECT COUNT(*)::bigint
                FROM coord.tasks
                WHERE plan_id = $1::uuid
                  AND status IN ('ready', 'pending')
                  AND assigned_session_id IS NULL
                "#,
                &[&plan.id],
            )
            .await
            .map_err(|e| format!("Failed to count ready tasks: {}", e))?;
        let ready_task_count: i64 = ready_row.get(0);

        if ready_task_count == 0 {
            continue;
        }

        // Upcoming claims for the plan that don't collide with any path
        // currently held by the active registry. Cheap intersection.
        let upcoming = app_state
            .upcoming_file_registry
            .snapshot_for_plan(&plan.id)
            .await;
        let unconflicting_claim_count: i64 = upcoming
            .iter()
            .filter(|c| !active_paths.contains(&c.path))
            .count() as i64;

        let reasoning = format!(
            "{} ready task{}, {} idle session{}, {} un-conflicting claim{}",
            ready_task_count,
            if ready_task_count == 1 { "" } else { "s" },
            idle_session_count,
            if idle_session_count == 1 { "" } else { "s" },
            unconflicting_claim_count,
            if unconflicting_claim_count == 1 {
                ""
            } else {
                "s"
            },
        );

        candidates.push(PlanRecommendation {
            plan_id: plan.id,
            title: plan.title,
            ready_task_count,
            idle_session_count,
            unconflicting_claim_count,
            reasoning,
        });
    }

    // Rank: ready desc, then unconflicting desc.
    candidates.sort_by(|a, b| {
        b.ready_task_count.cmp(&a.ready_task_count).then_with(|| {
            b.unconflicting_claim_count
                .cmp(&a.unconflicting_claim_count)
        })
    });
    candidates.truncate(3);

    Ok(candidates)
}

async fn plan_recommendations_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<PlanRecommendation>>, (StatusCode, String)> {
    let session_manager = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|sm| sm.inner().clone());
    match compute_plan_recommendations(&state.app_state, session_manager).await {
        Ok(out) => Ok(Json(out)),
        Err(e) => {
            warn!("plan_recommendations_handler: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, e))
        }
    }
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/productivity/reflection/{plan_id}",
            get(get_reflection_handler),
        )
        .route(
            "/productivity/plan-recommendations",
            get(plan_recommendations_handler),
        )
}
