//! ProcessCaptureManager: orchestrator for managed processes.

// CheckpointDb/Connection removed — all persistence is via PG
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{error, info, warn};

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
}

impl ProcessCaptureManager {
    pub fn new(
        error_monitor: Arc<RwLock<Option<ErrorMonitorHandle>>>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        Self {
            processes: Arc::new(RwLock::new(HashMap::new())),
            error_monitor,
            app_handle,
        }
    }

    // SQLite database handle removed — session tracking is now PG-only.

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

        // Create a database session record (SQLite removed — PG handles this)
        let session_id = uuid::Uuid::new_v4().to_string();
        process.runtime.session_id = Some(session_id.clone());

        // Clone handles for the event loop
        let processes_ref = self.processes.clone();
        let error_monitor = self.error_monitor.clone();
        let app_handle = self.app_handle.clone();
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
        session_id: String,
    ) {
        // SQLite removed - no-op
    }

    /// Rebuild and restart a process: stop → run build command → start.
    pub async fn rebuild_and_restart_process(&self, id: &str) -> Result<(), String> {
        // 1. Get config and validate build_command exists and rebuild is enabled
        let (build_cmd, build_args, cwd, env, health_port, process_name) = {
            let processes = self.processes.read().await;
            let process = processes
                .get(id)
                .ok_or_else(|| format!("Process '{}' not found", id))?;
            if !process.config.rebuild_enabled {
                return Err("Rebuild is disabled for this process".to_string());
            }
            // Guard against concurrent rebuilds
            if process.runtime.state == ProcessState::Building {
                return Err("Process is already being rebuilt".to_string());
            }
            let bc = process
                .config
                .build_command
                .as_ref()
                .ok_or("No build command configured for this process")?
                .clone();
            let ba = process.config.build_args.clone();
            let c = process.config.cwd.clone();
            let e = process.config.env.clone();
            let hp = process.config.health_port;
            let name = process.config.name.clone();
            (bc, ba, c, e, hp, name)
        };

        // 2. Stop the process (idempotent — safe to call on already-stopped processes)
        let _ = self.stop_process(id).await;

        // Wait for port to be free
        if let Some(port) = health_port {
            if !health::wait_for_port_free(port, Duration::from_secs(10)).await {
                warn!("Port {} still occupied before rebuild", port);
            }
        }

        // 3. Set state to Building
        {
            let mut processes = self.processes.write().await;
            if let Some(p) = processes.get_mut(id) {
                p.runtime.state = ProcessState::Building;
                let status = p.status();
                self.emit_state_change(&status);
            }
        }

        // 4. Emit a separator line to the output buffer
        let separator = OutputLine {
            timestamp: chrono::Utc::now().to_rfc3339(),
            stream: OutputStream::Stdout,
            line: format!("━━━ Rebuilding {} ━━━", process_name),
        };
        {
            let processes = self.processes.read().await;
            if let Some(p) = processes.get(id) {
                p.push_output(separator.clone()).await;
            }
        }
        let _ = tauri::Emitter::emit(
            &self.app_handle,
            "process-output",
            serde_json::json!({
                "id": id,
                "line": separator,
            }),
        );

        // 5. Run the build command
        info!(
            "Running build command for '{}': {} {:?}",
            process_name, build_cmd, build_args
        );

        let build_result = self
            .run_build_command(id, &build_cmd, &build_args, &cwd, &env)
            .await;

        match build_result {
            Ok(()) => {
                // Check if stop was called during the build (state would no longer be Building)
                let was_cancelled = {
                    let processes = self.processes.read().await;
                    processes
                        .get(id)
                        .map(|p| p.runtime.state != ProcessState::Building)
                        .unwrap_or(true)
                };
                if was_cancelled {
                    info!("Build for '{}' completed but process was stopped during build — not starting", process_name);
                    return Ok(());
                }

                info!("Build succeeded for '{}'", process_name);
                let success_line = OutputLine {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    stream: OutputStream::Stdout,
                    line: "━━━ Build succeeded, starting process ━━━".to_string(),
                };
                {
                    let processes = self.processes.read().await;
                    if let Some(p) = processes.get(id) {
                        p.push_output(success_line.clone()).await;
                    }
                }
                let _ = tauri::Emitter::emit(
                    &self.app_handle,
                    "process-output",
                    serde_json::json!({
                        "id": id,
                        "line": success_line,
                    }),
                );

                // 6. Start the process
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.start_process(id).await
            }
            Err(e) => {
                error!("Build failed for '{}': {}", process_name, e);
                let fail_line = OutputLine {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    stream: OutputStream::Stderr,
                    line: format!("━━━ Build failed: {} ━━━", e),
                };
                {
                    let processes = self.processes.read().await;
                    if let Some(p) = processes.get(id) {
                        p.push_output(fail_line.clone()).await;
                    }
                }
                let _ = tauri::Emitter::emit(
                    &self.app_handle,
                    "process-output",
                    serde_json::json!({
                        "id": id,
                        "line": fail_line,
                    }),
                );

                // Set state to Failed
                {
                    let mut processes = self.processes.write().await;
                    if let Some(p) = processes.get_mut(id) {
                        p.runtime.state = ProcessState::Failed;
                        let status = p.status();
                        self.emit_state_change(&status);
                    }
                }
                Err(format!("Build failed: {}", e))
            }
        }
    }

    /// Run a build command and stream its output to the process's output buffer.
    async fn run_build_command(
        &self,
        process_id: &str,
        build_cmd: &str,
        build_args: &[String],
        cwd: &str,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<(), String> {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd;

        #[cfg(windows)]
        {
            let full_command = if build_args.is_empty() {
                build_cmd.to_string()
            } else {
                format!("{} {}", build_cmd, build_args.join(" "))
            };
            cmd = tokio::process::Command::new("cmd");
            cmd.args(["/C", &full_command]);

            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
use crate::database::CheckpointDb;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        #[cfg(not(windows))]
        {
            cmd = tokio::process::Command::new(build_cmd);
            cmd.args(build_args);
        }

        cmd.current_dir(cwd);
        cmd.envs(env);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn build command: {}", e))?;

        let stdout = child
            .stdout
            .take()
            .ok_or("Failed to capture build stdout")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("Failed to capture build stderr")?;

        let processes = self.processes.clone();
        let app_handle = self.app_handle.clone();

        // Stream stdout
        let pid_stdout = process_id.to_string();
        let procs_stdout = processes.clone();
        let ah_stdout = app_handle.clone();
        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let output_line = OutputLine {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    stream: OutputStream::Stdout,
                    line,
                };
                {
                    let procs = procs_stdout.read().await;
                    if let Some(p) = procs.get(&pid_stdout) {
                        p.push_output(output_line.clone()).await;
                    }
                }
                let _ = tauri::Emitter::emit(
                    &ah_stdout,
                    "process-output",
                    serde_json::json!({
                        "id": pid_stdout,
                        "line": output_line,
                    }),
                );
            }
        });

        // Stream stderr
        let pid_stderr = process_id.to_string();
        let procs_stderr = processes.clone();
        let ah_stderr = app_handle.clone();
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let output_line = OutputLine {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    stream: OutputStream::Stderr,
                    line,
                };
                {
                    let procs = procs_stderr.read().await;
                    if let Some(p) = procs.get(&pid_stderr) {
                        p.push_output(output_line.clone()).await;
                    }
                }
                let _ = tauri::Emitter::emit(
                    &ah_stderr,
                    "process-output",
                    serde_json::json!({
                        "id": pid_stderr,
                        "line": output_line,
                    }),
                );
            }
        });

        // Wait for the build command to finish (5 min timeout)
        let status = tokio::time::timeout(Duration::from_secs(300), child.wait())
            .await
            .map_err(|_| "Build timed out after 5 minutes".to_string())?
            .map_err(|e| format!("Build process error: {}", e))?;

        // Wait for output tasks to finish
        let _ = stdout_task.await;
        let _ = stderr_task.await;

        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "Build exited with code {}",
                status.code().unwrap_or(-1)
            ))
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
    app_handle: &tauri::AppHandle,
) {
    // SQLite removed - no-op
}
