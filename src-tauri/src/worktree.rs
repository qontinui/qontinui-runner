//! Git Worktree Management for Isolated Workflow Execution
//!
//! Provides functions to create, track, and clean up git worktrees so that
//! workflows can run in isolation without affecting the main working directory.
//!
//! ## Usage
//!
//! When a workflow has `use_worktree: true`, the loop controller:
//! 1. Calls `create_worktree()` before execution to get an isolated copy
//! 2. Overrides `project_path` to point to the worktree
//! 3. After completion, the worktree and branch remain for review/merge
//! 4. The user can merge via `merge_worktree()` or discard via `remove_worktree()`
//!
//! Worktree records are persisted in the database for tracking across restarts.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{error, info, warn};

// =============================================================================
// Types
// =============================================================================

/// Status of a managed worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeStatus {
    /// Worktree is active (workflow running or completed, not yet merged/removed).
    Active,
    /// Workflow completed successfully — ready for merge.
    Ready,
    /// Worktree has been merged back to source branch.
    Merged,
    /// Worktree has been removed/discarded.
    Removed,
    /// Worktree creation or workflow failed.
    Failed,
}

impl std::fmt::Display for WorktreeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Ready => write!(f, "ready"),
            Self::Merged => write!(f, "merged"),
            Self::Removed => write!(f, "removed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl WorktreeStatus {
    pub fn from_str(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "ready" => Self::Ready,
            "merged" => Self::Merged,
            "removed" => Self::Removed,
            "failed" => Self::Failed,
            _ => Self::Active,
        }
    }
}

/// Record of a managed worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRecord {
    /// Unique ID (typically the task_run_id).
    pub id: String,
    /// Absolute path to the worktree directory.
    pub worktree_path: String,
    /// Branch name created for this worktree.
    pub branch_name: String,
    /// Source branch the worktree was created from.
    pub source_branch: String,
    /// Commit hash the worktree was created from.
    pub source_commit: String,
    /// Path to the original repo (the main working directory).
    pub repo_path: String,
    /// Associated workflow run ID.
    pub task_run_id: Option<String>,
    /// Workflow name (for display).
    pub workflow_name: Option<String>,
    /// Current status.
    pub status: WorktreeStatus,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last update timestamp.
    pub updated_at: String,
}

/// Result of a worktree creation.
#[derive(Debug, Clone)]
pub struct WorktreeCreateResult {
    /// Absolute path to the new worktree.
    pub worktree_path: PathBuf,
    /// Branch name.
    pub branch_name: String,
    /// Source commit hash.
    pub source_commit: String,
    /// Source branch name.
    pub source_branch: String,
}

/// Result of creating worktrees across multiple repos in a monorepo.
#[derive(Debug, Clone)]
pub struct MultiRepoWorktreeResult {
    pub monorepo_root: PathBuf,
    pub path_mappings: Vec<(String, String)>, // (original_path, worktree_path)
    pub results: Vec<WorktreeCreateResult>,
    pub errors: Vec<String>,
}

/// Result of a merge attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeResult {
    /// Whether the merge completed successfully.
    pub success: bool,
    /// Merge commit hash (if successful).
    pub merge_commit: Option<String>,
    /// Conflicted files (if merge had conflicts).
    pub conflicts: Vec<String>,
    /// Human-readable summary.
    pub summary: String,
    /// Phase F pre-merge guard: when populated, the merge was REFUSED because
    /// the destination working tree had uncommitted edits that the merge
    /// would silently overwrite. The merge was not attempted. `success` is
    /// always `false` when this is `Some`. `conflicts` is empty (those are
    /// merge-time conflicts, a different concept).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blocked: Option<MergeBlockedDetails>,
}

/// Phase F: structured payload for "would clobber sibling work" outcomes.
/// Surfaced to the API so callers can show a "force merge?" UI with the
/// list of files at risk and which sessions own them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeBlockedDetails {
    /// Files modified in destination AND about to change in the merge.
    pub conflicting_files: Vec<String>,
    /// Sessions known to have touched any of those files, deduped by
    /// `task_run_id` and ordered most-recent first. May contain a synthetic
    /// `task_run_id == "(unknown)"` entry for files that are dirty but not
    /// tracked by the per-session file-set tracker (manual edits, sessions
    /// from before Phase A).
    pub owning_sessions: Vec<OwningSession>,
}

/// Phase F: one session's slice of the conflicting-files set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwningSession {
    pub task_run_id: String,
    pub touched_files: Vec<String>,
}

// =============================================================================
// Core Operations
// =============================================================================

/// Create a git worktree for isolated workflow execution.
///
/// Creates a new branch and worktree directory from the current HEAD of the
/// source repo. The worktree is placed in a `.worktrees/` directory adjacent
/// to the repo root.
///
/// # Arguments
/// * `repo_path` — Path to the git repository
/// * `execution_id` — Unique ID for naming the branch and directory
/// * `workflow_name` — For the branch name (sanitized)
pub fn create_worktree(
    repo_path: &Path,
    execution_id: &str,
    workflow_name: &str,
) -> Result<WorktreeCreateResult, String> {
    // Verify repo_path is a git repo
    if !repo_path.join(".git").exists() && !repo_path.join("../.git").exists() {
        return Err(format!("Not a git repository: {}", repo_path.display()));
    }

    // Get current branch and commit
    let source_branch = get_current_branch(repo_path)?;
    let source_commit = get_head_commit(repo_path)?;

    // Sanitize workflow name for branch name
    let sanitized_name: String = workflow_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .chars()
        .take(40)
        .collect();

    let short_id = &execution_id[..execution_id.len().min(8)];
    let branch_name = format!("worktree/{}-{}", sanitized_name, short_id);

    // Create worktree directory
    let worktrees_dir = repo_path.join(".worktrees");
    let worktree_dir_name = format!("{}-{}", sanitized_name, short_id);
    let worktree_path = worktrees_dir.join(&worktree_dir_name);

    // Ensure .worktrees directory exists
    if let Err(e) = std::fs::create_dir_all(&worktrees_dir) {
        return Err(format!("Failed to create .worktrees directory: {}", e));
    }

    // Add .worktrees to .gitignore if not already there
    let gitignore_path = repo_path.join(".gitignore");
    if gitignore_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&gitignore_path) {
            if !content.contains(".worktrees") {
                if let Err(e) = std::fs::write(
                    &gitignore_path,
                    format!("{}\n.worktrees/\n", content.trim_end()),
                ) {
                    warn!("Failed to update .gitignore with .worktrees: {}", e);
                }
            }
        }
    }

    // Remove existing worktree at this path if it exists (stale from a previous run)
    if worktree_path.exists() {
        info!("Removing stale worktree at {}", worktree_path.display());
        let _ = run_git_command(
            repo_path,
            &[
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ],
        );
        // Also try deleting the branch
        let _ = run_git_command(repo_path, &["branch", "-D", &branch_name]);
    }

    // Create the worktree with a new branch
    info!(
        "Creating worktree: branch={}, path={}, from={}@{}",
        branch_name,
        worktree_path.display(),
        source_branch,
        &source_commit[..8]
    );

    let output = run_git_command(
        repo_path,
        &[
            "worktree",
            "add",
            "-b",
            &branch_name,
            &worktree_path.to_string_lossy(),
            "HEAD",
        ],
    )?;

    info!("Worktree created successfully: {}", output.trim());

    Ok(WorktreeCreateResult {
        worktree_path,
        branch_name,
        source_commit,
        source_branch,
    })
}

/// Remove a git worktree and optionally delete its branch.
pub fn remove_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch_name: &str,
    delete_branch: bool,
) -> Result<(), String> {
    info!("Removing worktree: {}", worktree_path.display());

    // Remove the worktree
    let result = run_git_command(
        repo_path,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ],
    );

    if let Err(e) = &result {
        warn!("git worktree remove failed: {}. Trying manual cleanup.", e);
        // Manual cleanup as fallback
        if worktree_path.exists() {
            if let Err(e2) = std::fs::remove_dir_all(worktree_path) {
                error!("Manual worktree cleanup also failed: {}", e2);
                return Err(format!("Failed to remove worktree: {}", e));
            }
        }
        // Prune stale worktree entries
        let _ = run_git_command(repo_path, &["worktree", "prune"]);
    }

    // Delete the branch if requested
    if delete_branch {
        info!("Deleting worktree branch: {}", branch_name);
        if let Err(e) = run_git_command(repo_path, &["branch", "-D", branch_name]) {
            warn!("Failed to delete branch {}: {}", branch_name, e);
        }
    }

    Ok(())
}

/// Merge a worktree branch back into the source branch.
///
/// This performs a fast-forward merge if possible, otherwise a regular merge.
/// Returns conflict information if the merge can't auto-resolve.
///
/// Phase F pre-merge guard: BEFORE running `git merge`, this function computes
/// the intersection of (a) files that the merge would change and (b) files
/// the destination working tree has uncommitted edits to. If non-empty, the
/// merge is REFUSED and a [`MergeBlockedDetails`] payload is returned so the
/// caller can surface which sibling sessions own the at-risk work.
///
/// Use [`merge_worktree_force`] to bypass the guard (it stashes the dirty
/// changes first so the user has an explicit recovery path).
pub fn merge_worktree(
    repo_path: &Path,
    branch_name: &str,
    source_branch: &str,
) -> Result<MergeResult, String> {
    info!(
        "Merging worktree branch '{}' into '{}'",
        branch_name, source_branch
    );

    // Check if we're on the source branch
    let current = get_current_branch(repo_path)?;
    if current != source_branch {
        return Err(format!(
            "Not on source branch '{}' (currently on '{}'). Switch to the source branch first.",
            source_branch, current
        ));
    }

    // Phase F: structured pre-merge guard. Compute "what would change" ∩
    // "what is currently dirty in destination" and refuse the merge if the
    // intersection is non-empty. Old behaviour ("just refuse if anything is
    // dirty") was unsafe because it gave callers no way to distinguish "your
    // sibling will lose work" from "you have an unrelated stray edit".
    let conflicting_files = compute_premerge_conflicts(repo_path, branch_name, true)?;
    if !conflicting_files.is_empty() {
        warn!(
            "WORKTREE: pre-merge guard blocked merge of '{}' into '{}' — \
             {} dirty file(s) would be overwritten",
            branch_name,
            source_branch,
            conflicting_files.len()
        );
        return Ok(MergeResult {
            success: false,
            merge_commit: None,
            conflicts: vec![],
            summary: format!(
                "Merge blocked: {} file(s) in '{}' have uncommitted changes that would be overwritten. \
                 Commit/stash them, or call force-merge to auto-stash.",
                conflicting_files.len(),
                source_branch,
            ),
            blocked: Some(MergeBlockedDetails {
                conflicting_files,
                // owning_sessions is populated by the caller (HTTP handler /
                // Tauri command) which has access to PgDb. The pure-git layer
                // can't reach the DB, and we don't want to make this fn async
                // just for a best-effort attribution lookup.
                owning_sessions: Vec::new(),
            }),
        });
    }

    // Try the merge
    let merge_result = run_git_command_with_status(repo_path, &["merge", branch_name, "--no-edit"]);

    match merge_result {
        Ok(output) => {
            let merge_commit = get_head_commit(repo_path).unwrap_or_default();
            Ok(MergeResult {
                success: true,
                merge_commit: Some(merge_commit),
                conflicts: vec![],
                summary: format!(
                    "Merged '{}' into '{}': {}",
                    branch_name,
                    source_branch,
                    output.trim()
                ),
                blocked: None,
            })
        }
        Err(e) => {
            // Check for merge conflicts
            let conflict_output =
                run_git_command(repo_path, &["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default();
            let conflicts: Vec<String> = conflict_output
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();

            if !conflicts.is_empty() {
                // Abort the merge so the user/AI can handle it
                let _ = run_git_command(repo_path, &["merge", "--abort"]);
                Ok(MergeResult {
                    success: false,
                    merge_commit: None,
                    conflicts,
                    summary: "Merge conflicts detected. Merge aborted. Use AI merge to resolve."
                        .to_string(),
                    blocked: None,
                })
            } else {
                Err(format!("Merge failed: {}", e))
            }
        }
    }
}

/// Phase F: compute the pre-merge conflict set for [`merge_worktree`].
///
/// Returns the intersection of:
///   - files that would change in `git merge <source_branch>` against
///     `repo_path`'s current HEAD (`git diff --name-only HEAD..<branch>`)
///   - files dirty in `repo_path`'s working tree right now
///     (`git status --porcelain` parsed for modified-tracked, plus optionally
///     untracked when `include_untracked` is true)
///
/// "Modified tracked" means the porcelain entry has `M` or `D` in either the
/// index or working-tree column. This catches `M `, ` M`, `MM`, `AM`, ` D`,
/// `D `, etc. — anything where the user has staged or unstaged content git
/// would refuse to clobber. Untracked entries (`??`) are added when
/// `include_untracked` is set; that's the safer default since the pre-merge
/// check is exactly the place to catch "scratch file the user hasn't added
/// yet that the merge will overwrite".
pub(crate) fn compute_premerge_conflicts(
    repo_path: &Path,
    source_branch: &str,
    include_untracked: bool,
) -> Result<Vec<String>, String> {
    // (a) files the merge would change. Two-dot `HEAD..<branch>` is
    // intentional — we want "what's coming in," NOT the symmetric diff
    // (which would also include destination-only commits the merge wouldn't
    // touch).
    let diff_output =
        run_git_command(repo_path, &["diff", "--name-only", &format!("HEAD..{}", source_branch)])?;
    let incoming: std::collections::HashSet<String> = diff_output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    if incoming.is_empty() {
        // Nothing's coming in (already merged, or empty branch). Can't
        // possibly clobber anything.
        return Ok(Vec::new());
    }

    // (b) dirty paths in destination working tree.
    let status_output = run_git_command(repo_path, &["status", "--porcelain"])?;
    let dirty: std::collections::HashSet<String> = parse_dirty_paths(&status_output, include_untracked);

    // Intersection, sorted for determinism.
    let mut overlap: Vec<String> = incoming.intersection(&dirty).cloned().collect();
    overlap.sort();
    Ok(overlap)
}

/// Parse `git status --porcelain` (v1) output into a set of dirty paths.
///
/// Porcelain v1 lines are `XY <space> <path>` where X is index status and Y
/// is working-tree status (e.g. ` M file`, `MM file`, `?? file`). We treat as
/// dirty:
///   - any entry whose X or Y is `M` (modified) or `D` (deleted)
///   - any entry whose X is `A` (added/staged-new) — those are uncommitted edits
///   - untracked (`??`) when `include_untracked` is true
///
/// Renames (`R old -> new`) are normalised to the new path; the index has
/// already moved on so the new name is what would collide with the merge.
pub(crate) fn parse_dirty_paths(
    porcelain: &str,
    include_untracked: bool,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in porcelain.lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[..2];
        let rest = &line[3..]; // skip "XY "

        let x = xy.as_bytes()[0] as char;
        let y = xy.as_bytes()[1] as char;

        let is_untracked = xy == "??";
        let is_modified_tracked = matches!(x, 'M' | 'D' | 'A' | 'R' | 'C')
            || matches!(y, 'M' | 'D');

        if is_untracked {
            if include_untracked {
                out.insert(rest.to_string());
            }
            continue;
        }

        if !is_modified_tracked {
            continue;
        }

        // Renames look like "R  old -> new". Take the new name; that's the
        // path that lives in the working tree post-rename.
        let path = if let Some(idx) = rest.find(" -> ") {
            rest[idx + 4..].to_string()
        } else {
            rest.to_string()
        };
        out.insert(path);
    }
    out
}

/// Phase F force-merge: stash the conflicting destination changes, then run
/// the merge. Returns the stash ref so the user has a recovery path.
///
/// If there's nothing to stash (the guard would have allowed the merge
/// anyway), this still works — it's just a no-op stash and a normal merge.
pub fn merge_worktree_force(
    repo_path: &Path,
    branch_name: &str,
    source_branch: &str,
) -> Result<ForceMergeResult, String> {
    info!(
        "WORKTREE: force-merging '{}' into '{}' (auto-stashing dirty overlap)",
        branch_name, source_branch
    );

    let current = get_current_branch(repo_path)?;
    if current != source_branch {
        return Err(format!(
            "Not on source branch '{}' (currently on '{}'). Switch to the source branch first.",
            source_branch, current
        ));
    }

    // Recompute the conflict set from scratch — caller may have raced with
    // the user, and we want the stash to actually capture what's dirty.
    let conflicting_files = compute_premerge_conflicts(repo_path, branch_name, true)?;

    let stash_ref = if conflicting_files.is_empty() {
        None
    } else {
        let stash_msg = format!("auto-stash before force-merge of {}", branch_name);
        // `stash push --include-untracked -m <msg> -- <files>` is the most
        // surgical form: only the listed paths are stashed. We don't want
        // to wholesale-stash the user's working tree because it may contain
        // unrelated work we don't need to disturb.
        let mut args: Vec<String> = vec![
            "stash".into(),
            "push".into(),
            "--include-untracked".into(),
            "-m".into(),
            stash_msg.clone(),
            "--".into(),
        ];
        args.extend(conflicting_files.iter().cloned());
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_git_command(repo_path, &arg_refs).map_err(|e| {
            format!("force-merge: failed to stash dirty files (merge NOT attempted): {}", e)
        })?;

        // Get the stash ref of what we just pushed (top of stack).
        let stash_list = run_git_command(repo_path, &["stash", "list", "-n", "1"])?;
        // First line looks like: stash@{0}: On main: auto-stash before...
        let stash_ref = stash_list
            .lines()
            .next()
            .and_then(|l| l.split(':').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "stash@{0}".to_string());
        info!(
            "WORKTREE: force-merge stashed {} file(s) as {}",
            conflicting_files.len(),
            stash_ref
        );
        Some(stash_ref)
    };

    // Now do the actual merge. We don't call `merge_worktree` here because
    // the guard would re-fire (it doesn't know we just stashed) — go
    // directly to the git layer.
    let merge_result = run_git_command_with_status(repo_path, &["merge", branch_name, "--no-edit"]);

    match merge_result {
        Ok(output) => {
            let merge_commit = get_head_commit(repo_path).unwrap_or_default();
            Ok(ForceMergeResult {
                merge: MergeResult {
                    success: true,
                    merge_commit: Some(merge_commit),
                    conflicts: vec![],
                    summary: format!(
                        "Merged '{}' into '{}': {}",
                        branch_name,
                        source_branch,
                        output.trim()
                    ),
                    blocked: None,
                },
                stash_ref,
                stashed_files: conflicting_files,
            })
        }
        Err(e) => {
            // Real merge conflicts after the stash; surface them but keep
            // the stash_ref so the user can recover.
            let conflict_output =
                run_git_command(repo_path, &["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default();
            let conflicts: Vec<String> = conflict_output
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();

            if !conflicts.is_empty() {
                let _ = run_git_command(repo_path, &["merge", "--abort"]);
                Ok(ForceMergeResult {
                    merge: MergeResult {
                        success: false,
                        merge_commit: None,
                        conflicts,
                        summary: "Merge conflicts detected after stash. Merge aborted. \
                                  Stash preserved — restore with `git stash pop`."
                            .to_string(),
                        blocked: None,
                    },
                    stash_ref,
                    stashed_files: conflicting_files,
                })
            } else {
                Err(format!("Force-merge failed: {}", e))
            }
        }
    }
}

/// Result of a force-merge: the underlying merge plus stash bookkeeping so
/// the user knows where their dirty changes went.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForceMergeResult {
    pub merge: MergeResult,
    /// Stash ref the dirty files were saved to (e.g. `stash@{0}`), or
    /// `None` if there was nothing to stash.
    pub stash_ref: Option<String>,
    /// The exact files that were stashed before the merge.
    pub stashed_files: Vec<String>,
}

/// Get a summary of changes in a worktree branch compared to its source.
pub fn get_worktree_diff_summary(
    repo_path: &Path,
    branch_name: &str,
    source_branch: &str,
) -> Result<String, String> {
    // Get commit count
    let log = run_git_command(
        repo_path,
        &[
            "log",
            "--oneline",
            &format!("{}..{}", source_branch, branch_name),
        ],
    )?;
    let commit_count = log.lines().count();

    // Get diffstat
    let diffstat = run_git_command(
        repo_path,
        &[
            "diff",
            "--stat",
            &format!("{}...{}", source_branch, branch_name),
        ],
    )?;

    Ok(format!(
        "{} commit(s) ahead of '{}':\n{}\nDiff:\n{}",
        commit_count,
        source_branch,
        log.trim(),
        diffstat.trim()
    ))
}

/// List all active worktrees for a repo.
pub fn list_worktrees(repo_path: &Path) -> Result<Vec<String>, String> {
    let output = run_git_command(repo_path, &["worktree", "list", "--porcelain"])?;
    let paths: Vec<String> = output
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .map(|l| l.trim_start_matches("worktree ").to_string())
        .collect();
    Ok(paths)
}

// =============================================================================
// AI-Assisted Merge
// =============================================================================

/// Get the full diff for AI review before merging.
pub fn get_full_diff(
    repo_path: &Path,
    branch_name: &str,
    source_branch: &str,
) -> Result<String, String> {
    run_git_command(
        repo_path,
        &["diff", &format!("{}...{}", source_branch, branch_name)],
    )
}

/// Get conflict markers content for AI resolution.
/// Call this DURING a merge (after merge starts, before abort/commit).
pub fn get_conflict_content(repo_path: &Path, file_path: &str) -> Result<String, String> {
    let full_path = repo_path.join(file_path);
    std::fs::read_to_string(&full_path)
        .map_err(|e| format!("Failed to read conflict file {}: {}", file_path, e))
}

/// Start a merge without aborting on conflicts — leaves conflict markers in files.
pub fn start_merge_with_conflicts(
    repo_path: &Path,
    branch_name: &str,
) -> Result<MergeResult, String> {
    let result = run_git_command_with_status(repo_path, &["merge", branch_name, "--no-edit"]);

    match result {
        Ok(output) => {
            let merge_commit = get_head_commit(repo_path).unwrap_or_default();
            Ok(MergeResult {
                success: true,
                merge_commit: Some(merge_commit),
                conflicts: vec![],
                summary: format!("Clean merge: {}", output.trim()),
                blocked: None,
            })
        }
        Err(_) => {
            // Check for conflicts — don't abort, let AI resolve them
            let conflict_output =
                run_git_command(repo_path, &["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default();

            let conflicts: Vec<String> = conflict_output
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();

            Ok(MergeResult {
                success: false,
                merge_commit: None,
                conflicts,
                summary: "Merge has conflicts. Files contain conflict markers for resolution."
                    .to_string(),
                blocked: None,
            })
        }
    }
}

/// After AI resolves conflicts, stage the resolved files and commit.
pub fn complete_merge_after_resolution(
    repo_path: &Path,
    resolved_files: &[&str],
) -> Result<String, String> {
    // Stage resolved files
    for file in resolved_files {
        run_git_command(repo_path, &["add", file])?;
    }

    // Check if there are still unresolved conflicts
    let remaining =
        run_git_command(repo_path, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();

    if !remaining.trim().is_empty() {
        return Err(format!(
            "Still have unresolved conflicts in: {}",
            remaining.trim()
        ));
    }

    // Complete the merge commit
    run_git_command(repo_path, &["commit", "--no-edit"])?;
    get_head_commit(repo_path)
}

/// Abort an in-progress merge.
pub fn abort_merge(repo_path: &Path) -> Result<(), String> {
    run_git_command(repo_path, &["merge", "--abort"])?;
    Ok(())
}

/// Build the AI merge prompt for reviewing and merging worktree changes.
pub fn build_ai_merge_prompt(
    branch_name: &str,
    source_branch: &str,
    diff_summary: &str,
    full_diff: &str,
    workflow_name: &str,
    conflicts: Option<&[String]>,
) -> String {
    let conflict_section = if let Some(conflicts) = conflicts {
        if conflicts.is_empty() {
            String::new()
        } else {
            format!(
                r#"
## Merge Conflicts

The following files have conflicts that need resolution:
{}

The conflict markers are in the working directory. For each conflicted file:
1. Read the file to see the conflict markers (<<<<<<< / ======= / >>>>>>>)
2. Understand what both sides changed and why
3. Write the correct merged version (remove all conflict markers)
4. The merged version should incorporate the intent of both changes

After resolving all conflicts, stage the files with `git add` and run `git commit --no-edit` to complete the merge.
"#,
                conflicts
                    .iter()
                    .map(|f| format!("- `{}`", f))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    } else {
        String::new()
    };

    let truncated_diff = if full_diff.len() > 50000 {
        format!(
            "{}...\n[diff truncated, {} more chars]",
            &full_diff[..50000],
            full_diff.len() - 50000
        )
    } else {
        full_diff.to_string()
    };

    format!(
        r#"You are reviewing and merging changes from an isolated workflow run.

## Context
- **Workflow:** {workflow_name}
- **Branch:** `{branch_name}` → `{source_branch}`

## Change Summary
{diff_summary}

## Full Diff
```diff
{truncated_diff}
```
{conflict_section}
## Your Task

1. **Review the changes.** Are they correct? Do they introduce any bugs, security issues, or regressions?
2. **If there are conflicts**, resolve them by reading each conflicted file and writing the correct merged version.
3. **If changes look good** (no conflicts or all resolved), complete the merge.
4. **If changes have problems**, describe what's wrong. Do NOT merge broken code — leave it on the branch for the user to decide.

After review, summarize:
- What the changes do
- Any concerns or issues found
- Whether the merge was completed or left for manual review"#,
        workflow_name = workflow_name,
        branch_name = branch_name,
        source_branch = source_branch,
        diff_summary = diff_summary,
        truncated_diff = truncated_diff,
        conflict_section = conflict_section,
    )
}

/// Build a prompt for comparing two worktree implementations.
pub fn build_comparison_prompt(
    branches: &[(String, String, String)], // (branch_name, diff_summary, workflow_result_summary)
    workflow_name: &str,
    source_branch: &str,
) -> String {
    let mut branch_sections = String::new();
    for (i, (branch, diff_summary, result_summary)) in branches.iter().enumerate() {
        branch_sections.push_str(&format!(
            r#"
### Implementation {num} — `{branch}`

**Workflow Result:** {result_summary}

**Changes:**
{diff_summary}
"#,
            num = i + 1,
            branch = branch,
            result_summary = result_summary,
            diff_summary = diff_summary,
        ));
    }

    format!(
        r#"You are comparing {count} different implementations of the same workflow task.

## Task
**Workflow:** {workflow_name}
**Source branch:** `{source_branch}`

Each implementation ran in an isolated worktree from the same starting point.

{branch_sections}

## Your Task

Compare all implementations and produce a structured analysis:

### 1. Approach Comparison
For each implementation, describe:
- What approach/strategy was used
- Key architectural decisions
- Trade-offs made

### 2. Quality Assessment
Rate each implementation on:
- **Correctness** (1-5): Does it work? Does it pass verification?
- **Code quality** (1-5): Clean, maintainable, well-structured?
- **Completeness** (1-5): Does it fully address the task?
- **Robustness** (1-5): Error handling, edge cases?

### 3. Differences
Highlight the key differences between implementations:
- Files that differ between implementations
- Different algorithms or patterns used
- Features present in one but not others

### 4. Recommendation
Which implementation should be merged? Why?
If none is clearly best, describe what a hybrid of the best parts would look like.

### 5. Summary Table
| Metric | {header_row} |
|--------|{header_sep}|
| Correctness | ... |
| Code Quality | ... |
| Completeness | ... |
| Robustness | ... |
| **Overall** | ... |"#,
        count = branches.len(),
        workflow_name = workflow_name,
        source_branch = source_branch,
        branch_sections = branch_sections,
        header_row = branches
            .iter()
            .enumerate()
            .map(|(i, _)| format!("Impl {} ", i + 1))
            .collect::<Vec<_>>()
            .join("| "),
        header_sep = branches
            .iter()
            .map(|_| "------")
            .collect::<Vec<_>>()
            .join("|"),
    )
}

// =============================================================================
// Multi-Repo Operations
// =============================================================================

/// Discover all git repositories under the given root directory.
/// Returns a list of (repo_name, repo_path) for each directory containing a .git folder.
pub fn discover_repos(root: &Path) -> Vec<(String, PathBuf)> {
    let mut repos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join(".git").exists() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                repos.push((name, path));
            }
        }
    }
    repos.sort_by(|a, b| a.0.cmp(&b.0));
    repos
}

/// Create worktrees in all git repos under the monorepo root.
/// Returns a mapping of original_repo_path -> worktree_path.
pub fn create_multi_repo_worktrees(
    monorepo_root: &Path,
    execution_id: &str,
    workflow_name: &str,
) -> Result<MultiRepoWorktreeResult, String> {
    let repos = discover_repos(monorepo_root);
    if repos.is_empty() {
        return Err(format!(
            "No git repos found under {}",
            monorepo_root.display()
        ));
    }

    let mut path_mappings: Vec<(String, String)> = Vec::new();
    let mut results: Vec<WorktreeCreateResult> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (name, repo_path) in &repos {
        match create_worktree(repo_path, execution_id, workflow_name) {
            Ok(result) => {
                path_mappings.push((
                    repo_path.to_string_lossy().to_string(),
                    result.worktree_path.to_string_lossy().to_string(),
                ));
                results.push(result);
            }
            Err(e) => {
                // Non-fatal — some repos might have uncommitted changes or other issues
                warn!("Failed to create worktree for {}: {}", name, e);
                errors.push(format!("{}: {}", name, e));
            }
        }
    }

    if results.is_empty() {
        return Err(format!(
            "Failed to create worktrees in any repo: {:?}",
            errors
        ));
    }

    Ok(MultiRepoWorktreeResult {
        monorepo_root: monorepo_root.to_path_buf(),
        path_mappings,
        results,
        errors,
    })
}

// =============================================================================
// Git Helpers
// =============================================================================

fn get_current_branch(repo_path: &Path) -> Result<String, String> {
    let output = run_git_command(repo_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    Ok(output.trim().to_string())
}

fn get_head_commit(repo_path: &Path) -> Result<String, String> {
    let output = run_git_command(repo_path, &["rev-parse", "HEAD"])?;
    Ok(output.trim().to_string())
}

pub(crate) fn run_git_command(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn run_git_command_with_status(repo_path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_worktree_status_roundtrip() {
        assert_eq!(WorktreeStatus::from_str("active"), WorktreeStatus::Active);
        assert_eq!(WorktreeStatus::from_str("ready"), WorktreeStatus::Ready);
        assert_eq!(WorktreeStatus::from_str("merged"), WorktreeStatus::Merged);
        assert_eq!(WorktreeStatus::from_str("removed"), WorktreeStatus::Removed);
        assert_eq!(WorktreeStatus::from_str("failed"), WorktreeStatus::Failed);
        assert_eq!(WorktreeStatus::from_str("unknown"), WorktreeStatus::Active);
    }

    // ------------------------------------------------------------------------
    // Phase F pre-merge guard tests
    //
    // Mirrors the harness pattern from auto_commit.rs::tests — temp git repo
    // with one initial commit, then drive scenarios via plain `git` calls.
    // ------------------------------------------------------------------------

    fn run(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {:?}: {}", args, e));
        assert!(
            out.status.success(),
            "git {:?} in {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            cwd,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Build a repo with main + an initial committed `foo.rs`. Then create a
    /// branch `feat-x` that modifies `foo.rs`. Returns to main with a clean
    /// tree. The caller dirties or doesn't dirty `foo.rs` to drive the
    /// guard scenarios.
    fn init_repo_with_branch() -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().canonicalize().expect("canonicalize");

        run(&path, &["init", "-q", "-b", "main"]);
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test User"]);
        run(&path, &["config", "commit.gpgsign", "false"]);

        // Initial commit: foo.rs and bar.rs live on main.
        fs::write(path.join("foo.rs"), "// main version of foo\n").unwrap();
        fs::write(path.join("bar.rs"), "// main version of bar\n").unwrap();
        run(&path, &["add", "."]);
        run(&path, &["commit", "-q", "-m", "init"]);

        // Branch off and modify foo.rs there. Return to main clean.
        run(&path, &["checkout", "-q", "-b", "feat-x"]);
        fs::write(path.join("foo.rs"), "// branch version of foo\n").unwrap();
        run(&path, &["commit", "-q", "-am", "feat-x: change foo"]);
        run(&path, &["checkout", "-q", "main"]);

        (dir, path, "feat-x".to_string())
    }

    #[test]
    fn test_parse_dirty_paths_modified_tracked() {
        let porcelain = " M foo.rs\nMM bar.rs\n?? scratch.txt\n D gone.rs\n";
        let dirty = parse_dirty_paths(porcelain, false);
        assert!(dirty.contains("foo.rs"));
        assert!(dirty.contains("bar.rs"));
        assert!(dirty.contains("gone.rs"));
        assert!(!dirty.contains("scratch.txt"), "untracked excluded by default");
        assert_eq!(dirty.len(), 3);

        let dirty_with_untracked = parse_dirty_paths(porcelain, true);
        assert!(dirty_with_untracked.contains("scratch.txt"));
        assert_eq!(dirty_with_untracked.len(), 4);
    }

    #[test]
    fn test_parse_dirty_paths_renames() {
        // Rename: porcelain shows "R  old -> new" — we want the new path.
        let porcelain = "R  old.rs -> new.rs\n";
        let dirty = parse_dirty_paths(porcelain, true);
        assert!(dirty.contains("new.rs"), "renamed-to path must be in dirty set");
        assert!(!dirty.contains("old.rs"));
    }

    #[test]
    fn test_premerge_guard_blocks_when_dirty_overlaps() {
        let (_dir, repo, branch) = init_repo_with_branch();

        // Dirty foo.rs on main. Branch also touched foo.rs => overlap.
        fs::write(repo.join("foo.rs"), "// SIBLING'S uncommitted edit\n").unwrap();

        let result = merge_worktree(&repo, &branch, "main").expect("merge_worktree");
        assert!(!result.success, "merge must be refused");
        assert_eq!(result.merge_commit, None);
        let blocked = result.blocked.expect("blocked details must be populated");
        assert!(
            blocked.conflicting_files.iter().any(|f| f == "foo.rs"),
            "foo.rs must be in conflicting_files: {:?}",
            blocked.conflicting_files
        );
        // Sibling edit still present — guard didn't touch it.
        let foo_now = fs::read_to_string(repo.join("foo.rs")).unwrap();
        assert!(foo_now.contains("SIBLING"), "sibling edit must survive guard");
    }

    #[test]
    fn test_premerge_guard_allows_unrelated_dirty() {
        let (_dir, repo, branch) = init_repo_with_branch();

        // Dirty bar.rs on main — branch only touched foo.rs. No overlap, so
        // merge should proceed normally (this is the load-bearing
        // "we don't break the happy path" assertion).
        fs::write(repo.join("bar.rs"), "// unrelated dirty edit\n").unwrap();

        let result = merge_worktree(&repo, &branch, "main").expect("merge_worktree");
        assert!(
            result.success,
            "merge must succeed when dirty file doesn't overlap incoming changes; got: {:?}",
            result
        );
        assert!(result.blocked.is_none());
        // bar.rs still dirty (we didn't touch it).
        let bar_now = fs::read_to_string(repo.join("bar.rs")).unwrap();
        assert!(bar_now.contains("unrelated"));
        // foo.rs got the branch's version.
        let foo_now = fs::read_to_string(repo.join("foo.rs")).unwrap();
        assert!(foo_now.contains("branch version"));
    }

    #[test]
    fn test_premerge_guard_allows_clean_destination() {
        let (_dir, repo, branch) = init_repo_with_branch();
        // No dirty files at all — the canonical happy path.
        let result = merge_worktree(&repo, &branch, "main").expect("merge_worktree");
        assert!(result.success);
        assert!(result.blocked.is_none());
    }

    #[test]
    fn test_force_merge_stashes_then_merges() {
        let (_dir, repo, branch) = init_repo_with_branch();

        // Sibling has dirty edits to foo.rs (which the branch also touched).
        fs::write(repo.join("foo.rs"), "// SIBLING'S uncommitted edit\n").unwrap();

        let result =
            merge_worktree_force(&repo, &branch, "main").expect("merge_worktree_force");
        assert!(result.merge.success, "force-merge must succeed: {:?}", result);
        assert!(
            result.stash_ref.is_some(),
            "stash_ref must be populated when there were files to stash"
        );
        assert!(
            result.stashed_files.iter().any(|f| f == "foo.rs"),
            "foo.rs must be in stashed_files: {:?}",
            result.stashed_files
        );

        // foo.rs is now the branch version (merge applied), sibling work is
        // safe in the stash.
        let foo_after = fs::read_to_string(repo.join("foo.rs")).unwrap();
        assert!(
            foo_after.contains("branch version"),
            "foo.rs should have branch's content after force-merge: {}",
            foo_after
        );
        // Verify stash contains the sibling work.
        let stash_show = run(&repo, &["stash", "show", "-p"]);
        assert!(
            stash_show.contains("SIBLING") || stash_show.contains("uncommitted"),
            "stash must contain the sibling's edit: {}",
            stash_show
        );
    }
}
