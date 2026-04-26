//! Scheduler Service
//!
//! Background service that monitors scheduled tasks and executes them
//! at their scheduled times. Integrates with existing workflow and prompt
//! execution infrastructure.

use crate::commands::AppState;
use crate::database::pg::PgDb;
use crate::scheduler::{
    compute_next_run, condition_status_default, CatchUpPolicy, ConditionStatus, RepositoryWatch,
    ScheduleExpression, ScheduledTask, ScheduledTaskExt, ScheduledTaskStatus, ScheduledTaskType,
    TaskExecutionRecord, TaskExecutionRecordExt,
};
use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use walkdir::WalkDir;

// ============================================================================
// Catch-up context (Phase B)
// ============================================================================

/// Context threaded through the dispatch path when an execution is launched
/// by the missed-run reconciler rather than the normal scheduler tick.
///
/// When `Some(_)`, [`SchedulerService::execute_task`] (and its async
/// launch-and-poll helper) stamp `scheduled_for` and `catch_up_run` on the
/// resulting [`TaskExecutionRecord`] at insert time, so reconciled rows are
/// distinguishable from regular runs in `scheduler_history`.
#[derive(Debug, Clone)]
pub struct CatchUpContext {
    /// Originally-scheduled slot this execution is "covering" — distinct
    /// from the actual `started_at` (now). Stored in UTC.
    pub scheduled_for: DateTime<Utc>,
}

impl CatchUpContext {
    fn apply(&self, record: &mut TaskExecutionRecord) {
        record.scheduled_for = Some(self.scheduled_for.to_rfc3339());
        record.catch_up_run = true;
    }
}

// ============================================================================
// Scheduler Service
// ============================================================================

/// Background scheduler service that executes tasks at their scheduled times
pub struct SchedulerService {
    /// PostgreSQL database for activity timeline and watchers (optional)
    pg_db: Option<Arc<PgDb>>,
    /// AppState for reading the actually-bound API port (optional for tests)
    app_state: Option<Arc<AppState>>,
    /// Flag to stop the service
    stop_signal: Arc<AtomicBool>,
    /// Currently running task IDs
    running_tasks: Arc<RwLock<Vec<String>>>,
    /// Check interval in seconds
    check_interval_secs: u64,
}

impl SchedulerService {
    /// Create a new scheduler service
    pub fn new(pg_db: impl Into<Option<Arc<PgDb>>>) -> Self {
        Self {
            pg_db: pg_db.into(),
            app_state: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            running_tasks: Arc::new(RwLock::new(Vec::new())),
            check_interval_secs: 60, // Check every minute
        }
    }

    /// Create a new scheduler service with AppState for port-aware URL construction.
    pub fn with_app_state(pg_db: impl Into<Option<Arc<PgDb>>>, app_state: Arc<AppState>) -> Self {
        Self {
            pg_db: pg_db.into(),
            app_state: Some(app_state),
            stop_signal: Arc::new(AtomicBool::new(false)),
            running_tasks: Arc::new(RwLock::new(Vec::new())),
            check_interval_secs: 60,
        }
    }

    /// Build the runner's own base URL, preferring AppState's bound port when
    /// available and falling back to the env-var lookup otherwise.
    fn self_base_url(&self) -> String {
        match &self.app_state {
            Some(state) => crate::mcp::types::get_self_base_url(state),
            None => crate::mcp::types::get_self_base_url_from_env(),
        }
    }

    /// Get a reference to the PgDb, or an error if not configured.
    fn pg(&self) -> Result<&PgDb, String> {
        self.pg_db
            .as_deref()
            .ok_or_else(|| "PostgreSQL database not configured for scheduler".to_string())
    }

    /// Start the scheduler loop (runs in background).
    ///
    /// Takes `Arc<Self>` rather than `&self` because the tick loop spawns
    /// background polling tasks for asynchronously-launched workflows, and
    /// those tasks need to outlive the tick frame they were created in.
    pub async fn start(self: Arc<Self>) {
        info!("Starting scheduler service");

        // Update all next_run times on startup
        if let Err(e) = self.update_all_next_runs_db().await {
            error!("Failed to update next run times: {}", e);
        }

        // Phase B: reconcile missed runs from while the runner was down.
        // Errors here must NOT block startup — log and continue so the
        // normal tick loop still fires.
        if let Err(e) = self.clone().reconcile_missed_runs().await {
            error!("Scheduler reconciler failed (non-fatal): {}", e);
        }

        while !self.stop_signal.load(Ordering::SeqCst) {
            // tick() checks enabled status internally to avoid double-loading state
            self.clone().tick().await;

            // Wait for next check interval
            tokio::time::sleep(tokio::time::Duration::from_secs(self.check_interval_secs)).await;
        }

        info!("Scheduler service stopped");
    }

    /// Stop the scheduler gracefully
    pub fn stop(&self) {
        info!("Stopping scheduler service");
        self.stop_signal.store(true, Ordering::SeqCst);
    }

    /// Update next_run for all tasks (DB-backed replacement for scheduler::update_all_next_runs)
    async fn update_all_next_runs_db(&self) -> Result<(), String> {
        let pg = self.pg()?;
        let tasks = pg.get_all_scheduled_tasks().await?;
        let now = chrono::Utc::now();

        for task in &tasks {
            let next = if task.enabled {
                compute_next_run(&task.schedule, now).map(|dt| dt.to_rfc3339())
            } else {
                None
            };
            pg.update_task_next_run(&task.id, next.as_deref()).await?;
        }

        Ok(())
    }

    /// Check and execute due tasks.
    ///
    /// Public so external triggers (Phase F.1 wake-from-web deep-link handler,
    /// Phase B missed-run reconciler) can fire an immediate cycle without
    /// waiting for the next `check_interval_secs` heartbeat.
    pub async fn tick(self: Arc<Self>) {
        let pg = match self.pg() {
            Ok(pg) => pg,
            Err(e) => {
                error!("Scheduler tick: {}", e);
                return;
            }
        };

        let settings = match pg.get_scheduler_settings().await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to load scheduler settings: {}", e);
                return;
            }
        };

        // Skip if scheduler is disabled
        if !settings.enabled {
            return;
        }

        let tasks = match pg.get_all_scheduled_tasks().await {
            Ok(t) => t,
            Err(e) => {
                error!("Failed to load scheduled tasks: {}", e);
                return;
            }
        };

        let now = chrono::Utc::now();

        // Find tasks that are:
        // 1. Due for execution (next_run <= now), OR
        // 2. Already waiting for conditions (have condition_status set)
        let mut due_tasks: Vec<ScheduledTask> = tasks
            .into_iter()
            .filter(|task| {
                // Must be enabled
                if !task.enabled {
                    return false;
                }

                // Include if already waiting for conditions
                if task.is_waiting_for_conditions() {
                    return true;
                }

                // Include if due for execution
                if let Some(ref next_run) = task.next_run {
                    if let Ok(next_dt) = chrono::DateTime::parse_from_rfc3339(next_run) {
                        return next_dt.with_timezone(&chrono::Utc) <= now;
                    }
                }

                false
            })
            .collect();

        // Sort by: waiting tasks first (by waiting_since), then by next_run time
        due_tasks.sort_by(|a, b| {
            // Waiting tasks come first
            match (&a.condition_status, &b.condition_status) {
                (Some(a_status), Some(b_status)) => {
                    a_status.waiting_since.cmp(&b_status.waiting_since)
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => match (&a.next_run, &b.next_run) {
                    (Some(a_time), Some(b_time)) => a_time.cmp(b_time),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
            }
        });

        // Check concurrent task limit
        let running = self.running_tasks.read().await;
        let running_count = running.len() as u32;
        drop(running);

        if running_count >= settings.max_concurrent {
            info!(
                "Scheduler: {} tasks running (max {}), waiting",
                running_count, settings.max_concurrent
            );
            return;
        }

        // Execute due tasks up to the concurrent limit
        let available_slots = (settings.max_concurrent - running_count) as usize;
        for mut task in due_tasks.into_iter().take(available_slots) {
            // Skip if already running
            {
                let running = self.running_tasks.read().await;
                if running.contains(&task.id) {
                    continue;
                }
            }

            // Check if should skip (completed and skip_if_completed)
            if task.should_skip() {
                info!(
                    "Scheduler: Skipping task '{}' (already completed)",
                    task.name
                );
                self.record_skipped(&task).await;
                continue;
            }

            // For Condition schedule tasks, check rearm delay.
            if matches!(
                task.schedule,
                crate::scheduler::ScheduleExpression::Condition(_)
            ) {
                if task.last_run.is_none() {
                    if let Ok(history) = pg.get_execution_history(&task.id, 1).await {
                        if let Some(latest) = history.into_iter().next() {
                            task.last_run = Some(latest);
                        }
                    }
                }
                if !task.is_rearm_ready() {
                    continue;
                }
            }

            // Check conditions if task has any
            if task.has_conditions() {
                let (conditions_met, status) = self.check_conditions(&task).await;

                if status.timed_out {
                    info!(
                        "Scheduler: Task '{}' timed out waiting for conditions",
                        task.name
                    );
                    self.record_condition_timeout(&task).await;
                    continue;
                }

                if !conditions_met {
                    info!(
                        "Scheduler: Task '{}' waiting for conditions (idle: {:?}, repos: {:?})",
                        task.name, status.idle_met, status.repo_inactive_met
                    );
                    let status_json = serde_json::to_string(&status).ok();
                    if let Err(e) = pg
                        .update_task_condition_status(&task.id, status_json.as_deref())
                        .await
                    {
                        error!("Failed to update condition status: {}", e);
                    }
                    continue;
                }

                // Conditions met - clear status before execution
                if let Err(e) = pg.update_task_condition_status(&task.id, None).await {
                    error!("Failed to clear condition status: {}", e);
                }
                info!("Scheduler: Task '{}' conditions met, executing", task.name);
            }

            info!("Scheduler: Executing task '{}'", task.name);
            self.clone().execute_task(task).await;
        }
    }

    /// Record a skipped execution
    async fn record_skipped(&self, task: &ScheduledTask) {
        let pg = match self.pg() {
            Ok(pg) => pg,
            Err(e) => {
                error!("record_skipped: {}", e);
                return;
            }
        };
        let mut record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        record.status = ScheduledTaskStatus::Skipped;
        record.ended_at = Some(chrono::Utc::now().to_rfc3339());

        if let Err(e) = pg.insert_execution_record(&task.id, &record).await {
            error!("Failed to record skipped execution: {}", e);
        }
        if let Err(e) = pg
            .update_task_last_run(&task.id, Some(&record.execution_id))
            .await
        {
            error!("Failed to update task last_run: {}", e);
        }

        // Update next_run
        self.update_task_next_run_db(&task.id).await;
    }

    /// Execute a scheduled task.
    ///
    /// Task types fall into two categories based on whether their launch
    /// HTTP call is blocking-until-complete or non-blocking-spawn-and-return:
    ///
    /// **Async (launch-and-poll)**: unified workflows, prompts, auto-fix.
    /// These spawn an AI session or background workflow that runs for
    /// minutes-to-hours, with the launch endpoint returning a `task_run_id`
    /// (or `session_id` — same thing for prompts) almost immediately. We
    /// insert a `Running` `scheduler_history` row right after launch and
    /// spawn a background poller that updates the row when the underlying
    /// `task_run` exits its `running` state.
    ///
    /// **Sync (launch-and-record)**: legacy config-path workflows, watchers,
    /// background-capture. These either run to completion in the launch call
    /// (legacy workflow via `action_service`) or are pure one-shot operations
    /// (watcher AI evaluation). The original synchronous record-after-launch
    /// flow applies.
    ///
    /// Splitting on this axis matters because tick() must not block on
    /// long-running tasks — async tasks must hand off to a spawned poller
    /// so the next tick can fire on schedule.
    async fn execute_task(self: Arc<Self>, task: ScheduledTask) {
        self.execute_task_with_context(task, None).await
    }

    /// Like [`execute_task`] but with an optional [`CatchUpContext`] that
    /// gets stamped onto the resulting [`TaskExecutionRecord`] at insert
    /// time. Used by the Phase B missed-run reconciler.
    async fn execute_task_with_context(
        self: Arc<Self>,
        task: ScheduledTask,
        catch_up: Option<CatchUpContext>,
    ) {
        let task_id = task.id.clone();
        let task_name = task.name.clone();
        let auto_fix_on_failure = task.auto_fix_on_failure;

        // Mark as running (stays marked until the spawned poller completes
        // for async workflows; until the sync execute_* returns otherwise).
        {
            let mut running = self.running_tasks.write().await;
            running.push(task_id.clone());
        }

        // === Async launch-and-poll paths ===
        match &task.task {
            ScheduledTaskType::Workflow {
                workflow_id: Some(workflow_id),
                monitor_index,
                ..
            } => {
                let workflow_id = workflow_id.clone();
                let monitor_index = *monitor_index;
                self.clone()
                    .launch_and_poll(
                        task_id,
                        task_name,
                        auto_fix_on_failure,
                        catch_up,
                        Box::pin(async move {
                            self.launch_unified_workflow(&workflow_id, monitor_index)
                                .await
                        }),
                    )
                    .await;
                return;
            }
            ScheduledTaskType::Prompt {
                prompt_id,
                max_sessions,
            } => {
                let prompt_id = prompt_id.clone();
                let max_sessions = *max_sessions;
                self.clone()
                    .launch_and_poll(
                        task_id,
                        task_name,
                        auto_fix_on_failure,
                        catch_up,
                        Box::pin(async move { self.launch_prompt(&prompt_id, max_sessions).await }),
                    )
                    .await;
                return;
            }
            ScheduledTaskType::AutoFix {
                check_findings,
                force_run,
            } => {
                let check_findings = *check_findings;
                let force_run = *force_run;
                self.clone()
                    .launch_and_poll(
                        task_id,
                        task_name,
                        auto_fix_on_failure,
                        catch_up,
                        Box::pin(
                            async move { self.launch_auto_fix(check_findings, force_run).await },
                        ),
                    )
                    .await;
                return;
            }
            ScheduledTaskType::RemoteAgent {
                prompt,
                working_directory,
                allowed_tools,
                model,
                mcp_connections,
                max_turns,
                timeout_seconds,
            } => {
                // Clone every field out of the variant so the future can be
                // 'static — same shape `launch_prompt`/`launch_auto_fix` use.
                let prompt = prompt.clone();
                let working_directory = working_directory.clone();
                let allowed_tools = allowed_tools.clone();
                let model = model.clone();
                let mcp_connections = mcp_connections.clone();
                let max_turns = *max_turns;
                let timeout_seconds = *timeout_seconds;
                let task_name_for_launch = task_name.clone();
                self.clone()
                    .launch_and_poll(
                        task_id,
                        task_name,
                        auto_fix_on_failure,
                        catch_up,
                        Box::pin(async move {
                            self.launch_remote_agent(
                                &task_name_for_launch,
                                &prompt,
                                working_directory.as_deref(),
                                model.as_deref(),
                                &allowed_tools,
                                &mcp_connections,
                                max_turns,
                                timeout_seconds,
                            )
                            .await
                        }),
                    )
                    .await;
                return;
            }
            _ => {}
        }

        // === Sync (launch-blocks-until-complete) paths ===
        let mut record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        if let Some(ref ctx) = catch_up {
            ctx.apply(&mut record);
        }

        let result = match &task.task {
            ScheduledTaskType::Workflow {
                workflow_name,
                config_path,
                monitor_index,
                workflow_id: _, // handled above
            } => {
                self.execute_workflow(workflow_name, config_path.as_deref(), *monitor_index)
                    .await
            }
            ScheduledTaskType::Watcher { watcher_id } => self.execute_watcher(watcher_id).await,
            ScheduledTaskType::BackgroundCapture {
                monitor_index: _,
                capture_interval_secs,
                capture_on_focus_change: _,
            } => {
                // BackgroundCapture is a long-running service, not a one-shot task.
                // The scheduler creates and starts it; it runs until stopped.
                warn!(
                    "Scheduler: BackgroundCapture is a long-running service (interval={}s). \
                     Use the BackgroundCaptureService API to start/stop.",
                    capture_interval_secs
                );
                Ok((true, None))
            }
            // Async task types are handled by the launch_and_poll branch above
            // and return early; the compiler still needs them in this match.
            ScheduledTaskType::Prompt { .. }
            | ScheduledTaskType::AutoFix { .. }
            | ScheduledTaskType::RemoteAgent { .. } => unreachable!(
                "Async task types must be handled by the launch_and_poll dispatch above"
            ),
        };

        self.finalize_sync_execution(
            &task_id,
            &task_name,
            auto_fix_on_failure,
            &mut record,
            result,
        )
        .await;
    }

    /// Shared post-processing for synchronous task-type executions.
    ///
    /// Phase C semantics:
    /// - `Err(_)` is treated as a **launch failure** — the synchronous
    ///   launchers (`execute_workflow`, `execute_watcher`) only return
    ///   `Err(_)` when the start path itself fails (HTTP error, config
    ///   load failure, watcher-not-found, etc.), all of which happen
    ///   before any runtime work. We mark the record `LaunchFailed`,
    ///   bump `consecutive_launch_failures`, and push `next_run` out by
    ///   the exponential backoff (capped at 24h).
    /// - `Ok((false, _))` is a runtime failure (the task started but
    ///   reported failure) — recorded as `Failed`, schedule untouched,
    ///   `auto_fix_on_failure` fires as before.
    /// - `Ok((_, _))` resets the launch-failure counter (a task that
    ///   *did* launch cleanly clears the streak even if the runtime
    ///   reports failure — the start path worked, the streak is
    ///   specifically "couldn't even start").
    async fn finalize_sync_execution(
        &self,
        task_id: &str,
        task_name: &str,
        auto_fix_on_failure: bool,
        record: &mut TaskExecutionRecord,
        result: Result<(bool, Option<String>), String>,
    ) {
        let mut launch_failed = false;
        match result {
            Ok((success, session_id)) => {
                record.session_id = session_id;
                record.complete(success, None);
                info!(
                    "Scheduler: Task '{}' completed (success: {})",
                    task_name, success
                );

                // The launch path returned Ok — clear any prior backoff
                // streak. We do this whether or not the runtime
                // succeeded; backoff is for "couldn't even start," not
                // "started but failed."
                self.clear_launch_failure_counter(task_id).await;

                if !success && auto_fix_on_failure {
                    info!(
                        "Scheduler: Triggering auto-fix for failed task '{}'",
                        task_name
                    );
                    if let Ok(session_id) = self.launch_auto_fix(true, false).await {
                        record.mark_auto_fix_triggered(session_id);
                    }
                }
            }
            Err(e) => {
                // Sync launchers only produce Err on pre-runtime failures
                // (HTTP errors, load_config failure, missing watcher,
                // etc.). Treat as LaunchFailed and trigger backoff.
                record.mark_launch_failed(Some(e.clone()));
                launch_failed = true;
                error!(
                    "Scheduler: Task '{}' failed to launch: {}",
                    task_name, e
                );

                if auto_fix_on_failure {
                    info!(
                        "Scheduler: Triggering auto-fix for failed task '{}'",
                        task_name
                    );
                    if let Ok(session_id) = self.launch_auto_fix(true, false).await {
                        record.mark_auto_fix_triggered(session_id);
                    }
                }
            }
        }

        if let Ok(pg) = self.pg() {
            if let Err(e) = pg.insert_execution_record(task_id, record).await {
                error!("Failed to record execution: {}", e);
            }
            if let Err(e) = pg
                .update_task_last_run(task_id, Some(&record.execution_id))
                .await
            {
                error!("Failed to update task last_run: {}", e);
            }
        } else {
            error!("PG not configured, cannot record execution for {}", task_id);
        }

        if launch_failed {
            self.apply_launch_failure_backoff(task_id, task_name).await;
        } else {
            self.update_task_next_run_db(task_id).await;
        }

        {
            let mut running = self.running_tasks.write().await;
            running.retain(|id| id != task_id);
        }
    }

    /// Compute and persist the launch-failure backoff for a task.
    ///
    /// Loads the latest task row from PG, increments the in-memory
    /// `consecutive_launch_failures`, computes the backoff via
    /// [`ScheduledTaskExt::launch_failure_backoff`], and pushes
    /// `next_run` to `max(normal_next_run, now + backoff)`. The "max"
    /// guarantees a small backoff (e.g. 60s) doesn't pull a cron's
    /// natural fire time *earlier* than it would otherwise be.
    ///
    /// The counter is persisted via [`PgDb::update_task_launch_failure_counter`]
    /// and `next_run` via [`PgDb::update_task_next_run`] — separate writes
    /// so a transient PG error on one doesn't roll back the other.
    async fn apply_launch_failure_backoff(&self, task_id: &str, task_name: &str) {
        let pg = match self.pg() {
            Ok(pg) => pg,
            Err(e) => {
                error!(
                    "apply_launch_failure_backoff: PG unavailable for {}: {}",
                    task_id, e
                );
                return;
            }
        };

        let mut task = match pg.get_scheduled_task(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                error!(
                    "apply_launch_failure_backoff: task {} not found",
                    task_id
                );
                return;
            }
            Err(e) => {
                error!(
                    "apply_launch_failure_backoff: failed to load task {}: {}",
                    task_id, e
                );
                return;
            }
        };

        task.record_launch_failure();
        let failures = task.consecutive_launch_failures;
        let backoff = task.launch_failure_backoff();

        let now = Utc::now();
        let normal_next = compute_next_run(&task.schedule, now);
        let next_run = compute_launch_failed_next_run(normal_next, backoff, now);
        let backoff_seconds = backoff.map(|d| d.num_seconds()).unwrap_or(0);
        warn!(
            task_id = %task_id,
            task_name = %task_name,
            failures = failures,
            backoff_seconds = backoff_seconds,
            next_run = ?next_run,
            "scheduler launch failed; backing off"
        );

        if let Err(e) = pg
            .update_task_launch_failure_counter(task_id, failures)
            .await
        {
            error!(
                "Failed to persist launch_failure counter for {}: {}",
                task_id, e
            );
        }

        let next_run_str = next_run.map(|dt| dt.to_rfc3339());
        if let Err(e) = pg
            .update_task_next_run(task_id, next_run_str.as_deref())
            .await
        {
            error!("Failed to update next_run after launch failure: {}", e);
        }
    }

    /// Reset the launch-failure counter for a task (no-op if already 0).
    /// Called the first time a task's launch path returns `Ok(...)` — i.e.
    /// the underlying session/run started cleanly. Runtime failures during
    /// execution are not counted as launch failures, so this also runs on
    /// `Ok((false, _))` (a started-but-failed sync task).
    async fn clear_launch_failure_counter(&self, task_id: &str) {
        let pg = match self.pg() {
            Ok(pg) => pg,
            Err(_) => return,
        };

        // Skip the write if the counter is already zero — avoids needless
        // PG round-trips on every successful launch.
        match pg.get_scheduled_task(task_id).await {
            Ok(Some(t)) => {
                if t.consecutive_launch_failures == 0 {
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                warn!(
                    "clear_launch_failure_counter: failed to load task {}: {}",
                    task_id, e
                );
                return;
            }
        }

        info!(
            task_id = %task_id,
            "scheduler launch succeeded; resetting failure counter"
        );

        if let Err(e) = pg.update_task_launch_failure_counter(task_id, 0).await {
            warn!(
                "Failed to reset launch_failure counter for {}: {}",
                task_id, e
            );
        }
    }

    /// Generic launch-and-poll for async task types.
    ///
    /// Drives the lifecycle:
    /// 1. Run the provided launch future, which kicks off the underlying
    ///    work (via HTTP) and returns a `task_run_id`.
    /// 2. INSERT a `Running` `scheduler_history` row immediately so the UI
    ///    reflects the in-flight execution.
    /// 3. Spawn a detached poller that watches `task_runs.status` and
    ///    UPDATEs the row when the run leaves `running`. The poller also
    ///    fires `auto_fix_on_failure` if configured and clears the entry
    ///    from `running_tasks` so concurrent-task quotas stay accurate.
    ///
    /// If the launch itself fails, the lifecycle short-circuits to a failed
    /// row (no poller is spawned).
    async fn launch_and_poll(
        self: Arc<Self>,
        task_id: String,
        task_name: String,
        auto_fix_on_failure: bool,
        catch_up: Option<CatchUpContext>,
        launch_fut: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
        >,
    ) {
        let mut record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        if let Some(ref ctx) = catch_up {
            ctx.apply(&mut record);
        }
        let execution_id = record.execution_id.clone();

        let launch = launch_fut.await;

        match launch {
            Ok(task_run_id) => {
                record.session_id = Some(task_run_id.clone());
                info!(
                    "Scheduler: launched task '{}' (task_run_id={})",
                    task_name, task_run_id
                );

                if let Ok(pg) = self.pg() {
                    if let Err(e) = pg.insert_execution_record(&task_id, &record).await {
                        error!("Failed to insert initial running execution record: {}", e);
                    }
                    if let Err(e) = pg
                        .update_task_last_run(&task_id, Some(&record.execution_id))
                        .await
                    {
                        error!("Failed to update task last_run: {}", e);
                    }
                }

                // The launch HTTP call returned Ok with a session/run id —
                // the task is now actually running. Clear any prior
                // launch-failure streak before kicking off the poller.
                // Phase C: this is "first successful start" per the plan.
                self.clear_launch_failure_counter(&task_id).await;

                let service = self.clone();
                tokio::spawn(async move {
                    service
                        .poll_task_run_to_completion(
                            task_id,
                            task_name,
                            execution_id,
                            task_run_id,
                            record,
                            auto_fix_on_failure,
                        )
                        .await;
                });
            }
            Err(e) => {
                // The launch path itself failed (HTTP error, missing
                // session_id in response, prompt-resolve failure, etc.) —
                // this is a LaunchFailed, not a runtime Failed. Mark
                // accordingly so the backoff path fires.
                record.mark_launch_failed(Some(e.clone()));
                error!("Scheduler: task '{}' failed to launch: {}", task_name, e);

                if auto_fix_on_failure {
                    if let Ok(session_id) = self.launch_auto_fix(true, false).await {
                        record.mark_auto_fix_triggered(session_id);
                    }
                }

                if let Ok(pg) = self.pg() {
                    if let Err(err) = pg.insert_execution_record(&task_id, &record).await {
                        error!("Failed to record launch-failure execution: {}", err);
                    }
                    if let Err(err) = pg
                        .update_task_last_run(&task_id, Some(&record.execution_id))
                        .await
                    {
                        error!("Failed to update task last_run: {}", err);
                    }
                }

                self.apply_launch_failure_backoff(&task_id, &task_name).await;

                let mut running = self.running_tasks.write().await;
                running.retain(|id| id != &task_id);
            }
        }
    }

    /// Poll `task_runs` until the given workflow leaves the `running` state,
    /// then UPDATE the scheduler_history row with the real outcome. Runs in
    /// a detached tokio task so the scheduler tick loop is not blocked.
    async fn poll_task_run_to_completion(
        self: Arc<Self>,
        task_id: String,
        task_name: String,
        execution_id: String,
        task_run_id: String,
        mut record: TaskExecutionRecord,
        auto_fix_on_failure: bool,
    ) {
        // Poll every 30s. Hard-cap matches MAX_RUNTIME_AUTO_STOP in the
        // zombie sweep — after 24h we stop polling and let the sweep
        // eventually fail the task_run itself.
        const POLL_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(30);
        const MAX_POLL_DURATION: tokio::time::Duration =
            tokio::time::Duration::from_secs(60 * 60 * 24);

        let start = tokio::time::Instant::now();

        let (success, error_message) = loop {
            if self.stop_signal.load(Ordering::SeqCst) {
                break (
                    false,
                    Some("Scheduler stopped while workflow was in-flight".to_string()),
                );
            }

            if start.elapsed() >= MAX_POLL_DURATION {
                warn!(
                    "Scheduler: task_run {} still running after 24h — giving up on polling",
                    task_run_id
                );
                break (
                    false,
                    Some("Polling timed out after 24h; see zombie sweep".to_string()),
                );
            }

            tokio::time::sleep(POLL_INTERVAL).await;

            let pg = match self.pg() {
                Ok(pg) => pg,
                Err(e) => {
                    error!("Scheduler poller: {}", e);
                    break (false, Some(e));
                }
            };

            match pg.get_task_run(&task_run_id).await {
                Ok(Some(tr)) => match tr.status.as_str() {
                    "running" | "pending" => continue,
                    "complete" | "completed" => {
                        break (true, None);
                    }
                    other => {
                        let err = if tr.error_message.is_some() {
                            tr.error_message
                        } else {
                            Some(format!("task_run ended in status '{}'", other))
                        };
                        break (false, err);
                    }
                },
                Ok(None) => {
                    warn!(
                        "Scheduler poller: task_run {} disappeared from DB",
                        task_run_id
                    );
                    break (
                        false,
                        Some(format!(
                            "task_run {} not found — may have been deleted",
                            task_run_id
                        )),
                    );
                }
                Err(e) => {
                    warn!(
                        "Scheduler poller: transient error reading task_run {}: {}",
                        task_run_id, e
                    );
                    // Don't terminate on transient DB errors — just retry.
                    continue;
                }
            }
        };

        record.complete(success, error_message.clone());
        info!(
            "Scheduler: task '{}' finished (success: {}, took: {:?})",
            task_name,
            success,
            start.elapsed()
        );

        if !success && auto_fix_on_failure {
            info!(
                "Scheduler: Triggering auto-fix for failed task '{}'",
                task_name
            );
            // Fire-and-forget: we just record that auto-fix was triggered.
            // The auto-fix run itself will be tracked as its own task_run if
            // it's a primary scheduled AutoFix task; here it's a side effect
            // so we don't poll for its completion.
            if let Ok(session_id) = self.launch_auto_fix(true, false).await {
                record.mark_auto_fix_triggered(session_id);
            }
        }

        if let Ok(pg) = self.pg() {
            if let Err(e) = pg.update_execution_record(&execution_id, &record).await {
                error!("Failed to update execution record on completion: {}", e);
            }
        }

        self.update_task_next_run_db(&task_id).await;

        let mut running = self.running_tasks.write().await;
        running.retain(|id| id != &task_id);
    }

    /// Update the next_run time for a task (DB-backed)
    async fn update_task_next_run_db(&self, task_id: &str) {
        let pg = match self.pg() {
            Ok(pg) => pg,
            Err(e) => {
                error!("update_task_next_run_db: {}", e);
                return;
            }
        };
        let task = match pg.get_scheduled_task(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                error!("Task not found for next_run update: {}", task_id);
                return;
            }
            Err(e) => {
                error!("Failed to load task for next_run update: {}", e);
                return;
            }
        };

        let now = chrono::Utc::now();
        let next = compute_next_run(&task.schedule, now).map(|dt| dt.to_rfc3339());

        if let Err(e) = pg.update_task_next_run(task_id, next.as_deref()).await {
            error!("Failed to update task next_run: {}", e);
        }
    }

    /// Execute a workflow task
    async fn execute_workflow(
        &self,
        workflow_name: &str,
        config_path: Option<&str>,
        monitor_index: Option<i32>,
    ) -> Result<(bool, Option<String>), String> {
        info!(
            "Executing workflow '{}' (config: {:?}, monitor: {:?})",
            workflow_name, config_path, monitor_index
        );

        // Build the request to run workflow via HTTP API
        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        // Load config if specified
        if let Some(path) = config_path {
            let load_response = client
                .post(format!("{}/load-config", base_url))
                .json(&serde_json::json!({
                    "path": path
                }))
                .send()
                .await
                .map_err(|e| format!("Failed to load config: {}", e))?;

            if !load_response.status().is_success() {
                let error_text = load_response.text().await.unwrap_or_default();
                return Err(format!("Failed to load config: {}", error_text));
            }
        }

        // Run the workflow
        let mut request_body = serde_json::json!({
            "workflow_name": workflow_name
        });

        if let Some(monitor) = monitor_index {
            request_body["monitor_index"] = serde_json::json!(monitor);
        }

        let run_response = client
            .post(format!("{}/run-workflow", base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Failed to run workflow: {}", e))?;

        if !run_response.status().is_success() {
            let error_text = run_response.text().await.unwrap_or_default();
            return Err(format!("Failed to run workflow: {}", error_text));
        }

        // Parse response to get session ID
        let response_json: serde_json::Value = run_response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let session_id = response_json
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Wait for completion and check success
        // For now, we assume the workflow API handles completion detection
        // The success is determined by the checkpoint file
        let success = response_json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok((success, session_id))
    }

    /// Launch a unified workflow via HTTP and return its `task_run_id`.
    ///
    /// The endpoint is non-blocking: it spawns the workflow in the
    /// background and returns immediately with a `task_run_id`. Callers that
    /// need the final outcome must poll the task_run's status afterwards
    /// (see `poll_task_run_to_completion`).
    async fn launch_unified_workflow(
        &self,
        workflow_id: &str,
        monitor_index: Option<i32>,
    ) -> Result<String, String> {
        info!(
            "Launching unified workflow '{}' (monitor: {:?})",
            workflow_id, monitor_index
        );

        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        let mut request_body = serde_json::json!({});
        if let Some(monitor) = monitor_index {
            request_body["monitor_index"] = serde_json::json!(monitor);
        }

        let run_response = client
            .post(format!(
                "{}/unified-workflows/{}/run",
                base_url, workflow_id
            ))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Failed to run unified workflow: {}", e))?;

        if !run_response.status().is_success() {
            let error_text = run_response.text().await.unwrap_or_default();
            return Err(format!("Failed to run unified workflow: {}", error_text));
        }

        let response_json: serde_json::Value = run_response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let launched = response_json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !launched {
            return Err(format!(
                "Launch endpoint reported failure: {}",
                response_json
            ));
        }

        response_json
            .get("data")
            .and_then(|d| d.get("task_run_id"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "Launch endpoint reported success but omitted task_run_id".to_string())
    }

    /// Launch a prompt and return the resulting `session_id` (which is also
    /// the `task_run_id` — see `mcp/ai_session.rs`'s `run_prompt` handler).
    /// The actual prompt execution runs in the background; callers must poll
    /// `task_runs.status` to learn the real outcome.
    async fn launch_prompt(
        &self,
        prompt_id: &str,
        max_sessions: Option<u32>,
    ) -> Result<String, String> {
        info!(
            "Launching prompt '{}' (max_sessions: {:?})",
            prompt_id, max_sessions
        );

        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        let mut request_body = serde_json::json!({
            "prompt_id": prompt_id
        });

        if let Some(max_sess) = max_sessions {
            request_body["max_sessions"] = serde_json::json!(max_sess);
        }

        let response = client
            .post(format!("{}/prompts/{}/run", base_url, prompt_id))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Failed to run prompt: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to run prompt: {}", error_text));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        response_json
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "Prompt run endpoint omitted session_id".to_string())
    }

    /// Launch a remote agent via the ad-hoc prompts endpoint and return the
    /// `session_id`. Same non-blocking semantics as `launch_prompt`.
    async fn launch_remote_agent(&self, prompt: &str) -> Result<String, String> {
        info!(
            "Launching remote agent with prompt ({} chars)",
            prompt.len()
        );

        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        let response = client
            .post(format!("{}/prompts/run", base_url))
            .json(&serde_json::json!({
                "name": "Scheduled Remote Agent",
                "content": prompt
            }))
            .send()
            .await
            .map_err(|e| format!("Failed to launch remote agent: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to launch remote agent: {}", error_text));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        response_json
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "Remote agent run endpoint omitted session_id".to_string())
    }

    /// Launch an auto-fix run via the ad-hoc prompts endpoint and return the
    /// `session_id`. Same non-blocking semantics as `launch_prompt`.
    async fn launch_auto_fix(
        &self,
        check_findings: bool,
        _force_run: bool,
    ) -> Result<String, String> {
        info!("Launching auto-fix (check_findings: {})", check_findings);

        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        let prompt = if check_findings {
            r#"You are in auto-fix mode. Check for any auto-fixable findings (code_bug, security, test_issue, documentation) and fix them.

Instructions:
1. Review the current findings in the Issues/All Findings pages
2. For each auto-fixable finding, make the necessary code fixes
3. Output findings with [FINDING:category:severity] markers
4. Include Resolution: field for each fixed finding

Auto-fixable categories:
- code_bug: Fix actual code bugs
- security: Fix security vulnerabilities
- test_issue: Fix test code problems
- documentation: Fix documentation issues

After making fixes, run tests if applicable to verify the fixes work."#
                .to_string()
        } else {
            "Run auto-fix on any detected issues.".to_string()
        };

        let request_body = serde_json::json!({
            "name": "scheduled-auto-fix",
            "content": prompt,
            "display_prompt": "Scheduler: Auto-Fix",
            "timeout_seconds": 600,
            "max_sessions": 1
        });

        let response = client
            .post(format!("{}/prompts/run", base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Failed to trigger auto-fix: {}", e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!("Failed to trigger auto-fix: {}", error_text));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        response_json
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| "Auto-fix run endpoint omitted session_id".to_string())
    }

    /// Launch a `RemoteAgent` scheduled task via the runner's existing
    /// ad-hoc `POST /prompts/run` surface.
    ///
    /// `RemoteAgent` is "an arbitrary Claude prompt as a scheduled task" —
    /// no separate Claude CLI plumbing is needed. We forward the user's
    /// prompt + tuning knobs to `/prompts/run`'s ad-hoc mode (mode 2:
    /// `name + content`). The endpoint returns a `session_id` that doubles
    /// as a `task_run_id`; the same `launch_and_poll` machinery used by
    /// `Prompt`/`AutoFix` then takes over for completion polling.
    ///
    /// The new `RunPromptRequest` knobs are mapped as follows:
    /// - `working_directory` → `--working-directory` flag on the spawn
    ///   wrapper, becomes the spawned Claude CLI's CWD
    /// - `model` → `--model` (Claude CLI native flag)
    /// - `allowed_tools` → `--allowed-tools` (Claude CLI native flag)
    /// - `max_turns` → `--max-turns` (Claude CLI native flag)
    /// - `mcp_connections` → injected as a header section in the prompt
    ///   (per-call MCP-config merging is not yet wired; tracked as Phase D
    ///   follow-up in `tmp_scheduler_reliability_plan.md`)
    ///
    /// Defaults follow the plan: `max_turns = 50`, `timeout_seconds = 600`.
    /// Failures are returned as `Result::Err` so the existing
    /// `launch_and_poll` failure path can convert them into a `Failed`
    /// `scheduler_history` row (Phase C will route these to `LaunchFailed`).
    #[allow(clippy::too_many_arguments)]
    async fn launch_remote_agent(
        &self,
        task_name: &str,
        prompt: &str,
        working_directory: Option<&str>,
        model: Option<&str>,
        allowed_tools: &[String],
        mcp_connections: &[qontinui_types::scheduler::McpConnectionRef],
        max_turns: Option<u32>,
        timeout_seconds: Option<u64>,
    ) -> Result<String, String> {
        info!(
            "Launching RemoteAgent task '{}' (model: {:?}, max_turns: {:?}, timeout: {:?}s)",
            task_name, model, max_turns, timeout_seconds
        );

        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        // Plan defaults (see tmp_scheduler_reliability_plan.md, Phase D §5).
        let effective_max_turns = max_turns.unwrap_or(50);
        let effective_timeout = timeout_seconds.unwrap_or(600);

        let mut request_body = serde_json::json!({
            "name": format!("scheduled-remote-agent-{}", task_name),
            "content": prompt,
            "display_prompt": format!("Scheduler: RemoteAgent ({})", task_name),
            "timeout_seconds": effective_timeout,
            "max_sessions": 1,
            "max_turns": effective_max_turns,
        });

        if let Some(wd) = working_directory {
            request_body["working_directory"] = serde_json::json!(wd);
        }
        if let Some(m) = model {
            request_body["model"] = serde_json::json!(m);
        }
        if !allowed_tools.is_empty() {
            request_body["allowed_tools"] = serde_json::json!(allowed_tools);
        }
        if !mcp_connections.is_empty() {
            request_body["mcp_connections"] = serde_json::json!(mcp_connections);
        }

        let response = client
            .post(format!("{}/prompts/run", base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Failed to launch RemoteAgent: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!(
                "RemoteAgent /prompts/run returned HTTP {}: {}",
                status, error_text
            ));
        }

        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse RemoteAgent response: {}", e))?;

        // The /prompts/run handler wraps the body in
        // `{ "success": bool, "data": { ... } }` (see ApiResponse). We accept
        // either a top-level `session_id` or `data.session_id` so the
        // contract is the same as `launch_auto_fix`'s `Value::get` path.
        let session_id_opt = response_json
            .get("session_id")
            .and_then(|v| v.as_str())
            .or_else(|| {
                response_json
                    .get("data")
                    .and_then(|d| d.get("session_id"))
                    .and_then(|v| v.as_str())
            })
            .map(String::from);

        match session_id_opt {
            Some(sid) => {
                info!(
                    "RemoteAgent task '{}' launched with session_id={}",
                    task_name, sid
                );

                // 30s no-activity guard (Phase D §5 + plan §4 hand-off note).
                // Fire-and-forget: only logs a warning if no `task_runs` row
                // appears for `sid` within 30s. The existing
                // `poll_task_run_to_completion` poller will then surface
                // the missing row as a failure via its
                // `task_run {} disappeared from DB` branch.
                if let Some(pg_db) = &self.pg_db {
                    let pg = pg_db.clone();
                    let sid_for_check = sid.clone();
                    let task_name_for_check = task_name.to_string();
                    tokio::spawn(async move {
                        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                        match pg.get_task_run(&sid_for_check).await {
                            Ok(Some(_)) => {
                                // Activity recorded — nothing to do.
                            }
                            Ok(None) => {
                                warn!(
                                    "RemoteAgent task '{}' (session_id={}) shows no task_runs activity within 30s of launch — possible silent spawn hang",
                                    task_name_for_check, sid_for_check
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "RemoteAgent task '{}' (session_id={}) 30s activity check failed to query task_runs: {}",
                                    task_name_for_check, sid_for_check, e
                                );
                            }
                        }
                    });
                }

                Ok(sid)
            }
            None => Err(format!(
                "RemoteAgent /prompts/run omitted session_id; response={}",
                response_json
            )),
        }
    }

    /// Check if a specific task is currently running
    pub async fn is_task_running(&self, task_id: &str) -> bool {
        let running = self.running_tasks.read().await;
        running.contains(&task_id.to_string())
    }

    /// Get list of currently running task IDs
    pub async fn get_running_tasks(&self) -> Vec<String> {
        let running = self.running_tasks.read().await;
        running.clone()
    }

    // ========================================================================
    // Watcher Execution
    // ========================================================================

    /// Execute a watcher: query the activity timeline, format results into an AI prompt,
    /// and trigger the configured action if the AI determines the condition is met.
    async fn execute_watcher(&self, watcher_id: &str) -> Result<(bool, Option<String>), String> {
        let pg = self.pg_db.as_ref().ok_or_else(|| {
            "PostgreSQL database not configured; cannot execute watcher".to_string()
        })?;

        // 1. Load watcher definition
        let watcher = pg
            .get_watcher(watcher_id)
            .await?
            .ok_or_else(|| format!("Watcher '{}' not found", watcher_id))?;

        if !watcher.enabled {
            return Ok((true, None)); // Skip disabled watchers
        }

        info!(
            "Executing watcher '{}': query='{}', lookback='{}'",
            watcher.name, watcher.timeline_query, watcher.lookback_window
        );

        // 2. Query the activity timeline with lookback window
        // Parse lookback_window (e.g., "15 minutes", "1 hour", "30 seconds")
        // and filter results by creation time. The FTS query runs first,
        // then we post-filter by time since Clorinde doesn't have a combined
        // FTS+time-range query yet.
        let lookback_cutoff = parse_lookback_window(&watcher.lookback_window);

        let all_results = pg
            .search_timeline_filtered(
                &watcher.timeline_query,
                watcher.app_name_filter.as_deref(),
                watcher.source_type_filter.as_deref(),
                None, // no task_run_id filter
                200,  // fetch more, then filter by time
            )
            .await?;

        // Filter by lookback window
        let results: Vec<_> = if let Some(cutoff) = lookback_cutoff {
            let cutoff_str = cutoff.to_rfc3339();
            all_results
                .into_iter()
                .filter(|r| r.created_at >= cutoff_str)
                .take(50)
                .collect()
        } else {
            all_results.into_iter().take(50).collect()
        };

        // 3. Format the AI reasoning prompt
        let results_text = if results.is_empty() {
            "No matching entries found in the activity timeline.".to_string()
        } else {
            results
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "{}. [{}] {} — {} ({})",
                        i + 1,
                        r.source_type,
                        r.app_name.as_deref().unwrap_or("unknown"),
                        r.text_preview,
                        r.created_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let prompt = watcher
            .reasoning_prompt
            .replace("{{results}}", &results_text)
            .replace("{{result_count}}", &results.len().to_string())
            .replace("{{query}}", &watcher.timeline_query);

        // 4. Send to AI for reasoning via the runner's prompt execution API
        let client = reqwest::Client::new();
        let base_url = self.self_base_url();

        let request_body = serde_json::json!({
            "name": format!("watcher-{}", watcher.name),
            "content": prompt,
            "display_prompt": format!("Watcher: {}", watcher.name),
            "timeout_seconds": 120,
            "max_sessions": 1
        });

        let response = client
            .post(format!("{}/prompts/run", base_url))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Watcher AI request failed: {}", e))?;

        let success = response.status().is_success();
        let result_text = response.text().await.unwrap_or_default();

        // 5. Record execution result
        pg.record_watcher_run(watcher_id, Some(&result_text))
            .await
            .unwrap_or_else(|e| warn!("Failed to record watcher run: {}", e));

        info!(
            "Watcher '{}' completed (success={}, results={})",
            watcher.name,
            success,
            results.len()
        );

        Ok((success, None))
    }

    // ========================================================================
    // Condition Checking
    // ========================================================================
}

/// Combine the schedule's natural next-run time with a launch-failure
/// backoff, returning whichever is *later* (Phase C).
///
/// Pure function so the launch-failure rescheduling logic can be tested
/// without a database. The math:
/// - If the schedule has no future slot (e.g., a one-shot `Once` already
///   in the past) and there is a backoff, schedule purely from `now +
///   backoff`.
/// - If the schedule has a future slot but no backoff (i.e. the failure
///   counter is zero — should not happen on the LaunchFailed path, but
///   defended for clarity), keep the natural slot.
/// - If both are present, return `max(normal_next, now + backoff)` so a
///   small backoff (e.g. 60s) doesn't pull a cron's natural fire time
///   *earlier* than it would otherwise be.
fn compute_launch_failed_next_run(
    normal_next: Option<DateTime<Utc>>,
    backoff: Option<chrono::Duration>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let backoff_next = backoff.map(|d| now + d);
    match (normal_next, backoff_next) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Parse a human-friendly lookback window string (e.g., "15 minutes", "1 hour", "30 seconds")
/// into an absolute cutoff DateTime. Returns None if the string cannot be parsed.
fn parse_lookback_window(window: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let parts: Vec<&str> = window.split_whitespace().collect();
    if parts.len() != 2 {
        warn!(
            "Cannot parse lookback_window '{}': expected '<number> <unit>'",
            window
        );
        return None;
    }

    let amount: i64 = match parts[0].parse() {
        Ok(n) => n,
        Err(_) => {
            warn!("Cannot parse lookback_window number: '{}'", parts[0]);
            return None;
        }
    };

    let duration = match parts[1].trim_end_matches('s') {
        "second" => chrono::Duration::seconds(amount),
        "minute" => chrono::Duration::minutes(amount),
        "hour" => chrono::Duration::hours(amount),
        "day" => chrono::Duration::days(amount),
        _ => {
            warn!("Unknown lookback_window unit: '{}'", parts[1]);
            return None;
        }
    };

    Some(chrono::Utc::now() - duration)
}

impl SchedulerService {
    /// Check if a task's conditions are met
    /// Returns (all_conditions_met, updated_status)
    async fn check_conditions(&self, task: &ScheduledTask) -> (bool, ConditionStatus) {
        let conditions = match &task.conditions {
            Some(c) => c,
            None => return (true, condition_status_default()),
        };

        // Check if any conditions are actually enabled
        if !task.has_conditions() {
            return (true, condition_status_default());
        }

        // Use existing status or create new one
        let mut status = task
            .condition_status
            .clone()
            .unwrap_or_else(|| ConditionStatus {
                waiting_since: chrono::Utc::now().to_rfc3339(),
                idle_met: None,
                repo_inactive_met: None,
                timed_out: false,
            });

        // Check timeout first
        if let Some(timeout_mins) = conditions.timeout_minutes {
            if let Ok(waiting_since) = chrono::DateTime::parse_from_rfc3339(&status.waiting_since) {
                let elapsed = chrono::Utc::now() - waiting_since.with_timezone(&chrono::Utc);
                if elapsed > chrono::Duration::minutes(timeout_mins as i64) {
                    status.timed_out = true;
                    return (false, status);
                }
            }
        }

        let mut all_met = true;

        // Check idle condition
        if let Some(idle_cond) = &conditions.require_idle {
            if idle_cond.enabled {
                let idle = self.check_idle().await;
                status.idle_met = Some(idle);
                if !idle {
                    all_met = false;
                }
            }
        }

        // Check repo inactive condition
        if let Some(repo_cond) = &conditions.require_repo_inactive {
            if repo_cond.enabled && !repo_cond.repositories.is_empty() {
                let repo_status = self.check_repos_inactive(&repo_cond.repositories);
                let all_repos_inactive = repo_status.iter().all(|(_, inactive)| *inactive);
                status.repo_inactive_met = Some(repo_status);
                if !all_repos_inactive {
                    all_met = false;
                }
            }
        }

        (all_met, status)
    }

    /// Check if runner is idle (not executing workflows or AI tasks)
    async fn check_idle(&self) -> bool {
        let client = reqwest::Client::new();
        let status_url = format!("{}/status", self.self_base_url());
        match client.get(&status_url).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let executor_state = json
                        .get("executor_state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let ai_running = json
                        .get("ai_analysis_running")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    // Idle = Ready state and no AI analysis running
                    executor_state == "Ready" && !ai_running
                } else {
                    false
                }
            }
            Err(e) => {
                warn!("Failed to check idle status: {}", e);
                false
            }
        }
    }

    /// Check if repositories have been inactive for the required duration
    fn check_repos_inactive(&self, repos: &[RepositoryWatch]) -> Vec<(String, bool)> {
        let now = std::time::SystemTime::now();

        repos
            .iter()
            .map(|repo| {
                let inactive = match get_most_recent_modification(&repo.path) {
                    Ok(last_modified) => {
                        let elapsed = now.duration_since(last_modified).unwrap_or_default();
                        elapsed.as_secs() >= (repo.inactive_minutes as u64 * 60)
                    }
                    Err(e) => {
                        warn!(
                            "Failed to check repository inactivity for '{}': {}",
                            repo.path, e
                        );
                        // If we can't read, assume not inactive (safer)
                        false
                    }
                };
                (repo.path.clone(), inactive)
            })
            .collect()
    }

    /// Record a condition timeout execution
    async fn record_condition_timeout(&self, task: &ScheduledTask) {
        let pg = match self.pg() {
            Ok(pg) => pg,
            Err(e) => {
                error!("record_condition_timeout: {}", e);
                return;
            }
        };
        let mut record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        record.status = ScheduledTaskStatus::Skipped;
        record.ended_at = Some(chrono::Utc::now().to_rfc3339());
        record.error_message = Some("Condition timeout exceeded".to_string());

        if let Err(e) = pg.insert_execution_record(&task.id, &record).await {
            error!("Failed to record condition timeout: {}", e);
        }
        if let Err(e) = pg
            .update_task_last_run(&task.id, Some(&record.execution_id))
            .await
        {
            error!("Failed to update task last_run: {}", e);
        }

        // Clear condition status
        if let Err(e) = pg.update_task_condition_status(&task.id, None).await {
            error!("Failed to clear condition status: {}", e);
        }

        // Update next_run
        self.update_task_next_run_db(&task.id).await;
    }
}

// ============================================================================
// Phase B — Missed-run reconciler
// ============================================================================

/// One reconciler-issued action for a single missed slot.
///
/// Returned by [`plan_catch_up_actions`] — extracted as a pure data type so
/// the policy logic can be unit-tested without a database or HTTP server.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CatchUpAction {
    /// Enqueue a catch-up execution for the given slot.
    Enqueue { scheduled_for: DateTime<Utc> },
    /// Insert a `MissedRunnerDown` history row for the given slot, no
    /// dispatch.
    Skip { scheduled_for: DateTime<Utc> },
}

/// Apply a [`CatchUpPolicy`] to a list of missed slots and return the
/// concrete actions to take.
///
/// The input is expected to be in chronological order (oldest first). For
/// `RunOnce` we collapse to the most recent slot, matching the documented
/// "the latest missed run wins" semantics.
fn plan_catch_up_actions(
    missed_slots: &[DateTime<Utc>],
    policy: CatchUpPolicy,
) -> Vec<CatchUpAction> {
    if missed_slots.is_empty() {
        return Vec::new();
    }

    match policy {
        CatchUpPolicy::Run => missed_slots
            .iter()
            .map(|slot| CatchUpAction::Enqueue {
                scheduled_for: *slot,
            })
            .collect(),
        CatchUpPolicy::RunOnce => {
            // Last slot in chronological order is the most recent. Use that
            // as the representative scheduled_for so history reflects "we
            // covered up through this slot".
            let latest = *missed_slots.last().expect("non-empty checked above");
            vec![CatchUpAction::Enqueue {
                scheduled_for: latest,
            }]
        }
        CatchUpPolicy::Skip => missed_slots
            .iter()
            .map(|slot| CatchUpAction::Skip {
                scheduled_for: *slot,
            })
            .collect(),
    }
}

/// Yield all expected fire times within the closed interval `[from, to]`
/// for the given schedule expression.
///
/// - `Cron`: uses [`cron::Schedule::after(&from)`] to seek strictly past
///   `from`, then takes while `t <= to`. Times are returned in `Utc`,
///   matching the runner's convention (see [`compute_next_run`]).
/// - `Interval(secs)`: synthesises slots at `from + secs`, `from + 2*secs`,
///   …, while each is `<= to`.
/// - `Once(iso)`: returns the single parsed timestamp if it lies in
///   `[from, to]`, else empty.
/// - `Condition(_)`: condition-based scheduling has no time slots, so
///   returns empty.
///
/// The cron crate's `after(&from)` excludes `from` itself, which is what
/// we want — the reconciler treats the last successful run's scheduled
/// timestamp as already-handled and only catches up *future* slots from
/// that point.
fn iter_slots_in_window(
    schedule: &ScheduleExpression,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    if to <= from {
        return Vec::new();
    }

    match schedule {
        ScheduleExpression::Cron(cron_expr) => {
            use cron::Schedule;
            use std::str::FromStr;

            // Match the same 5/6/7-field normalization as `compute_next_run`
            // so the reconciler agrees with the live scheduler about what a
            // slot is.
            let normalized = if cron_expr.split_whitespace().count() == 5 {
                format!("0 {}", cron_expr)
            } else {
                cron_expr.clone()
            };

            match Schedule::from_str(&normalized) {
                Ok(schedule) => schedule
                    .after(&from)
                    .take_while(|t| *t <= to)
                    // Hard cap to avoid pathological loops with sub-second
                    // crons; matching the spec's grace-window semantics any
                    // expression that would emit > 100k slots is almost
                    // certainly malformed for our use case.
                    .take(100_000)
                    .collect(),
                Err(e) => {
                    warn!(
                        "iter_slots_in_window: invalid cron '{}': {}; returning no slots",
                        cron_expr, e
                    );
                    Vec::new()
                }
            }
        }
        ScheduleExpression::Interval(secs) => {
            let secs = *secs;
            if secs == 0 {
                return Vec::new();
            }
            let step = chrono::Duration::seconds(secs as i64);
            let mut out = Vec::new();
            let mut cursor = from + step;
            // Same defensive cap as the cron branch.
            while cursor <= to && out.len() < 100_000 {
                out.push(cursor);
                cursor = cursor + step;
            }
            out
        }
        ScheduleExpression::Once(iso) => {
            match chrono::DateTime::parse_from_rfc3339(iso) {
                Ok(dt) => {
                    let dt_utc = dt.with_timezone(&Utc);
                    if dt_utc > from && dt_utc <= to {
                        vec![dt_utc]
                    } else {
                        Vec::new()
                    }
                }
                Err(_) => Vec::new(),
            }
        }
        ScheduleExpression::Condition(_) => Vec::new(),
    }
}

impl SchedulerService {
    /// Phase B — reconcile missed runs at scheduler startup.
    ///
    /// For each enabled task:
    /// 1. Look up the last successful run's `scheduled_for` (or `started_at`
    ///    as a fallback for legacy rows). If none, fall back to the task's
    ///    `created_at`.
    /// 2. Compute candidate slots in the window `[lookback_start, now -
    ///    catch_up_grace_seconds]` from the task's schedule expression.
    /// 3. Diff candidates against `scheduler_history.scheduled_for` to find
    ///    slots that have no matching history row.
    /// 4. Apply the task's [`CatchUpPolicy`] (Run / RunOnce / Skip) and
    ///    either enqueue executions (with `catch_up_run=true` and
    ///    `scheduled_for` stamped) or insert `MissedRunnerDown` history
    ///    rows.
    ///
    /// Errors per-task are logged and swallowed so one broken task can't
    /// block the entire fleet from reconciling.
    pub async fn reconcile_missed_runs(self: Arc<Self>) -> Result<(), String> {
        let pg = self.pg()?;

        let settings = match pg.get_scheduler_settings().await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Scheduler reconciler: skipping (failed to load settings): {}",
                    e
                );
                return Ok(());
            }
        };

        if !settings.enabled {
            info!("Scheduler reconciler: scheduler disabled, skipping");
            return Ok(());
        }

        let tasks = pg
            .get_all_scheduled_tasks()
            .await
            .map_err(|e| format!("reconcile_missed_runs: load tasks: {}", e))?;

        let now = Utc::now();

        for task in tasks {
            if !task.enabled {
                continue;
            }
            // Condition-only schedules have no time slots; nothing to
            // reconcile.
            if matches!(task.schedule, ScheduleExpression::Condition(_)) {
                continue;
            }

            if let Err(e) = self.clone().reconcile_task(&task, now).await {
                error!(
                    "Scheduler reconciler: task '{}' (id={}) failed: {}",
                    task.name, task.id, e
                );
            }
        }

        Ok(())
    }

    /// Reconcile a single task. Extracted so per-task errors don't kill the
    /// fleet-level loop.
    async fn reconcile_task(
        self: Arc<Self>,
        task: &ScheduledTask,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let pg = self.pg()?;

        let grace = chrono::Duration::seconds(task.catch_up_grace_seconds as i64);
        let window_end = now - grace;

        // Lookback start: the most recent successful run's scheduled_for
        // (preferred) or started_at (fallback for legacy rows). If no
        // success yet, fall back to created_at.
        let lookback_start = match self.last_successful_run_anchor(&task.id).await {
            Ok(Some(dt)) => dt,
            Ok(None) => match chrono::DateTime::parse_from_rfc3339(&task.created_at) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    return Err(format!(
                        "task created_at '{}' is not RFC3339: {}",
                        task.created_at, e
                    ));
                }
            },
            Err(e) => return Err(format!("last_successful_run_anchor: {}", e)),
        };

        if window_end <= lookback_start {
            // Either the grace window covers everything since the last
            // success, or the task is brand-new. Nothing to reconcile.
            return Ok(());
        }

        let candidates = iter_slots_in_window(&task.schedule, lookback_start, window_end);
        if candidates.is_empty() {
            return Ok(());
        }

        let missed = pg
            .find_missed_slots(&task.id, &candidates)
            .await
            .map_err(|e| format!("find_missed_slots: {}", e))?;

        if missed.is_empty() {
            return Ok(());
        }

        let actions = plan_catch_up_actions(&missed, task.catch_up_policy);
        let policy = task.catch_up_policy;

        info!(
            task_id = %task.id,
            slot_count = missed.len(),
            policy = ?policy,
            "scheduler reconciler: catch-up enqueued"
        );

        for action in actions {
            match action {
                CatchUpAction::Enqueue { scheduled_for } => {
                    let ctx = CatchUpContext { scheduled_for };
                    // Spawn the execution in the background so the
                    // reconciler doesn't block on long-running work.
                    let svc = self.clone();
                    let task_clone = task.clone();
                    tokio::spawn(async move {
                        svc.execute_task_with_context(task_clone, Some(ctx)).await;
                    });
                }
                CatchUpAction::Skip { scheduled_for } => {
                    if let Err(e) = self
                        .insert_missed_runner_down(&task.id, scheduled_for, now)
                        .await
                    {
                        error!(
                            "Scheduler reconciler: failed to insert MissedRunnerDown row \
                             for task {} slot {}: {}",
                            task.id, scheduled_for, e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Find the most recent successful execution's `scheduled_for` (preferred)
    /// or `started_at` (fallback) timestamp for the given task. Returns
    /// `None` if the task has no successful runs yet.
    async fn last_successful_run_anchor(
        &self,
        task_id: &str,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let pg = self.pg()?;
        // Pull a generous batch and pick the most recent successful one.
        // The reconciler only runs at startup (or via explicit invocation),
        // so the cost is not on the hot path.
        let history = pg.get_execution_history(task_id, 100).await?;

        for record in history {
            if !record.success {
                continue;
            }
            // Prefer scheduled_for (the original slot), fall back to
            // started_at for legacy rows that pre-date the v12 migration.
            if let Some(ref s) = record.scheduled_for {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                    return Ok(Some(dt.with_timezone(&Utc)));
                }
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&record.started_at) {
                return Ok(Some(dt.with_timezone(&Utc)));
            }
        }

        Ok(None)
    }

    /// Insert a `MissedRunnerDown` history row for a slot the reconciler
    /// has decided to skip (per [`CatchUpPolicy::Skip`]). Both `started_at`
    /// and `ended_at` are set to `now` so the row sorts naturally with
    /// other start-time-ordered queries.
    async fn insert_missed_runner_down(
        &self,
        task_id: &str,
        scheduled_for: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let pg = self.pg()?;
        let now_iso = now.to_rfc3339();

        let mut record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        record.started_at = now_iso.clone();
        record.ended_at = Some(now_iso);
        record.status = ScheduledTaskStatus::MissedRunnerDown;
        record.success = false;
        record.scheduled_for = Some(scheduled_for.to_rfc3339());
        record.catch_up_run = true;

        pg.insert_execution_record(task_id, &record).await
    }
}

// ============================================================================
// File System Helpers
// ============================================================================

/// Get the most recent modification time of any file in a directory tree
fn get_most_recent_modification(path: &str) -> Result<std::time::SystemTime, std::io::Error> {
    let mut most_recent = std::time::SystemTime::UNIX_EPOCH;

    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_ignored_path(e.path()))
        .flatten()
    {
        if let Ok(metadata) = entry.metadata() {
            if let Ok(modified) = metadata.modified() {
                if modified > most_recent {
                    most_recent = modified;
                }
            }
        }
    }

    if most_recent == std::time::SystemTime::UNIX_EPOCH {
        // No files found - return current time (treat as not inactive)
        return Ok(std::time::SystemTime::now());
    }

    Ok(most_recent)
}

/// Check if a path should be ignored (common build/cache directories)
fn is_ignored_path(path: &std::path::Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(
        name,
        "node_modules"
            | ".git"
            | "target"
            | "__pycache__"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
            | ".next"
            | ".cache"
            | ".turbo"
            | ".nuxt"
            | ".svelte-kit"
            | "coverage"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
    )
}

// ============================================================================
// Global Instance
// ============================================================================

use once_cell::sync::Lazy;
use tokio::sync::Mutex;

/// Global scheduler service instance
static SCHEDULER_SERVICE: Lazy<Mutex<Option<Arc<SchedulerService>>>> =
    Lazy::new(|| Mutex::new(None));

/// Start the global scheduler service
pub async fn start_scheduler_service(pg_db: Arc<PgDb>, app_state: Arc<AppState>) {
    let mut service_guard = SCHEDULER_SERVICE.lock().await;

    if service_guard.is_some() {
        warn!("Scheduler service already running");
        return;
    }

    let service = Arc::new(SchedulerService::with_app_state(pg_db, app_state));
    *service_guard = Some(service.clone());
    drop(service_guard);

    // Start the service loop in a background task
    tokio::spawn(async move {
        service.start().await;
    });

    info!("Scheduler service started");
}

/// Stop the global scheduler service
pub async fn stop_scheduler_service() {
    let mut service_guard = SCHEDULER_SERVICE.lock().await;

    if let Some(service) = service_guard.take() {
        service.stop();
        info!("Scheduler service stopped");
    }
}

/// Get the global scheduler service (if running)
pub async fn get_scheduler_service() -> Option<Arc<SchedulerService>> {
    let service_guard = SCHEDULER_SERVICE.lock().await;
    service_guard.clone()
}

/// Run a task immediately (outside its schedule)
pub async fn run_task_now(task_id: &str) -> Result<(), String> {
    let service_guard = SCHEDULER_SERVICE.lock().await;
    let service = service_guard
        .as_ref()
        .ok_or("Scheduler service not running")?
        .clone();
    drop(service_guard);

    // Look up the task from PG via the service's pg_db handle
    let pg = service.pg()?;
    let task = pg
        .get_scheduled_task(task_id)
        .await
        .map_err(|e| format!("Failed to look up task: {}", e))?
        .ok_or_else(|| format!("Task not found: {}", task_id))?;

    // Check if already running
    if service.is_task_running(task_id).await {
        return Err("Task is already running".to_string());
    }

    // Execute in background
    tokio::spawn(async move {
        service.execute_task(task).await;
    });

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scheduler_service_creation() {
        let service = SchedulerService::new(None);
        assert_eq!(service.check_interval_secs, 60);
    }

    #[tokio::test]
    async fn test_running_tasks_tracking() {
        let service = SchedulerService::new(None);

        // Initially empty
        let running = service.get_running_tasks().await;
        assert!(running.is_empty());

        // Add a task
        {
            let mut running = service.running_tasks.write().await;
            running.push("test-task".to_string());
        }

        // Should be running
        assert!(service.is_task_running("test-task").await);
        assert!(!service.is_task_running("other-task").await);
    }

    // ========================================================================
    // Phase D — RemoteAgent dispatcher tests (launch_remote_agent)
    //
    // These exercise the HTTP boundary only — we never actually spawn Claude.
    // A tiny axum mock listens on a random port; QONTINUI_PORT is set so
    // `self_base_url()` (env-only path; AppState is None in tests) targets
    // it. Each test asserts the shape of the request body and the
    // success/error mapping of the response.
    //
    // Tests are serialized via a process-wide mutex because they all
    // mutate the QONTINUI_PORT env var.
    // ========================================================================

    /// Serialize env-mutating tests. `Mutex<()>` is fine — we only need
    /// mutual exclusion, not data sharing.
    static REMOTE_AGENT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Spin up a tiny axum mock on a random localhost port that captures
    /// the most recent `/prompts/run` body and replies with `response`.
    /// Returns `(port, captured_body)` so the test can assert on the body
    /// after `launch_remote_agent` returns.
    async fn spawn_prompts_run_mock(
        response: axum::response::Response,
    ) -> (
        u16,
        Arc<tokio::sync::Mutex<Option<serde_json::Value>>>,
    ) {
        use axum::{extract::State, routing::post, Router};

        let captured = Arc::new(tokio::sync::Mutex::new(None::<serde_json::Value>));
        // The axum response is consumed once, so we wrap it in an Option +
        // Mutex and take() it on the first request.
        let response_slot = Arc::new(tokio::sync::Mutex::new(Some(response)));

        async fn handler(
            State((captured, response_slot)): State<(
                Arc<tokio::sync::Mutex<Option<serde_json::Value>>>,
                Arc<tokio::sync::Mutex<Option<axum::response::Response>>>,
            )>,
            axum::Json(body): axum::Json<serde_json::Value>,
        ) -> axum::response::Response {
            *captured.lock().await = Some(body);
            response_slot
                .lock()
                .await
                .take()
                .unwrap_or_else(|| {
                    axum::response::Response::builder()
                        .status(500)
                        .body(axum::body::Body::from(
                            "mock response already consumed",
                        ))
                        .unwrap()
                })
        }

        let app = Router::new()
            .route("/prompts/run", post(handler))
            .with_state((captured.clone(), response_slot));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Best-effort wait for the listener to be accepting.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        (port, captured)
    }

    fn ok_response(body: serde_json::Value) -> axum::response::Response {
        axum::response::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_launch_remote_agent_posts_full_body_when_all_fields_set() {
        let _guard = REMOTE_AGENT_TEST_LOCK.lock().unwrap();

        let (port, captured) = spawn_prompts_run_mock(ok_response(serde_json::json!({
            "session_id": "sid-123"
        })))
        .await;
        std::env::set_var("QONTINUI_PORT", port.to_string());

        let service = SchedulerService::new(None);
        let mcp_conns = vec![qontinui_types::scheduler::McpConnectionRef {
            name: "filesystem".to_string(),
            url: Some("http://example.com/mcp".to_string()),
        }];
        let result = service
            .launch_remote_agent(
                "nightly-cleanup",
                "List files in cwd.",
                Some("/tmp/work"),
                Some("claude-sonnet-4-6"),
                &["Bash".to_string(), "Read".to_string()],
                &mcp_conns,
                Some(25),
                Some(120),
            )
            .await;

        assert_eq!(result.unwrap(), "sid-123");

        let body = captured.lock().await.clone().expect("no body captured");
        assert_eq!(body["name"], "scheduled-remote-agent-nightly-cleanup");
        assert_eq!(body["content"], "List files in cwd.");
        assert_eq!(body["max_sessions"], 1);
        assert_eq!(body["timeout_seconds"], 120);
        assert_eq!(body["max_turns"], 25);
        assert_eq!(body["working_directory"], "/tmp/work");
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(
            body["allowed_tools"],
            serde_json::json!(["Bash", "Read"])
        );
        assert_eq!(body["mcp_connections"][0]["name"], "filesystem");
        assert_eq!(
            body["mcp_connections"][0]["url"],
            "http://example.com/mcp"
        );
    }

    #[tokio::test]
    async fn test_launch_remote_agent_omits_optional_fields_when_none() {
        let _guard = REMOTE_AGENT_TEST_LOCK.lock().unwrap();

        let (port, captured) = spawn_prompts_run_mock(ok_response(serde_json::json!({
            "session_id": "sid-456"
        })))
        .await;
        std::env::set_var("QONTINUI_PORT", port.to_string());

        let service = SchedulerService::new(None);
        let result = service
            .launch_remote_agent(
                "minimal",
                "do the thing",
                None,
                None,
                &[],
                &[],
                None,
                None,
            )
            .await;

        assert_eq!(result.unwrap(), "sid-456");

        let body = captured.lock().await.clone().expect("no body captured");
        // Required fields present.
        assert_eq!(body["name"], "scheduled-remote-agent-minimal");
        assert_eq!(body["content"], "do the thing");
        assert_eq!(body["max_sessions"], 1);
        // Plan defaults.
        assert_eq!(body["timeout_seconds"], 600);
        assert_eq!(body["max_turns"], 50);
        // Optional fields omitted entirely.
        assert!(body.get("working_directory").is_none());
        assert!(body.get("model").is_none());
        assert!(body.get("allowed_tools").is_none());
        assert!(body.get("mcp_connections").is_none());
    }

    #[tokio::test]
    async fn test_launch_remote_agent_extracts_session_id_from_data_envelope() {
        let _guard = REMOTE_AGENT_TEST_LOCK.lock().unwrap();

        // /prompts/run wraps successful responses in
        // `{ "success": true, "data": { ... } }`.
        let (port, _captured) = spawn_prompts_run_mock(ok_response(serde_json::json!({
            "success": true,
            "data": { "session_id": "wrapped-sid", "task_run_id": "wrapped-sid" }
        })))
        .await;
        std::env::set_var("QONTINUI_PORT", port.to_string());

        let service = SchedulerService::new(None);
        let result = service
            .launch_remote_agent("e", "p", None, None, &[], &[], None, None)
            .await;

        assert_eq!(result.unwrap(), "wrapped-sid");
    }

    #[tokio::test]
    async fn test_launch_remote_agent_500_returns_err() {
        let _guard = REMOTE_AGENT_TEST_LOCK.lock().unwrap();

        let err_resp = axum::response::Response::builder()
            .status(500)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::to_vec(&serde_json::json!({"error": "boom"})).unwrap(),
            ))
            .unwrap();
        let (port, _captured) = spawn_prompts_run_mock(err_resp).await;
        std::env::set_var("QONTINUI_PORT", port.to_string());

        let service = SchedulerService::new(None);
        let result = service
            .launch_remote_agent("e", "p", None, None, &[], &[], None, None)
            .await;

        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP 500"),
            "expected HTTP 500 error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_launch_remote_agent_missing_session_id_returns_err() {
        let _guard = REMOTE_AGENT_TEST_LOCK.lock().unwrap();

        // 200 OK but no session_id anywhere.
        let (port, _captured) = spawn_prompts_run_mock(ok_response(serde_json::json!({
            "ok": true,
            "data": { "task_run_id": "x" }
        })))
        .await;
        std::env::set_var("QONTINUI_PORT", port.to_string());

        let service = SchedulerService::new(None);
        let result = service
            .launch_remote_agent("e", "p", None, None, &[], &[], None, None)
            .await;

        let err = result.unwrap_err();
        assert!(
            err.contains("omitted session_id"),
            "expected missing-session_id error, got: {}",
            err
        );
    }

    // ========================================================================
    // Phase B — Missed-run reconciler tests
    //
    // These exercise the pure helpers (`iter_slots_in_window`,
    // `plan_catch_up_actions`) which carry the policy logic. The DB-driven
    // `reconcile_missed_runs` orchestration is integration-tested via the
    // existing live-PG harness (`new_blocking_for_test`); see the
    // `proj_pg_dual_schema_runner_public.md` memory entry for fixture setup.
    // ========================================================================

    /// Build an `Interval` schedule for the given seconds.
    fn interval(secs: u64) -> ScheduleExpression {
        ScheduleExpression::Interval(secs)
    }

    #[test]
    fn reconciler_iter_slots_interval_basic() {
        // 1h interval, 6h window → 6 slots.
        let now = Utc::now();
        let from = now - chrono::Duration::hours(6);
        let to = now;
        let slots = iter_slots_in_window(&interval(3600), from, to);
        assert_eq!(slots.len(), 6, "expected 6 hourly slots in 6h window");
        // First slot should be from + 1h, last should be from + 6h == to.
        assert_eq!(slots.first().copied(), Some(from + chrono::Duration::hours(1)));
        assert_eq!(slots.last().copied(), Some(from + chrono::Duration::hours(6)));
    }

    #[test]
    fn reconciler_iter_slots_interval_empty_window() {
        // to <= from → empty.
        let now = Utc::now();
        let slots = iter_slots_in_window(&interval(60), now, now);
        assert!(slots.is_empty());

        let slots = iter_slots_in_window(&interval(60), now, now - chrono::Duration::seconds(1));
        assert!(slots.is_empty());
    }

    #[test]
    fn reconciler_iter_slots_cron_5_field_normalized() {
        // Hourly at minute 0. From 00:30 to 07:00 (390 min later) the
        // top-of-hour slots strictly after `from` and `<= to` are
        // 01:00..=07:00 = 7 slots. The window-end is inclusive, matching
        // the doc-comment's `[from, to]` semantics.
        let from = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::minutes(390); // exactly 07:00
        let slots = iter_slots_in_window(
            &ScheduleExpression::Cron("0 * * * *".to_string()),
            from,
            to,
        );
        assert_eq!(slots.len(), 7, "expected 01:00..=07:00 inclusive");
        // First slot is 01:00.
        let first = chrono::DateTime::parse_from_rfc3339("2026-01-01T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(slots[0], first);
        // Last slot is exactly 07:00 (== to).
        let last = chrono::DateTime::parse_from_rfc3339("2026-01-01T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(slots.last().copied(), Some(last));
    }

    #[test]
    fn reconciler_iter_slots_once_inside_window() {
        let from = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::hours(2);
        let target = from + chrono::Duration::hours(1);
        let slots = iter_slots_in_window(
            &ScheduleExpression::Once(target.to_rfc3339()),
            from,
            to,
        );
        assert_eq!(slots, vec![target]);
    }

    #[test]
    fn reconciler_iter_slots_once_outside_window_returns_empty() {
        let from = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let to = from + chrono::Duration::hours(2);

        // Before window.
        let before = from - chrono::Duration::hours(1);
        assert!(iter_slots_in_window(
            &ScheduleExpression::Once(before.to_rfc3339()),
            from,
            to
        )
        .is_empty());

        // After window.
        let after = to + chrono::Duration::hours(1);
        assert!(iter_slots_in_window(
            &ScheduleExpression::Once(after.to_rfc3339()),
            from,
            to
        )
        .is_empty());

        // Exactly at `from` is excluded (same convention as cron's
        // `after(&from)`).
        assert!(iter_slots_in_window(
            &ScheduleExpression::Once(from.to_rfc3339()),
            from,
            to
        )
        .is_empty());
    }

    #[test]
    fn reconciler_iter_slots_condition_returns_empty() {
        use crate::scheduler::ConditionScheduleConfig;
        let now = Utc::now();
        let slots = iter_slots_in_window(
            &ScheduleExpression::Condition(ConditionScheduleConfig::default()),
            now - chrono::Duration::hours(6),
            now,
        );
        assert!(slots.is_empty());
    }

    /// Build six hourly slots ending at `now` for a task that's been down
    /// for 6h, used as a fixture by the policy tests.
    fn six_hourly_missed_slots() -> Vec<DateTime<Utc>> {
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        (1..=6)
            .map(|h| now - chrono::Duration::hours(7 - h as i64))
            .collect()
    }

    #[test]
    fn reconciler_policy_run_once_collapses_to_latest() {
        let missed = six_hourly_missed_slots();
        let actions = plan_catch_up_actions(&missed, CatchUpPolicy::RunOnce);
        assert_eq!(actions.len(), 1, "RunOnce must collapse to one enqueue");
        match &actions[0] {
            CatchUpAction::Enqueue { scheduled_for } => {
                assert_eq!(*scheduled_for, *missed.last().unwrap());
            }
            other => panic!("expected Enqueue, got {:?}", other),
        }
    }

    #[test]
    fn reconciler_policy_run_enqueues_all() {
        let missed = six_hourly_missed_slots();
        let actions = plan_catch_up_actions(&missed, CatchUpPolicy::Run);
        assert_eq!(actions.len(), 6);
        for (i, action) in actions.iter().enumerate() {
            match action {
                CatchUpAction::Enqueue { scheduled_for } => {
                    assert_eq!(*scheduled_for, missed[i]);
                }
                other => panic!("expected Enqueue, got {:?}", other),
            }
        }
    }

    #[test]
    fn reconciler_policy_skip_inserts_missed_runner_down() {
        let missed = six_hourly_missed_slots();
        let actions = plan_catch_up_actions(&missed, CatchUpPolicy::Skip);
        assert_eq!(actions.len(), 6);
        for (i, action) in actions.iter().enumerate() {
            match action {
                CatchUpAction::Skip { scheduled_for } => {
                    assert_eq!(*scheduled_for, missed[i]);
                }
                other => panic!("expected Skip, got {:?}", other),
            }
        }
    }

    #[test]
    fn reconciler_policy_empty_missed_yields_no_actions() {
        let actions = plan_catch_up_actions(&[], CatchUpPolicy::Run);
        assert!(actions.is_empty());
        let actions = plan_catch_up_actions(&[], CatchUpPolicy::RunOnce);
        assert!(actions.is_empty());
        let actions = plan_catch_up_actions(&[], CatchUpPolicy::Skip);
        assert!(actions.is_empty());
    }

    #[test]
    fn reconciler_grace_window_excludes_recent_slots() {
        // Simulate the reconciler's window computation: window_end =
        // now - grace. A slot 4 minutes ago should fall *after* window_end
        // (i.e. excluded) given a 5-minute grace, while 6 minutes ago is
        // before window_end (included).
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let grace = chrono::Duration::seconds(300);
        let window_end = now - grace; // = 11:55:00

        // The lookback start needs to bracket both candidates strictly.
        let lookback_start = now - chrono::Duration::hours(1);

        // Use a 1-minute interval so the window emits slots at every minute
        // inside (lookback_start, window_end]. The "4 minutes ago" slot
        // (11:56) should be absent; the "6 minutes ago" slot (11:54)
        // should be present.
        let slots = iter_slots_in_window(&interval(60), lookback_start, window_end);

        let four_min_ago = now - chrono::Duration::minutes(4);
        let six_min_ago = now - chrono::Duration::minutes(6);

        assert!(
            !slots.contains(&four_min_ago),
            "slot 4 minutes ago must NOT be in [start, now-grace]"
        );
        assert!(
            slots.contains(&six_min_ago),
            "slot 6 minutes ago MUST be in [start, now-grace]"
        );
    }

    #[test]
    fn reconciler_first_time_task_window_starts_at_created_at() {
        // For a task with no successful runs, the lookback start is
        // `created_at`. Verify the slot iterator yields slots strictly after
        // `created_at` (not at it) so we don't double-count the initial
        // tick. This is enforced by `cron::Schedule::after(&from)` and the
        // interval branch's `from + step` initialiser — assert here so a
        // future refactor can't silently break the contract.
        let created_at = chrono::DateTime::parse_from_rfc3339("2026-04-26T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = created_at + chrono::Duration::hours(3);
        let grace = chrono::Duration::seconds(300);
        let window_end = now - grace;

        let slots = iter_slots_in_window(&interval(3600), created_at, window_end);
        // 3h - 5min = 2h55min. With hourly slots from created_at + 1h, we
        // get slots at +1h, +2h. (+3h would land at exactly `now`, which is
        // past window_end.)
        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|s| *s > created_at));
        assert!(slots.iter().all(|s| *s <= window_end));
    }

    // ========================================================================
    // Phase C — Failure-aware rescheduling tests
    //
    // The DB-driven `apply_launch_failure_backoff` orchestration is
    // covered indirectly by the integration harness (a live PG via
    // `new_blocking_for_test`). The pure `compute_launch_failed_next_run`
    // helper plus `ScheduledTaskExt::{record_launch_failure,
    // launch_failure_backoff, reset_launch_failures}` carry the actual
    // policy and are unit-tested here without a database.
    // ========================================================================

    use crate::scheduler::{schedule_expression_default, scheduled_task_type_default};

    fn fixture_task(base_backoff: u32) -> ScheduledTask {
        let mut task = ScheduledTask::new(
            "Phase C fixture".to_string(),
            None,
            schedule_expression_default(),
            scheduled_task_type_default(),
        );
        task.launch_failure_backoff_seconds = base_backoff;
        task
    }

    #[test]
    fn launch_fail_compute_next_run_picks_backoff_when_later() {
        // Cron is hourly; backoff is 240s (4 min). The hourly cron will
        // fire much later than `now + 4min`, so the cron wins.
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let normal_next = Some(now + chrono::Duration::minutes(30)); // top of next hour
        let backoff = Some(chrono::Duration::seconds(240));

        let next = compute_launch_failed_next_run(normal_next, backoff, now)
            .expect("next_run is Some");
        assert_eq!(next, normal_next.unwrap());
        // Distance from now should be > backoff (cron-driven)
        assert!((next - now).num_seconds() > 240);
    }

    #[test]
    fn launch_fail_compute_next_run_picks_backoff_when_earlier_normal() {
        // Cron's natural fire is in 30s; backoff is 60s. Backoff wins —
        // we never want to *pull in* the schedule earlier than it would
        // naturally be, but we do want to push it back to at least
        // `now + backoff`.
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let normal_next = Some(now + chrono::Duration::seconds(30));
        let backoff = Some(chrono::Duration::seconds(60));

        let next = compute_launch_failed_next_run(normal_next, backoff, now)
            .expect("next_run is Some");
        assert_eq!((next - now).num_seconds(), 60);
    }

    #[test]
    fn launch_fail_compute_next_run_no_normal_uses_backoff() {
        // `Once` schedule already fired → normal_next = None. With a
        // backoff present we still set a future fire time to preserve
        // any one-shot retry semantics.
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = compute_launch_failed_next_run(None, Some(chrono::Duration::seconds(120)), now)
            .expect("next_run is Some");
        assert_eq!((next - now).num_seconds(), 120);
    }

    #[test]
    fn launch_fail_compute_next_run_no_normal_no_backoff_yields_none() {
        let now = Utc::now();
        assert!(compute_launch_failed_next_run(None, None, now).is_none());
    }

    #[test]
    fn launch_fail_compute_next_run_no_backoff_keeps_normal() {
        // Defensive case: `launch_failure_backoff()` returned None even
        // though we're on the LaunchFailed path (e.g., misconfigured
        // base of 0). Don't lose the normal schedule.
        let now = Utc::now();
        let normal = Some(now + chrono::Duration::hours(1));
        let next = compute_launch_failed_next_run(normal, None, now).expect("next_run is Some");
        assert_eq!(next, normal.unwrap());
    }

    #[test]
    fn launch_fail_three_consecutive_pushes_next_run_by_240s() {
        // Plan check: 3 consecutive launch failures → backoff = 60 *
        // 2^(3-1) = 240s. With a far-future cron's normal next run, the
        // backoff is the floor; with no schedule (Once already fired),
        // it's also exactly 240s. Use an Interval(7200) (2h) so the
        // backoff path exercised here is the "backoff < normal_next"
        // case — next_run distance from now must be ~7200s, NOT 240s.
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut task = fixture_task(60);
        task.schedule = ScheduleExpression::Interval(7200);

        // Simulate three consecutive launch failures.
        task.record_launch_failure();
        task.record_launch_failure();
        task.record_launch_failure();
        assert_eq!(task.consecutive_launch_failures, 3);

        let backoff = task.launch_failure_backoff().expect("backoff present");
        assert_eq!(backoff.num_seconds(), 240);

        let normal_next = compute_next_run(&task.schedule, now);
        let next = compute_launch_failed_next_run(normal_next, Some(backoff), now)
            .expect("next_run is Some");
        // Cron/interval far-future wins, not the 240s backoff.
        assert_eq!((next - now).num_seconds(), 7200);
    }

    #[test]
    fn launch_fail_three_consecutive_with_short_schedule_uses_backoff() {
        // Same 3-failure scenario but with a 30s interval — backoff is
        // 240s, normal_next is 30s away → backoff wins.
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-26T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut task = fixture_task(60);
        task.schedule = ScheduleExpression::Interval(30);

        task.record_launch_failure();
        task.record_launch_failure();
        task.record_launch_failure();

        let backoff = task.launch_failure_backoff().expect("backoff present");
        let normal_next = compute_next_run(&task.schedule, now);
        let next = compute_launch_failed_next_run(normal_next, Some(backoff), now)
            .expect("next_run is Some");

        // 240s backoff overrides the 30s natural cadence.
        assert_eq!((next - now).num_seconds(), 240);
    }

    #[test]
    fn launch_fail_then_success_then_fail_resets_streak() {
        // The "failure → success → failure" resilience pattern from the
        // plan: after a successful start the counter is zeroed, so the
        // next failure starts the sequence over from base (60s), not
        // from 480s as it would if we kept counting.
        let mut task = fixture_task(60);

        // 3 failures
        for _ in 0..3 {
            task.record_launch_failure();
        }
        assert_eq!(task.launch_failure_backoff().unwrap().num_seconds(), 240);

        // Success — counter reset.
        task.reset_launch_failures();
        assert!(task.launch_failure_backoff().is_none());

        // Next failure starts the sequence over.
        task.record_launch_failure();
        assert_eq!(task.launch_failure_backoff().unwrap().num_seconds(), 60);
    }

    #[test]
    fn launch_fail_records_carry_status_through_execution_record() {
        // `mark_launch_failed` must set status to LaunchFailed (distinct
        // from runtime Failed). This is what drives the backoff trigger
        // in `finalize_sync_execution` and `launch_and_poll`.
        let mut record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        record.mark_launch_failed(Some("workflow file missing".to_string()));
        assert!(matches!(record.status, ScheduledTaskStatus::LaunchFailed));
        assert!(!record.success);
        assert_eq!(
            record.error_message.as_deref(),
            Some("workflow file missing")
        );
        assert!(record.ended_at.is_some());

        // Distinct from `complete(false, _)` which yields runtime Failed.
        let mut runtime_record = <TaskExecutionRecord as TaskExecutionRecordExt>::new();
        runtime_record.complete(false, Some("runtime error".to_string()));
        assert!(matches!(
            runtime_record.status,
            ScheduledTaskStatus::Failed
        ));
    }
}
