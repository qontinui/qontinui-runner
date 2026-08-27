//! MCP API endpoints for worktree management.
//!
//! Provides tools for listing, reviewing, merging, comparing, and discarding
//! worktrees created by isolated workflow runs.

use axum::extract::{Json, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

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

/// Query string for `GET /worktrees`.
///
/// `repo` is optional so the endpoint stays callable with no parameters at
/// all (its historical shape). When supplied it must be an absolute path to a
/// git repository, and the response describes exactly that repository.
#[derive(Debug, Default, Deserialize)]
pub struct ListWorktreesQuery {
    #[serde(default)]
    pub repo: Option<String>,
}

// =============================================================================
// Repo-root resolution
// =============================================================================

/// Child directory names never scanned for git repositories.
///
/// These are build output, dependency trees, tool state, or worktree scratch
/// space — never source clones. `qontinui-worktrees` and `.spawn-origin_main`
/// hold *linked* worktrees whose `git worktree list` output is already
/// reported by the clone that owns them, so scanning them would duplicate
/// every entry under a scratch checkout instead of under its real repo.
/// `target` / `target-pool` / `dist` / `node_modules` can be enormous, and
/// stat-ing them buys nothing.
const SKIPPED_SCAN_DIRS: &[&str] = &[
    ".spawn-origin_main",
    "qontinui-worktrees",
    ".claude",
    "node_modules",
    "target",
    "target-pool",
    "dist",
];

/// Upper bound on how many repo roots one scan will report. A workspace root
/// with more immediate children than this is almost certainly not the
/// umbrella directory we think it is; cap rather than shell out to git
/// hundreds of times.
const MAX_SCANNED_REPOS: usize = 32;

/// True when `path` is a git repository root. A normal clone has a `.git`
/// directory; a linked worktree or submodule has a `.git` *file* pointing at
/// the real gitdir. Both are valid `git worktree list` cwds, so test for the
/// entry's existence rather than its kind.
fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

/// Resolve the set of git repo roots to list worktrees for, starting at
/// `root`.
///
/// - If `root` is itself a git repository, that is the whole answer.
/// - Otherwise `root` is treated as an umbrella workspace directory (which
///   `current_project_path()` returns, and which is *not* a git repo) and its
///   **depth-1** children are scanned for repositories. No recursion.
///
/// Returns an empty vec when neither holds — the caller must treat that as an
/// unknown ("no repos found here"), never as "this repo has no worktrees".
///
/// Pure and synchronous: it only reads the directory tree, shells out to
/// nothing, and so is unit-testable with `tempfile` and no git installed.
fn resolve_repo_roots(root: &Path) -> Vec<PathBuf> {
    if is_git_repo(root) {
        return vec![root.to_path_buf()];
    }

    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "WORKTREE: cannot scan {} for git repos: {}",
                root.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut found: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Dot-directories are tool/VCS state, not source clones.
        if name.starts_with('.') || SKIPPED_SCAN_DIRS.contains(&name) {
            continue;
        }
        if is_git_repo(&path) {
            found.push(path);
        }
    }

    // Sort before truncating so the cap is deterministic — `read_dir` order
    // is filesystem-defined and varies across platforms.
    found.sort();
    if found.len() > MAX_SCANNED_REPOS {
        warn!(
            "WORKTREE: {} contains {} git repos; capping scan at {}",
            root.display(),
            found.len(),
            MAX_SCANNED_REPOS
        );
        found.truncate(MAX_SCANNED_REPOS);
    }
    found
}

// =============================================================================
// Handlers
// =============================================================================

/// GET /worktrees[?repo=<abs path>] — List all managed worktrees.
///
/// With `repo`, lists worktrees for exactly that repository and fails if that
/// repository fails. Without it, resolves a *set* of repo roots from the
/// workspace root (see [`resolve_repo_roots`]) and unions their worktrees —
/// the workspace root is an umbrella directory holding sibling clones, not a
/// git repo itself, so assuming a single repo there always failed.
pub async fn list_worktrees_handler(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<ListWorktreesQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // An explicitly named repo is authoritative: one repo, and its failure is
    // the response's failure.
    let explicit = query.repo.is_some();
    let repos: Vec<PathBuf> = match query.repo.as_deref() {
        Some(repo) => vec![PathBuf::from(repo)],
        None => {
            let root = crate::mcp::shared::current_project_path().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(api_error("No project path available")),
                )
            })?;
            let root = PathBuf::from(root);
            let found = resolve_repo_roots(&root);
            if found.is_empty() {
                // Never answer "no worktrees" for an unknown: name the
                // directory and say what we failed to find under it.
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!(
                        "No git repositories found: {} is not a git repository, and none of its \
                         immediate subdirectories are either. Pass ?repo=<absolute path> to list \
                         worktrees for a specific repository.",
                        root.display()
                    ))),
                ));
            }
            found
        }
    };

    let mut managed: Vec<String> = Vec::new();
    let mut repos_scanned: Vec<String> = Vec::new();
    let mut repos_failed: Vec<serde_json::Value> = Vec::new();

    for repo in &repos {
        match worktree::list_worktrees(repo) {
            Ok(paths) => {
                repos_scanned.push(repo.to_string_lossy().to_string());
                for path in paths {
                    // Preserve the historical "managed worktree" filter.
                    if path.contains(".worktrees") && !managed.contains(&path) {
                        managed.push(path);
                    }
                }
            }
            Err(e) => {
                if explicit {
                    return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))));
                }
                warn!(
                    "WORKTREE: skipping {} while listing worktrees: {}",
                    repo.display(),
                    e
                );
                repos_failed.push(json!({
                    "repo": repo.to_string_lossy(),
                    "error": e,
                }));
            }
        }
    }

    // Every candidate failed — again, an unknown, not an empty answer.
    if repos_scanned.is_empty() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to list worktrees: all {} candidate repositories errored ({}).",
                repos.len(),
                repos_failed
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ))),
        ));
    }

    let mut body = json!({
        "worktrees": managed,
        "total": managed.len(),
        "reposScanned": repos_scanned,
    });
    if !repos_failed.is_empty() {
        body["reposFailed"] = json!(repos_failed);
    }
    Ok(Json(ApiResponse::success(body)))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Create `<root>/<name>` with a `.git` directory — a clone root as far
    /// as [`is_git_repo`] is concerned, without needing git installed.
    fn make_repo(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(p.join(".git")).unwrap();
        p
    }

    /// Create `<root>/<name>` as a plain directory (no `.git`).
    fn make_plain(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn root_that_is_itself_a_repo_resolves_to_just_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        // A child repo must NOT be added when the root is already a repo.
        make_repo(root, "vendored");

        assert_eq!(resolve_repo_roots(root), vec![root.to_path_buf()]);
    }

    #[test]
    fn non_repo_parent_yields_its_child_repos() {
        // THE REGRESSION: the workspace root (D:/qontinui-root) is not a git
        // repo, so the handler used to shell `git worktree list` there and
        // fail with "not a git repository" on every single call.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let a = make_repo(root, "qontinui");
        let b = make_repo(root, "qontinui-runner");
        let c = make_repo(root, "qontinui-web");
        // Non-repo siblings are ignored, not errors.
        make_plain(root, "knowledge-base");
        std::fs::write(root.join("README.md"), "not a dir").unwrap();

        let mut expected = vec![a, b, c];
        expected.sort();
        assert_eq!(resolve_repo_roots(root), expected);
    }

    #[test]
    fn skip_list_and_dot_directories_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let keep = make_repo(root, "qontinui-runner");

        // Every skip-list entry, made to look exactly like a repo.
        for name in SKIPPED_SCAN_DIRS {
            make_repo(root, name);
        }
        // Arbitrary dot-directories are excluded by the leading-dot rule even
        // though they are not named in the skip list.
        make_repo(root, ".git-mirrors");
        make_repo(root, ".cache");

        assert_eq!(resolve_repo_roots(root), vec![keep]);
    }

    #[test]
    fn scan_is_capped_at_the_maximum() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let over = MAX_SCANNED_REPOS + 7;
        for i in 0..over {
            // Zero-padded so lexicographic order is stable and the truncation
            // is deterministic.
            make_repo(root, &format!("repo-{:03}", i));
        }

        let found = resolve_repo_roots(root);
        assert_eq!(found.len(), MAX_SCANNED_REPOS);
        // Truncation happens after sorting, so it keeps the first N by name.
        assert_eq!(found[0], root.join("repo-000"));
        assert_eq!(
            found[MAX_SCANNED_REPOS - 1],
            root.join(format!("repo-{:03}", MAX_SCANNED_REPOS - 1))
        );
    }

    #[test]
    fn directory_with_no_repos_yields_empty_vec() {
        // The handler contract: an empty vec here is an UNKNOWN (HTTP 404
        // naming the directory), never a confident `{"worktrees": [],
        // "total": 0}`.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_plain(root, "docs");
        make_plain(root, "notes");

        assert!(resolve_repo_roots(root).is_empty());
    }

    #[test]
    fn nonexistent_directory_yields_empty_vec_rather_than_panicking() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_repo_roots(&dir.path().join("does-not-exist")).is_empty());
    }

    #[test]
    fn child_repos_are_not_recursed_into() {
        // Depth 1 only: a repo nested two levels down is invisible.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outer = make_plain(root, "group");
        make_repo(&outer, "nested-repo");
        let direct = make_repo(root, "direct-repo");

        assert_eq!(resolve_repo_roots(root), vec![direct]);
    }

    #[test]
    fn dot_git_file_counts_as_a_repo() {
        // Linked worktrees and submodules have a `.git` FILE, not a dir.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let wt = root.join("linked-worktree");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /somewhere/.git/worktrees/x").unwrap();

        assert_eq!(resolve_repo_roots(root), vec![wt]);
    }
}
