//! Background TriggerService that manages watchers and processes events.
//!
//! Follows the same pattern as `scheduler_service.rs`: global instance,
//! AtomicBool stop signal, background tokio task.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::AppState;

use super::evaluator::TriggerEvaluator;
use super::executor::{self, TriggerExecutorDeps};

use super::types::{TriggerEvent, TriggerHistoryEntry, TriggerSystemStatus, WorkflowTrigger};
use super::watchers;

// ============================================================================
// Service
// ============================================================================

/// Handle to a registered watcher, keyed by trigger ID.
enum WatcherHandle {
    /// Async task (schedule, health check)
    Task(tokio::task::JoinHandle<()>),
    /// File-system watcher (must be kept alive)
    FileWatcher(notify::RecommendedWatcher),
}

/// Background service that manages trigger watchers and processes events.
pub struct TriggerService {
    /// Flag to stop the service
    stop_signal: Arc<AtomicBool>,
    /// Per-watcher stop signals (so we can stop individual watchers)
    watcher_stop_signals: RwLock<HashMap<String, Arc<AtomicBool>>>,
    /// Event channel sender (watchers send events here)
    event_tx: mpsc::Sender<TriggerEvent>,
    /// Event channel receiver (service processes events)
    event_rx: Mutex<Option<mpsc::Receiver<TriggerEvent>>>,
    /// Evaluator for debounce/throttle/conditions
    evaluator: Arc<TriggerEvaluator>,
    /// Dependencies for spawning workflows
    deps: TriggerExecutorDeps,
    /// Active watcher handles keyed by trigger ID (for lifecycle management)
    watcher_handles: RwLock<HashMap<String, WatcherHandle>>,
    /// Count of dropped events (channel overflow)
    dropped_events: std::sync::atomic::AtomicU64,
}

impl TriggerService {
    /// Create a new trigger service.
    pub fn new(deps: TriggerExecutorDeps) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);

        Self {
            stop_signal: Arc::new(AtomicBool::new(false)),
            watcher_stop_signals: RwLock::new(HashMap::new()),
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            evaluator: Arc::new(TriggerEvaluator::new()),
            deps,
            watcher_handles: RwLock::new(HashMap::new()),
            dropped_events: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Get a clone of the event sender (for watchers to send events).
    pub fn event_sender(&self) -> mpsc::Sender<TriggerEvent> {
        self.event_tx.clone()
    }

    /// Get a reference to the evaluator.
    pub fn evaluator(&self) -> &Arc<TriggerEvaluator> {
        &self.evaluator
    }

    /// Start the service: register watchers and begin processing events.
    pub async fn start(&self) {
        info!("Starting trigger service");

        // Load enabled triggers and register watchers
        if let Err(e) = self.register_all_watchers().await {
            error!("Failed to register trigger watchers: {}", e);
        }

        // Subscribe to workflow event bus for reactive chain triggers.
        // When a workflow completes/fails, the event bus fires and we convert
        // the WorkflowEvent into TriggerEvents for matching WorkflowChain triggers.
        {
            let bus = crate::workflow_event_bus::get_workflow_event_bus();
            let mut event_rx = bus.subscribe_broadcast();
            let event_tx_clone = self.event_tx.clone();
            let pg_db = self.deps.app_state.pg_db.clone();
            let stop = self.stop_signal.clone();
            tokio::spawn(async move {
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match event_rx.recv().await {
                        Ok(event) => {
                            if event.name.starts_with("workflow.") {
                                let status = match event.name.as_str() {
                                    "workflow.completed" => "completed",
                                    "workflow.failed" => "failed",
                                    _ => continue,
                                };
                                let task_run_id = event
                                    .source
                                    .task_run_id
                                    .as_deref()
                                    .unwrap_or_default()
                                    .to_string();
                                let workflow_id = event
                                    .source
                                    .workflow_id
                                    .as_deref()
                                    .unwrap_or_default()
                                    .to_string();

                                if workflow_id.is_empty() {
                                    debug!(
                                        "Skipping chain trigger check: no workflow_id in event '{}'",
                                        event.name
                                    );
                                    continue;
                                }

                                // Delegate to the existing chain-matching logic, which
                                // loads triggers from DB, filters by source_workflow_id
                                // and on_status, and sends TriggerEvents through the channel.
                                super::watchers::workflow_chain::check_workflow_chains(
                                    &pg_db,
                                    &event_tx_clone,
                                    &workflow_id,
                                    &task_run_id,
                                    status,
                                    None,
                                )
                                .await;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                "TriggerService event bus subscriber lagged, missed {} events",
                                n
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("TriggerService: workflow event bus closed");
                            break;
                        }
                    }
                }
                debug!("TriggerService: event bus bridge task exiting");
            });
            info!("TriggerService: subscribed to workflow event bus for chain triggers");
        }

        // Take the receiver (can only be called once)
        let mut rx = match self.event_rx.lock().await.take() {
            Some(rx) => rx,
            None => {
                error!("TriggerService: event receiver already taken");
                return;
            }
        };

        // Process events until stopped
        while !self.stop_signal.load(Ordering::SeqCst) {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Some(event) => self.handle_event(event).await,
                        None => {
                            info!("TriggerService: event channel closed");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                    // Periodic check for stop signal
                }
            }
        }

        info!("Trigger service stopped");
    }

    /// Stop the service gracefully.
    pub fn stop(&self) {
        info!("Stopping trigger service");
        self.stop_signal.store(true, Ordering::SeqCst);

        // Signal all individual watchers to stop
        if let Ok(signals) = self.watcher_stop_signals.try_read() {
            for (id, signal) in signals.iter() {
                debug!("Signaling watcher '{}' to stop", id);
                signal.store(true, Ordering::SeqCst);
            }
        }

        // Abort watcher tasks
        if let Ok(handles) = self.watcher_handles.try_read() {
            for (id, handle) in handles.iter() {
                debug!("Aborting watcher handle for '{}'", id);
                if let WatcherHandle::Task(h) = handle {
                    h.abort();
                }
            }
        }
    }

    /// Unregister a watcher for a specific trigger.
    ///
    /// Stops the watcher task and removes it from the handle map.
    pub async fn unregister_watcher(&self, trigger_id: &str) {
        // Signal the watcher to stop
        {
            let mut signals = self.watcher_stop_signals.write().await;
            if let Some(signal) = signals.remove(trigger_id) {
                signal.store(true, Ordering::SeqCst);
            }
        }

        // Remove and drop the handle
        let mut handles = self.watcher_handles.write().await;
        if let Some(handle) = handles.remove(trigger_id) {
            match handle {
                WatcherHandle::Task(h) => {
                    h.abort();
                    info!("Unregistered async watcher for trigger '{}'", trigger_id);
                }
                WatcherHandle::FileWatcher(_w) => {
                    // Dropping the RecommendedWatcher stops it
                    info!("Unregistered file watcher for trigger '{}'", trigger_id);
                }
            }
        }

        // Clear evaluator state for this trigger
        self.evaluator.clear_trigger_state(trigger_id).await;
    }

    /// Get the number of dropped events (channel overflow).
    pub fn dropped_event_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Increment the dropped event counter.
    pub fn record_dropped_event(&self) {
        self.dropped_events.fetch_add(1, Ordering::Relaxed);
    }

    /// Register watchers for all enabled triggers.
    async fn register_all_watchers(&self) -> Result<(), String> {
        let triggers = self.deps.app_state.pg_db.get_enabled_triggers().await?;

        info!(
            "Registering watchers for {} enabled triggers",
            triggers.len()
        );

        for trigger in &triggers {
            if let Err(e) = self.register_watcher(trigger).await {
                warn!(
                    "Failed to register watcher for trigger '{}': {}",
                    trigger.name, e
                );
            }
        }

        Ok(())
    }

    /// Register a watcher for a single trigger.
    ///
    /// If a watcher is already registered for this trigger, it is unregistered first.
    pub async fn register_watcher(&self, trigger: &WorkflowTrigger) -> Result<(), String> {
        // Unregister existing watcher for this trigger (if any)
        self.unregister_watcher(&trigger.id).await;

        let tx = self.event_tx.clone();

        // Create a per-watcher stop signal (also responds to global stop)
        let watcher_stop = Arc::new(AtomicBool::new(false));
        {
            let mut signals = self.watcher_stop_signals.write().await;
            signals.insert(trigger.id.clone(), watcher_stop.clone());
        }

        match &trigger.trigger_config {
            super::types::TriggerConfig::FileWatch {
                paths,
                patterns,
                ignore_patterns,
                recursive,
            } => {
                let watcher = watchers::file_watcher::start_file_watcher(
                    trigger.id.clone(),
                    paths.clone(),
                    patterns.clone(),
                    ignore_patterns.clone(),
                    *recursive,
                    tx,
                    watcher_stop,
                )?;

                let mut handles = self.watcher_handles.write().await;
                handles.insert(trigger.id.clone(), WatcherHandle::FileWatcher(watcher));
                info!("Registered file watcher for trigger '{}'", trigger.name);
            }

            super::types::TriggerConfig::GitEvent {
                repo_path,
                events,
                branch_filter,
            } => {
                let watcher = watchers::git_watcher::start_git_watcher(
                    trigger.id.clone(),
                    repo_path.clone(),
                    events.clone(),
                    branch_filter.clone(),
                    tx,
                    watcher_stop,
                )?;

                let mut handles = self.watcher_handles.write().await;
                handles.insert(trigger.id.clone(), WatcherHandle::FileWatcher(watcher));
                info!("Registered git watcher for trigger '{}'", trigger.name);
            }

            super::types::TriggerConfig::HealthCheck {
                urls,
                check_interval_seconds,
                consecutive_failures,
            } => {
                let handle = watchers::health_check::start_health_check(
                    trigger.id.clone(),
                    urls.clone(),
                    *check_interval_seconds,
                    *consecutive_failures,
                    tx,
                    watcher_stop,
                );

                let mut handles = self.watcher_handles.write().await;
                handles.insert(trigger.id.clone(), WatcherHandle::Task(handle));
                info!(
                    "Registered health check for trigger '{}' ({}s interval)",
                    trigger.name, check_interval_seconds
                );
            }

            super::types::TriggerConfig::Schedule {
                cron_expression,
                timezone,
            } => {
                let handle = watchers::schedule::start_schedule(
                    trigger.id.clone(),
                    cron_expression.clone(),
                    timezone.clone(),
                    tx,
                    watcher_stop,
                )?;

                let mut handles = self.watcher_handles.write().await;
                handles.insert(trigger.id.clone(), WatcherHandle::Task(handle));
                info!(
                    "Registered schedule for trigger '{}' (cron: {}, tz: {})",
                    trigger.name, cron_expression, timezone
                );
            }

            super::types::TriggerConfig::WorkflowChain {
                source_workflow_id,
                on_status: _,
                pass_context: _,
            } => {
                // Workflow chain watchers are passive -- they listen for events
                // sent by the post-execution hook. No active watcher needed.
                info!(
                    "Registered workflow chain for trigger '{}' (source: {})",
                    trigger.name, source_workflow_id
                );
            }

            super::types::TriggerConfig::Webhook { .. } => {
                // Webhooks are passive -- they receive HTTP requests.
                // No active watcher needed.
                info!("Registered webhook endpoint for trigger '{}'", trigger.name);
            }
        }

        Ok(())
    }

    /// Handle an incoming trigger event.
    async fn handle_event(&self, event: TriggerEvent) {
        // Load the trigger definition
        let trigger = match self.deps.app_state.pg_db.get_trigger(&event.trigger_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                warn!("Trigger not found for event: {}", event.trigger_id);
                return;
            }
            Err(e) => {
                error!("Failed to load trigger {}: {}", event.trigger_id, e);
                return;
            }
        };

        // Evaluate debounce/throttle/conditions
        let eval_result = self.evaluator.evaluate(&trigger, &event).await;
        let action = eval_result.action_name().to_string();

        if eval_result.should_execute() {
            // Execute: spawn the workflow (with retry support)
            self.evaluator.record_execution_start(&trigger.id).await;

            let max_attempts = trigger.retry_count + 1; // retry_count=0 means 1 attempt
            let mut last_error: Option<String> = None;
            let mut succeeded = false;

            for attempt in 0..max_attempts {
                if attempt > 0 {
                    // Exponential backoff: base_delay * 2^(attempt-1)
                    let delay_secs = trigger.retry_delay_seconds * (1u64 << (attempt - 1));
                    info!(
                        "Trigger '{}' retry {}/{} after {}s delay",
                        trigger.name, attempt, trigger.retry_count, delay_secs
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)).await;

                    // Check stop signal between retries
                    if self.stop_signal.load(Ordering::SeqCst) {
                        info!(
                            "Trigger '{}' retries aborted: service stopping",
                            trigger.name
                        );
                        break;
                    }
                }

                let exec_result = executor::execute_triggered_workflow(
                    &self.deps,
                    &trigger.workflow_id,
                    trigger.workflow_overrides.as_ref(),
                    &event.variables,
                    &trigger.name,
                    event.chain_depth,
                );

                match exec_result {
                    Ok(execution_id) => {
                        // Record success
                        if let Err(e) =
                            self.deps.app_state.pg_db.record_trigger_fired(&trigger.id, Some(&execution_id)).await
                        {
                            error!("Failed to record trigger fired: {}", e);
                        }

                        let history = TriggerHistoryEntry {
                            id: uuid::Uuid::new_v4().to_string(),
                            trigger_id: trigger.id.clone(),
                            event_type: event.event_type.clone(),
                            event_data: event.event_data.clone(),
                            action: if attempt > 0 {
                                format!("executed (retry {})", attempt)
                            } else {
                                "executed".to_string()
                            },
                            task_run_id: Some(execution_id.clone()),
                            error_message: None,
                            triggered_at: chrono::Utc::now().to_rfc3339(),
                        };

                        if let Err(e) = self.deps.app_state.pg_db.record_trigger_history(&history).await {
                            error!("Failed to record trigger history: {}", e);
                        }

                        info!(
                            "Trigger '{}' executed -> task_run: {}{}",
                            trigger.name,
                            execution_id,
                            if attempt > 0 {
                                format!(" (after {} retries)", attempt)
                            } else {
                                String::new()
                            }
                        );
                        succeeded = true;
                        break;
                    }
                    Err(e) => {
                        warn!(
                            "Trigger '{}' execution failed (attempt {}/{}): {}",
                            trigger.name,
                            attempt + 1,
                            max_attempts,
                            e
                        );
                        last_error = Some(e);
                    }
                }
            }

            if !succeeded {
                if let Some(e) = last_error {
                    error!(
                        "Trigger '{}' execution failed after {} attempt(s): {}",
                        trigger.name, max_attempts, e
                    );

                    let history = TriggerHistoryEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        trigger_id: trigger.id.clone(),
                        event_type: event.event_type.clone(),
                        event_data: event.event_data.clone(),
                        action: "error".to_string(),
                        task_run_id: None,
                        error_message: Some(if max_attempts > 1 {
                            format!("Failed after {} attempts: {}", max_attempts, e)
                        } else {
                            e
                        }),
                        triggered_at: chrono::Utc::now().to_rfc3339(),
                    };

                    if let Err(e) = self.deps.app_state.pg_db.record_trigger_history(&history).await {
                        error!("Failed to record trigger history: {}", e);
                    }
                }
            }

            self.evaluator.record_execution_end(&trigger.id).await;
        } else {
            // Record non-execution in history
            let history = TriggerHistoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                trigger_id: trigger.id.clone(),
                event_type: event.event_type.clone(),
                event_data: event.event_data.clone(),
                action,
                task_run_id: None,
                error_message: None,
                triggered_at: chrono::Utc::now().to_rfc3339(),
            };

            if let Err(e) = self.deps.app_state.pg_db.record_trigger_history(&history).await {
                error!("Failed to record trigger history: {}", e);
            }
        }
    }

    /// Get overall system status.
    pub async fn get_status(&self) -> TriggerSystemStatus {
        let (total, enabled) = self.deps.app_state.pg_db.get_trigger_stats().await.unwrap_or((0, 0));
        let active_watchers = {
            let handles = self.watcher_handles.read().await;
            handles.len() as u64
        };
        let active_executions = self.evaluator.get_total_active().await;

        TriggerSystemStatus {
            running: !self.stop_signal.load(Ordering::SeqCst),
            total_triggers: total,
            enabled_triggers: enabled,
            active_watchers,
            active_executions,
            dropped_events: self.dropped_event_count(),
        }
    }
}

// ============================================================================
// Global Instance
// ============================================================================

static TRIGGER_SERVICE: Lazy<Mutex<Option<Arc<TriggerService>>>> = Lazy::new(|| Mutex::new(None));

/// Start the global trigger service.
pub async fn start_trigger_service(
    app_state: Arc<AppState>,
    config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    app_handle: tauri::AppHandle,
    pid_tracker: Arc<std::sync::Mutex<Vec<u32>>>,
) {
    let mut service_guard = TRIGGER_SERVICE.lock().await;

    if service_guard.is_some() {
        warn!("Trigger service already running");
        return;
    }

    let deps = TriggerExecutorDeps {
        app_state,
        config_storage,
        app_handle,
        pid_tracker,
    };

    let service = Arc::new(TriggerService::new(deps));
    *service_guard = Some(service.clone());
    drop(service_guard);

    tokio::spawn(async move {
        service.start().await;
    });

    info!("Trigger service started");
}

/// Stop the global trigger service.
pub async fn stop_trigger_service() {
    let mut service_guard = TRIGGER_SERVICE.lock().await;

    if let Some(service) = service_guard.take() {
        service.stop();
        info!("Trigger service stopped");
    }
}

/// Get the global trigger service (if running).
pub async fn get_trigger_service() -> Option<Arc<TriggerService>> {
    let service_guard = TRIGGER_SERVICE.lock().await;
    service_guard.clone()
}

/// Send an event to the trigger service.
pub async fn send_trigger_event(event: TriggerEvent) -> Result<(), String> {
    let service_guard = TRIGGER_SERVICE.lock().await;
    let service = service_guard
        .as_ref()
        .ok_or("Trigger service not running")?;

    service
        .event_sender()
        .send(event)
        .await
        .map_err(|e| format!("Failed to send trigger event: {}", e))
}
