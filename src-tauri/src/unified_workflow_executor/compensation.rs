//! Compensation flows for automatic git rollback on workflow failure.
//!
//! Inspired by Netflix Conductor's compensation pattern, this module provides
//! opt-in git-based rollback when workflows fail. It records commit hashes
//! after each agentic iteration and can revert to a known-good state on failure.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::database::pg::PgDb;
use crate::str_utils::truncate_str;

use super::types::{
    CompensationAction, CompensationResult, CompensationType, IterationCommit, IterationDiff,
    RollbackPolicy,
};

/// Manages commit checkpoints and rollback operations for workflow executions.
///
/// The LIFO compensation stack lives behind `Arc<Mutex<..>>` so the manager
/// can be held by value in `LoopContext` while still supporting mutation
/// through shared `&self` references across handler call sites.
pub struct CompensationManager {
    pg_db: Arc<PgDb>,
    /// In-memory LIFO stack of compensation actions recorded during execution.
    stack: Arc<Mutex<Vec<CompensationAction>>>,
    /// Map from in-memory CompensationAction.id (String) to the PG row id
    /// returned by the INSERT. Needed so `execute_all` can atomically claim
    /// rows via UPDATE..WHERE NOT executed.
    id_map: Arc<Mutex<HashMap<String, i64>>>,
}

impl CompensationManager {
    pub fn new(pg_db: Arc<PgDb>) -> Self {
        Self {
            pg_db,
            stack: Arc::new(Mutex::new(Vec::new())),
            id_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Push a compensation action onto the in-memory LIFO stack and persist
    /// it to `compensation_actions` (under the `runner` schema via search_path).
    ///
    /// The in-memory push succeeds unconditionally — a persistence failure is
    /// logged but does not prevent compensation from being attempted in the
    /// current run. Only a catastrophic serialization failure returns `Err`.
    pub async fn push(
        &self,
        execution_id: &str,
        action: CompensationAction,
    ) -> Result<(), String> {
        // Serialize first so we bail early on the only truly fatal case.
        let action_json = serde_json::to_value(&action)
            .map_err(|e| format!("Failed to serialize CompensationAction: {}", e))?;

        let action_id = action.id.clone();

        // Push to in-memory stack first so compensation is attempted even if
        // the DB write fails below.
        let new_len = {
            let mut stack = self.stack.lock().await;
            stack.push(action);
            stack.len()
        };
        let action_index = (new_len - 1) as i32;

        // Persist to PG. Failures are logged but non-fatal.
        match self
            .pg_db
            .insert_compensation_action(execution_id, action_index, &action_json)
            .await
        {
            Ok(row_id) => {
                let mut id_map = self.id_map.lock().await;
                id_map.insert(action_id, row_id);
            }
            Err(e) => {
                warn!(
                    "COMPENSATION: Failed to persist action to PG (execution_id={}, index={}): {} — in-memory stack still holds the entry",
                    execution_id, action_index, e
                );
            }
        }

        Ok(())
    }

    /// Execute compensations based on the given rollback policy.
    ///
    /// Claims each action via PG `UPDATE .. WHERE NOT executed RETURNING id`
    /// before running it; if the claim returns no rows another run already
    /// executed that action (crash-resume idempotency guarantee — important
    /// because e.g. PID reuse on Windows would make re-killing a real bug).
    ///
    /// Individual compensation failures do NOT short-circuit the loop — this
    /// matches the behavior of the free-function `execute_all_compensations`.
    pub async fn execute_all(
        &self,
        policy: &RollbackPolicy,
        ctx_iteration_results: &[super::types::IterationResult],
    ) -> Vec<CompensationResult> {
        // Snapshot the stack so we don't hold the mutex across awaits.
        let actions: Vec<CompensationAction> = {
            let stack = self.stack.lock().await;
            stack.clone()
        };

        // Policy-based filtering of which actions to run.
        let to_run: Vec<CompensationAction> = match policy {
            RollbackPolicy::None => {
                info!("COMPENSATION: policy=None — skipping stack execution");
                return Vec::new();
            }
            RollbackPolicy::Clean => {
                // Full LIFO: reverse iteration over the whole stack.
                actions.iter().rev().cloned().collect()
            }
            RollbackPolicy::LastGood => {
                let best = find_last_good_iteration(ctx_iteration_results);
                match best {
                    Some(best_iter) => {
                        info!(
                            "COMPENSATION: policy=LastGood — rolling back entries with iteration > Some({})",
                            best_iter
                        );
                        // Roll back everything past the good point, LIFO.
                        actions
                            .iter()
                            .rev()
                            .filter(|a| a.iteration > Some(best_iter))
                            .cloned()
                            .collect()
                    }
                    None => {
                        info!(
                            "COMPENSATION: policy=LastGood found no good iteration — falling back to Clean"
                        );
                        actions.iter().rev().cloned().collect()
                    }
                }
            }
        };

        let mut results = Vec::with_capacity(to_run.len());
        let id_map_snapshot: HashMap<String, i64> = {
            let m = self.id_map.lock().await;
            m.clone()
        };

        for action in to_run.iter() {
            // Try to atomically claim the row in PG. If we don't have a row id
            // (persistence failed at push-time), execute unconditionally — the
            // in-memory stack is authoritative for this run.
            let row_id_opt = id_map_snapshot.get(&action.id).copied();

            if let Some(row_id) = row_id_opt {
                match self.pg_db.claim_compensation_action(row_id).await {
                    Ok(true) => {
                        // Claimed — proceed to execute.
                    }
                    Ok(false) => {
                        info!(
                            "COMPENSATION: action {} (row_id={}) already executed by another run — skipping",
                            action.id, row_id
                        );
                        continue;
                    }
                    Err(e) => {
                        warn!(
                            "COMPENSATION: Failed to claim action {} (row_id={}): {} — executing anyway",
                            action.id, row_id, e
                        );
                    }
                }
            }

            let result = execute_compensation(action).await;

            // Persist the result JSON back to the row (best-effort).
            if let Some(row_id) = row_id_opt {
                let result_json = match serde_json::to_value(&result) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "COMPENSATION: Failed to serialize result for action {}: {}",
                            action.id, e
                        );
                        serde_json::Value::Null
                    }
                };
                if let Err(e) = self
                    .pg_db
                    .save_compensation_result(row_id, &result_json)
                    .await
                {
                    warn!(
                        "COMPENSATION: Failed to save result for action {} (row_id={}): {}",
                        action.id, row_id, e
                    );
                }
            }

            results.push(result);
        }

        let succeeded = results.iter().filter(|r| r.success).count();
        let failed = results.iter().filter(|r| !r.success).count();
        info!(
            "COMPENSATION: execute_all complete ({:?}): {}/{} succeeded, {} failed",
            policy,
            succeeded,
            results.len(),
            failed
        );

        results
    }

    /// Rehydrate the in-memory stack from unexecuted PG rows for crash
    /// recovery. Overwrites current in-memory state — intended to be called
    /// only during startup-resume (no live run should have in-flight actions).
    ///
    /// Phase 1 has no caller for this method; the startup-resume integration
    /// is deferred (see plan §1.3 option (a)). The method must exist and must
    /// work — a later session will wire the call site.
    pub async fn load_pending(&self, execution_id: &str) -> Result<(), String> {
        let rows = self
            .pg_db
            .load_pending_compensation_actions(execution_id)
            .await?;

        let mut new_stack: Vec<CompensationAction> = Vec::with_capacity(rows.len());
        let mut new_map: HashMap<String, i64> = HashMap::with_capacity(rows.len());

        for (row_id, action_json) in rows {
            match serde_json::from_value::<CompensationAction>(action_json) {
                Ok(action) => {
                    new_map.insert(action.id.clone(), row_id);
                    new_stack.push(action);
                }
                Err(e) => {
                    warn!(
                        "COMPENSATION: Failed to deserialize pending action row_id={}: {} — skipping",
                        row_id, e
                    );
                }
            }
        }

        {
            let mut stack = self.stack.lock().await;
            *stack = new_stack;
        }
        {
            let mut id_map = self.id_map.lock().await;
            *id_map = new_map;
        }

        Ok(())
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
                    handle.block_on(async move { pg.append_iteration_commit(&eid, &commit).await })
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
                    handle.block_on(async move { pg.get_iteration_commits(&eid).await })
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

// ═════════════════════════════════════════════════════════════════════════
// Free compensation executor functions (ported from restate/compensation.rs).
//
// These are module-level `pub async fn`s — they are not methods on
// `CompensationManager`. A later pass will wire the LIFO stack into the
// manager; these functions provide the primitive "execute one compensation"
// operations used by both the manager and the Restate service.
// ═════════════════════════════════════════════════════════════════════════

/// Execute a single compensation action.
pub async fn execute_compensation(action: &CompensationAction) -> CompensationResult {
    let start = std::time::Instant::now();
    info!(
        "Executing compensation [{}]: {} ({})",
        action.id, action.description, action.phase
    );

    let result = match &action.action_type {
        CompensationType::GitReset {
            commit_hash,
            repo_path,
        } => execute_git_reset(commit_hash, repo_path).await,

        CompensationType::FileCleanup { paths } => execute_file_cleanup(paths).await,

        CompensationType::WorktreeRemove {
            worktree_path,
            branch_name,
        } => execute_worktree_remove(worktree_path, branch_name.as_deref()).await,

        CompensationType::ProcessKill { pid } => execute_process_kill(*pid).await,

        CompensationType::CustomCommand { command, args, cwd } => {
            execute_custom_command(command, args, cwd).await
        }
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    match &result {
        Ok(()) => info!(
            "Compensation [{}] succeeded in {}ms",
            action.id, duration_ms
        ),
        Err(e) => error!(
            "Compensation [{}] failed in {}ms: {}",
            action.id, duration_ms, e
        ),
    }

    CompensationResult {
        action_id: action.id.clone(),
        success: result.is_ok(),
        error: result.err(),
        duration_ms,
    }
}

/// Execute all compensation actions in LIFO (reverse) order.
/// Continues executing even if individual compensations fail.
pub async fn execute_all_compensations(actions: &[CompensationAction]) -> Vec<CompensationResult> {
    let mut results = Vec::with_capacity(actions.len());

    // Execute in reverse order (LIFO)
    for action in actions.iter().rev() {
        let result = execute_compensation(action).await;
        results.push(result);
    }

    let succeeded = results.iter().filter(|r| r.success).count();
    let failed = results.iter().filter(|r| !r.success).count();
    info!(
        "Compensation complete: {}/{} succeeded, {} failed",
        succeeded,
        actions.len(),
        failed
    );

    results
}

pub async fn execute_git_reset(commit_hash: &str, repo_path: &str) -> Result<(), String> {
    let output = crate::process_helpers::tokio_no_window("git")
        .args(["reset", "--hard", commit_hash])
        .current_dir(repo_path)
        .output()
        .await
        .map_err(|e| format!("Failed to run git reset: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git reset failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub async fn execute_file_cleanup(paths: &[String]) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in paths {
        if let Err(e) = tokio::fs::remove_file(path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("{}: {}", path, e));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("File cleanup errors: {}", errors.join(", ")))
    }
}

pub async fn execute_worktree_remove(
    worktree_path: &str,
    _branch_name: Option<&str>,
) -> Result<(), String> {
    let output = crate::process_helpers::tokio_no_window("git")
        .args(["worktree", "remove", "--force", worktree_path])
        .output()
        .await
        .map_err(|e| format!("Failed to run git worktree remove: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        // Fallback: just delete the directory
        warn!(
            "git worktree remove failed, falling back to directory removal: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        tokio::fs::remove_dir_all(worktree_path)
            .await
            .map_err(|e| format!("Failed to remove worktree directory: {}", e))
    }
}

pub async fn execute_process_kill(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = crate::process_helpers::tokio_no_window("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .await
            .map_err(|e| format!("taskkill failed: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "taskkill failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if result == 0 {
            Ok(())
        } else {
            Err(format!("kill({}) failed with errno", pid))
        }
    }
}

pub async fn execute_custom_command(
    command: &str,
    args: &[String],
    cwd: &str,
) -> Result<(), String> {
    let output = crate::process_helpers::tokio_no_window(command)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("Failed to run command '{}': {}", command, e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Command '{}' failed: {}",
            command,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
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

    // ── §1.7 compensation executor tests ─────────────────────────────

    /// Build a `CompensationAction` of the given type for tests.
    fn make_action(id: &str, phase: &str, action_type: CompensationType) -> CompensationAction {
        CompensationAction {
            id: id.to_string(),
            phase: phase.to_string(),
            iteration: None,
            action_type,
            recorded_at: "1970-01-01T00:00:00Z".to_string(),
            description: format!("test action {}", id),
        }
    }

    #[tokio::test]
    async fn test_push_and_execute_file_cleanup() {
        // Create a tempdir with two real files.
        let dir = tempfile::tempdir().expect("tempdir");
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        tokio::fs::write(&path_a, b"alpha").await.expect("write a");
        tokio::fs::write(&path_b, b"beta").await.expect("write b");
        assert!(path_a.exists());
        assert!(path_b.exists());

        let paths = vec![
            path_a.to_string_lossy().into_owned(),
            path_b.to_string_lossy().into_owned(),
        ];
        let action = make_action(
            "cleanup-1",
            "test",
            CompensationType::FileCleanup { paths },
        );

        let result = execute_compensation(&action).await;

        assert!(
            result.success,
            "FileCleanup should succeed, got error: {:?}",
            result.error
        );
        assert_eq!(result.action_id, "cleanup-1");
        assert!(!path_a.exists(), "a.txt should have been deleted");
        assert!(!path_b.exists(), "b.txt should have been deleted");
    }

    #[tokio::test]
    async fn test_push_and_execute_worktree_remove() {
        // We don't stand up a real git worktree here — just verify that
        // `execute_worktree_remove` returns a structured `CompensationResult`
        // for a nonexistent path (the fallback branch in the ported function
        // delegates to `tokio::fs::remove_dir_all`, which surfaces as an Err
        // rather than a panic). The point is to exercise the error path
        // without requiring a live git repository in the test environment.
        let nonexistent =
            std::env::temp_dir().join("qontinui-nonexistent-worktree-xyz-42");
        // Make sure it really doesn't exist.
        let _ = tokio::fs::remove_dir_all(&nonexistent).await;
        assert!(!nonexistent.exists());

        let action = make_action(
            "wt-1",
            "test",
            CompensationType::WorktreeRemove {
                worktree_path: nonexistent.to_string_lossy().into_owned(),
                branch_name: Some("test-branch".to_string()),
            },
        );

        // Must not panic; must always populate duration_ms.
        let result = execute_compensation(&action).await;

        assert_eq!(result.action_id, "wt-1");
        // duration_ms is u64 — assert it was at least recorded (0 is a valid
        // value on a very fast machine, but `success == false` here should
        // accompany a populated error string).
        if !result.success {
            assert!(
                result.error.is_some(),
                "failed worktree remove should carry an error message"
            );
        }
        // duration_ms is u64 (always populated by execute_compensation at line ~659).
        let _ = result.duration_ms;
    }

    #[tokio::test]
    async fn test_lifo_ordering() {
        // Exercises the lower-level `execute_all_compensations` free function,
        // which tests the LIFO intent without requiring a PG connection.
        // Push 3 FileCleanup actions with distinct file targets and verify
        // that the first element of the returned `results` vector corresponds
        // to the LAST pushed action.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut actions: Vec<CompensationAction> = Vec::new();
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for i in 0..3u32 {
            let p = dir.path().join(format!("f{}.txt", i));
            tokio::fs::write(&p, format!("body-{}", i).as_bytes())
                .await
                .expect("write");
            paths.push(p.clone());
            actions.push(make_action(
                &format!("a-{}", i),
                "test",
                CompensationType::FileCleanup {
                    paths: vec![p.to_string_lossy().into_owned()],
                },
            ));
        }

        let results = execute_all_compensations(&actions).await;
        assert_eq!(results.len(), 3);

        // LIFO: results[0] is the LAST pushed action ("a-2"), results[2] is
        // the FIRST pushed ("a-0").
        assert_eq!(results[0].action_id, "a-2", "LIFO: first result = last pushed");
        assert_eq!(results[1].action_id, "a-1");
        assert_eq!(results[2].action_id, "a-0", "LIFO: last result = first pushed");

        // All three files should be gone regardless of order.
        for p in &paths {
            assert!(!p.exists(), "{:?} should have been deleted", p);
        }
    }

    #[tokio::test]
    #[ignore = "requires PG via DATABASE_URL"]
    async fn test_persist_and_load_stack() {
        // Verifies §1.3 persistence + rehydrate semantics:
        // push N actions through manager A, instantiate manager B on the same
        // PgDb, call `load_pending`, and confirm the in-memory stack matches.
        let db = PgDb::new_blocking_for_test();
        let execution_id = format!(
            "test-persist-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );

        let mgr_a = CompensationManager::new(db.clone());
        for i in 0..3u32 {
            let action = make_action(
                &format!("persist-{}-{}", execution_id, i),
                "test",
                CompensationType::FileCleanup {
                    paths: vec![format!("/tmp/does-not-matter-{}.txt", i)],
                },
            );
            mgr_a
                .push(&execution_id, action)
                .await
                .expect("push must succeed with PG");
        }

        let mgr_b = CompensationManager::new(db.clone());
        mgr_b
            .load_pending(&execution_id)
            .await
            .expect("load_pending must succeed");

        let stack_b = mgr_b.stack.lock().await;
        assert_eq!(stack_b.len(), 3, "fresh manager should see 3 pending rows");
        // action_index ordering (0,1,2) is preserved by load_pending.
        assert!(stack_b[0].id.ends_with("-0"));
        assert!(stack_b[1].id.ends_with("-1"));
        assert!(stack_b[2].id.ends_with("-2"));
    }

    #[tokio::test]
    #[ignore = "requires PG"]
    async fn test_startup_compensation_load_and_execute() {
        // Verifies the startup-resume auto-compensation flow (plan §1.3 option-b):
        // 1. Create CompensationManager A, push 2 FileCleanup actions, don't execute.
        // 2. Create a fresh CompensationManager B (simulating runner startup).
        // 3. Call load_pending with the execution_id.
        // 4. Call execute_all(&RollbackPolicy::Clean, &[]).
        // 5. Verify both rows are now executed=TRUE in PG
        //    (by confirming load_pending_compensation_actions returns an empty Vec).
        let db = PgDb::new_blocking_for_test();
        let execution_id = format!(
            "test-startup-compensation-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );

        // Build two FileCleanup actions against real temp files so that
        // execute_compensation has something to actually delete and will
        // report success.
        let dir = tempfile::tempdir().expect("tempdir");
        let path_a = dir.path().join("startup-a.txt");
        let path_b = dir.path().join("startup-b.txt");
        tokio::fs::write(&path_a, b"alpha").await.expect("write a");
        tokio::fs::write(&path_b, b"beta").await.expect("write b");

        // --- Phase 1: push through manager A, simulating a live run ---
        let mgr_a = CompensationManager::new(db.clone());
        let action_a = make_action(
            &format!("startup-{}-0", execution_id),
            "test",
            CompensationType::FileCleanup {
                paths: vec![path_a.to_string_lossy().into_owned()],
            },
        );
        let action_b = make_action(
            &format!("startup-{}-1", execution_id),
            "test",
            CompensationType::FileCleanup {
                paths: vec![path_b.to_string_lossy().into_owned()],
            },
        );
        mgr_a
            .push(&execution_id, action_a)
            .await
            .expect("push A must succeed with PG");
        mgr_a
            .push(&execution_id, action_b)
            .await
            .expect("push B must succeed with PG");

        // Drop mgr_a so there's no doubt about which manager is executing.
        drop(mgr_a);

        // --- Phase 2: fresh manager simulating startup-resume ---
        let mgr_b = CompensationManager::new(db.clone());

        // Before load_pending: both files still exist, both rows still pending.
        let pending_before = db
            .load_pending_compensation_actions(&execution_id)
            .await
            .expect("query pending");
        assert_eq!(
            pending_before.len(),
            2,
            "two rows should be pending before startup compensation runs"
        );

        // --- Phase 3: load_pending ---
        mgr_b
            .load_pending(&execution_id)
            .await
            .expect("load_pending must succeed");

        // --- Phase 4: execute_all(Clean, &[]) — startup has no iteration context ---
        let results = mgr_b.execute_all(&RollbackPolicy::Clean, &[]).await;
        assert_eq!(results.len(), 2, "both actions should have been attempted");
        assert!(
            results.iter().all(|r| r.success),
            "both FileCleanup actions should succeed: {:?}",
            results
        );

        // --- Phase 5: verify both rows are now executed=TRUE in PG ---
        // `load_pending_compensation_actions` filters on `NOT executed`, so
        // if both rows were claimed and executed, it should return empty.
        let pending_after = db
            .load_pending_compensation_actions(&execution_id)
            .await
            .expect("query pending after");
        assert!(
            pending_after.is_empty(),
            "no rows should remain pending after execute_all, got {} row(s)",
            pending_after.len()
        );

        // And the files themselves should be gone.
        assert!(!path_a.exists(), "startup-a.txt should have been deleted");
        assert!(!path_b.exists(), "startup-b.txt should have been deleted");
    }

    #[tokio::test]
    async fn test_execute_continues_on_failure() {
        // Build 3 actions: [success, guaranteed-failure, success].
        // `execute_all_compensations` runs them in REVERSE order, so the
        // middle one is still the middle one in the output. Verify that the
        // failing action does NOT short-circuit the loop — all 3 results
        // must be present and the middle one must report success=false.
        let dir = tempfile::tempdir().expect("tempdir");
        let path_first = dir.path().join("first.txt");
        let path_last = dir.path().join("last.txt");
        tokio::fs::write(&path_first, b"x").await.expect("write first");
        tokio::fs::write(&path_last, b"y").await.expect("write last");

        // Use the system temp dir as cwd — exists on all platforms.
        let safe_cwd = std::env::temp_dir().to_string_lossy().into_owned();

        let actions = vec![
            // pushed first → executed LAST (LIFO)
            make_action(
                "ok-first",
                "test",
                CompensationType::FileCleanup {
                    paths: vec![path_first.to_string_lossy().into_owned()],
                },
            ),
            // pushed second → executed in the middle; must fail
            make_action(
                "fail-middle",
                "test",
                CompensationType::CustomCommand {
                    command: "qontinui-nonexistent-binary-xyz123".to_string(),
                    args: vec![],
                    cwd: safe_cwd,
                },
            ),
            // pushed last → executed FIRST (LIFO)
            make_action(
                "ok-last",
                "test",
                CompensationType::FileCleanup {
                    paths: vec![path_last.to_string_lossy().into_owned()],
                },
            ),
        ];

        let results = execute_all_compensations(&actions).await;

        assert_eq!(results.len(), 3, "all three actions must be attempted");

        // LIFO: results[0]="ok-last", results[1]="fail-middle", results[2]="ok-first".
        assert_eq!(results[0].action_id, "ok-last");
        assert!(results[0].success, "ok-last should succeed");

        assert_eq!(results[1].action_id, "fail-middle");
        assert!(
            !results[1].success,
            "fail-middle should fail (nonexistent binary)"
        );
        assert!(
            results[1].error.is_some(),
            "failed action must carry an error message"
        );

        assert_eq!(results[2].action_id, "ok-first");
        assert!(
            results[2].success,
            "ok-first must still run even after the middle failure: {:?}",
            results[2].error
        );

        // Both cleanup targets should have been deleted (execution continued).
        assert!(!path_first.exists());
        assert!(!path_last.exists());
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
            contingent_on: Vec::new(),
        }
    }
}
