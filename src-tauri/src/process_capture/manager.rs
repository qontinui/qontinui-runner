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

    /// Start all processes marked as auto_start, respecting start_group ordering.
    ///
    /// Processes are started in order of their `start_group` (lower first).
    /// Within a group, all processes start together. Between groups, the runner
    /// waits for all health ports in the current group to become ready before
    /// starting the next group.
    pub async fn start_auto_processes(&self) {
        // Collect auto-start processes grouped by start_group
        let groups: std::collections::BTreeMap<u32, Vec<(String, Option<u16>)>> = {
            let processes = self.processes.read().await;
            let mut map: std::collections::BTreeMap<u32, Vec<(String, Option<u16>)>> =
                std::collections::BTreeMap::new();
            for p in processes.values() {
                if p.config.auto_start && p.config.enabled {
                    map.entry(p.config.start_group)
                        .or_default()
                        .push((p.config.id.clone(), p.config.health_port));
                }
            }
            map
        };

        if groups.is_empty() {
            return;
        }

        let total_groups = groups.len();
        for (group_idx, (group, entries)) in groups.into_iter().enumerate() {
            info!(
                "Starting auto-start group {} ({} processes)",
                group,
                entries.len()
            );

            let mut health_ports: Vec<u16> = Vec::new();

            for (id, health_port) in &entries {
                if let Err(e) = self.start_process(id).await {
                    error!("Failed to auto-start process '{}': {}", id, e);
                }
                if let Some(port) = health_port {
                    health_ports.push(*port);
                }
            }

            // Wait for health ports before starting next group (skip for last group)
            if group_idx + 1 < total_groups && !health_ports.is_empty() {
                info!(
                    "Waiting for health ports {:?} in group {} before starting next group",
                    health_ports, group
                );
                let timeout = Duration::from_secs(60);
                for port in health_ports {
                    if !health::wait_for_port_ready(port, timeout).await {
                        warn!(
                            "Health port {} did not become ready within {}s, proceeding anyway",
                            port,
                            timeout.as_secs()
                        );
                    } else {
                        info!("Health port {} is ready", port);
                    }
                }
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

                                // Auto-discover specs once per process start
                                if !p.runtime.specs_discovered {
                                    p.runtime.specs_discovered = true;
                                    let discover_port = port;
                                    let discover_db = db.clone();
                                    let discover_app_handle = app_handle.clone();
                                    let discover_name = p.config.name.clone();
                                    tokio::spawn(async move {
                                        auto_discover_specs(
                                            discover_port,
                                            &discover_name,
                                            &discover_db,
                                            &discover_app_handle,
                                        )
                                        .await;
                                    });
                                }
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

/// Attempt to connect to a managed process's UI Bridge SDK and cache its specs.
/// This runs as a background task — failures are logged but not propagated.
async fn auto_discover_specs(
    port: u16,
    process_name: &str,
    db: &Arc<CheckpointDb>,
    app_handle: &tauri::AppHandle,
) {
    let base_url = format!("http://127.0.0.1:{}", port);
    info!(
        port,
        process_name, "Auto-discovering specs for managed process"
    );

    // Wait a bit for the app to fully initialize after health port is available
    tokio::time::sleep(Duration::from_secs(3)).await;

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("Auto-discover: failed to create HTTP client: {}", e);
            return;
        }
    };

    // Try to find the UI Bridge health endpoint to determine the base path
    let health_paths: &[(&str, &str)] = &[
        ("/api/ui-bridge/health", "/api/ui-bridge"),
        ("/ui-bridge/health", "/ui-bridge"),
        ("/health", ""),
    ];

    let mut base_path = String::new();
    let mut app_name = process_name.to_string();

    for (health_path, bp) in health_paths {
        let health_url = format!("{}{}", base_url, health_path);
        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    base_path = bp.to_string();
                    // Extract app name from health response
                    if let Some(name) = json
                        .get("uiBridge")
                        .or_else(|| json.get("data").and_then(|d| d.get("uiBridge")))
                        .and_then(|m| m.get("appName"))
                        .and_then(|v| v.as_str())
                    {
                        app_name = name.to_string();
                    }
                    break;
                }
            }
            _ => continue,
        }
    }

    // Try to fetch specs
    let specs_url = format!("{}{}/control/specs", base_url, base_path);
    let specs_response = match client.get(&specs_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(json) => json,
            Err(e) => {
                info!(
                    "Auto-discover: {} has no parseable specs endpoint: {}",
                    process_name, e
                );
                return;
            }
        },
        Ok(resp) => {
            info!(
                "Auto-discover: {} specs endpoint returned HTTP {}",
                process_name,
                resp.status()
            );
            return;
        }
        Err(_) => {
            // App doesn't have a specs endpoint — this is normal for most apps
            info!(
                "Auto-discover: {} does not expose a specs endpoint (normal)",
                process_name
            );
            return;
        }
    };

    // Parse specs from response
    let specs_data = specs_response.get("data").unwrap_or(&specs_response);
    let specs_array: Vec<serde_json::Value> = if let Some(arr) = specs_data.as_array() {
        arr.clone()
    } else if specs_data.get("groups").is_some() {
        vec![specs_data.clone()]
    } else {
        info!(
            "Auto-discover: {} returned no recognizable specs",
            process_name
        );
        return;
    };

    let mut cached_count = 0;
    for spec in &specs_array {
        let spec_id = spec
            .get("specId")
            .or_else(|| spec.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let page_url = spec
            .get("metadata")
            .and_then(|m| m.get("pageUrl"))
            .and_then(|v| v.as_str());

        let spec_json_str = match serde_json::to_string(spec) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if let Err(e) =
            db.upsert_cached_spec(&base_url, &app_name, spec_id, &spec_json_str, page_url)
        {
            warn!("Auto-discover: failed to cache spec {}: {}", spec_id, e);
            continue;
        }
        cached_count += 1;
    }

    if cached_count > 0 {
        info!(
            "Auto-discovered and cached {} specs for {} ({})",
            cached_count, process_name, base_url
        );

        // Emit event so the frontend can refresh
        let _ = tauri::Emitter::emit(
            app_handle,
            "specs-discovered",
            &serde_json::json!({
                "app_url": base_url,
                "app_name": app_name,
                "spec_count": cached_count,
            }),
        );
    }
}
