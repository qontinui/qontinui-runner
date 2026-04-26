//! MCP API endpoints for worktree management.
//!
//! Provides tools for listing, reviewing, merging, comparing, and discarding
//! worktrees created by isolated workflow runs.

use axum::extract::{Json, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

use super::types::{api_error, ApiResponse, ApiState};
use crate::worktree;

// =============================================================================
// Request/Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct WorktreeDiffRequest {
    pub branch_name: String,
    pub source_branch: String,
    pub repo_path: String,
    #[serde(default)]
    pub full_diff: bool,
}

#[derive(Debug, Deserialize)]
pub struct MergeWorktreeRequest {
    pub branch_name: String,
    pub source_branch: String,
    pub repo_path: String,
    #[serde(default)]
    pub ai_resolve: bool,
    pub workflow_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveWorktreeRequest {
    pub worktree_path: String,
    pub branch_name: String,
    pub repo_path: String,
    #[serde(default)]
    pub delete_branch: bool,
}

#[derive(Debug, Deserialize)]
pub struct CompareWorktreesRequest {
    pub branches: Vec<BranchInfo>,
    pub source_branch: String,
    pub repo_path: String,
    pub workflow_name: String,
}

#[derive(Debug, Deserialize)]
pub struct BranchInfo {
    pub branch_name: String,
    pub task_run_id: Option<String>,
}

// =============================================================================
// Handlers
// =============================================================================

/// GET /worktrees — List all managed worktrees.
pub async fn list_worktrees_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo = crate::mcp::shared::current_project_path().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error("No project path available")),
        )
    })?;

    match worktree::list_worktrees(Path::new(&repo)) {
        Ok(paths) => {
            let managed: Vec<&String> = paths.iter().filter(|p| p.contains(".worktrees")).collect();
            Ok(Json(ApiResponse::success(json!({
                "worktrees": managed,
                "total": managed.len(),
            }))))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// POST /worktrees/diff — Get diff summary for a worktree branch.
pub async fn get_worktree_diff_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<WorktreeDiffRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo_path = Path::new(&request.repo_path);

    let summary = worktree::get_worktree_diff_summary(
        repo_path,
        &request.branch_name,
        &request.source_branch,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let mut result = json!({
        "summary": summary,
        "branch": request.branch_name,
        "source_branch": request.source_branch,
    });

    if request.full_diff {
        let diff = worktree::get_full_diff(repo_path, &request.branch_name, &request.source_branch)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
        result["full_diff"] = json!(diff);
    }

    Ok(Json(ApiResponse::success(result)))
}

/// POST /worktrees/merge — Merge a worktree branch back into its source.
///
/// Phase F: returns HTTP 409 with a structured `MergeBlockedDetails` payload
/// when the destination working tree has uncommitted edits that the merge
/// would overwrite. Callers should show a "force merge" prompt and, if the
/// user confirms, call `POST /worktrees/merge-force`.
pub async fn merge_worktree_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<MergeWorktreeRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo_path = Path::new(&request.repo_path);

    // First attempt a regular merge
    let result = worktree::merge_worktree(repo_path, &request.branch_name, &request.source_branch)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    // Phase F: pre-merge guard fired — destination has dirty overlap.
    // 409 Conflict semantically: the resource state prevents the operation.
    if let Some(blocked) = result.blocked {
        let owning_sessions =
            attribute_files_to_sessions(&state.app_state.pg_db, &blocked.conflicting_files).await;
        info!(
            "WORKTREE: pre-merge guard blocked merge of {} into {} ({} files, {} sessions)",
            request.branch_name,
            request.source_branch,
            blocked.conflicting_files.len(),
            owning_sessions.len()
        );
        // Use the typed `hint` slot on ApiResponse (sibling to `error`) to
        // carry the structured payload — keeps the envelope shape consistent
        // with other handlers, and clients can inspect `hint.status ==
        // "blocked"` to dispatch on the new failure mode.
        let hint = json!({
            "status": "blocked",
            "conflicting_files": blocked.conflicting_files,
            "owning_sessions": owning_sessions,
            "recovery_hint": "Call POST /worktrees/merge-force to auto-stash dirty files and merge.",
        });
        let mut resp: ApiResponse<()> = api_error(result.summary.clone());
        resp.hint = Some(hint);
        return Err((StatusCode::CONFLICT, Json(resp)));
    }

    if result.success {
        info!(
            "WORKTREE: Merge successful: {} into {}",
            request.branch_name, request.source_branch
        );
        return Ok(Json(ApiResponse::success(json!({
            "success": true,
            "status": "merged",
            "merge_commit": result.merge_commit,
            "summary": result.summary,
        }))));
    }

    // Merge has conflicts
    if !request.ai_resolve {
        return Ok(Json(ApiResponse::success(json!({
            "success": false,
            "conflicts": result.conflicts,
            "summary": result.summary,
            "hint": "Set ai_resolve: true to use AI-assisted conflict resolution",
        }))));
    }

    // AI-assisted merge: start the merge with conflicts in working directory
    info!(
        "WORKTREE: Starting AI-assisted merge for {} conflicts",
        result.conflicts.len()
    );

    let merge_state = worktree::start_merge_with_conflicts(repo_path, &request.branch_name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    if merge_state.success {
        return Ok(Json(ApiResponse::success(json!({
            "success": true,
            "merge_commit": merge_state.merge_commit,
            "summary": "Clean merge completed",
        }))));
    }

    // Build AI merge prompt
    let diff_summary = worktree::get_worktree_diff_summary(
        repo_path,
        &request.branch_name,
        &request.source_branch,
    )
    .unwrap_or_default();

    let full_diff =
        worktree::get_full_diff(repo_path, &request.branch_name, &request.source_branch)
            .unwrap_or_default();

    let workflow_name = request
        .workflow_name
        .as_deref()
        .unwrap_or("Unknown workflow");

    let prompt = worktree::build_ai_merge_prompt(
        &request.branch_name,
        &request.source_branch,
        &diff_summary,
        &full_diff,
        workflow_name,
        Some(&merge_state.conflicts),
    );

    Ok(Json(ApiResponse::success(json!({
        "success": false,
        "needs_ai_resolution": true,
        "conflicts": merge_state.conflicts,
        "ai_merge_prompt": prompt,
        "repo_path": request.repo_path,
        "summary": format!("{} conflict(s) need AI resolution", merge_state.conflicts.len()),
    }))))
}

/// POST /worktrees/remove — Remove a worktree and optionally its branch.
pub async fn remove_worktree_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<RemoveWorktreeRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    worktree::remove_worktree(
        Path::new(&request.repo_path),
        Path::new(&request.worktree_path),
        &request.branch_name,
        request.delete_branch,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    Ok(Json(ApiResponse::success(json!({
        "success": true,
        "removed": request.worktree_path,
        "branch_deleted": request.delete_branch,
    }))))
}

/// POST /worktrees/compare — Compare multiple worktree implementations.
pub async fn compare_worktrees_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CompareWorktreesRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo_path = Path::new(&request.repo_path);

    let mut branch_data: Vec<(String, String, String)> = Vec::new();

    for branch_info in &request.branches {
        let diff_summary = worktree::get_worktree_diff_summary(
            repo_path,
            &branch_info.branch_name,
            &request.source_branch,
        )
        .unwrap_or_else(|e| format!("Failed to get diff: {}", e));

        let result_summary = if let Some(ref task_run_id) = branch_info.task_run_id {
            state
                .app_state
                .pg_db
                .get_task_run(task_run_id)
                .await
                .ok()
                .flatten()
                .map(|run| {
                    format!(
                        "Status: {}, Summary: {}",
                        run.status,
                        run.summary.as_deref().unwrap_or("none")
                    )
                })
                .unwrap_or_else(|| "No summary available".to_string())
        } else {
            "No task run linked".to_string()
        };

        branch_data.push((
            branch_info.branch_name.clone(),
            diff_summary,
            result_summary,
        ));
    }

    let prompt = worktree::build_comparison_prompt(
        &branch_data,
        &request.workflow_name,
        &request.source_branch,
    );

    Ok(Json(ApiResponse::success(json!({
        "comparison_prompt": prompt,
        "branch_count": request.branches.len(),
        "branches": request.branches.iter().map(|b| &b.branch_name).collect::<Vec<_>>(),
    }))))
}

/// POST /worktrees/merge-force — Force-merge a worktree branch, auto-stashing
/// any dirty destination files first. The stash ref is returned so the user
/// can recover their work with `git stash pop <ref>`.
///
/// Use after a regular `/worktrees/merge` returned 409 with `status:
/// "blocked"` AND the user has confirmed they want to proceed despite the
/// risk of clobbering sibling work.
pub async fn merge_worktree_force_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<MergeWorktreeRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo_path = Path::new(&request.repo_path);

    let result =
        worktree::merge_worktree_force(repo_path, &request.branch_name, &request.source_branch)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    if result.merge.success {
        info!(
            "WORKTREE: force-merge succeeded: {} into {} (stashed {} file(s) as {:?})",
            request.branch_name,
            request.source_branch,
            result.stashed_files.len(),
            result.stash_ref,
        );
        let stash_note = result.stash_ref.as_ref().map(|s| {
            format!(
                "Your dirty changes were stashed before merge. Recover with: git stash pop {}",
                s
            )
        });
        return Ok(Json(ApiResponse::success(json!({
            "success": true,
            "status": "merged",
            "merge_commit": result.merge.merge_commit,
            "summary": result.merge.summary,
            "stash_ref": result.stash_ref,
            "stashed_files": result.stashed_files,
            "stash_note": stash_note,
        }))));
    }

    // Real merge conflicts (after stash). Stash is preserved; surface the ref.
    Ok(Json(ApiResponse::success(json!({
        "success": false,
        "status": "conflicted",
        "conflicts": result.merge.conflicts,
        "summary": result.merge.summary,
        "stash_ref": result.stash_ref,
        "stashed_files": result.stashed_files,
    }))))
}

/// Phase F: best-effort attribution of a list of conflicting files to the
/// sessions that touched them. Looks up `session_touched_files` in PG and
/// groups by `task_run_id`. Files with no DB record (manual edits, sessions
/// from before Phase A) are bucketed under a synthetic `"(unknown)"`
/// task_run_id so the UI still surfaces them.
async fn attribute_files_to_sessions(
    pg_db: &crate::database::pg::PgDb,
    conflicting_files: &[String],
) -> Vec<worktree::OwningSession> {
    use std::collections::BTreeMap;

    // Most-recent-first ordering from PG; we dedup by task_run_id and keep
    // each session's full subset of touched files.
    let pairs = match pg_db.get_sessions_for_files(conflicting_files).await {
        Ok(pairs) => pairs,
        Err(e) => {
            tracing::warn!("attribute_files_to_sessions: PG lookup failed: {}", e);
            Vec::new()
        }
    };

    // (task_run_id, position-in-pairs-list) → set of files. position
    // preserves most-recent-first ordering when we emit.
    let mut by_session: BTreeMap<usize, (String, Vec<String>)> = BTreeMap::new();
    let mut session_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut attributed: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (file_path, task_run_id) in pairs {
        let idx = *session_index
            .entry(task_run_id.clone())
            .or_insert_with(|| by_session.len());
        let entry = by_session
            .entry(idx)
            .or_insert_with(|| (task_run_id.clone(), Vec::new()));
        if !entry.1.contains(&file_path) {
            entry.1.push(file_path.clone());
        }
        attributed.insert(file_path);
    }

    let mut sessions: Vec<worktree::OwningSession> = by_session
        .into_values()
        .map(|(task_run_id, touched_files)| worktree::OwningSession {
            task_run_id,
            touched_files,
        })
        .collect();

    // Files dirty in destination but with no PG record — bucket as "(unknown)".
    let orphans: Vec<String> = conflicting_files
        .iter()
        .filter(|f| !attributed.contains(*f))
        .cloned()
        .collect();
    if !orphans.is_empty() {
        sessions.push(worktree::OwningSession {
            task_run_id: "(unknown)".to_string(),
            touched_files: orphans,
        });
    }

    sessions
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/worktrees", get(list_worktrees_handler))
        .route("/worktrees/diff", post(get_worktree_diff_handler))
        .route("/worktrees/merge", post(merge_worktree_handler))
        .route("/worktrees/merge-force", post(merge_worktree_force_handler))
        .route("/worktrees/remove", post(remove_worktree_handler))
        .route("/worktrees/compare", post(compare_worktrees_handler))
}
