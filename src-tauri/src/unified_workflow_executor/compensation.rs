//! Compensation flows for automatic git rollback on workflow failure.
//!
//! Inspired by Netflix Conductor's compensation pattern, this module provides
//! opt-in git-based rollback when workflows fail. It records commit hashes
//! after each agentic iteration and can revert to a known-good state on failure.

use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::pg::PgDb;
use crate::str_utils::truncate_str;

use super::types::{IterationCommit, IterationDiff, RollbackPolicy};

/// Manages commit checkpoints and rollback operations for workflow executions.
pub struct CompensationManager {
    pg_db: Arc<PgDb>,
}

impl CompensationManager {
    pub fn new(pg_db: Arc<PgDb>) -> Self {
        Self { pg_db }
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
        let pg = self.pg_db.clone();
        let eid = execution_id.to_string();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::task::block_in_place(|| {
                    handle.block_on(async move {
                        pg.append_iteration_commit(&eid, &commit).await
                    })
                })
            }));
            match result {
                Ok(inner) => inner,
                Err(_) => Err("block_in_place panicked in record_iteration_commit".to_string()),
            }
        } else {
            Err("No tokio runtime available for record_iteration_commit".to_string())
        }
    }

    /// Get all recorded iteration commits for an execution.
    pub fn get_iteration_commits(
        &self,
        execution_id: &str,
    ) -> Result<Vec<IterationCommit>, String> {
        let pg = self.pg_db.clone();
        let eid = execution_id.to_string();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tokio::task::block_in_place(|| {
                    handle.block_on(async move {
                        pg.get_iteration_commits(&eid).await
                    })
                })
            }));
            match result {
                Ok(inner) => inner,
                Err(_) => Err("block_in_place panicked in get_iteration_commits".to_string()),
            }
        } else {
            Err("No tokio runtime available for get_iteration_commits".to_string())
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── RollbackPolicy::from_str ──────────────────────────────────────

    #[test]
    fn rollback_policy_from_str_none() {
        assert_eq!(RollbackPolicy::from_str("none"), RollbackPolicy::None);
    }

    #[test]
    fn rollback_policy_from_str_last_good() {
        assert_eq!(
            RollbackPolicy::from_str("last_good"),
            RollbackPolicy::LastGood
        );
    }

    #[test]
    fn rollback_policy_from_str_clean() {
        assert_eq!(RollbackPolicy::from_str("clean"), RollbackPolicy::Clean);
    }

    #[test]
    fn rollback_policy_from_str_invalid_defaults_to_none() {
        assert_eq!(RollbackPolicy::from_str("bogus"), RollbackPolicy::None);
        assert_eq!(RollbackPolicy::from_str(""), RollbackPolicy::None);
        assert_eq!(RollbackPolicy::from_str("CLEAN"), RollbackPolicy::None);
    }

    #[test]
    fn rollback_policy_roundtrip() {
        for policy in &[
            RollbackPolicy::None,
            RollbackPolicy::LastGood,
            RollbackPolicy::Clean,
        ] {
            assert_eq!(RollbackPolicy::from_str(policy.as_str()), *policy);
        }
    }

    // ── format_iteration_diffs_context ────────────────────────────────

    #[test]
    fn format_diffs_empty_returns_empty_string() {
        let result = format_iteration_diffs_context(&[], 8000);
        assert!(result.is_empty(), "Expected empty string, got: {result}");
    }

    #[test]
    fn format_diffs_single_diff_contains_header() {
        let diff = IterationDiff {
            iteration: 1,
            files_changed: vec!["src/foo.rs".to_string()],
            diff_stat: " src/foo.rs | 5 +++++\n 1 file changed, 5 insertions(+)".to_string(),
            diff_summary: String::new(),
            insertions: 5,
            deletions: 0,
            commit_before: None,
            commit_after: None,
        };
        let result = format_iteration_diffs_context(&[diff], 8000);
        assert!(
            result.contains("## Changes from Previous Iterations"),
            "Missing header in: {result}"
        );
        assert!(
            result.contains("### Iteration 1"),
            "Missing iteration heading in: {result}"
        );
        assert!(
            result.contains("src/foo.rs"),
            "Missing diff stat content in: {result}"
        );
    }

    #[test]
    fn format_diffs_multiple_iterations() {
        let diffs: Vec<IterationDiff> = (1..=3)
            .map(|i| IterationDiff {
                iteration: i,
                files_changed: vec![format!("file_{i}.rs")],
                diff_stat: format!(" file_{i}.rs | {i} +\n 1 file changed, {i} insertions(+)"),
                diff_summary: String::new(),
                insertions: i,
                deletions: 0,
                commit_before: None,
                commit_after: None,
            })
            .collect();

        let result = format_iteration_diffs_context(&diffs, 8000);
        assert!(result.contains("### Iteration 1"));
        assert!(result.contains("### Iteration 2"));
        assert!(result.contains("### Iteration 3"));
    }

    #[test]
    fn format_diffs_truncates_when_over_budget() {
        // Create diffs large enough to exceed a small budget
        let diffs: Vec<IterationDiff> = (1..=20)
            .map(|i| IterationDiff {
                iteration: i,
                files_changed: vec![format!("very_long_filename_for_iteration_{i}.rs")],
                diff_stat: format!(
                    " very_long_filename_for_iteration_{i}.rs | 100 {}\n 1 file changed, 100 insertions(+)",
                    "+".repeat(80)
                ),
                diff_summary: String::new(),
                insertions: 100,
                deletions: 0,
                commit_before: None,
                commit_after: None,
            })
            .collect();

        // Use a small budget that can't fit all 20 iterations
        let result = format_iteration_diffs_context(&diffs, 400);
        assert!(
            result.contains("[older iteration diffs truncated]"),
            "Expected truncation marker in: {result}"
        );
        // Should contain at least iteration 1
        assert!(result.contains("### Iteration 1"));
        // Should NOT contain all 20
        assert!(
            !result.contains("### Iteration 20"),
            "Should have been truncated before iteration 20"
        );
    }

    #[test]
    fn format_diffs_respects_exact_budget() {
        let diff = IterationDiff {
            iteration: 1,
            files_changed: vec!["a.rs".to_string()],
            diff_stat: "a.rs | 1 +".to_string(),
            diff_summary: String::new(),
            insertions: 1,
            deletions: 0,
            commit_before: None,
            commit_after: None,
        };
        let result = format_iteration_diffs_context(&[diff], 8000);
        assert!(
            result.len() <= 8000,
            "Result exceeded budget: {} chars",
            result.len()
        );
    }

    // ── find_last_good_iteration ──────────────────────────────────────

    #[test]
    fn find_last_good_empty_results() {
        assert_eq!(find_last_good_iteration(&[]), None);
    }

    #[test]
    fn find_last_good_single_iteration_returns_none() {
        // Only one iteration — no point rolling back to current
        let results = vec![make_iter_result(1, 3)];
        assert_eq!(find_last_good_iteration(&results), None);
    }

    #[test]
    fn find_last_good_monotonic_improvement_returns_none() {
        // Each iteration is better — last is best, so no rollback target
        let results = vec![
            make_iter_result(1, 5),
            make_iter_result(2, 3),
            make_iter_result(3, 1),
        ];
        assert_eq!(find_last_good_iteration(&results), None);
    }

    #[test]
    fn find_last_good_regression_returns_best() {
        // Iteration 2 was best (1 failure), iteration 3 regressed
        let results = vec![
            make_iter_result(1, 5),
            make_iter_result(2, 1),
            make_iter_result(3, 4),
        ];
        assert_eq!(find_last_good_iteration(&results), Some(2));
    }

    #[test]
    fn find_last_good_early_best_with_late_regression() {
        let results = vec![
            make_iter_result(1, 0),
            make_iter_result(2, 2),
            make_iter_result(3, 5),
        ];
        assert_eq!(find_last_good_iteration(&results), Some(1));
    }

    #[test]
    fn find_last_good_ties_prefer_latest() {
        // Two iterations tied at 2 failures, then regression at 3
        let results = vec![
            make_iter_result(1, 2),
            make_iter_result(2, 2),
            make_iter_result(3, 5),
        ];
        // <= means ties go to the later iteration
        assert_eq!(find_last_good_iteration(&results), Some(2));
    }

    // ── parse_diff_stat ───────────────────────────────────────────────

    #[test]
    fn parse_diff_stat_basic() {
        let stat = " src/foo.rs | 10 ++++------\n src/bar.rs | 3 +++\n 2 files changed, 7 insertions(+), 6 deletions(-)";
        let (files, ins, del) = parse_diff_stat(stat);
        assert_eq!(files, vec!["src/foo.rs", "src/bar.rs"]);
        assert_eq!(ins, 7);
        assert_eq!(del, 6);
    }

    #[test]
    fn parse_diff_stat_single_insertion() {
        let stat = " a.rs | 1 +\n 1 file changed, 1 insertion(+)";
        let (files, ins, del) = parse_diff_stat(stat);
        assert_eq!(files, vec!["a.rs"]);
        assert_eq!(ins, 1);
        assert_eq!(del, 0);
    }

    #[test]
    fn parse_diff_stat_empty() {
        let (files, ins, del) = parse_diff_stat("");
        assert!(files.is_empty());
        assert_eq!(ins, 0);
        assert_eq!(del, 0);
    }

    // ── CompensationManager with PG ─────────────────────────

    #[test]
    #[ignore = "requires PG via DATABASE_URL"]
    fn compensation_manager_new_succeeds() {
        let db = PgDb::new_blocking_for_test();
        let _mgr = CompensationManager::new(db);
    }

    #[test]
    #[ignore = "requires PG via DATABASE_URL"]
    fn record_and_get_iteration_commits() {
        let db = PgDb::new_blocking_for_test();
        let exec_id = "test-exec-001";
        let mgr = CompensationManager::new(db);

        // Initially empty
        let commits = mgr.get_iteration_commits(exec_id).unwrap();
        assert!(commits.is_empty());

        // Record two iterations
        mgr.record_iteration_commit(exec_id, 1, "abc123").unwrap();
        mgr.record_iteration_commit(exec_id, 2, "def456").unwrap();

        let commits = mgr.get_iteration_commits(exec_id).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].iteration, 1);
        assert_eq!(commits[0].commit_hash, "abc123");
        assert_eq!(commits[1].iteration, 2);
        assert_eq!(commits[1].commit_hash, "def456");
    }

    #[test]
    #[ignore = "requires PG via DATABASE_URL"]
    fn get_commits_isolated_by_execution_id() {
        let db = PgDb::new_blocking_for_test();
        let mgr = CompensationManager::new(db);

        mgr.record_iteration_commit("exec-A", 1, "aaa111").unwrap();
        mgr.record_iteration_commit("exec-B", 1, "bbb222").unwrap();

        let a_commits = mgr.get_iteration_commits("exec-A").unwrap();
        assert_eq!(a_commits.len(), 1);
        assert_eq!(a_commits[0].commit_hash, "aaa111");

        let b_commits = mgr.get_iteration_commits("exec-B").unwrap();
        assert_eq!(b_commits.len(), 1);
        assert_eq!(b_commits[0].commit_hash, "bbb222");
    }

    // ── helpers ───────────────────────────────────────────────────────

    fn make_iter_result(
        iteration: u32,
        failed_checks: usize,
    ) -> super::super::types::IterationResult {
        super::super::types::IterationResult {
            iteration,
            verification_passed: failed_checks == 0,
            critical_failure: false,
            passed_checks: 10_usize.saturating_sub(failed_checks),
            failed_checks,
            failure_context: String::new(),
            agentic_phase_ran: true,
            agentic_phase_success: Some(true),
            blame_json: None,
        }
    }
}
