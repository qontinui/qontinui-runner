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

use crate::commands::AppState;
use crate::database::pg::coordinator_decisions::CoordinatorDecisionRow;
use crate::database::pg::coordinator_leader::CoordinatorLeaderRow;
use crate::database::pg::plans::PlanRow;
use crate::database::pg::productivity_knowledge::KnowledgeHit;
use crate::database::pg::reviews::ReviewRow;
use crate::database::pg::tasks::TaskRow;
use crate::executor::upcoming_file_registry::UpcomingClaim;
use crate::terminal::TerminalManager;

/// Detail payload for a single task — bundles the row plus
/// upcoming-claim peers (other tasks claiming the same paths).
///
/// `latest_review_summary` is populated by Phase 3 when a `reviews` row
/// exists for the task: it's a one-paragraph "verdict + confidence + top
/// reason" digest. `worker_session_meta` remains `None` until Phase 5.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    pub task: TaskRow,
    pub claimers_by_path: HashMap<String, Vec<UpcomingClaim>>,
    pub latest_review_summary: Option<String>,
    pub worker_session_meta: Option<serde_json::Value>,
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

    Ok(Some(TaskDetail {
        task,
        claimers_by_path,
        latest_review_summary,
        worker_session_meta: None,
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
            let payload = serde_json::json!({
                "reviewId": review.id,
                "taskId": review.task_id,
                "userDecision": "approved",
            });
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
            let payload = serde_json::json!({
                "reviewId": review.id,
                "taskId": review.task_id,
                "userDecision": "rejected",
            });
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
    Ok(LeaderResponse {
        leader,
        lease_status,
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
        });
    }

    // mode == "claude_skill": legacy pty-spawn path.
    let runner_port = crate::mcp::types::runner_api_port(&app_state);
    let repo_path = resolve_runner_repo_path()
        .ok_or_else(|| "Failed to resolve qontinui-runner repo path".to_string())?;
    let initial_command = build_coordinator_initial_command(runner_port, plan_path.as_deref())?;
    let title = title_hint.unwrap_or_else(|| "Coordinator".to_string());

    let terminal_manager = app_handle
        .try_state::<Arc<TerminalManager>>()
        .ok_or_else(|| "TerminalManager not initialised".to_string())?
        .inner()
        .clone();

    let info = terminal_manager.create(
        Some(title),
        Some(repo_path),
        None,
        None,
        None,
        app_handle.clone(),
    )?;

    schedule_initial_command(terminal_manager, info.id.clone(), initial_command);

    Ok(LaunchResult {
        mode,
        terminal_id: Some(info.id),
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
#[tauri::command]
pub async fn spawn_worker_session(
    app_handle: tauri::AppHandle,
    title_hint: Option<String>,
) -> Result<LaunchResult, String> {
    let _app_state = require_app_state(&app_handle)?;

    let repo_path = resolve_runner_repo_path()
        .ok_or_else(|| "Failed to resolve qontinui-runner repo path".to_string())?;

    let terminal_manager = app_handle
        .try_state::<Arc<TerminalManager>>()
        .ok_or_else(|| "TerminalManager not initialised".to_string())?
        .inner()
        .clone();

    let title = title_hint.unwrap_or_else(|| next_worker_title(&terminal_manager));
    // PowerShell 5.1 doesn't accept `&&`, and the pty already starts
    // with `working_dir = Some(repo_path)` — no `cd` needed. Worker tabs
    // sit idle at the Claude prompt; Coordinator dispatches via
    // assign-task → POST /sessions/<id>/message. The
    // --dangerously-skip-permissions flag lets the worker run the
    // dispatched task without per-tool approval prompts (matches the
    // Coordinator's flag for the same reason).
    let initial_command = "claude --dangerously-skip-permissions".to_string();

    let info = terminal_manager.create(
        Some(title),
        Some(repo_path),
        None,
        None,
        None,
        app_handle.clone(),
    )?;

    schedule_initial_command(terminal_manager, info.id.clone(), initial_command);

    Ok(LaunchResult {
        mode: "worker".to_string(),
        terminal_id: Some(info.id),
    })
}
