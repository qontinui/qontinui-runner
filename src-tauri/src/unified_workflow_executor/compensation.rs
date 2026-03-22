//! Compensation flows for automatic git rollback on workflow failure.
//!
//! Inspired by Netflix Conductor's compensation pattern, this module provides
//! opt-in git-based rollback when workflows fail. It records commit hashes
//! after each agentic iteration and can revert to a known-good state on failure.

use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::CheckpointDb;
use crate::str_utils::truncate_str;

use super::types::{IterationCommit, IterationDiff, RollbackPolicy};

/// Manages commit checkpoints and rollback operations for workflow executions.
pub struct CompensationManager {
    checkpoint_db: Arc<CheckpointDb>,
}

impl CompensationManager {
    pub fn new(checkpoint_db: Arc<CheckpointDb>) -> Self {
        Self { checkpoint_db }
    }

    /// Record a commit hash checkpoint after an agentic phase completes.
    pub fn record_iteration_commit(
        &self,
        execution_id: &str,
        iteration: u32,
        commit_hash: &str,
    ) -> Result<(), String> {
        let commit = IterationCommit {
            iteration,
            commit_hash: commit_hash.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.checkpoint_db
            .append_iteration_commit(execution_id, &commit)
    }

    /// Get all recorded iteration commits for an execution.
    pub fn get_iteration_commits(
        &self,
        execution_id: &str,
    ) -> Result<Vec<IterationCommit>, String> {
        self.checkpoint_db.get_iteration_commits(execution_id)
    }

    /// Execute rollback based on policy. Returns the commit hash rolled back to, if any.
    pub async fn execute_rollback(
        &self,
        execution_id: &str,
        policy: &RollbackPolicy,
        working_dir: &Path,
        source_commit: Option<&str>,
        iteration_results: &[super::types::IterationResult],
    ) -> Result<Option<String>, String> {
        match policy {
            RollbackPolicy::None => Ok(None),
            RollbackPolicy::Clean => {
                let commit = source_commit.ok_or_else(|| {
                    "Cannot rollback to clean state: no source_commit recorded".to_string()
                })?;
                info!(
                    "COMPENSATION: Rolling back to clean state (source commit {})",
                    &commit[..commit.len().min(8)]
                );
                self.git_reset(working_dir, commit).await?;
                Ok(Some(commit.to_string()))
            }
            RollbackPolicy::LastGood => {
                let commits = self.get_iteration_commits(execution_id)?;
                if commits.is_empty() {
                    warn!(
                        "COMPENSATION: No iteration commits recorded, cannot rollback to last good"
                    );
                    return Ok(None);
                }

                // Find the last iteration where verification improved (fewer failures
                // than the subsequent iteration, or the last one before failures increased).
                let last_good_iteration = find_last_good_iteration(iteration_results);

                if let Some(good_iter) = last_good_iteration {
                    // Find the commit for that iteration
                    if let Some(commit) = commits.iter().find(|c| c.iteration == good_iter) {
                        info!(
                            "COMPENSATION: Rolling back to last good iteration {} (commit {})",
                            good_iter,
                            &commit.commit_hash[..commit.commit_hash.len().min(8)]
                        );
                        self.git_reset(working_dir, &commit.commit_hash).await?;
                        return Ok(Some(commit.commit_hash.clone()));
                    }
                }

                // Fallback: revert to source commit if no good iteration found
                if let Some(commit) = source_commit {
                    info!("COMPENSATION: No good iteration found, rolling back to source commit");
                    self.git_reset(working_dir, commit).await?;
                    Ok(Some(commit.to_string()))
                } else {
                    warn!("COMPENSATION: No source commit available for fallback rollback");
                    Ok(None)
                }
            }
        }
    }

    /// Manually rollback to a specific iteration's commit.
    pub async fn rollback_to_iteration(
        &self,
        execution_id: &str,
        target_iteration: u32,
        working_dir: &Path,
    ) -> Result<String, String> {
        let commits = self.get_iteration_commits(execution_id)?;
        let commit = commits
            .iter()
            .find(|c| c.iteration == target_iteration)
            .ok_or_else(|| {
                format!(
                    "No commit checkpoint found for iteration {}",
                    target_iteration
                )
            })?;

        info!(
            "COMPENSATION: Manual rollback to iteration {} (commit {})",
            target_iteration,
            &commit.commit_hash[..commit.commit_hash.len().min(8)]
        );
        self.git_reset(working_dir, &commit.commit_hash).await?;
        Ok(commit.commit_hash.clone())
    }

    /// Reset git to a specific commit. Uses `git reset --hard` in worktree mode
    /// (safe since worktrees are isolated).
    async fn git_reset(&self, working_dir: &Path, commit: &str) -> Result<(), String> {
        let output = crate::process_helpers::tokio_no_window("git")
            .args(["reset", "--hard", commit])
            .current_dir(working_dir)
            .output()
            .await
            .map_err(|e| format!("Failed to run git reset: {}", e))?;

        if output.status.success() {
            info!(
                "COMPENSATION: git reset --hard {} succeeded",
                &commit[..commit.len().min(8)]
            );
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("git reset --hard failed: {}", stderr.trim()))
        }
    }
}

/// Capture a structured diff for the current iteration.
///
/// Runs `git diff --stat` and `git diff` between `commit_before` and `commit_after`
/// (or against working tree if no commits are available).
pub async fn capture_iteration_diff(
    working_dir: &Path,
    iteration: u32,
    commit_before: Option<&str>,
    commit_after: Option<&str>,
) -> Option<IterationDiff> {
    // Build diff args based on available commits
    let (stat_args, diff_args) = match (commit_before, commit_after) {
        (Some(before), Some(after)) => (
            vec!["diff", "--stat", before, after],
            vec!["diff", before, after],
        ),
        _ => (vec!["diff", "--stat"], vec!["diff"]),
    };

    // Capture diff stat
    let stat_output = crate::process_helpers::tokio_no_window("git")
        .args(&stat_args)
        .current_dir(working_dir)
        .output()
        .await
        .ok()?;

    if !stat_output.status.success() {
        return None;
    }

    let diff_stat = String::from_utf8_lossy(&stat_output.stdout)
        .trim()
        .to_string();
    if diff_stat.is_empty() {
        return None;
    }

    // Parse files changed and insertions/deletions from stat
    let (files_changed, insertions, deletions) = parse_diff_stat(&diff_stat);

    // Capture truncated diff
    let diff_output = crate::process_helpers::tokio_no_window("git")
        .args(&diff_args)
        .current_dir(working_dir)
        .output()
        .await
        .ok()?;

    let diff_summary = if diff_output.status.success() {
        let raw = String::from_utf8_lossy(&diff_output.stdout).to_string();
        if raw.len() > 4000 {
            let truncated = truncate_str(&raw, 4000);
            format!(
                "{}...\n[truncated, {} more chars]",
                truncated,
                raw.len() - truncated.len()
            )
        } else {
            raw
        }
    } else {
        String::new()
    };

    Some(IterationDiff {
        iteration,
        files_changed,
        diff_stat,
        diff_summary,
        insertions,
        deletions,
        commit_before: commit_before.map(|s| s.to_string()),
        commit_after: commit_after.map(|s| s.to_string()),
    })
}

/// Get current HEAD commit hash, or None if git is not available.
pub async fn get_head_commit_async(working_dir: &Path) -> Option<String> {
    let output = crate::process_helpers::tokio_no_window("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(working_dir)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Format accumulated iteration diffs as context for the AI prompt.
///
/// Returns a section like:
/// ```text
/// ## Changes from Previous Iterations
///
/// ### Iteration 1
/// 2 files changed: src/foo.rs (+15, -3), src/bar.ts (+7, -1)
///
/// ### Iteration 2
/// 1 file changed: src/foo.rs (+5, -10)
/// ```
pub fn format_iteration_diffs_context(diffs: &[IterationDiff], max_chars: usize) -> String {
    if diffs.is_empty() {
        return String::new();
    }

    let mut result = String::from("## Changes from Previous Iterations\n");
    let mut remaining = max_chars.saturating_sub(result.len());

    // Iterate newest-first, but we build oldest-first so newest gets priority
    // Actually, show all in order but truncate oldest if over budget
    for diff in diffs {
        let section = format!("\n### Iteration {}\n{}\n", diff.iteration, diff.diff_stat);

        if section.len() > remaining {
            if result.len() > 50 {
                result.push_str("\n[older iteration diffs truncated]\n");
            }
            break;
        }

        result.push_str(&section);
        remaining = remaining.saturating_sub(section.len());
    }

    result
}

/// Find the last iteration where verification was at its best (fewest failures).
fn find_last_good_iteration(iteration_results: &[super::types::IterationResult]) -> Option<u32> {
    if iteration_results.is_empty() {
        return None;
    }

    let mut best_iteration = iteration_results[0].iteration;
    let mut best_failures = iteration_results[0].failed_checks;

    for result in iteration_results.iter() {
        if result.failed_checks <= best_failures {
            best_failures = result.failed_checks;
            best_iteration = result.iteration;
        }
    }

    // Only return if the best iteration isn't the last one (no point rolling back to current)
    if best_iteration < iteration_results.last().map(|r| r.iteration).unwrap_or(0) {
        Some(best_iteration)
    } else {
        None
    }
}

/// Parse `git diff --stat` output to extract file list and insertion/deletion counts.
fn parse_diff_stat(stat: &str) -> (Vec<String>, u32, u32) {
    let mut files = Vec::new();
    let mut total_insertions = 0u32;
    let mut total_deletions = 0u32;

    for line in stat.lines() {
        let line = line.trim();
        // File lines look like: " src/foo.rs | 10 +++---"
        if line.contains('|') {
            if let Some(file_part) = line.split('|').next() {
                let file = file_part.trim().to_string();
                if !file.is_empty() {
                    files.push(file);
                }
            }
        }
        // Summary line looks like: " 2 files changed, 10 insertions(+), 3 deletions(-)"
        if line.contains("insertions(+)") || line.contains("insertion(+)") {
            for part in line.split(',') {
                let part = part.trim();
                if part.contains("insertion") {
                    if let Some(n) = part.split_whitespace().next().and_then(|s| s.parse().ok()) {
                        total_insertions = n;
                    }
                }
                if part.contains("deletion") {
                    if let Some(n) = part.split_whitespace().next().and_then(|s| s.parse().ok()) {
                        total_deletions = n;
                    }
                }
            }
        }
    }

    (files, total_insertions, total_deletions)
}
