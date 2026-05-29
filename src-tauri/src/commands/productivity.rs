//! Tauri commands for the Productivity tab — Phase 1 foundation.
//!
//! Exposes plan/task listing, task detail, and upcoming-claim queries to
//! the React frontend. The shapes returned here are wired to the frontend
//! types declared in productivity-stack plan §8 Phase 1 / "Type contract".
//!
//! Phase 1 is read-only: there is no automation, no Coordinator, no
//! review subsystem. `latest_review_summary` and `worker_session_meta` in
//! [`TaskDetail`] return `None` until Phases 3/5 land.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{Emitter, Manager};
use tracing::warn;
use uuid::Uuid;

use crate::commands::AppState;
use crate::database::pg::completion_reports::{CompletionReport, CompletionSource};
use crate::database::pg::coordinator_decisions::CoordinatorDecisionRow;
use crate::database::pg::coordinator_leader::CoordinatorLeaderRow;
use crate::database::pg::plans::PlanRow;
use crate::database::pg::productivity_knowledge::KnowledgeHit;
use crate::database::pg::reviews::ReviewRow;
use crate::database::pg::tasks::TaskRow;
use crate::executor::upcoming_file_registry::UpcomingClaim;
use crate::mcp::types::ApiState;
use crate::terminal::TerminalManager;

/// Detail payload for a single task — bundles the row plus
/// upcoming-claim peers (other tasks claiming the same paths).
///
/// `latest_review_summary` is populated by Phase 3 when a `reviews` row
/// exists for the task: it's a one-paragraph "verdict + confidence + top
/// reason" digest. `worker_session_meta` remains `None` until Phase 5.
///
/// `completion_report` / `completion_source` / `has_assignment_brief_extras`
/// are populated from `coord.tasks` directly (Phase 4 of the
/// productivity-coordinator-completion-reports plan) so the TaskDetailPanel
/// can render the structured handoff payload without a second round-trip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    pub task: TaskRow,
    pub claimers_by_path: HashMap<String, Vec<UpcomingClaim>>,
    pub latest_review_summary: Option<String>,
    pub worker_session_meta: Option<serde_json::Value>,
    /// Structured completion report for this task, if one has been written.
    /// `None` when `coord.tasks.completion_report` is null.
    pub completion_report: Option<CompletionReport>,
    /// Tag identifying the actor that produced the report. `None` when no
    /// report exists.
    pub completion_source: Option<CompletionSource>,
    /// True when `coord.tasks.assignment_brief_extras` is non-null — the
    /// briefing-preview panel uses this as a cheap gate before invoking
    /// `preview_assignment_brief`.
    pub has_assignment_brief_extras: bool,
}

/// Format a review row as a one-paragraph summary for `TaskDetail`'s
/// `latest_review_summary`. Trims overlong reasoning bodies so the
/// payload stays small.
fn format_review_summary(row: &ReviewRow) -> String {
    let snippet: String = row
        .reasoning
        .lines()
        .next()
        .unwrap_or(&row.reasoning)
        .chars()
        .take(200)
        .collect();
    format!(
        "{} (confidence {:.2}) — {}",
        row.verdict, row.confidence, snippet
    )
}

fn require_app_state(app_handle: &tauri::AppHandle) -> Result<Arc<AppState>, String> {
    match app_handle.try_state::<Arc<AppState>>() {
        Some(s) => Ok(s.inner().clone()),
        None => {
            warn!("productivity command: AppState not yet available");
            Err("Application state is not yet initialised".to_string())
        }
    }
}

/// List every plan row in the productivity stack, newest-first. Excludes
/// archived (`done`/`abandoned`) plans by default — the v1 callers (the
/// PlanTaskBoard's main list) want only live plans. Use
/// [`list_plans_filtered`] when the "Show archived" toggle is on.
#[tauri::command]
pub async fn list_plans(app_handle: tauri::AppHandle) -> Result<Vec<PlanRow>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.list_plans().await
}

/// List plans, newest-first. When `include_archived` is true, includes
/// rows whose status is `done` or `abandoned`. Used by the plan board's
/// "Show archived" toggle (Phase 5).
#[tauri::command]
pub async fn list_plans_filtered(
    app_handle: tauri::AppHandle,
    include_archived: bool,
) -> Result<Vec<PlanRow>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.list_plans_filtered(include_archived).await
}

/// Archive a plan: status → 'abandoned'. Returns true on update.
#[tauri::command]
pub async fn archive_plan(app_handle: tauri::AppHandle, plan_id: String) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.archive_plan(&plan_id).await
}

/// Restore an archived plan to a live status. Flips to `decomposed` if
/// the plan has tasks, otherwise to `vetted`.
#[tauri::command]
pub async fn unarchive_plan(app_handle: tauri::AppHandle, plan_id: String) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.unarchive_plan(&plan_id).await
}

/// List every task belonging to a plan, ordered by phase then sequence.
#[tauri::command]
pub async fn get_plan_tasks(
    app_handle: tauri::AppHandle,
    plan_id: String,
) -> Result<Vec<TaskRow>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.list_tasks_for_plan(&plan_id).await
}

/// Detail view for a single task — task row + upcoming claimers grouped
/// by path. Phase 1 leaves the review/worker fields as `None`; later
/// phases will populate them.
#[tauri::command]
pub async fn get_task_detail(
    app_handle: tauri::AppHandle,
    task_id: String,
) -> Result<Option<TaskDetail>, String> {
    let app_state = require_app_state(&app_handle)?;

    let task = match app_state.pg_db.get_task_by_id(&task_id).await? {
        Some(t) => t,
        None => return Ok(None),
    };

    let claimers_by_path = if task.expected_file_claims.is_empty() {
        HashMap::new()
    } else {
        app_state
            .upcoming_file_registry
            .check_paths(&task.expected_file_claims)
            .await
    };

    // Populate latestReviewSummary when a reviews row exists for the task.
    // The list is ordered newest-first; we summarise the head row.
    let latest_review_summary = match app_state.pg_db.get_reviews_for_task(&task_id).await {
        Ok(rows) => rows.into_iter().next().map(|r| format_review_summary(&r)),
        Err(e) => {
            warn!("get_task_detail: get_reviews_for_task failed: {}", e);
            None
        }
    };

    // Pull the structured completion report (Phase 4) so the
    // TaskDetailPanel can render it without a second Tauri round-trip.
    // Failures here are non-fatal: log and treat as "no report".
    let (completion_report, completion_source) =
        match app_state.pg_db.get_completion_report(&task_id).await {
            Ok(Some((report, source))) => (Some(report), Some(source)),
            Ok(None) => (None, None),
            Err(e) => {
                warn!("get_task_detail: get_completion_report failed: {}", e);
                (None, None)
            }
        };

    let has_assignment_brief_extras =
        match app_state.pg_db.get_assignment_brief_extras(&task_id).await {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => {
                warn!("get_task_detail: get_assignment_brief_extras failed: {}", e);
                false
            }
        };

    Ok(Some(TaskDetail {
        task,
        claimers_by_path,
        latest_review_summary,
        worker_session_meta: None,
        completion_report,
        completion_source,
        has_assignment_brief_extras,
    }))
}

/// Snapshot of the in-memory upcoming-file registry. When `plan_id` is
/// provided, only claims for that plan are returned.
#[tauri::command]
pub async fn get_upcoming_claims(
    app_handle: tauri::AppHandle,
    plan_id: Option<String>,
) -> Result<Vec<UpcomingClaim>, String> {
    let app_state = require_app_state(&app_handle)?;
    Ok(match plan_id {
        Some(pid) => {
            app_state
                .upcoming_file_registry
                .snapshot_for_plan(&pid)
                .await
        }
        None => app_state.upcoming_file_registry.snapshot().await,
    })
}

/// Bulk lookup: per-input-path list of upcoming claimers. Used by the
/// frontend to highlight overlap when the user hovers a path.
#[tauri::command]
pub async fn check_path_claims(
    app_handle: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<HashMap<String, Vec<UpcomingClaim>>, String> {
    let app_state = require_app_state(&app_handle)?;
    Ok(app_state.upcoming_file_registry.check_paths(&paths).await)
}

/// FTS search over `productivity_knowledge`. Used by the knowledge-browser
/// modal (Ctrl+Shift+K and the Productivity tab's Knowledge sub-view).
/// Vector search remains exposed via the PG layer for advanced callers
/// but is not surfaced through this Tauri command in v1.
///
/// `area_filter` is an exact-match on the row's `area`. `top_k` is
/// clamped server-side to a sane range (see `search_knowledge_fts`).
#[tauri::command]
pub async fn search_knowledge(
    app_handle: tauri::AppHandle,
    query: String,
    area_filter: Option<String>,
    top_k: i32,
) -> Result<Vec<KnowledgeHit>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .search_knowledge_fts(&query, area_filter.as_deref(), top_k)
        .await
}

// ============================================================================
// Coordinator dashboard — Phase 2
// ============================================================================

/// Fetch the most recent coordinator-decision rows for the Decision Log
/// panel. Optional `rule_filter` and `action_filter` mirror the dashboard's
/// filter dropdowns; passing `None` returns the unfiltered feed.
///
/// `limit` is clamped client-side; v1 dashboard requests 200.
#[tauri::command]
pub async fn get_coordinator_decisions(
    app_handle: tauri::AppHandle,
    limit: i64,
    rule_filter: Option<String>,
    action_filter: Option<String>,
) -> Result<Vec<CoordinatorDecisionRow>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .list_recent_coordinator_decisions(limit, rule_filter.as_deref(), action_filter.as_deref())
        .await
}

/// List unresolved escalations for the Coordinator dashboard's Escalations
/// panel. Backed by the partial index `idx_cd_open_escalations` so the
/// query is constant-time even as the decision log grows unbounded.
#[tauri::command]
pub async fn get_escalations(
    app_handle: tauri::AppHandle,
) -> Result<Vec<CoordinatorDecisionRow>, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.list_open_escalations().await
}

/// Mark an escalation resolved with a free-form `resolution` note. Returns
/// `true` if a row was updated (i.e. the decision existed and wasn't
/// already resolved).
#[tauri::command]
pub async fn resolve_escalation(
    app_handle: tauri::AppHandle,
    decision_id: String,
    resolution: String,
) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .resolve_coordinator_decision(&decision_id, &resolution)
        .await
}

// ============================================================================
// Recommendations queue — Phase 3
// ============================================================================

/// A row enriched with per-task / per-plan context for the Coordinator
/// dashboard's Recommendations panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    /// Embedded ReviewRow flattened so the TS contract (`Recommendation =
    /// ReviewRow & {...}`) lines up at the JSON layer. We expand each
    /// field rather than `#[serde(flatten)]` because flatten + camelCase
    /// rename interact badly with serde-tauri.
    pub id: String,
    pub task_id: String,
    pub reviewer_session_id: String,
    pub reviewed_session_id: String,
    pub verdict: String,
    pub confidence: f64,
    pub reasoning: String,
    pub diff_summary: Option<serde_json::Value>,
    pub test_results: Option<serde_json::Value>,
    pub user_decision: Option<String>,
    pub user_decided_at: Option<String>,
    pub created_at: String,
    pub task_description: String,
    pub plan_title: Option<String>,
}

/// List medium-confidence approved reviews waiting on the user's
/// approve/reject decision. Powers the Recommendations panel on the
/// Coordinator dashboard.
#[tauri::command]
pub async fn get_recommendations(
    app_handle: tauri::AppHandle,
) -> Result<Vec<Recommendation>, String> {
    let app_state = require_app_state(&app_handle)?;
    let rows = app_state.pg_db.list_pending_recommendations().await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        // Resolve the task description and plan title for display. Failures
        // here are non-fatal — render the row with placeholders rather than
        // dropping it.
        let (task_description, plan_title) = match app_state.pg_db.get_task_by_id(&r.task_id).await
        {
            Ok(Some(task)) => {
                let plan_title = match app_state.pg_db.get_plan_by_id(&task.plan_id).await {
                    Ok(Some(plan)) => plan.title,
                    Ok(None) => None,
                    Err(e) => {
                        warn!("get_recommendations: get_plan_by_id failed: {}", e);
                        None
                    }
                };
                (task.description, plan_title)
            }
            Ok(None) => (String::from("(task no longer exists)"), None),
            Err(e) => {
                warn!("get_recommendations: get_task_by_id failed: {}", e);
                (String::from("(task lookup failed)"), None)
            }
        };

        out.push(Recommendation {
            id: r.id,
            task_id: r.task_id,
            reviewer_session_id: r.reviewer_session_id,
            reviewed_session_id: r.reviewed_session_id,
            verdict: r.verdict,
            confidence: r.confidence,
            reasoning: r.reasoning,
            diff_summary: r.diff_summary,
            test_results: r.test_results,
            user_decision: r.user_decision,
            user_decided_at: r.user_decided_at,
            created_at: r.created_at,
            task_description,
            plan_title,
        });
    }

    Ok(out)
}

/// User accepts a medium-confidence recommendation: record the decision,
/// flip the linked task to `done`, and emit a `review-approved` event so
/// the dashboard / badges can refresh without polling.
#[tauri::command]
pub async fn approve_recommendation(
    app_handle: tauri::AppHandle,
    review_id: String,
) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    let updated = app_state
        .pg_db
        .record_review_user_decision(&review_id, "approved")
        .await?;

    if updated {
        if let Some(review) = app_state.pg_db.get_review_by_id(&review_id).await? {
            // Best-effort: review state is the operative state regardless
            // of the task transition outcome.
            if let Err(e) = app_state
                .pg_db
                .transition_task_status(&review.task_id, "review", "done")
                .await
            {
                warn!(
                    "approve_recommendation: transition review->done for task {} failed: {}",
                    review.task_id, e
                );
            }
            let payload =
                qontinui_runner_lib::tauri_event_payloads::RecommendationReviewDecisionPayload {
                    review_id: review.id,
                    task_id: review.task_id,
                    user_decision: "approved".to_string(),
                };
            if let Err(e) = app_handle.emit("review-approved", &payload) {
                warn!("approve_recommendation: emit failed: {}", e);
            }
        }
    }

    Ok(updated)
}

/// User declines a medium-confidence recommendation: record the rejection
/// and emit a `review-rejected` event. The task remains in its current
/// state for the user to triage manually.
#[tauri::command]
pub async fn reject_recommendation(
    app_handle: tauri::AppHandle,
    review_id: String,
) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    let updated = app_state
        .pg_db
        .record_review_user_decision(&review_id, "rejected")
        .await?;

    if updated {
        if let Some(review) = app_state.pg_db.get_review_by_id(&review_id).await? {
            let payload =
                qontinui_runner_lib::tauri_event_payloads::RecommendationReviewDecisionPayload {
                    review_id: review.id,
                    task_id: review.task_id,
                    user_decision: "rejected".to_string(),
                };
            if let Err(e) = app_handle.emit("review-rejected", &payload) {
                warn!("reject_recommendation: emit failed: {}", e);
            }
        }
    }

    Ok(updated)
}

// ============================================================================
// Reflection panel + plan recommendations — Phase 5
// ============================================================================

/// Per-plan reflection rollup — tasks tally, avg review confidence,
/// knowledge count, top knowledge entries, and the chronological
/// review trail. Powers the `<ReflectionPanel>` inline panel above the
/// plan board's task list.
#[tauri::command]
pub async fn get_reflection(
    app_handle: tauri::AppHandle,
    plan_id: String,
) -> Result<crate::mcp::reflection::Reflection, String> {
    let app_state = require_app_state(&app_handle)?;
    crate::mcp::reflection::compute_reflection(&app_state, &plan_id).await
}

/// Server-side cheap-rule plan ranking. Returns up to 3 candidate plans
/// the Coordinator dashboard can suggest as "next plan to start". The LLM
/// is not consulted here — the LLM only enters the picture inside the
/// `/coordinate` loop. Heuristic: rank by ready/unassigned tasks,
/// tie-break by un-conflicting upcoming claims.
#[tauri::command]
pub async fn get_plan_recommendations(
    app_handle: tauri::AppHandle,
) -> Result<Vec<crate::mcp::reflection::PlanRecommendation>, String> {
    let app_state = require_app_state(&app_handle)?;
    let session_manager = app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .map(|sm| sm.inner().clone());
    crate::mcp::reflection::compute_plan_recommendations(&app_state, session_manager).await
}

/// Acknowledge an LLM-driven advisory — flips the underlying
/// `coordinator_decisions.resolved` flag. Sibling to
/// [`resolve_escalation`] but works against any decision row,
/// including auto-acted advisories whose action is `advise-with-text`.
/// The user uses this from the dashboard's Advisories panel.
#[tauri::command]
pub async fn acknowledge_advisory(
    app_handle: tauri::AppHandle,
    decision_id: String,
) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .resolve_coordinator_decision(&decision_id, "acknowledged")
        .await
}

// ============================================================================
// Coordinator launch controls — Phases 1 & 2
// ============================================================================

/// Snapshot of the coordinator-leader lease + a discriminator the dashboard
/// uses to colour the status pill.
///
/// `lease_status` rules (computed against `chrono::Utc::now()`):
/// - `"active"`: `leased_until > NOW()` AND `renewed_at` within last 90s
/// - `"stale"`:  `leased_until > NOW()` but `renewed_at` older than 90s
/// - `"vacant"`: no leader row OR `leased_until <= NOW()`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderResponse {
    pub leader: Option<CoordinatorLeaderRow>,
    /// One of `"active" | "stale" | "vacant"`.
    pub lease_status: String,
    pub seconds_since_renew: Option<i64>,
}

/// Result returned by [`launch_coordinator_session`] / [`spawn_worker_session`].
///
/// - `terminal_id` is `Some(id)` only for the legacy `claude_skill` mode
///   (and for `spawn_worker_session`), where a pty was opened — the
///   frontend uses it to focus the new tab. For `mode: "rust"` (the
///   default Phase 1.5 path) the scheduler runs in-process so there is
///   no terminal to focus and the field is `None`.
/// - `mode` echoes back which path ran ("rust" | "claude_skill" |
///   "worker") so the frontend can branch its post-launch UX (reveal
///   terminal tab vs. just refresh the lease pill).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    pub mode: String,
    pub terminal_id: Option<String>,
    /// Phase 6: the `task_run_id` under which a worker is registered with
    /// `SessionManager`. `None` for `mode = "rust"` and `mode =
    /// "claude_skill"` paths (which don't allocate a worker).
    pub task_run_id: Option<String>,
}

/// Compute the `lease_status` discriminator for a `CoordinatorLeaderRow`.
/// `leased_until` and `renewed_at` come back from PG as text via `::text`
/// cast on `TIMESTAMPTZ`. PG's text format is `YYYY-MM-DD HH:MM:SS.ffffff+00`
/// — note the SPACE separator (not `T`) and the trailing `+NN` offset
/// (not RFC3339's `+NN:NN`). Parse both shapes so the dashboard pill works
/// regardless of which serialiser fed the row.
pub(crate) fn compute_lease_status_for_http(leader: &Option<CoordinatorLeaderRow>) -> String {
    compute_lease_status(leader)
}

pub(crate) fn compute_seconds_since_renew(leader: &Option<CoordinatorLeaderRow>) -> Option<i64> {
    let row = leader.as_ref()?;
    let renewed_at = parse_pg_timestamptz(&row.renewed_at)?;
    Some(
        chrono::Utc::now()
            .signed_duration_since(renewed_at)
            .num_seconds(),
    )
}

/// Parse a PG-style timestamptz text representation into UTC.
/// Accepts both:
/// - `2026-05-03 18:01:08.681561+00` (PG ::text cast — space, no-colon offset)
/// - `2026-05-03T18:01:08.681561+00:00` (strict RFC3339)
fn parse_pg_timestamptz(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // PG's ::text cast: `YYYY-MM-DD HH:MM:SS[.fff]+NN` (NN may be 1 or 2
    // digits, no colon). The `%#z` specifier accepts colon-less and
    // colon-bearing offsets; `%.f` accepts optional fractional seconds.
    if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%#z") {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    // Fallback: replace first space with T and try strict RFC3339 again
    // (handles older driver versions that emit colon-less offsets without
    // fractional seconds).
    if let Some(idx) = s.find(' ') {
        let mut rfc = String::with_capacity(s.len() + 1);
        rfc.push_str(&s[..idx]);
        rfc.push('T');
        rfc.push_str(&s[idx + 1..]);
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&rfc) {
            return Some(dt.with_timezone(&chrono::Utc));
        }
    }
    None
}

fn compute_lease_status(leader: &Option<CoordinatorLeaderRow>) -> String {
    let Some(row) = leader.as_ref() else {
        return "vacant".to_string();
    };
    let now = chrono::Utc::now();

    let leased_until = match parse_pg_timestamptz(&row.leased_until) {
        Some(dt) => dt,
        None => return "vacant".to_string(),
    };
    if leased_until <= now {
        return "vacant".to_string();
    }

    let renewed_at = match parse_pg_timestamptz(&row.renewed_at) {
        Some(dt) => dt,
        None => return "stale".to_string(),
    };
    let age = now.signed_duration_since(renewed_at);
    if age <= chrono::Duration::seconds(90) {
        "active".to_string()
    } else {
        "stale".to_string()
    }
}

#[cfg(test)]
mod lease_status_tests {
    use super::*;
    use crate::database::pg::coordinator_leader::CoordinatorLeaderRow;

    fn row(leased_until: &str, renewed_at: &str) -> Option<CoordinatorLeaderRow> {
        Some(CoordinatorLeaderRow {
            instance_id: "rust-test".to_string(),
            leased_until: leased_until.to_string(),
            acquired_at: "2026-05-03 17:59:43.892179+00".to_string(),
            renewed_at: renewed_at.to_string(),
        })
    }

    #[test]
    fn parses_pg_text_format_with_fractional_seconds() {
        let dt = parse_pg_timestamptz("2026-05-03 18:01:08.681561+00")
            .expect("PG text format with fractional + colon-less offset must parse");
        assert_eq!(dt.timestamp(), 1777831268);
    }

    #[test]
    fn parses_strict_rfc3339() {
        let dt = parse_pg_timestamptz("2026-05-03T18:01:08.681561+00:00")
            .expect("RFC3339 strict must still parse");
        assert_eq!(dt.timestamp(), 1777831268);
    }

    #[test]
    fn parses_pg_text_without_fractional() {
        let dt = parse_pg_timestamptz("2026-05-03 18:01:08+00")
            .expect("PG text without fractional must parse");
        assert_eq!(dt.timestamp(), 1777831268);
    }

    #[test]
    fn vacant_when_leader_none() {
        assert_eq!(compute_lease_status(&None), "vacant");
    }

    #[test]
    fn vacant_when_lease_expired() {
        let r = row("2020-01-01 00:00:00+00", "2020-01-01 00:00:00+00");
        assert_eq!(compute_lease_status(&r), "vacant");
    }

    #[test]
    fn active_when_lease_fresh_and_recently_renewed() {
        let now = chrono::Utc::now();
        let until = (now + chrono::Duration::seconds(60))
            .format("%Y-%m-%d %H:%M:%S%.6f+00")
            .to_string();
        let renewed = now.format("%Y-%m-%d %H:%M:%S%.6f+00").to_string();
        let r = row(&until, &renewed);
        assert_eq!(compute_lease_status(&r), "active");
    }

    #[test]
    fn stale_when_lease_fresh_but_renew_old() {
        let now = chrono::Utc::now();
        let until = (now + chrono::Duration::seconds(60))
            .format("%Y-%m-%d %H:%M:%S%.6f+00")
            .to_string();
        let renewed = (now - chrono::Duration::seconds(120))
            .format("%Y-%m-%d %H:%M:%S%.6f+00")
            .to_string();
        let r = row(&until, &renewed);
        assert_eq!(compute_lease_status(&r), "stale");
    }

    #[test]
    fn seconds_since_renew_none_when_no_leader() {
        assert_eq!(compute_seconds_since_renew(&None), None);
    }

    #[test]
    fn seconds_since_renew_matches_renewed_at_age() {
        let now = chrono::Utc::now();
        let renewed = (now - chrono::Duration::seconds(45))
            .format("%Y-%m-%d %H:%M:%S%.6f+00")
            .to_string();
        let until = (now + chrono::Duration::seconds(60))
            .format("%Y-%m-%d %H:%M:%S%.6f+00")
            .to_string();
        let r = row(&until, &renewed);
        let age = compute_seconds_since_renew(&r).expect("renewed_at parses");
        assert!(
            (44..=46).contains(&age),
            "expected ~45s since renew, got {}",
            age
        );
    }

    #[test]
    fn seconds_since_renew_none_when_renewed_at_unparseable() {
        let r = Some(CoordinatorLeaderRow {
            instance_id: "rust-test".to_string(),
            leased_until: "2026-05-03 18:01:08.681561+00".to_string(),
            acquired_at: "2026-05-03 17:59:43.892179+00".to_string(),
            renewed_at: "not-a-timestamp".to_string(),
        });
        assert_eq!(compute_seconds_since_renew(&r), None);
    }
}

/// Read the current coordinator-leader lease + computed status. Drives the
/// dashboard's "Coordinator: active/stale/vacant" status pill and the
/// enabled-state of the Start/Force-takeover button.
#[tauri::command]
pub async fn get_coordinator_leader(
    app_handle: tauri::AppHandle,
) -> Result<LeaderResponse, String> {
    let app_state = require_app_state(&app_handle)?;
    let leader = app_state.pg_db.current_coordinator_leader().await?;
    let lease_status = compute_lease_status(&leader);
    let seconds_since_renew = compute_seconds_since_renew(&leader);
    Ok(LeaderResponse {
        leader,
        lease_status,
        seconds_since_renew,
    })
}

/// Resolve `<workspace_root>/qontinui-runner` for use as the working dir
/// of the spawned terminal. Falls back to whatever
/// `current_project_path()` returns, since the terminal manager itself
/// also defaults there.
fn resolve_runner_repo_path() -> Option<String> {
    let workspace_root = crate::mcp::shared::current_project_path()?;
    let repo = std::path::Path::new(&workspace_root).join("qontinui-runner");
    if repo.is_dir() {
        Some(repo.to_string_lossy().to_string())
    } else {
        // Fallback: workspace root itself (terminal manager handles missing).
        Some(workspace_root)
    }
}

/// Build the wiring prompt the user would otherwise type by hand and
/// stage it on disk. Returns a single-line shell invocation that feeds
/// the prompt to `claude` via `Get-Content -Raw`.
///
/// Why temp-file + Get-Content rather than `cd <path> && claude "<prompt>"`:
/// 1. PowerShell 5.1 (the default Windows shell the pty inherits) does
///    NOT support `&&` as a chain operator — `cd ... && claude ...`
///    raises `The token '&&' is not a valid statement separator`.
/// 2. Multi-line prompts written into a pty are interpreted line-by-line
///    by PSReadLine; embedded `\n` characters break out of the quoted
///    arg and PowerShell tries to execute the prompt body as commands.
/// Bundling the prompt into a temp file sidesteps both — the pty sees
/// one short line that PowerShell parses cleanly, and `cd` is unneeded
/// because `TerminalManager::create` already starts the pty in
/// `working_dir = Some(repo_path)`.
fn build_coordinator_initial_command(
    runner_port: u16,
    plan_path: Option<&str>,
) -> Result<String, String> {
    let plan_line = match plan_path {
        Some(p) if !p.is_empty() => format!("\nPlan to schedule: {}", p),
        _ => String::new(),
    };
    let prompt = format!(
        "/coordinate\n\nCoordinate against THIS runner (port {port}, not 9876). \
All HTTP calls in your observe->decide->act loop must hit \
http://localhost:{port}/coordinator/state and /coordinator/act.{plan_line}\n",
        port = runner_port,
        plan_line = plan_line,
    );

    let dir = std::env::temp_dir().join("qontinui-launch-prompts");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create launch-prompt dir: {}", e))?;
    let file_name = format!("coordinator-{}.md", uuid::Uuid::new_v4());
    let path = dir.join(file_name);
    std::fs::write(&path, &prompt)
        .map_err(|e| format!("Failed to write launch-prompt file: {}", e))?;

    // PowerShell single-quoted strings don't expand variables and don't
    // interpret backslash escapes — safest quoting for a Windows path.
    // Double any literal single quotes per PS escape rules (rare on
    // temp paths but cheap defense).
    let escaped_path = path.to_string_lossy().replace('\'', "''");
    // --dangerously-skip-permissions is required for the Coordinator to
    // run its observe→decide→act loop autonomously. The /coordinate
    // skill is a trusted built-in role (see qontinui-claude-config/
    // .claude/commands/coordinate.md): cheap rules cover most cases,
    // destructive actions (kill-session, force-promote-to-worktree)
    // are server-enforced as advisory regardless of the agent's claim
    // (mcp/coordinator.rs::must_advise_only). Per the project CLAUDE.md
    // the flag now bypasses prompts for writes to .claude/, .git/, etc.
    Ok(format!(
        "claude --dangerously-skip-permissions (Get-Content -Raw '{}')",
        escaped_path
    ))
}

/// Auto-numbered "Worker N" title — finds the highest existing N across
/// open terminals matching `^Worker (\d+)$` and returns `Worker (N+1)`.
fn next_worker_title(manager: &TerminalManager) -> String {
    let mut max_n: u32 = 0;
    for info in manager.list() {
        let title = info.title.trim();
        if let Some(rest) = title.strip_prefix("Worker ") {
            if let Ok(n) = rest.trim().parse::<u32>() {
                if n > max_n {
                    max_n = n;
                }
            }
        }
    }
    format!("Worker {}", max_n + 1)
}

/// Spawn the post-init shell-line write that `terminal_create` doesn't
/// do for us when invoked via Tauri (the HTTP path at
/// `mcp/terminals.rs:115-126` does this; we replicate it here so the
/// dashboard's launch buttons get the same behaviour).
fn schedule_initial_command(
    manager: Arc<TerminalManager>,
    terminal_id: String,
    initial_command: String,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Some(session) = manager.get(&terminal_id) {
            let line = format!("{}\r\n", initial_command);
            if let Err(e) = session.write(line.as_bytes()) {
                warn!(
                    "launch session: failed to write initial command to {}: {}",
                    terminal_id, e
                );
            }
        } else {
            warn!(
                "launch session: terminal {} vanished before initial command write",
                terminal_id
            );
        }
    });
}

/// Start the coordinator. Two modes:
///
/// - `mode = "rust"` (default, Phase 1.5): flips the in-process Rust
///   scheduler's runtime-toggle flag to `true`. The scheduler task is
///   already running (started at boot in `mcp_api.rs`) — flipping the
///   flag makes its next tick acquire the lease and execute. No pty,
///   no Claude CLI dependency, no terminal tab. Returns
///   `terminal_id: None`.
///
/// - `mode = "claude_skill"`: spawns a fresh "Coordinator" terminal tab
///   running `claude "/coordinate ..."` with the runner port
///   pre-injected. Joshua's debug path — keeps working unchanged for
///   his personal setup. Returns the terminal id so the frontend can
///   focus the new tab.
///
/// Defends against double-launch by checking the leader lease first —
/// refuses when an existing lease is `active`. Stale and vacant leases
/// proceed.
#[tauri::command]
pub async fn launch_coordinator_session(
    app_handle: tauri::AppHandle,
    mode: Option<String>,
    plan_path: Option<String>,
    title_hint: Option<String>,
) -> Result<LaunchResult, String> {
    let app_state = require_app_state(&app_handle)?;
    let mode = mode.unwrap_or_else(|| "rust".to_string());

    // Reject unknown modes early — keeps the surface small and surfaces
    // typos in callers/tests immediately.
    if mode != "rust" && mode != "claude_skill" {
        return Err(format!(
            "Unknown coordinator mode {:?} (expected 'rust' or 'claude_skill')",
            mode
        ));
    }

    // Single-coordinator enforcement (defense-in-depth — frontend disables
    // the button anyway when status == "active"). Stale leases proceed —
    // the takeover happens transparently when the new Coordinator's
    // try_acquire_coordinator_lease fires.
    let leader = app_state.pg_db.current_coordinator_leader().await?;
    let status = compute_lease_status(&leader);
    if status == "active" {
        let id = leader
            .as_ref()
            .map(|l| l.instance_id.as_str())
            .unwrap_or("unknown");
        return Err(format!("Coordinator already running (instance {})", id));
    }

    if mode == "rust" {
        // Phase 1.5 path: flip the in-process scheduler's flag. The
        // scheduler task is started at boot in `mcp_api.rs` and stashed
        // its `CoordinatorSchedulerHandle` in Tauri-managed state.
        let handle = app_handle
            .try_state::<crate::coordinator::CoordinatorSchedulerHandle>()
            .ok_or_else(|| {
                "Coordinator scheduler handle not initialised — runner started without scheduler"
                    .to_string()
            })?
            .inner()
            .clone();
        let prev = handle.set_enabled(true);
        tracing::info!(
            "launch_coordinator_session(mode=rust): rust_scheduler_enabled flipped {}→true",
            prev
        );
        return Ok(LaunchResult {
            mode,
            terminal_id: None,
            task_run_id: None,
        });
    }

    // mode == "claude_skill": legacy pty-spawn path.
    let runner_port = crate::mcp::types::runner_api_port(&app_state);
    let primary_repo_path = resolve_runner_repo_path()
        .ok_or_else(|| "Failed to resolve qontinui-runner repo path".to_string())?;
    let initial_command = build_coordinator_initial_command(runner_port, plan_path.as_deref())?;
    let title = title_hint.unwrap_or_else(|| "Coordinator".to_string());

    let terminal_manager = app_handle
        .try_state::<Arc<TerminalManager>>()
        .ok_or_else(|| "TerminalManager not initialised".to_string())?
        .inner()
        .clone();

    // Phase 2 round 2 — Coordinator sessions always edit qontinui-runner
    // (same class as `spawn_worker_session`). Route through the shared
    // facade so flag-on lands in a worktree, flag-off keeps the primary
    // checkout.
    let (effective_repo_path, isolated_ctx) =
        crate::agent_worktree::isolated_edit::acquire_for_terminal(
            Some("qontinui-runner"),
            "Coordinator session",
            Some(primary_repo_path.clone()),
        )
        .await;
    let repo_path = effective_repo_path.unwrap_or(primary_repo_path);

    let info = terminal_manager.create(
        Some(title),
        Some(repo_path),
        None,
        None,
        None,
        app_handle.clone(),
    )?;

    if let Some(ctx) = isolated_ctx {
        if let Some(session) = terminal_manager.get(&info.id) {
            session.set_isolated_edit_ctx(ctx);
        }
    }

    schedule_initial_command(terminal_manager, info.id.clone(), initial_command);

    Ok(LaunchResult {
        mode,
        terminal_id: Some(info.id),
        task_run_id: None,
    })
}

/// Stop the in-process Rust coordinator scheduler — Phase 1.5 sibling of
/// [`launch_coordinator_session`] for `mode = "rust"`. Flips the
/// runtime-toggle flag back to `false`; the next tick observes the flag
/// and short-circuits without acquiring the lease. Returns the previous
/// value of the flag so the frontend can detect no-op stops.
///
/// Has no effect on `claude_skill`-mode pty sessions — those are user-
/// owned terminal tabs and the user closes them via the terminal UI.
#[tauri::command]
pub async fn stop_coordinator_session(app_handle: tauri::AppHandle) -> Result<bool, String> {
    let handle = app_handle
        .try_state::<crate::coordinator::CoordinatorSchedulerHandle>()
        .ok_or_else(|| {
            "Coordinator scheduler handle not initialised — runner started without scheduler"
                .to_string()
        })?
        .inner()
        .clone();
    let prev = handle.set_enabled(false);
    tracing::info!(
        "stop_coordinator_session: rust_scheduler_enabled flipped {}→false",
        prev
    );
    Ok(prev)
}

/// Spawn a fresh "Worker N" terminal tab running plain `claude` (no slash
/// command). Workers sit idle at the prompt until the Coordinator
/// dispatches an `assign-task` action.
///
/// Phase 6 wires the worker into `SessionManager.worker_sessions` under a
/// fresh `task_run_id`. Rule B picks the worker up via
/// `Observation.live_sessions`, and `coordinator/act.rs::send_message_to_worker`
/// dispatches assign-task briefs to the pty via `WorkerSession::send_user_message`.
#[tauri::command]
pub async fn spawn_worker_session(
    app_handle: tauri::AppHandle,
    title_hint: Option<String>,
) -> Result<LaunchResult, String> {
    let app_state = require_app_state(&app_handle)?;

    let primary_repo_path = resolve_runner_repo_path()
        .ok_or_else(|| "Failed to resolve qontinui-runner repo path".to_string())?;

    // Phase 2 — worker sessions always intend to edit qontinui-runner
    // (the Coordinator's `assign-task` dispatch targets runner code).
    // Route through `acquire_for_terminal` so this path stays in
    // lockstep with the other three terminal-spawn entry points.
    let intent_repo = "qontinui-runner".to_string();
    let (effective_repo_path, isolated_ctx) =
        crate::agent_worktree::isolated_edit::acquire_for_terminal(
            Some(&intent_repo),
            "Worker session",
            Some(primary_repo_path.clone()),
        )
        .await;
    let repo_path = effective_repo_path.unwrap_or(primary_repo_path);

    let terminal_manager = app_handle
        .try_state::<Arc<TerminalManager>>()
        .ok_or_else(|| "TerminalManager not initialised".to_string())?
        .inner()
        .clone();

    let session_manager = app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
        .ok_or_else(|| "SessionManager not initialised".to_string())?
        .inner()
        .clone();

    // Refresh Claude credentials before spawning so the worker pty doesn't
    // hit an expired-token prompt mid-brief. Mirrors `ClaudeSession::spawn`
    // at session.rs:187.
    {
        let ai_settings = crate::settings::get_ai_settings();
        let effective_config_dir =
            crate::ai_provider::get_effective_config_dir(&ai_settings.claude_cli);
        crate::ai_provider::oauth_refresh::try_ensure_valid_credentials(
            effective_config_dir.as_deref(),
        );
    }

    let task_run_id = Uuid::new_v4().to_string();

    let title = title_hint.unwrap_or_else(|| next_worker_title(&terminal_manager));
    // PowerShell 5.1 doesn't accept `&&`, and the pty already starts
    // with `working_dir = Some(repo_path)` — no `cd` needed. Worker tabs
    // sit idle at the Claude prompt; Coordinator dispatches via
    // assign-task → POST /sessions/<id>/message. The
    // --dangerously-skip-permissions flag lets the worker run the
    // dispatched task without per-tool approval prompts (matches the
    // Coordinator's flag for the same reason).
    let initial_command = "claude --dangerously-skip-permissions".to_string();

    // Phase 4: pre-size the worker PTY to match the dominant existing
    // zone so worker briefs land on a grid the same size the user will
    // eventually see (rather than the 120×30 default, which produces
    // mis-wrapped output until the user activates the tab and fit-addon
    // resizes the PTY). Falls back to (120, 30) when no other terminals
    // exist — same as `terminal_manager.create(None, None)` historically.
    let (dom_cols, dom_rows) = terminal_manager.dominant_zone_dims();
    let repo_path_for_detect = repo_path.clone();
    let cred_helper_repo_path = repo_path.clone();

    let info = terminal_manager.create(
        Some(title.clone()),
        Some(repo_path),
        None,
        Some(dom_cols),
        Some(dom_rows),
        app_handle.clone(),
    )?;

    // Phase 2 — park the isolated edit context on the TerminalSession
    // so the heartbeat + claim live as long as the worker PTY. Cleared
    // in `TerminalSession::close()`.
    if let Some(ctx) = isolated_ctx {
        if let Some(session) = terminal_manager.get(&info.id) {
            session.set_isolated_edit_ctx(ctx);
        }
    }

    // Save the title for coord registration before it is moved into
    // WorkerSession::new.
    let coord_purpose = title.clone();

    let worker = crate::claude_session::WorkerSession::new(
        task_run_id.clone(),
        info.id.clone(),
        title,
        terminal_manager.clone(),
    );
    let worker_arc = Arc::new(worker);
    let register_ok = match session_manager.register_worker(worker_arc.clone()) {
        Ok(()) => true,
        Err(e) => {
            warn!("spawn_worker_session: register_worker failed: {}", e);
            false
        }
    };

    // Phase 2 frontend mirror of `set_title_unless_worker`: tell the
    // React tree which terminalId is worker-backed so `ZoneGrid::onTitleChange`
    // can skip OSC 0/2 `renameTab` (and the paired `terminal_set_title`
    // invoke) for this tab. The backend gate alone keeps `/terminals` and
    // `GET /workers` pinned at `Worker N`, but without this event the
    // operator-facing tab strip drifts to the Claude CLI's OSC 0 title
    // because the FE has no listener for `terminal-title-changed`.
    // Race-safe: the FE's `useTerminalManager` buffers marks that arrive
    // before their `terminal-created` event lands.
    if register_ok {
        let payload = serde_json::json!({
            "terminalId": info.id.clone(),
            "taskRunId": task_run_id.clone(),
        });
        if let Err(e) = app_handle.emit("worker-registered", &payload) {
            warn!("spawn_worker_session: emit worker-registered failed: {}", e);
        }
    }

    // Unconditional coord registration — mirror the worker session into
    // the coordinator's session plane so the dashboard renders it. Errors
    // are logged and swallowed so a coord hiccup never blocks the worker.
    let mut coord_session_id: Option<uuid::Uuid> = None;
    {
        let session_registry = app_handle
            .try_state::<Arc<crate::session::SessionRegistry>>()
            .map(|s| s.inner().clone());
        if let Some(registry) = session_registry {
            let intent = crate::session::Intent {
                kind: crate::session::SessionKind::TerminalClaude,
                purpose: coord_purpose.clone(),
                // Phase 2 — worker sessions edit qontinui-runner.
                // Declaring the repo on the Intent makes coord.sessions
                // reflect the edit-intent that drove the worktree
                // allocation above.
                repo: Some(intent_repo.clone()),
                branch: None,
                declared_paths: vec![],
                share_output: true,
                redact_secrets: None,
            };
            match registry.register_external(intent) {
                Ok(coord_id) => {
                    coord_session_id = Some(coord_id);
                    if let Some(session) = terminal_manager.get(&info.id) {
                        session.set_coord_session_id(coord_id);
                        let rx = session.subscribe_output();
                        registry.attach_output_pipe(coord_id, rx, true);
                    }
                    tracing::info!(
                        terminal_id = %info.id,
                        coord_session = %coord_id,
                        "spawn_worker_session: registered coord session"
                    );
                }
                Err(e) => {
                    warn!(
                        terminal_id = %info.id,
                        error = %e,
                        "spawn_worker_session: coord session registration failed — worker unaffected"
                    );
                }
            }
        }
    }

    // "Coord as Deconflicter, not Dispatcher" Phase 1 (§4.3): create an
    // emergent `coord.tasks` row keyed by this worker's task_run_id so
    // the deconflicter and in-session advisory banner have something to
    // attach to. Best-effort — never block worker spawn on this. The
    // partial unique index `idx_tasks_emergent_per_session` (alembic-owned)
    // makes the INSERT idempotent on re-registration.
    if let Err(e) = app_state
        .pg_db
        .create_emergent_task(&task_run_id, "in_progress", "session_emergent", None)
        .await
    {
        warn!(
            "spawn_worker_session: create_emergent_task failed for task_run_id={}: {}",
            task_run_id, e
        );
    }

    // Phase 1: readline-observer task. Workers register in `Initializing`
    // (see `WorkerSession::new`); Coordinator's `idle_session_count`
    // reader filters Ready-only. We flip to Ready once one of:
    //   - the embedded Claude CLI emits its OSC 0 title (`"✳ Claude
    //     Code"` on startup, observed by the reader thread's grid parser
    //     in `terminal/session.rs` and surfaced via
    //     `subscribe_first_osc_title`); or
    //   - 8 s elapses (defensive fallback for Claude binaries that fail
    //     to emit an OSC 0 — e.g. auth prompts, missing binary). The
    //     Coordinator's Rule A "stuck session" detector then catches the
    //     downstream failure.
    let osc_title_rx = terminal_manager
        .get(&info.id)
        .and_then(|s| s.subscribe_first_osc_title());
    let observer_worker = worker_arc.clone();
    let observer_terminal_id = info.id.clone();
    tokio::spawn(async move {
        let trigger = match osc_title_rx {
            Some(rx) => {
                tokio::select! {
                    res = rx => {
                        match res {
                            Ok(()) => "osc_title",
                            // Sender dropped (session closed before any
                            // OSC fired). Don't flip Ready in that case;
                            // the worker will be cleaned up by
                            // close_all_sessions / cleanup_closed.
                            Err(_) => "closed",
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(8)) => "timeout",
                }
            }
            // No receiver available means the OSC already fired (or the
            // session vanished before we could subscribe). Either way,
            // skip the wait and let the worker proceed.
            None => "already_subscribed",
        };
        if trigger == "closed" {
            tracing::warn!(
                terminal_id = %observer_terminal_id,
                "spawn_worker_session: PTY closed before readline; worker stays Initializing"
            );
            return;
        }
        tracing::info!(
            terminal_id = %observer_terminal_id,
            trigger = trigger,
            "spawn_worker_session: worker transition Initializing → Ready"
        );
        observer_worker.set_state(crate::claude_session::state::SessionState::Ready);
    });

    {
        let detect_handle = app_handle.clone();
        tokio::spawn(async move {
            crate::repo_detection::check_and_emit_unregistered(
                detect_handle,
                Some(repo_path_for_detect),
            )
            .await;
        });
    }

    if let Some(sid) = coord_session_id {
        let session_id_str = sid.to_string();
        tokio::spawn(async move {
            crate::credential_helper::setup_credential_helper(
                &cred_helper_repo_path,
                &session_id_str,
            )
            .await;
        });
    }

    schedule_initial_command(terminal_manager, info.id.clone(), initial_command);

    Ok(LaunchResult {
        mode: "worker".to_string(),
        terminal_id: Some(info.id),
        task_run_id: Some(task_run_id),
    })
}

/// Read-only observability for the Workers panel: joins the SessionManager
/// worker registry with TerminalManager titles and the coordinator's view
/// of each worker's currently-assigned task. Mirrors
/// `mcp::coordinator::get_workers_http`. Empty list when no workers are
/// registered or when SessionManager / TerminalManager aren't initialised
/// yet (matches the "graceful degrade" pattern of the rest of this file).
#[tauri::command]
pub async fn list_workers(
    app_handle: tauri::AppHandle,
) -> Result<Vec<crate::productivity::workers::WorkerInfo>, String> {
    let app_state = require_app_state(&app_handle)?;

    let session_manager = match app_handle.try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        Some(sm) => sm.inner().clone(),
        None => return Ok(Vec::new()),
    };
    let terminal_manager = match app_handle.try_state::<Arc<TerminalManager>>() {
        Some(tm) => tm.inner().clone(),
        None => return Ok(Vec::new()),
    };

    Ok(crate::productivity::workers::build_worker_infos(
        &session_manager,
        &terminal_manager,
        &app_state.pg_db,
    )
    .await)
}

// ============================================================================
// Decompose Plan — Phase 3 (in-product replacement for /decompose-plan)
// ============================================================================
//
// Promotes the personal `qontinui-claude-config/.claude/commands/decompose-
// plan.md` slash command to a Tauri command so any qontinui user can run
// it from the UI without depending on the Claude CLI. Underlying
// implementation lives at `crate::productivity::decompose`.

/// Hint surfaced in the UI when the active LLM provider is unconfigured.
/// Same string for `Disabled` and `NoCredentials` — both flow through the
/// same Settings -> AI affordance.
const DECOMPOSE_PROVIDER_HINT: &str =
    "Configure an LLM provider in Settings → AI to use Decompose Plan.";

/// Decompose a plan markdown into a structured task graph + populate the
/// upcoming-file claim registry. Wraps the in-process `OneshotLlm` call
/// followed by a loopback POST to `/plans/decompose`.
///
/// Returns a structured payload with `planId` + `taskCount` so the UI can
/// render a success toast. On `OneshotError::Disabled` /
/// `OneshotError::NoCredentials`, returns the error string
/// [`DECOMPOSE_PROVIDER_HINT`] so the modal can show the affordance + a
/// link to Settings.
#[tauri::command]
pub async fn decompose_plan(
    app_handle: tauri::AppHandle,
    plan_path: String,
) -> Result<crate::productivity::decompose::DecomposeResult, String> {
    let app_state = require_app_state(&app_handle)?;
    let pg = app_state.pg_db.clone();
    let runner_port = crate::mcp::types::runner_api_port(&app_state);

    let llm = crate::ai_provider::oneshot::oneshot_for_settings();

    crate::productivity::decompose::decompose_plan_in_product(
        &pg,
        llm.as_ref(),
        &plan_path,
        runner_port,
    )
    .await
    .map_err(|e| match e {
        crate::productivity::decompose::DecomposeError::LlmDisabled
        | crate::productivity::decompose::DecomposeError::LlmNoCredentials => {
            DECOMPOSE_PROVIDER_HINT.to_string()
        }
        other => other.to_string(),
    })
}

// ============================================================================
// Backfill stuck tasks — coord-task-status-hygiene plan, Phase 3
// ============================================================================
//
// One-shot cross-reference of non-terminal `coord.tasks` rows against
// `git log` on `main` in the known qontinui ecosystem repos. Flags rows
// whose `expected_file_claims` overlap commits as candidates and emits the
// preview SQL. When `options.apply == true`, executes the UPDATEs inside a
// single transaction. Dry-run by default.
//
// Companion to Phase 2 of the same plan (the github-merge listener that
// catches FUTURE merges); this command addresses the EXISTING backlog.

/// Walk non-terminal `coord.tasks` rows, look at each row's
/// `expected_file_claims`, and ask `git log` whether any commit on `main`
/// in the scanned repos touched those paths. Newest matching commit per
/// task wins.
///
/// `options` defaults: scan the known qontinui ecosystem repos, look at
/// commits since `2026-04-01`, **dry-run** (emit SQL only). Pass
/// `apply=true` to execute the generated UPDATEs in one transaction.
#[tauri::command]
pub async fn backfill_completed_tasks_from_history(
    app_handle: tauri::AppHandle,
    options: Option<crate::productivity::backfill_tasks::BackfillOptions>,
) -> Result<crate::productivity::backfill_tasks::BackfillResult, String> {
    let app_state = require_app_state(&app_handle)?;
    let pg = app_state.pg_db.clone();
    let opts = options.unwrap_or_default();
    crate::productivity::backfill_tasks::run_backfill(&pg, opts)
        .await
        .map_err(|e| e.to_string())
}

// ============================================================================
// Auto-Review — Phase 4 (in-product replacement for /auto-review)
// ============================================================================
//
// Manual trigger for `productivity::review::auto_review_in_product`. The
// scheduler also fires reviews automatically on task completion (see
// `coordinator/scheduler.rs`); this Tauri command is for the per-row "Review
// now" button next to ReviewBadge on the Productivity tab and the terminal
// tab bar.

/// Hint surfaced in the UI when the active LLM provider is unconfigured.
/// Mirrors `DECOMPOSE_PROVIDER_HINT` so the modal can render a single
/// "configure LLM" affordance in either flow.
const REVIEW_PROVIDER_HINT: &str =
    "LLM provider not configured; review queued as 'user must verify' — go to Settings → AI to enable auto-review.";

/// Stable identifier the manual-trigger path uses as the reviewer's
/// `reviewer_session_id`. We don't have a real second session here (the
/// review is being fired from the dashboard, not from another worker), so
/// we hardcode an in-product identity. This passes the `/reviews` self-
/// review check as long as no worker has the same id.
const RUST_AUTO_REVIEWER_ID: &str = "rust-auto-reviewer";

/// Run an auto-review against `task_id` and persist the verdict.
///
/// On `OneshotError::Disabled` / `OneshotError::NoCredentials`, inserts a
/// stub `escalate / confidence=0` review row so the dashboard surfaces it,
/// and returns [`REVIEW_PROVIDER_HINT`] as the error string. The frontend
/// distinguishes "queued for user" from a real failure by the prefix
/// `"LLM provider not configured"`.
#[tauri::command]
pub async fn auto_review_task(
    app_handle: tauri::AppHandle,
    task_id: String,
) -> Result<crate::productivity::review::ReviewResult, String> {
    // P2 follow-up from productivity-coordinator-completion-reports plan
    // §7 Phase 1: validate task_id is a UUID before passing to PG. Avoids
    // the "raw error serializing parameter 0" log path that bites every
    // PG-backed Tauri command taking `task_id: String`.
    Uuid::parse_str(&task_id).map_err(|e| format!("invalid task_id uuid: {e}"))?;

    let app_state = require_app_state(&app_handle)?;
    let pg = app_state.pg_db.clone();
    let runner_port = crate::mcp::types::runner_api_port(&app_state);
    let llm = crate::ai_provider::oneshot::oneshot_for_settings();

    let result = crate::productivity::review::auto_review_in_product(
        &pg,
        llm.as_ref(),
        runner_port,
        &task_id,
        RUST_AUTO_REVIEWER_ID,
    )
    .await;

    match result {
        Ok(r) => {
            // Best-effort: emit `review-completed` so the dashboard /
            // ReviewBadge picks it up without polling. The pg insert path
            // doesn't emit (the HTTP route does); we add the emit here so
            // the manual-trigger UX matches the slash-command POST path.
            let _ = app_handle.emit(
                "review-completed",
                serde_json::json!({
                    "id": r.review_id,
                    "taskId": task_id,
                    "reviewerSessionId": RUST_AUTO_REVIEWER_ID,
                    "verdict": r.verdict,
                    "confidence": r.confidence,
                }),
            );
            Ok(r)
        }
        Err(crate::productivity::review::ReviewError::LlmDisabled(_)) => {
            // Queue a stub row so the dashboard reflects "user must verify"
            // — same UX as the DECOMPOSE flow's "no provider" path.
            let task = pg
                .get_task_by_id(&task_id)
                .await
                .map_err(|e| format!("get_task_by_id failed: {}", e))?
                .ok_or_else(|| format!("task {} not found", task_id))?;
            let reviewed_session_id = task.assigned_session_id.clone().ok_or_else(|| {
                format!(
                    "task {} has no assigned worker session — cannot queue stub review",
                    task_id
                )
            })?;
            // Skip the stub-row insert if the only candidate reviewer is
            // the worker itself — the SelfReview check would 409 anyway.
            if reviewed_session_id == RUST_AUTO_REVIEWER_ID {
                return Err(REVIEW_PROVIDER_HINT.to_string());
            }
            match crate::productivity::review::insert_disabled_stub_review(
                &pg,
                &task_id,
                RUST_AUTO_REVIEWER_ID,
                &reviewed_session_id,
            )
            .await
            {
                Ok(_) => {}
                Err(e) => {
                    warn!(
                        "auto_review_task: stub-row insert failed for {}: {}",
                        task_id, e
                    );
                }
            }
            Err(REVIEW_PROVIDER_HINT.to_string())
        }
        Err(other) => Err(other.to_string()),
    }
}

// ============================================================================
// Summarize / Rewind Session — Phase 5 (in-product replacements for
// /summarize-session and /rewind-session)
// ============================================================================
//
// Promotes the personal `qontinui-claude-config/.claude/commands/
// {summarize,rewind}-session.md` slash commands to Tauri commands so any
// qontinui user can run them from the UI without depending on the Claude
// CLI. Underlying implementations live at
// `crate::productivity::{summarize, rewind}`.

/// Summarize a finished AI session: extract learnings via the configured
/// `OneshotLlm` and persist them to `productivity_knowledge`. The slash
/// command's §1 verdict-driven Outcome-tag rule is enforced server-side
/// via the system prompt (see `productivity::summarize`).
///
/// On `OneshotError::Disabled` / `OneshotError::NoCredentials`, falls
/// back to inserting a single placeholder knowledge row (`area="other"`,
/// `body="LLM provider not configured; manual summary required."`) so
/// the user has a UI affordance and the session still surfaces in the
/// knowledge browser.
#[tauri::command]
pub async fn summarize_session(
    app_handle: tauri::AppHandle,
    task_run_id: String,
) -> Result<crate::productivity::summarize::SummarizeResult, String> {
    let app_state = require_app_state(&app_handle)?;
    let pg = app_state.pg_db.clone();
    let runner_port = crate::mcp::types::runner_api_port(&app_state);

    let llm = crate::ai_provider::oneshot::oneshot_for_settings();

    match crate::productivity::summarize::summarize_session_in_product(
        &pg,
        llm.as_ref(),
        runner_port,
        &task_run_id,
    )
    .await
    {
        Ok(result) => Ok(result),
        Err(crate::productivity::summarize::SummarizeError::LlmDisabled)
        | Err(crate::productivity::summarize::SummarizeError::LlmNoCredentials) => {
            // No LLM provider configured — write a single placeholder
            // knowledge row so the user sees the session in the
            // knowledge browser with a "manual summary required" hint.
            crate::productivity::summarize::write_placeholder_summary(runner_port, &task_run_id)
                .await
                .map_err(|e| e.to_string())
        }
        Err(other) => Err(other.to_string()),
    }
}

/// Rewind a failed AI session: restore the pre-edit file snapshots, kill
/// the failed worker, and (by default) spawn a replacement with
/// failure-context prepended. `no_replay = Some(true)` flips this to
/// "revert + leave tab empty for manual re-prompt" per the slash
/// command's `--no-replay` flag.
///
/// File-restore + kill are LLM-independent so a disabled provider
/// doesn't break them; the summarize step (which builds the
/// failure-context block) silently skips when no LLM is configured.
#[tauri::command]
pub async fn rewind_session(
    app_handle: tauri::AppHandle,
    task_run_id: String,
    no_replay: Option<bool>,
) -> Result<crate::productivity::rewind::RewindResult, String> {
    let app_state = require_app_state(&app_handle)?;
    let pg = app_state.pg_db.clone();
    let runner_port = crate::mcp::types::runner_api_port(&app_state);

    let llm = crate::ai_provider::oneshot::oneshot_for_settings();

    let options = crate::productivity::rewind::RewindOptions {
        replay: !no_replay.unwrap_or(false),
    };

    crate::productivity::rewind::rewind_session_in_product(
        &pg,
        llm.as_ref(),
        runner_port,
        &task_run_id,
        options,
    )
    .await
    .map_err(|e| e.to_string())
}

// ============================================================================
// Completion reports (Phase 1 of
// productivity-coordinator-completion-reports.md §3 / §7)
// ============================================================================

/// Resolve `Arc<ApiState>` from the Tauri app handle so completion-report
/// helpers that need `app_handle.emit` and `SessionManager` access can run
/// from a `#[tauri::command]` body. Returns a 'string-typed' error on
/// failure for parity with existing `require_app_state`.
fn require_api_state(app_handle: &tauri::AppHandle) -> Result<Arc<ApiState>, String> {
    match app_handle.try_state::<Arc<ApiState>>() {
        Some(s) => Ok(s.inner().clone()),
        None => Err(
            "ApiState is not yet initialised — submit_task_completion_report \
             requires the HTTP API server to be running"
                .to_string(),
        ),
    }
}

/// Worker / dashboard self-attestation Tauri mirror of
/// `POST /tasks/{id}/report`. The frontend can call this directly so it
/// doesn't have to round-trip through the local HTTP server.
#[tauri::command]
pub async fn submit_task_completion_report(
    app_handle: tauri::AppHandle,
    task_id: String,
    report: CompletionReport,
) -> Result<TaskRow, String> {
    Uuid::parse_str(&task_id).map_err(|e| format!("invalid task_id uuid: {e}"))?;

    let app_state = require_app_state(&app_handle)?;
    let pg = app_state.pg_db.clone();

    pg.write_completion_report(&task_id, &report, CompletionSource::SessionSelfReport)
        .await?;

    if let Err(e) = app_handle.emit(
        "completion-report-written",
        serde_json::json!({
            "taskId": task_id,
            "source": CompletionSource::SessionSelfReport.as_str(),
        }),
    ) {
        warn!("emit completion-report-written failed: {}", e);
    }

    pg.get_task_by_id(&task_id)
        .await?
        .ok_or_else(|| format!("task {} disappeared after write", task_id))
}

/// Read the structured completion report (and source tag) for a task.
#[tauri::command]
pub async fn get_task_completion_report(
    app_handle: tauri::AppHandle,
    task_id: String,
) -> Result<Option<(CompletionReport, CompletionSource)>, String> {
    Uuid::parse_str(&task_id).map_err(|e| format!("invalid task_id uuid: {e}"))?;
    let app_state = require_app_state(&app_handle)?;
    app_state.pg_db.get_completion_report(&task_id).await
}

/// Server-side preview of the assignment brief that Rule B would inject
/// into the worker's first message for this task. Calls
/// `coordinator::act::build_assignment_brief` so the rendered Markdown is,
/// by construction, identical to what the worker will see when the task
/// is assigned.
///
/// Returns an empty string when the task has no `assignment_brief_extras`
/// AND no `WORKER_ADDED_DEPENDENCY` audit row — i.e. no brief would be
/// composed (fresh, no-deps assignment). The dashboard renders nothing in
/// that case.
#[tauri::command]
pub async fn preview_assignment_brief(
    app_handle: tauri::AppHandle,
    task_id: String,
) -> Result<String, String> {
    Uuid::parse_str(&task_id).map_err(|e| format!("invalid task_id uuid: {e}"))?;

    let api_state = require_api_state(&app_handle)?;

    // Cheap pre-check: if there are no upstreams stashed AND no audit row,
    // there's nothing to render. `build_assignment_brief` would fall through
    // to `task.description`-only, which the dashboard already shows in the
    // task detail header — surfacing it in the brief panel just duplicates.
    let pg = &api_state.app_state.pg_db;
    let has_extras = pg
        .get_assignment_brief_extras(&task_id)
        .await
        .map(|o| o.is_some())
        .unwrap_or(false);
    let has_audit = pg
        .latest_worker_added_dependency_for_task(&task_id)
        .await
        .map(|o| o.is_some())
        .unwrap_or(false);
    if !has_extras && !has_audit {
        return Ok(String::new());
    }

    crate::coordinator::act::build_assignment_brief(&api_state, &task_id).await
}

/// Worker-declared emergent dependency Tauri mirror of
/// `POST /tasks/{id}/add-dependency`. Shares the
/// `mcp::completion_reports::add_dependency_inner` business logic with the
/// HTTP handler. From the desktop UI there is no calling-session ownership
/// to enforce; pass `None` to skip the check.
#[tauri::command]
pub async fn add_task_dependency(
    app_handle: tauri::AppHandle,
    task_id: String,
    upstream_task_id: String,
    reason: String,
) -> Result<TaskRow, String> {
    Uuid::parse_str(&task_id).map_err(|e| format!("invalid task_id uuid: {e}"))?;
    Uuid::parse_str(&upstream_task_id)
        .map_err(|e| format!("invalid upstream_task_id uuid: {e}"))?;

    let api_state = require_api_state(&app_handle)?;
    crate::mcp::completion_reports::add_dependency_inner(
        &api_state,
        &task_id,
        &upstream_task_id,
        &reason,
        None,
    )
    .await
    .map_err(|(_, msg)| msg)
}

// ---------------------------------------------------------------------------
// Row 9 Phase 4 — fleet health + alerts (CoordinatorDashboard panel).
//
// coord publishes the latest per-machine snapshot to the `fleet-health`
// JetStream KV bucket *and* fans `events.fleet.health.<id>` over the
// Redis/JS bridge on each 30s poll. The browser can't speak NATS, so the
// dashboard reads coord's HTTP rollup instead: `/coord/fleet/health`
// (per-machine state + active-alert severity counts) + `/coord/alerts`
// (the firing list). This command is a thin proxy so the panel doesn't
// need to know the coord URL or handle CORS.
// ---------------------------------------------------------------------------

/// Resolve coord's HTTP base. Same source-of-truth chain as
/// `mcp::agent_worktrees::coord_http_base`: env `COORD_HTTP_URL` →
/// profile `coord_url` (ws→http via `coord_ws_to_http`) → default
/// `http://localhost:9870`.
fn coord_http_base_for_fleet() -> String {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        if !v.is_empty() {
            return v;
        }
    }
    // `qontinui_runner_lib::profiles` (not `crate::profiles`) — same
    // proven path as mcp::agent_worktrees::coord_http_base; `crate::`
    // doesn't resolve `profiles` from this lib compilation unit.
    if let Some(ws) = qontinui_runner_lib::profiles::load().coord_url.as_deref() {
        return crate::agent_worktree::coord_ws_to_http(ws);
    }
    "http://localhost:9870".to_string()
}

/// `get_fleet_health` — fetch coord's fleet-health rollup + active
/// alerts in one call for the dashboard panel. Returns the merged JSON
/// `{ health: <coord /fleet/health>, alerts: [...], coordBase }`.
/// Errors are returned as `Err(String)` so the panel can render a
/// retriable error state (coord down ≠ runner down).
#[tauri::command]
pub async fn get_fleet_health() -> Result<serde_json::Value, String> {
    let base = coord_http_base_for_fleet();
    let base = base.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    let health: serde_json::Value = client
        .get(format!("{base}/coord/fleet/health"))
        .send()
        .await
        .map_err(|e| format!("GET /coord/fleet/health: {e}"))?
        .error_for_status()
        .map_err(|e| format!("/coord/fleet/health status: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse /coord/fleet/health: {e}"))?;

    // Alerts are best-effort: a failure here still renders the machine
    // grid (the more load-bearing half).
    let alerts: serde_json::Value = match client
        .get(format!("{base}/coord/alerts"))
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => r
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({"alerts": []})),
        Err(e) => {
            warn!("get_fleet_health: /coord/alerts failed: {e}");
            serde_json::json!({"alerts": []})
        }
    };

    Ok(serde_json::json!({
        "health": health,
        "alerts": alerts.get("alerts").cloned().unwrap_or(serde_json::json!([])),
        "coordBase": base,
    }))
}

// ============================================================================
// Coordination Phase 1B (§4.10) — Overlapping intents panel
// ============================================================================

/// One pair of agents whose declared_overlap_paths intersect. Computed
/// in-process from the active-agents list so the dashboard reads a
/// single coherent snapshot per refresh — no race between "list" and
/// "for each, compute peer overlaps."
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlappingIntentPair {
    pub agent_a: String,
    pub agent_b: String,
    pub intent_a: Option<String>,
    pub intent_b: Option<String>,
    pub overlapping_paths: Vec<String>,
}

/// List unique unordered pairs of active agents whose declared
/// overlap-path sets intersect. Drives the Productivity dashboard's
/// "Overlapping intents" panel (Phase 1B §4.10).
///
/// Each pair is reported once: agents are ordered lexically so
/// (agent_a, agent_b) is stable across calls.
///
/// `limit` caps the underlying active-agents fetch — the panel is
/// informational and bounding it keeps the dashboard cheap even at
/// 300+ agents.
#[tauri::command]
pub async fn list_overlapping_intents(
    app_handle: tauri::AppHandle,
    limit: Option<i64>,
) -> Result<Vec<OverlappingIntentPair>, String> {
    let app_state = require_app_state(&app_handle)?;
    let cap = limit.unwrap_or(200);
    let agents = app_state.pg_db.list_active_agents_with_paths(cap).await?;

    let mut pairs: Vec<OverlappingIntentPair> = Vec::new();
    for i in 0..agents.len() {
        for j in (i + 1)..agents.len() {
            let a = &agents[i];
            let b = &agents[j];
            let Some(a_paths) = a.declared_overlap_paths.as_ref() else {
                continue;
            };
            let Some(b_paths) = b.declared_overlap_paths.as_ref() else {
                continue;
            };
            let overlap = compute_overlap(a_paths, b_paths);
            if overlap.is_empty() {
                continue;
            }
            // Stable lexical pair ordering.
            let (agent_a, agent_b, intent_a, intent_b) = if a.agent_id <= b.agent_id {
                (
                    a.agent_id.clone(),
                    b.agent_id.clone(),
                    a.intent.clone(),
                    b.intent.clone(),
                )
            } else {
                (
                    b.agent_id.clone(),
                    a.agent_id.clone(),
                    b.intent.clone(),
                    a.intent.clone(),
                )
            };
            pairs.push(OverlappingIntentPair {
                agent_a,
                agent_b,
                intent_a,
                intent_b,
                overlapping_paths: overlap,
            });
        }
    }
    Ok(pairs)
}

/// Two-pass glob-set intersection — same shape as the coord-side
/// `detect_overlap` so the dashboard's view matches what coord
/// publishes on `events.coord.overlap.detected`.
fn compute_overlap(a_paths: &[String], b_paths: &[String]) -> Vec<String> {
    use std::collections::BTreeSet;
    let a_set: BTreeSet<&String> = a_paths.iter().collect();
    let mut hits: BTreeSet<String> = BTreeSet::new();
    for p in b_paths {
        if a_set.contains(p) {
            hits.insert(p.clone());
        }
    }
    if hits.is_empty() {
        // Glob expansion: each a-side glob tested against each
        // b-side literal, and vice versa. Uses the same `glob-match`
        // crate as the trigger-system file watchers, so glob semantics
        // are consistent across the runner.
        for a_pat in a_paths {
            for p in b_paths {
                if glob_match::glob_match(a_pat, p) {
                    hits.insert(p.clone());
                }
            }
        }
        for b_pat in b_paths {
            for p in a_paths {
                if glob_match::glob_match(b_pat, p) {
                    hits.insert(p.clone());
                }
            }
        }
    }
    hits.into_iter().collect()
}

#[cfg(test)]
mod overlap_tests {
    use super::*;

    #[test]
    fn literal_intersection() {
        let r = compute_overlap(
            &["a/b.rs".to_string(), "c/d.rs".to_string()],
            &["x.rs".to_string(), "a/b.rs".to_string()],
        );
        assert_eq!(r, vec!["a/b.rs".to_string()]);
    }

    #[test]
    fn glob_intersection() {
        let r = compute_overlap(
            &["qontinui-web/backend/app/auth/**".to_string()],
            &["qontinui-web/backend/app/auth/token.py".to_string()],
        );
        assert_eq!(
            r,
            vec!["qontinui-web/backend/app/auth/token.py".to_string()]
        );
    }

    #[test]
    fn disjoint_empty() {
        let r = compute_overlap(&["a/b.rs".to_string()], &["x/y.rs".to_string()]);
        assert!(r.is_empty());
    }
}
