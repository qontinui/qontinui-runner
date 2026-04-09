//! PR watcher: polls GitHub PRs associated with task runs for CI/review status changes.
//!
//! Unlike other watchers that are tied to a single trigger, the PR watcher is a
//! singleton service that monitors ALL active PR watch entries in the database.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::database::pg::PgDb;
use crate::trigger_system::github_api::{CiStatus, GitHubClient, ReviewStatus};
use crate::unified_workflow_executor::CiFailureContext;

/// Database row for PR watch state.
#[derive(Debug, Clone)]
pub struct PrWatchState {
    pub id: String,
    pub task_run_id: String,
    pub pr_number: u64,
    pub repo_full_name: String,
    pub head_sha: String,
    pub last_checks_status: String,
    pub last_review_status: String,
    pub auto_resume_count: u32,
    pub max_auto_resumes: u32,
    pub workflow_id: String,
    pub github_token: String,
    pub auto_resume_enabled: bool,
}

/// Action to take based on PR state change.
#[derive(Debug, Clone, PartialEq)]
pub enum PrAction {
    /// No change -- do nothing.
    NoAction,
    /// CI failed -- auto-resume the task with failure context.
    AutoResume { ci_context: CiFailureContext },
    /// CI passed and reviews approved -- mark task ready for completion.
    CiPassed,
    /// PR was merged -- mark task complete.
    Merged,
    /// PR was closed without merge -- mark task failed.
    Closed,
    /// Auto-resume limit reached -- needs manual intervention.
    AutoResumeLimitReached,
}

/// Pure function: determine what action to take given old and new PR state.
/// This is the core decision logic, easily unit-testable.
pub fn determine_pr_action(
    old_checks: &str,
    new_checks: &CiStatus,
    _old_review: &str,
    _new_review: &ReviewStatus,
    pr_merged: bool,
    pr_closed: bool,
    merge_conflict: bool,
    auto_resume_count: u32,
    max_auto_resumes: u32,
) -> PrAction {
    // PR lifecycle events take priority
    if pr_merged {
        return PrAction::Merged;
    }
    if pr_closed {
        return PrAction::Closed;
    }

    // CI status change
    match new_checks {
        CiStatus::Failure { failed_checks } => {
            // Only act on transition to failure (idempotent)
            if old_checks != "failure" {
                if auto_resume_count >= max_auto_resumes {
                    return PrAction::AutoResumeLimitReached;
                }
                return PrAction::AutoResume {
                    ci_context: CiFailureContext {
                        failed_check_names: failed_checks.clone(),
                        check_logs: HashMap::new(), // Logs fetched separately
                        pr_number: None,             // Set by caller
                        merge_conflict,
                    },
                };
            }
        }
        CiStatus::Success => {
            if old_checks != "success" {
                return PrAction::CiPassed;
            }
        }
        CiStatus::Pending => {}
    }

    PrAction::NoAction
}

/// Start the PR watcher as a background polling task.
///
/// Follows the same pattern as `start_health_check`: spawns a tokio task that
/// polls on an interval and checks the stop signal each iteration.
pub fn start_pr_watcher(
    pg_db: Arc<PgDb>,
    github_token: String,
    poll_interval_seconds: u64,
    stop_signal: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = match GitHubClient::new(&github_token) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("PR watcher: failed to create GitHub client: {}", e);
                return;
            }
        };

        let interval = Duration::from_secs(poll_interval_seconds.max(10)); // min 10s
        tracing::info!("PR watcher started (poll interval: {}s)", interval.as_secs());

        loop {
            if stop_signal.load(Ordering::SeqCst) {
                tracing::info!("PR watcher: stop signal received, exiting");
                break;
            }

            if let Err(e) = poll_active_prs(&pg_db, &client).await {
                tracing::warn!("PR watcher poll error: {}", e);
            }

            // Sleep in 5-second chunks for responsiveness to stop signal
            let mut remaining = interval;
            while remaining > Duration::ZERO {
                if stop_signal.load(Ordering::SeqCst) {
                    break;
                }
                let chunk = remaining.min(Duration::from_secs(5));
                tokio::time::sleep(chunk).await;
                remaining = remaining.saturating_sub(chunk);
            }
        }

        tracing::info!("PR watcher stopped");
    })
}

/// Poll all active PR watch entries and take appropriate actions.
///
/// Groups watches by GitHub token so we create one client per unique token
/// rather than one per PR (avoids wasteful reqwest client creation).
async fn poll_active_prs(pg_db: &PgDb, default_client: &GitHubClient) -> Result<(), String> {
    let watches = pg_db.get_active_pr_watches().await?;
    if watches.is_empty() {
        return Ok(());
    }

    tracing::debug!("PR watcher: polling {} active PR watches", watches.len());

    // Group watches by token; empty token means use the default client.
    // We create one GitHubClient per unique non-empty token to avoid
    // wasteful per-PR client creation.
    let mut by_token: HashMap<String, Vec<&PrWatchState>> = HashMap::new();
    for watch in &watches {
        by_token
            .entry(watch.github_token.clone())
            .or_default()
            .push(watch);
    }

    // Cache of token -> GitHubClient for non-default tokens
    let mut client_cache: HashMap<String, GitHubClient> = HashMap::new();

    for (token, token_watches) in &by_token {
        let client_ref: &GitHubClient = if token.is_empty() {
            default_client
        } else {
            if !client_cache.contains_key(token) {
                match GitHubClient::new(token) {
                    Ok(c) => {
                        client_cache.insert(token.clone(), c);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "PR watcher: failed to create GitHub client for token ({}...): {}",
                            &token[..token.len().min(4)],
                            e
                        );
                        continue;
                    }
                }
            }
            client_cache.get(token).unwrap()
        };

        for watch in token_watches {
            if let Err(e) = poll_single_pr(pg_db, client_ref, watch).await {
                tracing::warn!(
                    "PR watcher: error polling PR #{} for task {}: {}",
                    watch.pr_number,
                    watch.task_run_id,
                    e
                );
            }
        }
    }

    Ok(())
}

/// Poll a single PR and take action if state changed.
async fn poll_single_pr(
    pg_db: &PgDb,
    client: &GitHubClient,
    watch: &PrWatchState,
) -> Result<(), String> {
    let parts: Vec<&str> = watch.repo_full_name.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid repo_full_name: {}", watch.repo_full_name));
    }
    let (owner, repo) = (parts[0], parts[1]);

    // Fetch current PR status
    let pr = client.get_pr(owner, repo, watch.pr_number).await?;
    let check_runs = client.get_check_runs(owner, repo, &pr.head_sha).await?;
    let reviews = client.get_pr_reviews(owner, repo, watch.pr_number).await?;

    let ci_status = GitHubClient::derive_ci_status(&check_runs);
    let review_status = GitHubClient::derive_review_status(&reviews);

    let pr_closed = pr.state == "closed" && !pr.merged;
    let merge_conflict = pr.mergeable == Some(false);

    // Determine action
    let action = determine_pr_action(
        &watch.last_checks_status,
        &ci_status,
        &watch.last_review_status,
        &review_status,
        pr.merged,
        pr_closed,
        merge_conflict,
        watch.auto_resume_count,
        watch.max_auto_resumes,
    );

    // Update stored state
    let new_checks_str = match &ci_status {
        CiStatus::Pending => "pending",
        CiStatus::Success => "success",
        CiStatus::Failure { .. } => "failure",
    };
    let new_review_str = match &review_status {
        ReviewStatus::Pending => "pending",
        ReviewStatus::Approved => "approved",
        ReviewStatus::ChangesRequested => "changes_requested",
    };

    pg_db
        .update_pr_watch_state(&watch.id, new_checks_str, new_review_str, &pr.head_sha)
        .await?;

    // Execute action
    match action {
        PrAction::NoAction => {}
        PrAction::AutoResume { .. } if !watch.auto_resume_enabled => {
            tracing::info!(
                "PR watcher: CI failure on PR #{} for task {} but auto_resume is disabled, skipping",
                watch.pr_number,
                watch.task_run_id,
            );
        }
        PrAction::AutoResume { mut ci_context } => {
            ci_context.pr_number = Some(watch.pr_number);

            // Fetch logs for failed checks (best effort, truncate each to 4KB)
            for check in &check_runs {
                if check.conclusion.as_deref() == Some("failure") {
                    match client.get_check_run_log(owner, repo, check.id).await {
                        Ok(log) => {
                            ci_context.check_logs.insert(check.name.clone(), log);
                        }
                        Err(e) => {
                            tracing::debug!(
                                "PR watcher: could not fetch log for check '{}': {}",
                                check.name,
                                e
                            );
                        }
                    }
                }
            }

            pg_db.increment_pr_watch_auto_resume(&watch.id).await?;

            tracing::info!(
                "PR watcher: CI failure on PR #{} for task {}, auto-resuming (attempt {}/{})",
                watch.pr_number,
                watch.task_run_id,
                watch.auto_resume_count + 1,
                watch.max_auto_resumes,
            );

            // Store the CI context as JSON in the task_run's result_data for the executor to pick up
            let ci_context_json = serde_json::to_string(&ci_context).unwrap_or_default();
            pg_db
                .set_task_run_ci_failure_context(&watch.task_run_id, &ci_context_json)
                .await?;

            // Update task status to trigger re-execution
            pg_db
                .update_task_run_status(&watch.task_run_id, "queued")
                .await?;
        }
        PrAction::CiPassed => {
            tracing::info!(
                "PR watcher: CI passed on PR #{} for task {}",
                watch.pr_number,
                watch.task_run_id,
            );

            // Spawn review subtask if one doesn't already exist (idempotency)
            let existing_review = pg_db
                .has_review_subtask_for_pr(&watch.task_run_id, watch.pr_number)
                .await
                .unwrap_or(false);

            if !existing_review {
                use crate::unified_workflow_executor::review_subtask::{
                    spawn_review_subtask, ReviewSubtaskConfig,
                };

                let config = ReviewSubtaskConfig {
                    parent_task_run_id: watch.task_run_id.clone(),
                    pr_number: watch.pr_number,
                    repo_full_name: watch.repo_full_name.clone(),
                    review_model: Some("sonnet".to_string()),
                    blocks_parent: true,
                };

                match spawn_review_subtask(pg_db, config).await {
                    Ok(review_id) => {
                        tracing::info!(
                            "PR watcher: spawned review subtask {} for PR #{}",
                            review_id,
                            watch.pr_number
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "PR watcher: failed to spawn review subtask for PR #{}: {}",
                            watch.pr_number,
                            e
                        );
                    }
                }
            }
        }
        PrAction::Merged => {
            tracing::info!(
                "PR watcher: PR #{} merged for task {}",
                watch.pr_number,
                watch.task_run_id,
            );
            pg_db
                .mark_pr_watch_complete(&watch.id, "merged")
                .await?;
            pg_db
                .update_task_run_status(&watch.task_run_id, "complete")
                .await?;
        }
        PrAction::Closed => {
            tracing::info!(
                "PR watcher: PR #{} closed without merge for task {}",
                watch.pr_number,
                watch.task_run_id,
            );
            pg_db.mark_pr_watch_complete(&watch.id, "closed").await?;
        }
        PrAction::AutoResumeLimitReached => {
            tracing::warn!(
                "PR watcher: auto-resume limit ({}) reached for PR #{} task {}",
                watch.max_auto_resumes,
                watch.pr_number,
                watch.task_run_id,
            );
            pg_db
                .mark_pr_watch_complete(&watch.id, "limit_reached")
                .await?;
        }
    }

    Ok(())
}
