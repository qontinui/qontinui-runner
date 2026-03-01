//! Background TriggerService that manages watchers and processes events.
//!
//! Follows the same pattern as `scheduler_service.rs`: global instance,
//! AtomicBool stop signal, background tokio task.

use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{error, info, warn};

use crate::config_storage::ConfigStorage;
use crate::AppState;

use super::evaluator::TriggerEvaluator;
use super::executor::{self, TriggerExecutorDeps};
use super::storage;
use super::types::{TriggerEvent, TriggerHistoryEntry, TriggerSystemStatus, WorkflowTrigger};
use super::watchers;

// ============================================================================
// Service
// ============================================================================

/// Background service that manages trigger watchers and processes events.
pub struct TriggerService {
    /// Flag to stop the service
    stop_signal: Arc<AtomicBool>,
    /// Event channel sender (watchers send events here)
    event_tx: mpsc::Sender<TriggerEvent>,
    /// Event channel receiver (service processes events)
    event_rx: Mutex<Option<mpsc::Receiver<TriggerEvent>>>,
    /// Evaluator for debounce/throttle/conditions
    evaluator: Arc<TriggerEvaluator>,
    /// Dependencies for spawning workflows
    deps: TriggerExecutorDeps,
    /// Active watcher handles (for cleanup)
    watcher_handles: RwLock<Vec<tokio::task::JoinHandle<()>>>,
    /// Active notify watchers (must be kept alive)
    #[allow(dead_code)]
    file_watchers: RwLock<Vec<notify::RecommendedWatcher>>,
}

impl TriggerService {
    /// Create a new trigger service.
    pub fn new(deps: TriggerExecutorDeps) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);

        Self {
            stop_signal: Arc::new(AtomicBool::new(false)),
            event_tx,
            event_rx: Mutex::new(Some(event_rx)),
            evaluator: Arc::new(TriggerEvaluator::new()),
            deps,
            watcher_handles: RwLock::new(Vec::new()),
            file_watchers: RwLock::new(Vec::new()),
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

        // Abort watcher tasks
        let handles = self.watcher_handles.try_read();
        if let Ok(handles) = handles {
            for handle in handles.iter() {
                handle.abort();
            }
        }
    }

    /// Register watchers for all enabled triggers.
    async fn register_all_watchers(&self) -> Result<(), String> {
        let db = &self.deps.app_state.checkpoint_db;
        let triggers = storage::get_enabled_triggers(db)?;

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
    pub async fn register_watcher(&self, trigger: &WorkflowTrigger) -> Result<(), String> {
        let tx = self.event_tx.clone();
        let stop = self.stop_signal.clone();

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
                    stop,
                )?;

                let mut file_watchers = self.file_watchers.write().await;
                file_watchers.push(watcher);
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
                    stop,
                )?;

                let mut file_watchers = self.file_watchers.write().await;
                file_watchers.push(watcher);
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
                    stop,
                );

                let mut handles = self.watcher_handles.write().await;
                handles.push(handle);
                info!(
                    "Registered health check for trigger '{}' ({}s interval)",
                    trigger.name, check_interval_seconds
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
        let db = &self.deps.app_state.checkpoint_db;

        // Load the trigger definition
        let trigger = match storage::get_trigger(db, &event.trigger_id) {
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
            // Execute: spawn the workflow
            self.evaluator.record_execution_start(&trigger.id).await;

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
                        storage::record_trigger_fired(db, &trigger.id, Some(&execution_id))
                    {
                        error!("Failed to record trigger fired: {}", e);
                    }

                    let history = TriggerHistoryEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        trigger_id: trigger.id.clone(),
                        event_type: event.event_type.clone(),
                        event_data: event.event_data.clone(),
                        action: "executed".to_string(),
                        task_run_id: Some(execution_id.clone()),
                        error_message: None,
                        triggered_at: chrono::Utc::now().to_rfc3339(),
                    };

                    if let Err(e) = storage::record_history(db, &history) {
                        error!("Failed to record trigger history: {}", e);
                    }

                    info!(
                        "Trigger '{}' executed -> task_run: {}",
                        trigger.name, execution_id
                    );
                }
                Err(e) => {
                    error!("Trigger '{}' execution failed: {}", trigger.name, e);

                    let history = TriggerHistoryEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        trigger_id: trigger.id.clone(),
                        event_type: event.event_type.clone(),
                        event_data: event.event_data.clone(),
                        action: "error".to_string(),
                        task_run_id: None,
                        error_message: Some(e),
                        triggered_at: chrono::Utc::now().to_rfc3339(),
                    };

                    if let Err(e) = storage::record_history(db, &history) {
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

            if let Err(e) = storage::record_history(db, &history) {
                error!("Failed to record trigger history: {}", e);
            }
        }
    }

    /// Get overall system status.
    pub async fn get_status(&self) -> TriggerSystemStatus {
        let db = &self.deps.app_state.checkpoint_db;
        let (total, enabled) = storage::get_trigger_stats(db).unwrap_or((0, 0));
        let active_watchers = {
            let handles = self.watcher_handles.read().await;
            let file_watchers = self.file_watchers.read().await;
            (handles.len() + file_watchers.len()) as u64
        };
        let active_executions = self.evaluator.get_total_active().await;

        TriggerSystemStatus {
            running: !self.stop_signal.load(Ordering::SeqCst),
            total_triggers: total,
            enabled_triggers: enabled,
            active_watchers,
            active_executions,
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
