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

    // Check if there are uncommitted changes
    let status = run_git_command(repo_path, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Err(
            "Working directory has uncommitted changes. Commit or stash them before merging."
                .to_string(),
        );
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
                })
            } else {
                Err(format!("Merge failed: {}", e))
            }
        }
    }
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

    #[test]
    fn test_worktree_status_roundtrip() {
        assert_eq!(WorktreeStatus::from_str("active"), WorktreeStatus::Active);
        assert_eq!(WorktreeStatus::from_str("ready"), WorktreeStatus::Ready);
        assert_eq!(WorktreeStatus::from_str("merged"), WorktreeStatus::Merged);
        assert_eq!(WorktreeStatus::from_str("removed"), WorktreeStatus::Removed);
        assert_eq!(WorktreeStatus::from_str("failed"), WorktreeStatus::Failed);
        assert_eq!(WorktreeStatus::from_str("unknown"), WorktreeStatus::Active);
    }
}
