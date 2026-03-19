//! MCP API endpoints for worktree management.
//!
//! Provides tools for listing, reviewing, merging, comparing, and discarding
//! worktrees created by isolated workflow runs.

use std::path::Path;
use std::sync::Arc;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use super::types::{ApiResponse, ApiState, api_error};
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
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo = crate::mcp::shared::current_project_path()
        .ok_or_else(|| {
            (StatusCode::BAD_REQUEST, Json(api_error("No project path available")))
        })?;

    match worktree::list_worktrees(Path::new(&repo)) {
        Ok(paths) => {
            let managed: Vec<&String> = paths
                .iter()
                .filter(|p| p.contains(".worktrees"))
                .collect();
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
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    let mut result = json!({
        "summary": summary,
        "branch": request.branch_name,
        "source_branch": request.source_branch,
    });

    if request.full_diff {
        let diff = worktree::get_full_diff(
            repo_path,
            &request.branch_name,
            &request.source_branch,
        ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;
        result["full_diff"] = json!(diff);
    }

    Ok(Json(ApiResponse::success(result)))
}

/// POST /worktrees/merge — Merge a worktree branch back into its source.
pub async fn merge_worktree_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<MergeWorktreeRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let repo_path = Path::new(&request.repo_path);

    // First attempt a regular merge
    let result = worktree::merge_worktree(
        repo_path,
        &request.branch_name,
        &request.source_branch,
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    if result.success {
        info!("WORKTREE: Merge successful: {} into {}", request.branch_name, request.source_branch);
        return Ok(Json(ApiResponse::success(json!({
            "success": true,
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
    info!("WORKTREE: Starting AI-assisted merge for {} conflicts", result.conflicts.len());

    let merge_state = worktree::start_merge_with_conflicts(
        repo_path,
        &request.branch_name,
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

    if merge_state.success {
        return Ok(Json(ApiResponse::success(json!({
            "success": true,
            "merge_commit": merge_state.merge_commit,
            "summary": "Clean merge completed",
        }))));
    }

    // Build AI merge prompt
    let diff_summary = worktree::get_worktree_diff_summary(
        repo_path, &request.branch_name, &request.source_branch,
    ).unwrap_or_default();

    let full_diff = worktree::get_full_diff(
        repo_path, &request.branch_name, &request.source_branch,
    ).unwrap_or_default();

    let workflow_name = request.workflow_name.as_deref().unwrap_or("Unknown workflow");

    let prompt = worktree::build_ai_merge_prompt(
        &request.branch_name, &request.source_branch,
        &diff_summary, &full_diff, workflow_name,
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
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))?;

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
            repo_path, &branch_info.branch_name, &request.source_branch,
        ).unwrap_or_else(|e| format!("Failed to get diff: {}", e));

        let result_summary = if let Some(ref task_run_id) = branch_info.task_run_id {
            state.app_state.checkpoint_db
                .get_task_run(task_run_id)
                .ok()
                .flatten()
                .map(|run| {
                    format!("Status: {}, Summary: {}",
                        run.status, run.summary.as_deref().unwrap_or("none"))
                })
                .unwrap_or_else(|| "No summary available".to_string())
        } else {
            "No task run linked".to_string()
        };

        branch_data.push((branch_info.branch_name.clone(), diff_summary, result_summary));
    }

    let prompt = worktree::build_comparison_prompt(
        &branch_data, &request.workflow_name, &request.source_branch,
    );

    Ok(Json(ApiResponse::success(json!({
        "comparison_prompt": prompt,
        "branch_count": request.branches.len(),
        "branches": request.branches.iter().map(|b| &b.branch_name).collect::<Vec<_>>(),
    }))))
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
        .route("/worktrees/remove", post(remove_worktree_handler))
        .route("/worktrees/compare", post(compare_worktrees_handler))
}
