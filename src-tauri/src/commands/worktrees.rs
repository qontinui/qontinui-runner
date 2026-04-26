//! Tauri-facing commands for worktree merge operations.
//!
//! Phase F (Commit Progress) introduced a pre-merge guard that prevents a
//! worktree merge from silently overwriting uncommitted edits in the
//! destination working tree. The HTTP route lives in `mcp/worktrees.rs`;
//! these `#[tauri::command]` wrappers expose the same functionality to the
//! React frontend over IPC.
//!
//! Two commands:
//!   - [`merge_worktree`] — guarded merge. Returns a structured "blocked"
//!     payload (with conflicting files + owning sessions) when the
//!     destination has dirty overlap; the frontend should show a confirm
//!     prompt and call [`merge_worktree_force`] if the user accepts.
//!   - [`merge_worktree_force`] — auto-stash dirty overlap, then merge.
//!     Always returns a `stash_ref` so the user can recover their work.

use std::path::Path;
use std::sync::Arc;

use tauri::Manager;
use tracing::{info, warn};

use crate::commands::AppState;
use crate::worktree;

/// Standard Tauri command response. Mirrors the shape used elsewhere in
/// `commands/` (see `commands::ai_session::CommandResponse`).
#[derive(serde::Serialize, Debug)]
pub struct CommandResponse {
    pub success: bool,
    pub message: Option<String>,
    pub data: Option<serde_json::Value>,
}

/// Tauri-facing guarded merge. Sibling of `POST /worktrees/merge`.
///
/// When the pre-merge guard fires (destination has dirty overlap with the
/// incoming changes), returns `success: false` with a structured `blocked`
/// payload in `data`:
///
/// ```json
/// {
///   "success": false,
///   "data": {
///     "status": "blocked",
///     "conflicting_files": ["src/foo.rs", ...],
///     "owning_sessions": [
///       { "task_run_id": "...", "touched_files": [...] },
///       { "task_run_id": "(unknown)", "touched_files": [...] }
///     ]
///   }
/// }
/// ```
///
/// On clean merge: `success: true`, `data.status == "merged"`,
/// `data.merge_commit` populated.
#[tauri::command]
pub async fn merge_worktree(
    app_handle: tauri::AppHandle,
    repo_path: String,
    branch_name: String,
    source_branch: String,
) -> Result<CommandResponse, String> {
    info!(
        "TAURI merge_worktree: repo={} branch={} -> {}",
        repo_path, branch_name, source_branch
    );

    let result =
        match worktree::merge_worktree(Path::new(&repo_path), &branch_name, &source_branch) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CommandResponse {
                    success: false,
                    message: Some(e),
                    data: None,
                });
            }
        };

    if let Some(blocked) = result.blocked {
        // Look up owning-sessions if AppState (and PG) is reachable;
        // otherwise return the bare conflicting_files (still surfaces the
        // problem to the user even without attribution).
        let owning_sessions: Vec<worktree::OwningSession> =
            match app_handle.try_state::<Arc<AppState>>() {
                Some(s) => {
                    let app_state: Arc<AppState> = s.inner().clone();
                    attribute_files_to_sessions_pg(&app_state.pg_db, &blocked.conflicting_files)
                        .await
                }
                None => {
                    warn!("merge_worktree: AppState unavailable, skipping session attribution");
                    Vec::new()
                }
            };

        warn!(
            "TAURI merge_worktree: blocked — {} files dirty, {} owning sessions",
            blocked.conflicting_files.len(),
            owning_sessions.len()
        );

        return Ok(CommandResponse {
            success: false,
            message: Some(result.summary.clone()),
            data: Some(serde_json::json!({
                "status": "blocked",
                "conflicting_files": blocked.conflicting_files,
                "owning_sessions": owning_sessions,
                "recovery_hint": "Call merge_worktree_force to auto-stash dirty files and merge.",
            })),
        });
    }

    if result.success {
        return Ok(CommandResponse {
            success: true,
            message: Some(result.summary.clone()),
            data: Some(serde_json::json!({
                "status": "merged",
                "merge_commit": result.merge_commit,
                "summary": result.summary,
            })),
        });
    }

    // Real merge conflicts (different from "blocked").
    Ok(CommandResponse {
        success: false,
        message: Some(result.summary.clone()),
        data: Some(serde_json::json!({
            "status": "conflicted",
            "conflicts": result.conflicts,
            "summary": result.summary,
        })),
    })
}

/// Tauri-facing force-merge. Sibling of `POST /worktrees/merge-force`.
///
/// Stashes any dirty destination files that overlap the incoming changes,
/// then merges. Returns the `stash_ref` so the user can recover the
/// stashed work with `git stash pop <ref>`.
#[tauri::command]
pub async fn merge_worktree_force(
    repo_path: String,
    branch_name: String,
    source_branch: String,
) -> Result<CommandResponse, String> {
    info!(
        "TAURI merge_worktree_force: repo={} branch={} -> {}",
        repo_path, branch_name, source_branch
    );

    let result = match worktree::merge_worktree_force(
        Path::new(&repo_path),
        &branch_name,
        &source_branch,
    ) {
        Ok(r) => r,
        Err(e) => {
            return Ok(CommandResponse {
                success: false,
                message: Some(e),
                data: None,
            });
        }
    };

    let stash_note = result.stash_ref.as_ref().map(|s| {
        format!(
            "Your dirty changes were stashed before merge. Recover with: git stash pop {}",
            s
        )
    });

    if result.merge.success {
        return Ok(CommandResponse {
            success: true,
            message: Some(result.merge.summary.clone()),
            data: Some(serde_json::json!({
                "status": "merged",
                "merge_commit": result.merge.merge_commit,
                "summary": result.merge.summary,
                "stash_ref": result.stash_ref,
                "stashed_files": result.stashed_files,
                "stash_note": stash_note,
            })),
        });
    }

    Ok(CommandResponse {
        success: false,
        message: Some(result.merge.summary.clone()),
        data: Some(serde_json::json!({
            "status": "conflicted",
            "conflicts": result.merge.conflicts,
            "summary": result.merge.summary,
            "stash_ref": result.stash_ref,
            "stashed_files": result.stashed_files,
            "stash_note": stash_note,
        })),
    })
}

/// Best-effort attribution: which sessions touched each conflicting file?
///
/// Mirrors `mcp::worktrees::attribute_files_to_sessions` but kept private
/// here so the Tauri command can stand alone without pulling MCP-internal
/// types. Files with no PG record are bucketed under `"(unknown)"`.
async fn attribute_files_to_sessions_pg(
    pg_db: &crate::database::pg::PgDb,
    conflicting_files: &[String],
) -> Vec<worktree::OwningSession> {
    use std::collections::{BTreeMap, HashMap, HashSet};

    let pairs = match pg_db.get_sessions_for_files(conflicting_files).await {
        Ok(p) => p,
        Err(e) => {
            warn!("attribute_files_to_sessions_pg: PG lookup failed: {}", e);
            Vec::new()
        }
    };

    let mut by_session: BTreeMap<usize, (String, Vec<String>)> = BTreeMap::new();
    let mut session_index: HashMap<String, usize> = HashMap::new();
    let mut attributed: HashSet<String> = HashSet::new();

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
