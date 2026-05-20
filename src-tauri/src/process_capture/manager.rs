//! ProcessCaptureManager: orchestrator for managed processes.

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

    /// Register a process config without starting it.
    pub async fn register(&self, config: ProcessConfig) {
        let id = config.id.clone();
        let process = ManagedProcess::new(config);
        self.processes.write().await.insert(id, process);
    }

    /// Start a managed process by ID.
    pub async fn start_process(&self, id: &str) -> Result<(), String> {
        // Pre-flight: read health_port + state under a brief read lock so the
        // async port cleanup below doesn't hold the manager's write lock.
        let (health_port, current_state) = {
            let processes = self.processes.read().await;
            let target = processes
                .get(id)
                .ok_or_else(|| format!("Process '{}' not found", id))?;
            (target.config.health_port, target.runtime.state)
        };

        // Mirror the state-check in `ManagedProcess::start` so the port cleanup
        // below can't kill our own running process.
        if !matches!(
            current_state,
            ProcessState::Stopped | ProcessState::Failed | ProcessState::Building
        ) {
            return Err(format!(
                "Process '{}' is in state {}, cannot start",
                id,
                current_state.as_str()
            ));
        }

        // If the health port is already bound (e.g. an orphaned dev server from
        // a previous session), reclaim it before spawning. Symmetric with
        // stop_process, which also kills lingering port owners.
        if let Some(port) = health_port {
            if health::is_port_in_use(port) {
                warn!(
                    "Port {} is bound by another process before starting '{}'; killing port owner",
                    port, id
                );
                health::kill_port_process(port).await;
                if !health::wait_for_port_free(port, Duration::from_secs(5)).await {
                    return Err(format!(
                        "Port {} still in use after attempting to free it; cannot start process '{}'",
                        port, id
                    ));
                }
            }
        }

        // Take the process out to start it (need mutable access)
        let mut processes = self.processes.write().await;

        // Check for CWD + command conflicts with already-running processes.
        // Two configs pointing at the same directory with the same command
        // (e.g. duplicate scan + dev-service entries) would fight over the port.
        {
            let target = processes
                .get(id)
                .ok_or_else(|| format!("Process '{}' not found", id))?;
            let target_cwd = target.config.cwd.replace('\\', "/").to_lowercase();
            let target_cmd = target.config.command.to_lowercase();

            for (other_id, other) in processes.iter() {
                if other_id == id {
                    continue;
                }
                let is_active = matches!(
                    other.runtime.state,
                    ProcessState::Starting
                        | ProcessState::Running
                        | ProcessState::Healthy
                        | ProcessState::Building
                );
                if !is_active {
                    continue;
                }
                let other_cwd = other.config.cwd.replace('\\', "/").to_lowercase();
                let other_cmd = other.config.command.to_lowercase();
                if other_cwd == target_cwd && other_cmd == target_cmd {
                    return Err(format!(
                        "Another process ('{}') is already running in the same directory with the same command. \
                         Stop it first or remove the duplicate configuration.",
                        other.config.name
                    ));
                }
            }
        }

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

        // Create a database session record in PG (best-effort — DB may not be initialized)
        let session_id = uuid::Uuid::new_v4().to_string();
        process.runtime.session_id = Some(session_id.clone());

        if let Some(pg_db) = crate::database::pg::PgDb::try_global() {
            if let Err(e) = pg_db
                .create_process_session(&session_id, &config_id, &process_name)
                .await
            {
                warn!(
                    "Failed to create process_session row for '{}': {}",
                    process_name, e
                );
            }
        }

        // Clone handles for the event loop
        let processes_ref = self.processes.clone();
        let error_monitor = self.error_monitor.clone();
        let app_handle = self.app_handle.clone();
        let process_id_for_event_loop = process_id.clone();
        let process_name_for_event_loop = process_name.clone();
        // Spawn the event loop for this process
        tokio::spawn(async move {
            Self::process_event_loop(
                process_id_for_event_loop,
                process_name_for_event_loop,
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

        // Spawn the port-health polling loop if a health port is configured.
        // The outer spawned PID is a shell wrapper (`cmd → poetry → python → uvicorn`
        // on Windows); the actual service can die inside while the wrapper keeps
        // running. The event loop only sees `Exited` from the wrapper, so without
        // this probe `port_healthy` stays `None` forever and the Processes page
        // can't tell "alive but not serving" from "alive and healthy".
        if let Some(port) = health_port {
            let processes_ref = self.processes.clone();
            let app_handle = self.app_handle.clone();
            let process_id_for_health = process_id.clone();
            let process_name_for_health = process_name.clone();
            tokio::spawn(async move {
                Self::port_health_loop(
                    process_id_for_health,
                    process_name_for_health,
                    port,
                    processes_ref,
                    app_handle,
                )
                .await;
            });
        }

        // Spawn the descendant-tree discovery task. After ~3s the spawned
        // child has typically forked its full wrapper chain
        // (`cmd → poetry → python → uvicorn`); we enumerate it once so
        // Phase 2's port_health_loop can probe the inner service PID and
        // §3.5 can persist descendants for orphan reclaim.
        {
            let processes_ref = self.processes.clone();
            let app_handle = self.app_handle.clone();
            let process_id_for_tree = process_id.clone();
            let process_name_for_tree = process_name.clone();
            let health_port_for_tree = health_port;
            tokio::spawn(async move {
                Self::descendant_discovery_once(
                    process_id_for_tree,
                    process_name_for_tree,
                    health_port_for_tree,
                    processes_ref,
                    app_handle,
                )
                .await;
            });
        }

        // Emit state change event
        let status = process.status();
        drop(processes);
        self.emit_state_change(&status);

        // Poll for readiness: the child must stay alive and either reach
        // Running/Healthy (when no health port is configured the event-loop
        // sets Running immediately; with a port, port_health_loop flips to
        // Healthy on first successful probe). If the child exits before
        // readiness, surface the captured stderr so the operator gets a real
        // diagnostic instead of a silently-stopped tile.
        self.await_ready(id, Duration::from_secs(30)).await
    }

    /// Wait up to `timeout` for a process to reach a stable ready state.
    ///
    /// Readiness rules:
    /// - If the process configures a `health_port`, ready = state `Healthy`
    ///   (port_health_loop has flipped it). Polling `Running` alone is
    ///   insufficient because the shell wrapper / pre-fork child can be
    ///   "running" while the inner service is still importing or crashing.
    /// - If no health_port is configured, ready = state `Running` that
    ///   persists past a 1-second post-spawn settle window. A child that
    ///   crashes during import (e.g. `'next' is not recognized`) flips
    ///   through Running → Stopped within a few hundred ms; the settle
    ///   guards against returning Ok before the Exited event lands.
    ///
    /// Returns `Err` with the captured stderr tail if the child exits before
    /// readiness, or a "did not reach ready state in {timeout}s" message if
    /// the timeout fires.
    ///
    /// Polls every 200ms — long enough to amortise lock acquisition, short
    /// enough that a sub-second post-spawn crash is observed within a few
    /// ticks.
    async fn await_ready(&self, id: &str, timeout: Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        let poll = Duration::from_millis(200);
        let settle_window = Duration::from_secs(1);

        // Snapshot the health_port + spawn baseline up front so we know which
        // ready predicate to use for this iteration.
        let (has_health_port, spawn_started_at) = {
            let processes = self.processes.read().await;
            match processes.get(id) {
                Some(p) => (
                    p.config.health_port.is_some(),
                    p.runtime.started_at.unwrap_or_else(std::time::Instant::now),
                ),
                None => return Err(format!("Process '{}' not found", id)),
            }
        };

        loop {
            let (state, name) = {
                let processes = self.processes.read().await;
                match processes.get(id) {
                    Some(p) => (p.runtime.state, p.config.name.clone()),
                    None => return Err(format!("Process '{}' not found", id)),
                }
            };

            match state {
                ProcessState::Healthy => return Ok(()),
                ProcessState::Running => {
                    // No health_port → Running past the settle window is ready.
                    // With a health_port → keep polling until Healthy flips on.
                    if !has_health_port && spawn_started_at.elapsed() >= settle_window {
                        return Ok(());
                    }
                }
                ProcessState::Stopped | ProcessState::Failed => {
                    // Child died before readiness. Capture the stderr tail
                    // (last 50 lines) so the caller's HTTP error carries the
                    // actual crash reason instead of a generic "Stopped".
                    let stderr_tail = self.collect_stderr_tail(id, 50).await;
                    let snippet = if stderr_tail.is_empty() {
                        String::from("(no stderr captured)")
                    } else {
                        stderr_tail.join("\n")
                    };
                    return Err(format!(
                        "Process '{}' exited before reaching ready state: {}",
                        name, snippet
                    ));
                }
                _ => {}
            }

            if std::time::Instant::now() >= deadline {
                let stderr_tail = self.collect_stderr_tail(id, 50).await;
                let snippet = if stderr_tail.is_empty() {
                    format!(
                        "did not reach ready state in {}s (last state: {})",
                        timeout.as_secs(),
                        state.as_str()
                    )
                } else {
                    format!(
                        "did not reach ready state in {}s (last state: {}); stderr tail:\n{}",
                        timeout.as_secs(),
                        state.as_str(),
                        stderr_tail.join("\n")
                    )
                };
                return Err(snippet);
            }

            tokio::time::sleep(poll).await;
        }
    }

    /// Walk the process's output ring buffer and pull the last `n` stderr
    /// entries. Returns an empty vec if the process is gone.
    async fn collect_stderr_tail(&self, id: &str, n: usize) -> Vec<String> {
        let processes = self.processes.read().await;
        let Some(process) = processes.get(id) else {
            return Vec::new();
        };
        let buf = process.output_buffer.read().await;
        let mut stderr_lines: Vec<String> = buf
            .iter()
            .filter(|entry| matches!(entry.stream, OutputStream::Stderr))
            .map(|entry| entry.line.clone())
            .collect();
        if stderr_lines.len() > n {
            let drop = stderr_lines.len() - n;
            stderr_lines.drain(..drop);
        }
        stderr_lines
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
    ///
    /// Phase 5: when `QONTINUI_REAP_RESTART=1` is set, also walks the
    /// previously-tracked descendant tree and force-kills each PID. This
    /// catches the case where `stop()` (which calls `taskkill /F /T`)
    /// fails to take down a late-forked uvicorn worker — without the
    /// explicit reap, the next `start_process` fights the orphan for the
    /// health port and lands in the `Bound`-not-`Listen` half-bound state
    /// that triggered this whole plan.
    pub async fn restart_process(&self, id: &str) -> Result<(), String> {
        // Snapshot the descendant tree BEFORE `stop()` clears it.
        let descendant_pids: Vec<u32> = {
            let processes = self.processes.read().await;
            processes
                .get(id)
                .map(|p| p.runtime.descendant_pids.iter().map(|d| d.pid).collect())
                .unwrap_or_default()
        };

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

        // Reap any descendant PIDs `stop()` may have missed. Gated behind
        // an env flag for the first release cycle so the existing
        // port-kill fallback (in `start_process`) stays armed and the new
        // behavior is opt-in until validated in dev.
        if std::env::var("QONTINUI_REAP_RESTART")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
            && !descendant_pids.is_empty()
        {
            let reaped: usize = futures::future::join_all(
                descendant_pids
                    .iter()
                    .copied()
                    .map(|pid| async move { health::kill_descendant_tree(pid).await }),
            )
            .await
            .into_iter()
            .sum();
            info!(
                "restart_process '{}': reaped {} descendants from prior generation",
                id, reaped
            );
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

    /// Get the last `lines` log lines for a process, split into stdout/stderr
    /// arrays. Returns `(stdout, stderr, truncated)` where `truncated` is true
    /// if the live ring buffer holds more lines than fit in `lines` for either
    /// stream (i.e. the caller may have missed older entries).
    ///
    /// Implementation note: the underlying `output_buffer` is a single
    /// chronological ring buffer mixing both streams (each entry tags itself
    /// via `OutputStream`). We walk the buffer in order, partition by stream,
    /// then per-stream tail to `lines` so the caller gets the *most recent*
    /// `lines` entries from each side rather than the most recent `lines`
    /// total entries projected onto each stream.
    pub async fn get_logs(
        &self,
        id: &str,
        lines: usize,
    ) -> Result<(Vec<String>, Vec<String>, bool), String> {
        let processes = self.processes.read().await;
        let process = processes
            .get(id)
            .ok_or_else(|| format!("Process '{}' not found", id))?;
        let buf = process.output_buffer.read().await;
        let mut stdout: Vec<String> = Vec::new();
        let mut stderr: Vec<String> = Vec::new();
        for entry in buf.iter() {
            match entry.stream {
                OutputStream::Stdout => stdout.push(entry.line.clone()),
                OutputStream::Stderr => stderr.push(entry.line.clone()),
            }
        }
        let truncated = stdout.len() > lines || stderr.len() > lines;
        if stdout.len() > lines {
            let drop = stdout.len() - lines;
            stdout.drain(..drop);
        }
        if stderr.len() > lines {
            let drop = stderr.len() - lines;
            stderr.drain(..drop);
        }
        Ok((stdout, stderr, truncated))
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

    /// Resolve a process identifier (UUID, category slug, or name substring) to a
    /// canonical process id. Returns `None` if no unambiguous match exists.
    ///
    /// HTTP routes like `POST /processes/{id}/restart` accept any of these forms
    /// so callers can use the convenient slug (`frontend`, `backend`) instead of
    /// looking up the UUID first. Resolution order: exact id, exact category
    /// (single match), name substring (single match). Multi-match resolutions
    /// return `None` to avoid silently restarting the wrong service.
    pub async fn resolve_process_id(&self, input: &str) -> Option<String> {
        let processes = self.processes.read().await;
        let entries: Vec<(&str, &str, &str)> = processes
            .values()
            .map(|p| {
                (
                    p.config.id.as_str(),
                    p.config.name.as_str(),
                    p.config.category.as_str(),
                )
            })
            .collect();
        resolve_process_id_from_entries(&entries, input)
    }

    /// The event loop for a single process: handles output, errors, health, exit.
    ///
    /// Responsibilities:
    /// - Drain `msg_rx` (output lines, errors, exit notifications from reader tasks)
    /// - Push output lines to the in-memory ring buffer (live source of truth)
    /// - Emit Tauri `process-output` events for the frontend
    /// - Forward errors to the ErrorMonitor
    /// - Batch-write output lines to PG (`process_session_output`) so logs survive restarts
    /// - Update the `process_sessions` row on exit with stopped_at, state, exit_code
    async fn process_event_loop(
        process_id: String,
        process_name: String,
        _health_port: Option<u16>,
        output_buffer: Arc<RwLock<std::collections::VecDeque<OutputLine>>>,
        buffer_size: usize,
        mut msg_rx: tokio::sync::mpsc::Receiver<ProcessMessage>,
        processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
        error_monitor: Arc<RwLock<Option<ErrorMonitorHandle>>>,
        app_handle: tauri::AppHandle,
        session_id: String,
    ) {
        // DB write batching configuration
        const DB_BATCH_SIZE: usize = 50;
        const DB_FLUSH_INTERVAL: Duration = Duration::from_millis(500);

        let pg_db = crate::database::pg::PgDb::try_global();
        let mut db_batch: Vec<(String, String, String)> = Vec::with_capacity(DB_BATCH_SIZE);
        let mut error_count: u32 = 0;
        let mut exit_code: Option<i32> = None;
        let mut exited = false;

        let mut flush_interval = tokio::time::interval(DB_FLUSH_INTERVAL);
        flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Periodic flush of buffered DB writes
                _ = flush_interval.tick() => {
                    if !db_batch.is_empty() {
                        Self::flush_db_batch(&pg_db, &session_id, &mut db_batch).await;
                    }
                    if exited && db_batch.is_empty() {
                        break;
                    }
                }

                // Drain process messages
                msg = msg_rx.recv() => {
                    match msg {
                        Some(ProcessMessage::OutputLine(line)) => {
                            // 1. Push to ring buffer (live view source of truth)
                            {
                                let mut buf = output_buffer.write().await;
                                if buf.len() >= buffer_size {
                                    buf.pop_front();
                                }
                                buf.push_back(line.clone());
                            }

                            // 2. Emit Tauri event for live frontend updates
                            let _ = tauri::Emitter::emit(
                                &app_handle,
                                "process-output",
                                serde_json::json!({
                                    "id": &process_id,
                                    "line": &line,
                                }),
                            );

                            // 3. Buffer for batched DB write
                            let stream_str = match line.stream {
                                OutputStream::Stdout => "stdout",
                                OutputStream::Stderr => "stderr",
                            };
                            db_batch.push((line.timestamp, stream_str.to_string(), line.line));

                            if db_batch.len() >= DB_BATCH_SIZE {
                                Self::flush_db_batch(&pg_db, &session_id, &mut db_batch).await;
                            }
                        }

                        Some(ProcessMessage::Errors(errors)) => {
                            error_count = error_count.saturating_add(errors.len() as u32);

                            // Update runtime error_count on the process
                            {
                                let mut procs = processes.write().await;
                                if let Some(p) = procs.get_mut(&process_id) {
                                    p.runtime.error_count = p.runtime.error_count.saturating_add(errors.len() as u32);
                                }
                            }

                            // Forward to ErrorMonitor if available
                            let monitor_guard = error_monitor.read().await;
                            if let Some(monitor) = monitor_guard.as_ref() {
                                let _ = monitor
                                    .ingest_stream_errors(process_name.clone(), errors)
                                    .await;
                            }
                        }

                        Some(ProcessMessage::Exited { code }) => {
                            info!(
                                "Process '{}' exited with code {:?}",
                                process_name, code
                            );
                            exit_code = code;
                            exited = true;

                            // Update process state
                            {
                                let mut procs = processes.write().await;
                                if let Some(p) = procs.get_mut(&process_id) {
                                    p.runtime.state = ProcessState::Stopped;
                                    p.runtime.pid = None;
                                    p.runtime.port_healthy = None;
                                    p.runtime.descendant_pids.clear();
                                    p.runtime.service_pid = None;
                                    let status = p.status();
                                    let _ = tauri::Emitter::emit(
                                        &app_handle,
                                        "process-state-changed",
                                        &status,
                                    );
                                }
                            }
                            // Don't break yet — drain remaining messages first
                        }

                        None => {
                            // Channel closed: all senders dropped (process gone)
                            exited = true;
                        }
                    }

                    if exited && msg_rx.is_empty() && db_batch.is_empty() {
                        break;
                    }
                }
            }
        }

        // Final flush of any remaining buffered lines
        if !db_batch.is_empty() {
            Self::flush_db_batch(&pg_db, &session_id, &mut db_batch).await;
        }

        // Update the session row with final state
        if let Some(pg_db_arc) = &pg_db {
            let final_state = if exit_code.unwrap_or(0) == 0 {
                "stopped"
            } else {
                "failed"
            };
            if let Err(e) = pg_db_arc
                .update_process_session(&session_id, final_state, exit_code, error_count)
                .await
            {
                warn!(
                    "Failed to update process_session row for '{}': {}",
                    process_name, e
                );
            }
        }

        // Prune session output to keep DB size bounded
        if let Some(pg_db_arc) = &pg_db {
            if let Err(e) = pg_db_arc
                .prune_session_output_lines(&session_id, MAX_SESSION_OUTPUT_LINES)
                .await
            {
                warn!(
                    "Failed to prune session output for '{}': {}",
                    process_name, e
                );
            }
        }

        info!("Event loop ended for process '{}'", process_name);
    }

    /// Periodically probe a managed process's health port and update its
    /// `port_healthy` + `state` fields. Transitions `Running ↔ Healthy` based
    /// on the probe; emits `process-state-changed` only when something
    /// actually changes.
    ///
    /// Stops when the process leaves the `Running`/`Healthy` states (i.e. on
    /// stop, restart, exit, or rebuild). No explicit cancellation needed —
    /// the loop self-terminates on the next tick.
    async fn port_health_loop(
        process_id: String,
        process_name: String,
        port: u16,
        processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
        app_handle: tauri::AppHandle,
    ) {
        const POLL_INTERVAL: Duration = Duration::from_secs(3);

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            // Bail if the process is no longer in a state we should probe;
            // also snapshot the inner service PID so we can probe its
            // liveness this tick (cheap `OpenProcess`/`kill(_,0)` check).
            let service_pid = {
                let procs = processes.read().await;
                let Some(p) = procs.get(&process_id) else {
                    break;
                };
                if !matches!(
                    p.runtime.state,
                    ProcessState::Running | ProcessState::Healthy
                ) {
                    break;
                }
                p.runtime.service_pid
            };

            // Service-PID liveness branch. The outer `child.wait().await` in
            // process.rs only fires `Exited` for the cmd.exe shim; an inner
            // uvicorn worker that crashes on import leaves the shim alive,
            // so without this branch the runner reports `running` forever.
            if let Some(pid) = service_pid {
                let alive_check = tokio::task::spawn_blocking(move || health::pid_alive(pid)).await;
                let alive = matches!(alive_check, Ok(true));
                if !alive {
                    let emit = {
                        let mut procs = processes.write().await;
                        let Some(p) = procs.get_mut(&process_id) else {
                            break;
                        };
                        if !matches!(
                            p.runtime.state,
                            ProcessState::Running | ProcessState::Healthy
                        ) {
                            break;
                        }
                        p.runtime.state = ProcessState::Failed;
                        p.runtime.port_healthy = Some(false);
                        Some(p.status())
                    };
                    if let Some(status) = emit {
                        tracing::warn!(
                            "Service PID {} for '{}' is no longer alive (outer shim PID still tracked); marking Failed",
                            pid,
                            process_name
                        );
                        let _ = tauri::Emitter::emit(&app_handle, "process-state-changed", &status);
                    }
                    break;
                }
            }

            // TCP connect probe with internal 500ms timeout. Runs on a
            // blocking thread because `socket2::Socket::connect_timeout` is
            // synchronous; otherwise it would stall the tokio worker.
            let healthy =
                match tokio::task::spawn_blocking(move || health::is_port_in_use(port)).await {
                    Ok(v) => v,
                    Err(_) => continue,
                };

            // Apply the result + decide whether to emit.
            let new_status = {
                let mut procs = processes.write().await;
                let Some(p) = procs.get_mut(&process_id) else {
                    break;
                };
                // State may have flipped during the probe (race with stop()
                // or Exited). Don't clobber a terminal state with our update.
                if !matches!(
                    p.runtime.state,
                    ProcessState::Running | ProcessState::Healthy
                ) {
                    break;
                }

                let prev_state = p.runtime.state;
                let prev_healthy = p.runtime.port_healthy;
                p.runtime.port_healthy = Some(healthy);
                p.runtime.state = if healthy {
                    ProcessState::Healthy
                } else {
                    ProcessState::Running
                };

                if prev_state != p.runtime.state || prev_healthy != Some(healthy) {
                    Some(p.status())
                } else {
                    None
                }
            };

            if let Some(status) = new_status {
                let _ = tauri::Emitter::emit(&app_handle, "process-state-changed", &status);
            }
        }

        tracing::debug!(
            "Port-health loop ended for process '{}' (port {})",
            process_name,
            port
        );
    }

    /// One-shot descendant-tree discovery 3s after spawn.
    ///
    /// The spawned PID is the outer wrapper (`cmd.exe` on Windows). Within
    /// ~3s it has forked the full chain down to the service worker. We
    /// snapshot the descendant tree once, identify the inner service PID,
    /// and write both onto `ProcessRuntime` so Phase 2 can probe the
    /// service-PID directly and Phase 4 can persist the tracked set for
    /// orphan reclaim. If the process leaves `Running`/`Healthy` before the
    /// discovery query returns, we drop the result rather than clobber the
    /// terminal state.
    async fn descendant_discovery_once(
        process_id: String,
        process_name: String,
        health_port: Option<u16>,
        processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
        app_handle: tauri::AppHandle,
    ) {
        use super::process_tree;

        tokio::time::sleep(Duration::from_secs(3)).await;

        // Capture the spawned PID under a brief read lock; bail if state moved on.
        let spawned_pid = {
            let procs = processes.read().await;
            let Some(p) = procs.get(&process_id) else {
                return;
            };
            if !matches!(
                p.runtime.state,
                ProcessState::Running | ProcessState::Healthy
            ) {
                return;
            }
            match p.runtime.pid {
                Some(pid) => pid,
                None => return,
            }
        };

        let (descendants, parent_map) =
            process_tree::discover_descendants_with_parent_map(spawned_pid).await;
        let service_pid =
            process_tree::identify_service_pid(&descendants, &parent_map, health_port, spawned_pid);

        let emit_status = {
            let mut procs = processes.write().await;
            let Some(p) = procs.get_mut(&process_id) else {
                return;
            };
            // Re-check state — discovery query is async; the process may
            // have stopped/failed while we were waiting for PowerShell.
            if !matches!(
                p.runtime.state,
                ProcessState::Running | ProcessState::Healthy
            ) {
                return;
            }
            let changed = p.runtime.service_pid != service_pid
                || p.runtime.descendant_pids.len() != descendants.len();
            p.runtime.descendant_pids = descendants.clone();
            p.runtime.service_pid = service_pid;
            if changed {
                Some(p.status())
            } else {
                None
            }
        };

        info!(
            "Discovered {} descendants for '{}' (spawned_pid={}), service_pid={:?}",
            descendants.len(),
            process_name,
            spawned_pid,
            service_pid
        );

        // Persist for §3.5 startup orphan-reclaim. Phase 4 — record the
        // tracked root PID so a crashed-without-shutdown runner can find
        // and reap its prior generation's orphan trees on next boot.
        let root_entry = super::process_tree::PidWithSpawnedAt {
            pid: spawned_pid,
            spawned_at_unix: chrono::Utc::now().timestamp(),
        };
        let mut to_persist = Vec::with_capacity(descendants.len() + 1);
        to_persist.push(root_entry);
        to_persist.extend(descendants.iter().copied());
        super::orphan_state::record_tree(&process_id, &to_persist);

        if let Some(status) = emit_status {
            let _ = tauri::Emitter::emit(&app_handle, "process-state-changed", &status);
        }
    }

    /// Flush a batch of output lines to PG. Best-effort: failures are logged, batch is cleared.
    async fn flush_db_batch(
        pg_db: &Option<Arc<crate::database::pg::PgDb>>,
        session_id: &str,
        batch: &mut Vec<(String, String, String)>,
    ) {
        if batch.is_empty() {
            return;
        }
        if let Some(db) = pg_db {
            if let Err(e) = db.insert_process_session_output(session_id, batch).await {
                warn!("Failed to flush process output batch to PG: {}", e);
            }
        }
        batch.clear();
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

/// Pure resolution helper backing [`ProcessCaptureManager::resolve_process_id`].
///
/// Each entry is `(id, name, category)`. Match precedence:
/// 1. Exact id (case-sensitive — these are UUIDs)
/// 2. Exact category (case-insensitive, requires single match)
/// 3. Name substring (case-insensitive, requires single match)
fn resolve_process_id_from_entries(entries: &[(&str, &str, &str)], input: &str) -> Option<String> {
    if let Some((id, _, _)) = entries.iter().find(|(id, _, _)| *id == input) {
        return Some((*id).to_string());
    }
    let input_lower = input.to_lowercase();
    let category_matches: Vec<&(&str, &str, &str)> = entries
        .iter()
        .filter(|(_, _, cat)| cat.to_lowercase() == input_lower)
        .collect();
    if category_matches.len() == 1 {
        return Some(category_matches[0].0.to_string());
    }
    let name_matches: Vec<&(&str, &str, &str)> = entries
        .iter()
        .filter(|(_, name, _)| name.to_lowercase().contains(&input_lower))
        .collect();
    if name_matches.len() == 1 {
        return Some(name_matches[0].0.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::resolve_process_id_from_entries;

    fn fixture() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            ("uuid-frontend", "Frontend (Next.js)", "frontend"),
            ("uuid-backend", "FastAPI Backend", "backend"),
            (
                "uuid-embed",
                "Embedding Service (MiniLM-L6-v2)",
                "infrastructure",
            ),
        ]
    }

    #[test]
    fn exact_uuid_match_wins() {
        let e = fixture();
        assert_eq!(
            resolve_process_id_from_entries(&e, "uuid-frontend"),
            Some("uuid-frontend".to_string())
        );
    }

    #[test]
    fn category_slug_resolves_to_unique_match() {
        let e = fixture();
        assert_eq!(
            resolve_process_id_from_entries(&e, "frontend"),
            Some("uuid-frontend".to_string())
        );
        assert_eq!(
            resolve_process_id_from_entries(&e, "BACKEND"),
            Some("uuid-backend".to_string())
        );
    }

    #[test]
    fn name_substring_resolves_when_unique() {
        let e = fixture();
        assert_eq!(
            resolve_process_id_from_entries(&e, "fastapi"),
            Some("uuid-backend".to_string())
        );
        assert_eq!(
            resolve_process_id_from_entries(&e, "MiniLM"),
            Some("uuid-embed".to_string())
        );
    }

    #[test]
    fn no_match_returns_none() {
        let e = fixture();
        assert_eq!(resolve_process_id_from_entries(&e, "mobile"), None);
        assert_eq!(resolve_process_id_from_entries(&e, ""), None);
    }

    #[test]
    fn ambiguous_substring_returns_none() {
        // Two processes whose names share a substring (e.g., "service") must
        // refuse to resolve rather than restart the wrong one.
        let e = vec![
            ("uuid-a", "Auth Service", "backend"),
            ("uuid-b", "Notification Service", "backend"),
        ];
        assert_eq!(resolve_process_id_from_entries(&e, "service"), None);
        // But the category itself is also ambiguous (both are "backend"),
        // so the category lookup also returns None.
        assert_eq!(resolve_process_id_from_entries(&e, "backend"), None);
    }
}
