//! ProcessCaptureManager: orchestrator for managed processes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::database::CheckpointDb;
use crate::error_monitor::ErrorMonitorHandle;

/// Maximum output lines persisted per session (older lines are dropped)
const MAX_SESSION_OUTPUT_LINES: u32 = 50_000;

use super::health;
use super::process::{ManagedProcess, ProcessMessage};
use super::types::*;

/// Manages all spawned child processes.
pub struct ProcessCaptureManager {
    processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
    error_monitor: Arc<RwLock<Option<ErrorMonitorHandle>>>,
    app_handle: tauri::AppHandle,
    db: Arc<CheckpointDb>,
}

impl ProcessCaptureManager {
    pub fn new(
        error_monitor: Arc<RwLock<Option<ErrorMonitorHandle>>>,
        app_handle: tauri::AppHandle,
        db: Arc<CheckpointDb>,
    ) -> Self {
        Self {
            processes: Arc::new(RwLock::new(HashMap::new())),
            error_monitor,
            app_handle,
            db,
        }
    }

    /// Register a process config without starting it.
    pub async fn register(&self, config: ProcessConfig) {
        let id = config.id.clone();
        let process = ManagedProcess::new(config);
        self.processes.write().await.insert(id, process);
    }

    /// Start a managed process by ID.
    pub async fn start_process(&self, id: &str) -> Result<(), String> {
        // Take the process out to start it (need mutable access)
        let mut processes = self.processes.write().await;
        let process = processes
            .get_mut(id)
            .ok_or_else(|| format!("Process '{}' not found", id))?;

        let msg_rx = process.start()?;
        let process_id = id.to_string();
        let process_name = process.config.name.clone();
        let config_id = process.config.id.clone();
        let health_port = process.config.health_port;
        let output_buffer = process.output_buffer.clone();
        let buffer_size = process.config.buffer_size;

        // Create a database session record
        let session_id = uuid::Uuid::new_v4().to_string();
        if let Err(e) = self
            .db
            .create_process_session(&session_id, &config_id, &process_name)
        {
            warn!("Failed to create process session record: {}", e);
        }
        // Prune old sessions (keep 10)
        if let Err(e) = self.db.prune_old_process_sessions(&config_id, 10) {
            warn!("Failed to prune old sessions: {}", e);
        }
        process.runtime.session_id = Some(session_id.clone());

        // Clone handles for the event loop
        let processes_ref = self.processes.clone();
        let error_monitor = self.error_monitor.clone();
        let app_handle = self.app_handle.clone();
        let db = self.db.clone();

        // Spawn the event loop for this process
        tokio::spawn(async move {
            Self::process_event_loop(
                process_id,
                process_name,
                health_port,
                output_buffer,
                buffer_size,
                msg_rx,
                processes_ref,
                error_monitor,
                app_handle,
                db,
                session_id,
            )
            .await;
        });

        // Emit state change event
        let status = process.status();
        drop(processes);
        self.emit_state_change(&status);

        Ok(())
    }

    /// Stop a managed process by ID.
    pub async fn stop_process(&self, id: &str) -> Result<(), String> {
        let mut processes = self.processes.write().await;
        let process = processes
            .get_mut(id)
            .ok_or_else(|| format!("Process '{}' not found", id))?;

        process.stop().await;

        let status = process.status();
        drop(processes);
        self.emit_state_change(&status);

        Ok(())
    }

    /// Restart a process: stop → wait for port free → start.
    pub async fn restart_process(&self, id: &str) -> Result<(), String> {
        {
            let mut processes = self.processes.write().await;
            let process = processes
                .get_mut(id)
                .ok_or_else(|| format!("Process '{}' not found", id))?;

            process.stop().await;
            process.runtime.restart_count += 1;

            // Wait for port to be free if configured
            if let Some(port) = process.config.health_port {
                if !health::wait_for_port_free(port, Duration::from_secs(10)).await {
                    warn!("Port {} still occupied after restart stop phase", port);
                }
            }
        }

        // Small delay between stop and start
        tokio::time::sleep(Duration::from_millis(500)).await;

        self.start_process(id).await
    }

    /// Get status of all processes.
    pub async fn get_all_status(&self) -> Vec<ProcessStatus> {
        let processes = self.processes.read().await;
        processes.values().map(|p| p.status()).collect()
    }

    /// Get output from a specific process.
    pub async fn get_output(&self, id: &str, tail: usize) -> Result<Vec<OutputLine>, String> {
        let processes = self.processes.read().await;
        let process = processes
            .get(id)
            .ok_or_else(|| format!("Process '{}' not found", id))?;
        Ok(process.get_output(tail).await)
    }

    /// Start all processes marked as auto_start.
    pub async fn start_auto_processes(&self) {
        let ids: Vec<String> = {
            let processes = self.processes.read().await;
            processes
                .values()
                .filter(|p| p.config.auto_start && p.config.enabled)
                .map(|p| p.config.id.clone())
                .collect()
        };

        for id in ids {
            if let Err(e) = self.start_process(&id).await {
                error!("Failed to auto-start process '{}': {}", id, e);
            }
        }
    }

    /// Stop all running processes.
    pub async fn stop_all(&self) {
        let ids: Vec<String> = {
            let processes = self.processes.read().await;
            processes
                .values()
                .filter(|p| {
                    p.runtime.state != ProcessState::Stopped
                        && p.runtime.state != ProcessState::Failed
                })
                .map(|p| p.config.id.clone())
                .collect()
        };

        for id in ids {
            if let Err(e) = self.stop_process(&id).await {
                error!("Failed to stop process '{}': {}", id, e);
            }
        }
    }

    /// Get all process configs.
    pub async fn get_configs(&self) -> Vec<ProcessConfig> {
        let processes = self.processes.read().await;
        processes.values().map(|p| p.config.clone()).collect()
    }

    /// Remove a process config (must be stopped first).
    pub async fn remove_process(&self, id: &str) -> Result<(), String> {
        let mut processes = self.processes.write().await;
        let process = processes
            .get(id)
            .ok_or_else(|| format!("Process '{}' not found", id))?;

        if process.runtime.state != ProcessState::Stopped
            && process.runtime.state != ProcessState::Failed
        {
            return Err("Process must be stopped before removal".to_string());
        }

        processes.remove(id);
        Ok(())
    }

    /// Find a process ID by name (case-insensitive).
    pub async fn find_process_id_by_name(&self, name: &str) -> Option<String> {
        let processes = self.processes.read().await;
        let name_lower = name.to_lowercase();
        processes
            .values()
            .find(|p| p.config.name.to_lowercase() == name_lower)
            .map(|p| p.config.id.clone())
    }

    /// The event loop for a single process: handles output, errors, health, exit.
    async fn process_event_loop(
        process_id: String,
        process_name: String,
        health_port: Option<u16>,
        output_buffer: Arc<RwLock<std::collections::VecDeque<OutputLine>>>,
        buffer_size: usize,
        mut msg_rx: tokio::sync::mpsc::Receiver<ProcessMessage>,
        processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
        error_monitor: Arc<RwLock<Option<ErrorMonitorHandle>>>,
        app_handle: tauri::AppHandle,
        db: Arc<CheckpointDb>,
        session_id: String,
    ) {
        let mut health_interval = tokio::time::interval(Duration::from_secs(2));
        let mut prev_port_healthy: Option<bool> = None;

        // Batched output lines for DB persistence
        let mut pending_lines: Vec<(String, String, String)> = Vec::new();
        let mut flush_interval = tokio::time::interval(Duration::from_secs(5));
        // Skip the first immediate tick
        flush_interval.tick().await;

        loop {
            tokio::select! {
                // Process messages from reader tasks
                msg = msg_rx.recv() => {
                    match msg {
                        Some(ProcessMessage::OutputLine(line)) => {
                            // Push to ring buffer
                            {
                                let mut buf = output_buffer.write().await;
                                if buf.len() >= buffer_size {
                                    buf.pop_front();
                                }
                                buf.push_back(line.clone());
                            }

                            // Queue for DB persistence
                            let stream_str = match line.stream {
                                OutputStream::Stdout => "stdout",
                                OutputStream::Stderr => "stderr",
                            };
                            pending_lines.push((
                                line.timestamp.clone(),
                                stream_str.to_string(),
                                line.line.clone(),
                            ));

                            // Flush if batch is large enough
                            if pending_lines.len() >= 100 {
                                let batch = std::mem::take(&mut pending_lines);
                                let db_ref = db.clone();
                                let sid = session_id.clone();
                                tokio::task::spawn_blocking(move || {
                                    if let Err(e) = db_ref.insert_process_session_output(&sid, &batch) {
                                        warn!("Failed to persist process output batch: {}", e);
                                    }
                                });
                            }

                            // Emit Tauri event for real-time frontend updates
                            let _ = tauri::Emitter::emit(&app_handle, "process-output", serde_json::json!({
                                "id": process_id,
                                "line": line,
                            }));
                        }
                        Some(ProcessMessage::Errors(errors)) => {
                            if errors.is_empty() {
                                continue;
                            }

                            // Update error count
                            {
                                let mut procs = processes.write().await;
                                if let Some(p) = procs.get_mut(&process_id) {
                                    p.runtime.error_count += errors.len() as u32;
                                }
                            }

                            // Send to error monitor
                            let monitor_lock = error_monitor.read().await;
                            if let Some(ref handle) = *monitor_lock {
                                if let Err(e) = handle.ingest_stream_errors(
                                    process_name.clone(),
                                    errors,
                                ).await {
                                    warn!("Failed to ingest stream errors: {}", e);
                                }
                            }
                        }
                        Some(ProcessMessage::Exited { code }) => {
                            info!(
                                "Process '{}' exited with code {:?}",
                                process_name, code
                            );

                            // Flush remaining output lines to DB
                            if !pending_lines.is_empty() {
                                let batch = std::mem::take(&mut pending_lines);
                                let db_ref = db.clone();
                                let sid = session_id.clone();
                                // Use spawn_blocking for sync DB call
                                let _ = tokio::task::spawn_blocking(move || {
                                    if let Err(e) = db_ref.insert_process_session_output(&sid, &batch) {
                                        warn!("Failed to persist final process output: {}", e);
                                    }
                                }).await;
                            }

                            // Prune if session accumulated too many lines
                            {
                                let db_ref = db.clone();
                                let sid = session_id.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    if let Err(e) = db_ref.prune_session_output_lines(&sid, MAX_SESSION_OUTPUT_LINES) {
                                        warn!("Failed to prune session output: {}", e);
                                    }
                                }).await;
                            }

                            // Update session record
                            let error_count = {
                                let procs = processes.read().await;
                                procs.get(&process_id).map(|p| p.runtime.error_count).unwrap_or(0)
                            };
                            let state_str = if code == Some(0) { "stopped" } else { "failed" };
                            {
                                let db_ref = db.clone();
                                let sid = session_id.clone();
                                let state_s = state_str.to_string();
                                let _ = tokio::task::spawn_blocking(move || {
                                    if let Err(e) = db_ref.update_process_session(&sid, &state_s, code, error_count) {
                                        warn!("Failed to update process session: {}", e);
                                    }
                                }).await;
                            }

                            let mut procs = processes.write().await;
                            if let Some(p) = procs.get_mut(&process_id) {
                                p.runtime.state = if code == Some(0) {
                                    ProcessState::Stopped
                                } else {
                                    ProcessState::Failed
                                };
                                p.runtime.pid = None;
                                p.runtime.session_id = None;

                                let status = p.status();
                                let _ = tauri::Emitter::emit(
                                    &app_handle,
                                    "process-state-changed",
                                    &status,
                                );
                            }

                            break;
                        }
                        None => {
                            // All senders dropped, process ended
                            // Flush any remaining lines
                            if !pending_lines.is_empty() {
                                let batch = std::mem::take(&mut pending_lines);
                                let db_ref = db.clone();
                                let sid = session_id.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    if let Err(e) = db_ref.insert_process_session_output(&sid, &batch) {
                                        warn!("Failed to persist remaining process output: {}", e);
                                    }
                                }).await;
                            }
                            break;
                        }
                    }
                }

                // Periodic flush of buffered output lines to DB
                _ = flush_interval.tick() => {
                    if !pending_lines.is_empty() {
                        let batch = std::mem::take(&mut pending_lines);
                        let db_ref = db.clone();
                        let sid = session_id.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = db_ref.insert_process_session_output(&sid, &batch) {
                                warn!("Failed to persist process output batch (periodic): {}", e);
                            }
                        });
                    }
                }

                // Health check polling
                _ = health_interval.tick(), if health_port.is_some() => {
                    let port = health_port.unwrap();
                    let healthy = health::is_port_in_use(port);

                    if Some(healthy) != prev_port_healthy {
                        prev_port_healthy = Some(healthy);

                        let mut procs = processes.write().await;
                        if let Some(p) = procs.get_mut(&process_id) {
                            p.runtime.port_healthy = Some(healthy);

                            // Transition state based on health
                            if healthy && p.runtime.state == ProcessState::Running {
                                p.runtime.state = ProcessState::Healthy;
                            } else if !healthy && p.runtime.state == ProcessState::Healthy {
                                p.runtime.state = ProcessState::Running;
                            }

                            let status = p.status();
                            let _ = tauri::Emitter::emit(
                                &app_handle,
                                "process-state-changed",
                                &status,
                            );
                        }
                    }
                }
            }
        }
    }

    fn emit_state_change(&self, status: &ProcessStatus) {
        let _ = tauri::Emitter::emit(&self.app_handle, "process-state-changed", status);
    }
}
