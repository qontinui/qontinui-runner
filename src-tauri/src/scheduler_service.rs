//! Scheduler Service
//!
//! Background service that monitors scheduled tasks and executes them
//! at their scheduled times. Integrates with existing workflow and prompt
//! execution infrastructure.

// Allow dead code - these are public API functions that may not be called yet
// but are part of the complete scheduler interface
#![allow(dead_code)]

use crate::scheduler::{
    clear_task_condition_status, compute_next_run, get_task, load_scheduler_state,
    record_execution, save_scheduler_state, update_task_condition_status, ConditionStatus,
    RepositoryWatch, ScheduledTask, ScheduledTaskStatus, ScheduledTaskType, TaskExecutionRecord,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use walkdir::WalkDir;

// ============================================================================
// Scheduler Service
// ============================================================================

/// Background scheduler service that executes tasks at their scheduled times
pub struct SchedulerService {
    /// Flag to stop the service
    stop_signal: Arc<AtomicBool>,
    /// Currently running task IDs
    running_tasks: Arc<RwLock<Vec<String>>>,
    /// Check interval in seconds
    check_interval_secs: u64,
}

impl SchedulerService {
    /// Create a new scheduler service
    pub fn new() -> Self {
        Self {
            stop_signal: Arc::new(AtomicBool::new(false)),
            running_tasks: Arc::new(RwLock::new(Vec::new())),
            check_interval_secs: 60, // Check every minute
        }
    }

    /// Start the scheduler loop (runs in background)
    pub async fn start(&self) {
        info!("Starting scheduler service");

        // Update all next_run times on startup
        if let Err(e) = crate::scheduler::update_all_next_runs() {
            error!("Failed to update next run times: {}", e);
        }

        while !self.stop_signal.load(Ordering::SeqCst) {
            // tick() checks enabled status internally to avoid double-loading state
            self.tick().await;

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

    /// Check and execute due tasks
    async fn tick(&self) {
        let state = load_scheduler_state();
        let settings = state.settings;

        // Skip if scheduler is disabled
        if !settings.enabled {
            return;
        }

        let now = chrono::Utc::now();

        // Find tasks that are:
        // 1. Due for execution (next_run <= now), OR
        // 2. Already waiting for conditions (have condition_status set)
        let mut due_tasks: Vec<ScheduledTask> = state
            .tasks
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
        for task in due_tasks.into_iter().take(available_slots) {
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
                    if let Err(e) = update_task_condition_status(&task.id, status) {
                        error!("Failed to update condition status: {}", e);
                    }
                    continue;
                }

                // Conditions met - clear status before execution
                if let Err(e) = clear_task_condition_status(&task.id) {
                    error!("Failed to clear condition status: {}", e);
                }
                info!("Scheduler: Task '{}' conditions met, executing", task.name);
            }

            info!("Scheduler: Executing task '{}'", task.name);
            self.execute_task(task).await;
        }
    }

    /// Record a skipped execution
    async fn record_skipped(&self, task: &ScheduledTask) {
        let mut record = TaskExecutionRecord::new();
        record.status = ScheduledTaskStatus::Skipped;
        record.ended_at = Some(chrono::Utc::now().to_rfc3339());

        if let Err(e) = record_execution(&task.id, record) {
            error!("Failed to record skipped execution: {}", e);
        }

        // Update next_run
        self.update_task_next_run(&task.id).await;
    }

    /// Execute a scheduled task
    async fn execute_task(&self, task: ScheduledTask) {
        let task_id = task.id.clone();
        let task_name = task.name.clone();
        let auto_fix_on_failure = task.auto_fix_on_failure;

        // Mark as running
        {
            let mut running = self.running_tasks.write().await;
            running.push(task_id.clone());
        }

        // Create execution record
        let mut record = TaskExecutionRecord::new();

        // Execute based on task type
        let result = match &task.task {
            ScheduledTaskType::Workflow {
                workflow_name,
                config_path,
                monitor_index,
            } => {
                self.execute_workflow(workflow_name, config_path.as_deref(), *monitor_index)
                    .await
            }
            ScheduledTaskType::Prompt {
                prompt_id,
                max_sessions,
            } => self.execute_prompt(prompt_id, *max_sessions).await,
            ScheduledTaskType::AutoFix {
                check_findings,
                force_run,
            } => self.execute_auto_fix(*check_findings, *force_run).await,
        };

        // Update record with result
        match result {
            Ok((success, session_id)) => {
                record.session_id = session_id;
                record.complete(success, None);
                info!(
                    "Scheduler: Task '{}' completed (success: {})",
                    task_name, success
                );

                // Trigger auto-fix if failed and configured
                if !success && auto_fix_on_failure {
                    info!(
                        "Scheduler: Triggering auto-fix for failed task '{}'",
                        task_name
                    );
                    if let Ok((_, Some(session_id))) = self.execute_auto_fix(true, false).await {
                        record.mark_auto_fix_triggered(session_id);
                    }
                }
            }
            Err(e) => {
                record.complete(false, Some(e.clone()));
                error!("Scheduler: Task '{}' failed: {}", task_name, e);

                // Trigger auto-fix if configured
                if auto_fix_on_failure {
                    info!(
                        "Scheduler: Triggering auto-fix for failed task '{}'",
                        task_name
                    );
                    if let Ok((_, Some(session_id))) = self.execute_auto_fix(true, false).await {
                        record.mark_auto_fix_triggered(session_id);
                    }
                }
            }
        }

        // Record the execution
        if let Err(e) = record_execution(&task_id, record) {
            error!("Failed to record execution: {}", e);
        }

        // Update next_run
        self.update_task_next_run(&task_id).await;

        // Remove from running
        {
            let mut running = self.running_tasks.write().await;
            running.retain(|id| id != &task_id);
        }
    }

    /// Update the next_run time for a task
    async fn update_task_next_run(&self, task_id: &str) {
        let mut state = load_scheduler_state();

        if let Some(task) = state.tasks.iter_mut().find(|t| t.id == task_id) {
            let now = chrono::Utc::now();
            task.next_run = compute_next_run(&task.schedule, now).map(|dt| dt.to_rfc3339());
            task.touch();
        }

        if let Err(e) = save_scheduler_state(&state) {
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
        let base_url = crate::mcp::types::get_self_base_url_from_env();

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

    /// Execute a prompt task
    async fn execute_prompt(
        &self,
        prompt_id: &str,
        max_sessions: Option<u32>,
    ) -> Result<(bool, Option<String>), String> {
        info!(
            "Executing prompt '{}' (max_sessions: {:?})",
            prompt_id, max_sessions
        );

        // Run prompt via HTTP API
        let client = reqwest::Client::new();
        let base_url = crate::mcp::types::get_self_base_url_from_env();

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

        // Parse response
        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let session_id = response_json
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Success is determined by checkpoint completion
        let success = response_json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok((success, session_id))
    }

    /// Execute an auto-fix task
    async fn execute_auto_fix(
        &self,
        check_findings: bool,
        force_run: bool,
    ) -> Result<(bool, Option<String>), String> {
        info!(
            "Executing auto-fix (check_findings: {}, force_run: {})",
            check_findings, force_run
        );

        // Trigger auto-fix via HTTP API
        let client = reqwest::Client::new();
        let base_url = crate::mcp::types::get_self_base_url_from_env();

        // Build the auto-fix prompt (similar to handleAnalyzeAll in ExecutionReport.tsx)
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

        // Parse response
        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let success = response_json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Auto-fix doesn't have a session ID in the traditional sense
        Ok((success, None))
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
    // Condition Checking
    // ========================================================================

    /// Check if a task's conditions are met
    /// Returns (all_conditions_met, updated_status)
    async fn check_conditions(&self, task: &ScheduledTask) -> (bool, ConditionStatus) {
        let conditions = match &task.conditions {
            Some(c) => c,
            None => return (true, ConditionStatus::default()),
        };

        // Check if any conditions are actually enabled
        if !task.has_conditions() {
            return (true, ConditionStatus::default());
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
        let status_url = format!("{}/status", crate::mcp::types::get_self_base_url_from_env());
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
        let mut record = TaskExecutionRecord::new();
        record.status = ScheduledTaskStatus::Skipped;
        record.ended_at = Some(chrono::Utc::now().to_rfc3339());
        record.error_message = Some("Condition timeout exceeded".to_string());

        if let Err(e) = record_execution(&task.id, record) {
            error!("Failed to record condition timeout: {}", e);
        }

        // Clear condition status
        if let Err(e) = clear_task_condition_status(&task.id) {
            error!("Failed to clear condition status: {}", e);
        }

        // Update next_run
        self.update_task_next_run(&task.id).await;
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

impl Default for SchedulerService {
    fn default() -> Self {
        Self::new()
    }
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
pub async fn start_scheduler_service() {
    let mut service_guard = SCHEDULER_SERVICE.lock().await;

    if service_guard.is_some() {
        warn!("Scheduler service already running");
        return;
    }

    let service = Arc::new(SchedulerService::new());
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
    let task = get_task(task_id).ok_or_else(|| format!("Task not found: {}", task_id))?;

    let service_guard = SCHEDULER_SERVICE.lock().await;
    let service = service_guard
        .as_ref()
        .ok_or("Scheduler service not running")?
        .clone();
    drop(service_guard);

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
        let service = SchedulerService::new();
        assert_eq!(service.check_interval_secs, 60);
    }

    #[tokio::test]
    async fn test_running_tasks_tracking() {
        let service = SchedulerService::new();

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
}
