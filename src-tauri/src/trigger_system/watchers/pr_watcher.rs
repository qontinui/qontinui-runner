//! PR watcher: polls GitHub PRs associated with task runs for CI/review status changes.
//!
//! Unlike other watchers that are tied to a single trigger, the PR watcher is a
//! singleton service that monitors ALL active PR watch entries in the database.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::config_storage::ConfigStorage;
use crate::database::pg::PgDb;
use crate::trigger_system::github_api::{CiStatus, GitHubClient, PrStatus, ReviewStatus};
use crate::trigger_system::pr_shepherd::{self, PendingPrSeed, SeedIdentity, SeedTarget};
use crate::unified_workflow_executor::CiFailureContext;
use crate::AppState;

/// Database row for PR watch state.
#[derive(Debug, Clone)]
pub struct PrWatchState {
    pub id: String,
    /// The authoring task run. `None` for PR-shepherd-seeded watches from
    /// interactive sessions (plan 2026-07-04-runner-pr-shepherd Phase 1) —
    /// those rows are keyed on `authoring_session_id` instead and never
    /// auto-resume (there is no task to re-queue).
    pub task_run_id: Option<String>,
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
    /// Coord session id of the authoring session (nullable; PR-shepherd
    /// Phase 1). Set alongside `task_run_id` when the registrar resolved a
    /// mapping, or alone for interactive-session watches.
    pub authoring_session_id: Option<String>,
    /// When all non-skipped checks first passed on the CURRENT head
    /// (PR-shepherd Phase 2). Carried while the green streak holds; reset on
    /// head change or any red/pending flip. Feeds the green-but-unmerged
    /// detection in [`determine_pr_action`].
    pub first_fully_green_at: Option<DateTime<Utc>>,
}

impl PrWatchState {
    /// Owner label for log lines: the task run id when present, else the
    /// authoring session id.
    fn owner_label(&self) -> &str {
        self.task_run_id
            .as_deref()
            .or(self.authoring_session_id.as_deref())
            .unwrap_or("<unattributed>")
    }
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
    /// PR-shepherd Phase 2: the PR has been fully green on an unchanged head
    /// for longer than the shepherd threshold and is still open — something
    /// downstream (merge train, operator) is not landing it. `green_for` is
    /// the elapsed streak duration. Fires on every poll while the condition
    /// holds; remediation-side dedup is PR-keyed and lands in a later phase.
    GreenButUnmerged { green_for: Duration },
}

/// Pure function: determine what action to take given old and new PR state.
/// This is the core decision logic, easily unit-testable.
///
/// `first_fully_green_at` is the (already set/carried/reset — see
/// [`compute_first_fully_green_at`]) start of the current all-green streak;
/// `green_threshold` is `Some` only when the PR shepherd is enabled — `None`
/// disables green-but-unmerged detection entirely.
#[allow(clippy::too_many_arguments)]
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
    head_sha_changed: bool,
    first_fully_green_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    green_threshold: Option<Duration>,
) -> PrAction {
    // PR lifecycle events take priority
    if pr_merged {
        return PrAction::Merged;
    }
    if pr_closed {
        return PrAction::Closed;
    }

    // Head SHA changed: developer pushed new commits.
    // All previously stored CI/review state is for the old SHA and is now stale.
    // Wait for CI to fully run on the new SHA before taking any action.
    if head_sha_changed {
        // The stored state will be updated to the new SHA's checks/reviews,
        // so the next poll evaluates the new SHA's results as a fresh baseline.
        return PrAction::NoAction;
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
                        pr_number: None,            // Set by caller
                        merge_conflict,
                    },
                };
            }
        }
        CiStatus::Success => {
            if old_checks != "success" {
                return PrAction::CiPassed;
            }

            // Green-but-unmerged (PR-shepherd Phase 2): the PR is open, the
            // head is unchanged, and the all-green streak has outlived the
            // threshold. Checked only in the steady green state (not on the
            // CiPassed transition — a fresh streak is by definition under
            // any sane threshold).
            if let (Some(green_since), Some(threshold)) = (first_fully_green_at, green_threshold)
            {
                // Clock skew putting `green_since` in the future yields a
                // negative signed duration; `to_std()` fails and we treat the
                // streak as zero-length.
                if let Ok(green_for) = now.signed_duration_since(green_since).to_std() {
                    if green_for > threshold {
                        return PrAction::GreenButUnmerged { green_for };
                    }
                }
            }
        }
        CiStatus::Pending => {}
    }

    PrAction::NoAction
}

/// Pure function (PR-shepherd Phase 2): compute the new `first_fully_green_at`
/// for a watch from this poll's observations.
///
/// - Any red/pending status clears the streak.
/// - A head change under a green status restarts the streak at `now` (the
///   stored timestamp belongs to the OLD head — CI truth is per-head;
///   remediation-side dedup in a later phase is PR-keyed, so resetting here
///   is correct).
/// - A continuing green streak on the same head carries the stored value,
///   starting one at `now` if none was recorded yet.
pub fn compute_first_fully_green_at(
    ci_green: bool,
    head_sha_changed: bool,
    stored: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !ci_green {
        return None;
    }
    if head_sha_changed {
        return Some(now);
    }
    stored.or(Some(now))
}

/// Dependencies for executing review subtasks from within the PR watcher.
///
/// These are cloneable references to the same state used by `TriggerExecutorDeps`.
#[derive(Clone)]
pub struct PrWatcherDeps {
    pub app_state: Arc<AppState>,
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    pub app_handle: tauri::AppHandle,
    pub pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
}

/// Start the PR watcher as a background polling task.
///
/// Follows the same pattern as `start_health_check`: spawns a tokio task that
/// polls on an interval and checks the stop signal each iteration.
pub fn start_pr_watcher(
    pg_db: Arc<PgDb>,
    github_token: String,
    poll_interval_seconds: u64,
    shepherd_repos: Vec<String>,
    stop_signal: Arc<AtomicBool>,
    execution_deps: Option<PrWatcherDeps>,
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
        tracing::info!(
            "PR watcher started (poll interval: {}s)",
            interval.as_secs()
        );

        loop {
            if stop_signal.load(Ordering::SeqCst) {
                tracing::info!("PR watcher: stop signal received, exiting");
                break;
            }

            if let Err(e) =
                poll_active_prs(&pg_db, &client, &shepherd_repos, execution_deps.as_ref()).await
            {
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
async fn poll_active_prs(
    pg_db: &PgDb,
    default_client: &GitHubClient,
    shepherd_repos: &[String],
    execution_deps: Option<&PrWatcherDeps>,
) -> Result<(), String> {
    // PR-shepherd Phase 1: land any transcript-observed `gh pr create` seeds
    // BEFORE loading active watches so a seed becomes a watch within one
    // poll. Best-effort — seed failures never break the poll itself.
    if pr_shepherd::autoseed_enabled() {
        drain_and_seed_pending(pg_db, default_client, shepherd_repos).await;
    }

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
            if let Err(e) = poll_single_pr(pg_db, client_ref, watch, execution_deps).await {
                tracing::warn!(
                    "PR watcher: error polling PR #{} for {}: {}",
                    watch.pr_number,
                    watch.owner_label(),
                    e
                );
            }
        }
    }

    Ok(())
}

/// PR-shepherd Phase 1: drain the pending-seed queue populated by the
/// transcript watcher's `gh pr create` detection and upsert `pr_watch_state`
/// rows. Branch-only seeds resolve to a PR number via
/// `GET /pulls?head=<owner>:<branch>`; already-closed/merged PRs and repos
/// outside the trigger's `shepherd_repos` allowlist are skipped. Best-effort
/// throughout — a failed seed is logged and dropped (the transcript watcher
/// will re-observe a genuinely open PR only if the operator re-runs
/// `gh pr create`, but the seed pipeline is idempotent when it does).
async fn drain_and_seed_pending(
    pg_db: &PgDb,
    client: &GitHubClient,
    shepherd_repos: &[String],
) {
    for seed in pr_shepherd::drain_seeds() {
        if let Err(e) = seed_one_pending(pg_db, client, shepherd_repos, &seed).await {
            tracing::warn!(
                "PR shepherd: failed to seed watch for {:?}: {}",
                seed.target,
                e
            );
        }
    }
}

/// Land one pending seed as a `pr_watch_state` row (or skip it, with a debug
/// log, when the allowlist excludes it / no open PR matches / the PR already
/// terminated).
async fn seed_one_pending(
    pg_db: &PgDb,
    client: &GitHubClient,
    shepherd_repos: &[String],
    seed: &PendingPrSeed,
) -> Result<(), String> {
    let repo_full_name = match &seed.target {
        SeedTarget::Pr { repo_full_name, .. } | SeedTarget::Branch { repo_full_name, .. } => {
            repo_full_name.clone()
        }
    };
    if !pr_shepherd::repo_allowed(&repo_full_name, shepherd_repos) {
        tracing::debug!(
            "PR shepherd: repo {} not in shepherd_repos allowlist — dropping seed",
            repo_full_name
        );
        return Ok(());
    }
    let parts: Vec<&str> = repo_full_name.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid repo_full_name: {}", repo_full_name));
    }
    let (owner, repo) = (parts[0], parts[1]);

    // Resolve to a live PR (number + current head SHA).
    let pr: PrStatus = match &seed.target {
        SeedTarget::Pr { pr_number, .. } => client.get_pr(owner, repo, *pr_number).await?,
        SeedTarget::Branch { branch, .. } => {
            let prs = client.list_open_prs_by_head(owner, repo, branch).await?;
            match prs.into_iter().next() {
                Some(pr) => pr,
                None => {
                    tracing::debug!(
                        "PR shepherd: no open PR for {}:{} — dropping seed",
                        repo_full_name,
                        branch
                    );
                    return Ok(());
                }
            }
        }
    };
    if pr.merged || pr.state == "closed" {
        tracing::debug!(
            "PR shepherd: PR #{} in {} already terminated — dropping seed",
            pr.number,
            repo_full_name
        );
        return Ok(());
    }

    // Auto-resume re-queues the authoring TASK RUN, so only task-run-keyed
    // watches enable it; an interactive session has nothing to re-queue
    // (Phase 4 wires its notification path).
    let (task_run_id, authoring_session_id, auto_resume_enabled) = match &seed.identity {
        SeedIdentity::TaskRun {
            task_run_id,
            authoring_session_id,
        } => (
            Some(task_run_id.as_str()),
            authoring_session_id.as_deref(),
            true,
        ),
        SeedIdentity::Session {
            authoring_session_id,
        } => (None, Some(authoring_session_id.as_str()), false),
    };

    // No workflow backs a shepherd-seeded watch; empty token means the
    // trigger's default GitHub client polls it. max_auto_resumes keeps the
    // trigger-config default (10) for task-run seeds.
    let watch_id = pg_db
        .upsert_pr_watch_state(
            task_run_id,
            authoring_session_id,
            pr.number,
            &repo_full_name,
            &pr.head_sha,
            "",
            10,
            "",
            auto_resume_enabled,
        )
        .await?;

    tracing::info!(
        target: "pr_shepherd",
        watch_id = %watch_id,
        repo = %repo_full_name,
        pr_number = pr.number,
        task_run_id = task_run_id.unwrap_or("<none>"),
        authoring_session_id = authoring_session_id.unwrap_or("<none>"),
        "pr_shepherd: seeded PR watch from transcript-observed gh pr create"
    );
    Ok(())
}

/// Poll a single PR and take action if state changed.
async fn poll_single_pr(
    pg_db: &PgDb,
    client: &GitHubClient,
    watch: &PrWatchState,
    execution_deps: Option<&PrWatcherDeps>,
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

    // Detect head SHA changes (developer pushed new commits)
    let head_sha_changed = watch.head_sha != pr.head_sha;
    if head_sha_changed {
        tracing::warn!(
            "PR #{} head SHA changed ({} → {}), developer pushed new commits",
            watch.pr_number,
            &watch.head_sha[..watch.head_sha.len().min(8)],
            &pr.head_sha[..pr.head_sha.len().min(8)],
        );
    }

    // PR-shepherd Phase 2: advance/reset the all-green streak clock. Tracked
    // (and detected) only while the shepherd master flag is on — the flag
    // gates ALL new behavior, so with it off the column stays NULL and
    // `determine_pr_action` receives no threshold.
    let now: DateTime<Utc> = Utc::now();
    let green_threshold: Option<Duration> = if pr_shepherd::shepherd_enabled() {
        Some(pr_shepherd::green_threshold())
    } else {
        None
    };
    let first_fully_green_at: Option<DateTime<Utc>> = if green_threshold.is_some() {
        compute_first_fully_green_at(
            matches!(ci_status, CiStatus::Success),
            head_sha_changed,
            watch.first_fully_green_at,
            now,
        )
    } else {
        None
    };

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
        head_sha_changed,
        first_fully_green_at,
        now,
        green_threshold,
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
        .update_pr_watch_state(
            &watch.id,
            new_checks_str,
            new_review_str,
            &pr.head_sha,
            first_fully_green_at,
        )
        .await?;

    // Execute action
    match action {
        PrAction::NoAction => {}
        PrAction::AutoResume { .. } if !watch.auto_resume_enabled => {
            tracing::info!(
                "PR watcher: CI failure on PR #{} for {} but auto_resume is disabled, skipping",
                watch.pr_number,
                watch.owner_label(),
            );
        }
        PrAction::AutoResume { mut ci_context } => {
            // Auto-resume re-queues the authoring TASK RUN; a session-seeded
            // watch has none. Seeds set auto_resume_enabled=false so the arm
            // above catches them — this guard is the defensive backstop.
            let Some(task_run_id) = watch.task_run_id.as_deref() else {
                tracing::info!(
                    "PR watcher: CI failure on PR #{} for session {} — no task run to auto-resume, skipping",
                    watch.pr_number,
                    watch.owner_label(),
                );
                return Ok(());
            };
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
                task_run_id,
                watch.auto_resume_count + 1,
                watch.max_auto_resumes,
            );

            // Store the CI context as JSON in the task_run's result_data for the executor to pick up
            let ci_context_json = match serde_json::to_string(&ci_context) {
                Ok(json) => json,
                Err(e) => {
                    tracing::warn!(
                        "PR watcher: failed to serialize CI context for task {}: {e}",
                        task_run_id
                    );
                    "{}".to_string()
                }
            };
            pg_db
                .set_task_run_ci_failure_context(task_run_id, &ci_context_json)
                .await?;

            // Update task status to trigger re-execution
            pg_db.update_task_run_status(task_run_id, "queued").await?;
        }
        PrAction::CiPassed => {
            tracing::info!(
                "PR watcher: CI passed on PR #{} for {}",
                watch.pr_number,
                watch.owner_label(),
            );

            // The review subtask is a child of the authoring TASK RUN; a
            // session-seeded watch has none, so its CI-pass is log-only.
            let Some(task_run_id) = watch.task_run_id.as_deref() else {
                return Ok(());
            };

            // Spawn review subtask if one doesn't already exist (idempotency)
            let existing_review = pg_db
                .has_review_subtask_for_pr(task_run_id, watch.pr_number)
                .await
                .unwrap_or(false);

            if !existing_review {
                use crate::unified_workflow_executor::review_subtask::{
                    spawn_review_subtask, ReviewSubtaskConfig,
                };

                let review_model = Some("sonnet".to_string());
                let config = ReviewSubtaskConfig {
                    parent_task_run_id: task_run_id.to_string(),
                    pr_number: watch.pr_number,
                    repo_full_name: watch.repo_full_name.clone(),
                    review_model: review_model.clone(),
                    blocks_parent: true,
                };

                match spawn_review_subtask(pg_db, config).await {
                    Ok(review_id) => {
                        tracing::info!(
                            "PR watcher: spawned review subtask {} for PR #{}",
                            review_id,
                            watch.pr_number
                        );

                        // Execute the review subtask (spawn the actual workflow)
                        if let Some(deps) = execution_deps {
                            use crate::unified_workflow_executor::review_subtask::{
                                execute_review_subtask, ReviewDeps,
                            };

                            let review_name = format!(
                                "Review PR #{} ({})",
                                watch.pr_number, watch.repo_full_name
                            );
                            // Read the prompt back from the task run we just created
                            let prompt = match pg_db.get_task_run(&review_id).await {
                                Ok(Some(tr)) => tr.prompt.unwrap_or_else(|| {
                                    format!(
                                        "Review PR #{} in {}",
                                        watch.pr_number, watch.repo_full_name
                                    )
                                }),
                                _ => format!(
                                    "Review PR #{} in {}",
                                    watch.pr_number, watch.repo_full_name
                                ),
                            };

                            let review_deps = ReviewDeps {
                                app_state: deps.app_state.clone(),
                                config_storage: deps.config_storage.clone(),
                                app_handle: deps.app_handle.clone(),
                                pid_tracker: deps.pid_tracker.clone(),
                            };

                            execute_review_subtask(
                                review_deps,
                                review_id,
                                review_name,
                                prompt,
                                review_model,
                            );
                        } else {
                            tracing::warn!(
                                "PR watcher: no execution deps available, review subtask {} created but not executed",
                                review_id
                            );
                        }
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
                "PR watcher: PR #{} merged for {}",
                watch.pr_number,
                watch.owner_label(),
            );
            pg_db.mark_pr_watch_complete(&watch.id, "merged").await?;
            // Only task-run-keyed watches have a task to complete.
            if let Some(task_run_id) = watch.task_run_id.as_deref() {
                pg_db
                    .update_task_run_status(task_run_id, "complete")
                    .await?;
            }
        }
        PrAction::Closed => {
            tracing::info!(
                "PR watcher: PR #{} closed without merge for {}",
                watch.pr_number,
                watch.owner_label(),
            );
            pg_db.mark_pr_watch_complete(&watch.id, "closed").await?;
        }
        PrAction::AutoResumeLimitReached => {
            tracing::warn!(
                "PR watcher: auto-resume limit ({}) reached for PR #{} ({})",
                watch.max_auto_resumes,
                watch.pr_number,
                watch.owner_label(),
            );
            pg_db
                .mark_pr_watch_complete(&watch.id, "limit_reached")
                .await?;
        }
        PrAction::GreenButUnmerged { green_for } => {
            handle_green_but_unmerged(watch, green_for);
        }
    }

    Ok(())
}

/// PR-shepherd Phase 2 seam: the PR has been fully green on an unchanged head
/// past `QONTINUI_PR_SHEPHERD_GREEN_THRESHOLD` and is still open. Phase 3
/// (coord diagnosis + classification) plugs in here; until then the handler
/// emits one structured info event per poll so the condition is observable
/// device-locally — deliberately tighter and coord-independent relative to
/// coord's own `pr_merge_green_unlanded` alert (2h dwell, coord-side).
fn handle_green_but_unmerged(watch: &PrWatchState, green_for: Duration) {
    tracing::info!(
        target: "pr_shepherd",
        watch_id = %watch.id,
        repo = %watch.repo_full_name,
        pr_number = watch.pr_number,
        head_sha = %watch.head_sha,
        owner = %watch.owner_label(),
        green_for_secs = green_for.as_secs(),
        "pr_shepherd: PR green-but-unmerged past threshold"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: Duration = Duration::from_secs(45 * 60);

    /// Call [`determine_pr_action`] with the boilerplate defaulted: open PR,
    /// no merge conflict, counters 0/10, review state pending.
    #[allow(clippy::too_many_arguments)]
    fn determine(
        old_checks: &str,
        new_checks: &CiStatus,
        pr_merged: bool,
        pr_closed: bool,
        head_sha_changed: bool,
        first_fully_green_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
        green_threshold: Option<Duration>,
    ) -> PrAction {
        determine_pr_action(
            old_checks,
            new_checks,
            "pending",
            &ReviewStatus::Pending,
            pr_merged,
            pr_closed,
            false,
            0,
            10,
            head_sha_changed,
            first_fully_green_at,
            now,
            green_threshold,
        )
    }

    fn minutes_ago(now: DateTime<Utc>, minutes: i64) -> DateTime<Utc> {
        now - chrono::Duration::minutes(minutes)
    }

    #[test]
    fn green_past_threshold_fires_green_but_unmerged() {
        let now = Utc::now();
        let since = minutes_ago(now, 46);
        let action = determine(
            "success",
            &CiStatus::Success,
            false,
            false,
            false,
            Some(since),
            now,
            Some(THRESHOLD),
        );
        match action {
            PrAction::GreenButUnmerged { green_for } => {
                assert_eq!(green_for.as_secs(), 46 * 60);
            }
            other => panic!("expected GreenButUnmerged, got {other:?}"),
        }
    }

    #[test]
    fn green_under_threshold_is_no_action() {
        let now = Utc::now();
        let action = determine(
            "success",
            &CiStatus::Success,
            false,
            false,
            false,
            Some(minutes_ago(now, 44)),
            now,
            Some(THRESHOLD),
        );
        assert_eq!(action, PrAction::NoAction);
    }

    #[test]
    fn transition_to_green_fires_ci_passed_not_green_but_unmerged() {
        // Fresh transition: old state pending — CiPassed wins even if the
        // caller somehow passed an ancient streak start.
        let now = Utc::now();
        let action = determine(
            "pending",
            &CiStatus::Success,
            false,
            false,
            false,
            Some(minutes_ago(now, 120)),
            now,
            Some(THRESHOLD),
        );
        assert_eq!(action, PrAction::CiPassed);
    }

    #[test]
    fn head_change_returns_no_action_even_when_green_past_threshold() {
        let now = Utc::now();
        let action = determine(
            "success",
            &CiStatus::Success,
            false,
            false,
            true, // head changed
            Some(minutes_ago(now, 120)),
            now,
            Some(THRESHOLD),
        );
        assert_eq!(action, PrAction::NoAction);
    }

    #[test]
    fn merged_and_closed_take_priority_over_green() {
        let now = Utc::now();
        let long_green = Some(minutes_ago(now, 120));
        assert_eq!(
            determine(
                "success",
                &CiStatus::Success,
                true,
                false,
                false,
                long_green,
                now,
                Some(THRESHOLD),
            ),
            PrAction::Merged
        );
        assert_eq!(
            determine(
                "success",
                &CiStatus::Success,
                false,
                true,
                false,
                long_green,
                now,
                Some(THRESHOLD),
            ),
            PrAction::Closed
        );
    }

    #[test]
    fn disabled_threshold_never_fires_green_but_unmerged() {
        let now = Utc::now();
        let action = determine(
            "success",
            &CiStatus::Success,
            false,
            false,
            false,
            Some(minutes_ago(now, 600)),
            now,
            None, // shepherd off
        );
        assert_eq!(action, PrAction::NoAction);
    }

    #[test]
    fn no_streak_start_is_no_action() {
        let now = Utc::now();
        let action = determine(
            "success",
            &CiStatus::Success,
            false,
            false,
            false,
            None,
            now,
            Some(THRESHOLD),
        );
        assert_eq!(action, PrAction::NoAction);
    }

    #[test]
    fn red_transition_still_auto_resumes() {
        // The Phase-2 params must not disturb the existing failure path.
        let now = Utc::now();
        let action = determine(
            "success",
            &CiStatus::Failure {
                failed_checks: vec!["build".to_string()],
            },
            false,
            false,
            false,
            Some(minutes_ago(now, 120)),
            now,
            Some(THRESHOLD),
        );
        match action {
            PrAction::AutoResume { ci_context } => {
                assert_eq!(ci_context.failed_check_names, vec!["build".to_string()]);
            }
            other => panic!("expected AutoResume, got {other:?}"),
        }
    }

    #[test]
    fn future_streak_start_is_no_action() {
        // Clock skew: a streak start in the future must not fire (to_std()
        // on the negative duration fails and is treated as zero-length).
        let now = Utc::now();
        let action = determine(
            "success",
            &CiStatus::Success,
            false,
            false,
            false,
            Some(now + chrono::Duration::minutes(5)),
            now,
            Some(THRESHOLD),
        );
        assert_eq!(action, PrAction::NoAction);
    }

    #[test]
    fn compute_streak_sets_carries_and_resets() {
        let now = Utc::now();
        let earlier = minutes_ago(now, 30);

        // Fresh green with no stored streak → starts at now.
        assert_eq!(
            compute_first_fully_green_at(true, false, None, now),
            Some(now)
        );
        // Continuing green streak on the same head → carries the stored start.
        assert_eq!(
            compute_first_fully_green_at(true, false, Some(earlier), now),
            Some(earlier)
        );
        // Red/pending flip → clears the streak.
        assert_eq!(compute_first_fully_green_at(false, false, Some(earlier), now), None);
        // Head change while green → restarts at now (CI truth is per-head).
        assert_eq!(
            compute_first_fully_green_at(true, true, Some(earlier), now),
            Some(now)
        );
        // Head change while red → still cleared.
        assert_eq!(compute_first_fully_green_at(false, true, Some(earlier), now), None);
    }
}
