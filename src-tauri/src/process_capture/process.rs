//! Managed process: spawn, stop, stream reading.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::error_monitor::types::ErrorEvent;
use regex::Regex;

use super::health;
use super::stream_parser::StreamErrorParser;
use super::types::*;

/// Filter errors by removing any whose message or raw_entry matches an ignore pattern.
fn filter_ignored_errors(errors: Vec<ErrorEvent>, ignore_patterns: &[Regex]) -> Vec<ErrorEvent> {
    if ignore_patterns.is_empty() {
        return errors;
    }
    errors
        .into_iter()
        .filter(|e| {
            !ignore_patterns
                .iter()
                .any(|pat| pat.is_match(&e.message) || pat.is_match(&e.raw_entry))
        })
        .collect()
}

/// Message from reader tasks to the process owner.
pub(crate) enum ProcessMessage {
    /// A line of output was captured.
    OutputLine(OutputLine),
    /// Errors detected from stream parsing.
    Errors(Vec<ErrorEvent>),
    /// The child process exited.
    Exited { code: Option<i32> },
}

/// A managed child process with stream readers and ring buffer.
pub(crate) struct ManagedProcess {
    pub config: ProcessConfig,
    pub runtime: ProcessRuntime,
    pub output_buffer: Arc<RwLock<VecDeque<OutputLine>>>,
    /// Handles for spawned reader/monitor tasks.
    task_handles: Vec<tokio::task::JoinHandle<()>>,
    /// Health check task handle.
    health_task: Option<tokio::task::JoinHandle<()>>,
}

impl ManagedProcess {
    pub fn new(config: ProcessConfig) -> Self {
        let buffer_size = config.buffer_size;
        Self {
            config,
            runtime: ProcessRuntime::default(),
            output_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(buffer_size))),
            task_handles: Vec::new(),
            health_task: None,
        }
    }

    /// Start the process, spawning stream reader and exit monitor tasks.
    /// Returns a receiver for process messages (output lines, errors, exit).
    pub fn start(&mut self) -> Result<mpsc::Receiver<ProcessMessage>, String> {
        if self.runtime.state != ProcessState::Stopped
            && self.runtime.state != ProcessState::Failed
            && self.runtime.state != ProcessState::Building
        {
            return Err(format!(
                "Process '{}' is in state {}, cannot start",
                self.config.name,
                self.runtime.state.as_str()
            ));
        }

        self.runtime.state = ProcessState::Starting;
        // Reset port-health each spawn so a restarted process doesn't briefly
        // inherit the prior generation's `port_healthy` value. The manager's
        // port-health loop overwrites this within one poll interval.
        self.runtime.port_healthy = None;
        // Descendant tree and inner service PID are repopulated by the
        // post-spawn discovery task ~3s after Running.
        self.runtime.descendant_pids.clear();
        self.runtime.service_pid = None;

        let (msg_tx, msg_rx) = mpsc::channel::<ProcessMessage>(500);

        // Build and spawn the command
        let mut cmd = self.build_command();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn '{}': {}", self.config.name, e))?;

        let pid = child.id();
        info!("Spawned process '{}' (PID: {:?})", self.config.name, pid);

        // Assign the spawned child to the singleton Job Object so it (and
        // — via `KILL_ON_JOB_CLOSE` on the parent JobObject's lifetime —
        // its descendant tree) auto-terminates when the runner exits, even
        // on crash. Mirrors `claude_session/runner.rs::assign_process_to_job`
        // and `terminal/session.rs:522`. Best-effort: if the JobObject
        // failed to initialize at boot, this is a silent no-op.
        #[cfg(target_os = "windows")]
        {
            if let Some(handle) = child.raw_handle() {
                crate::job_object::assign_process_to_job(
                    handle as windows_sys::Win32::Foundation::HANDLE,
                );
            } else {
                tracing::warn!(
                    "Spawned process '{}' has no raw handle; skipping Job Object assignment",
                    self.config.name
                );
            }
        }

        self.runtime.pid = pid;
        self.runtime.state = ProcessState::Running;
        self.runtime.started_at = Some(std::time::Instant::now());

        // Take stdout and stderr for reading
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;

        // Compile ignore patterns once for both stdout/stderr tasks
        let compiled_ignore: Arc<Vec<Regex>> = Arc::new(
            self.config
                .ignore_patterns
                .iter()
                .filter_map(|p| match Regex::new(p) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        warn!("Invalid ignore pattern '{}': {}", p, e);
                        None
                    }
                })
                .collect(),
        );

        // Spawn stdout reader task
        let stdout_tx = msg_tx.clone();
        let source_name = self.config.name.clone();
        let parser_type = self.config.parser.clone();
        let ignore = Arc::clone(&compiled_ignore);
        let stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut parser = StreamErrorParser::new(&parser_type, &source_name);

            while let Ok(Some(line)) = lines.next_line().await {
                let output_line = OutputLine {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    stream: OutputStream::Stdout,
                    line: line.clone(),
                };
                let _ = stdout_tx
                    .send(ProcessMessage::OutputLine(output_line))
                    .await;

                let errors = filter_ignored_errors(parser.process_line(&line), &ignore);
                if !errors.is_empty() {
                    let _ = stdout_tx.send(ProcessMessage::Errors(errors)).await;
                }
            }

            let errors = filter_ignored_errors(parser.flush(), &ignore);
            if !errors.is_empty() {
                let _ = stdout_tx.send(ProcessMessage::Errors(errors)).await;
            }
        });

        // Spawn stderr reader task
        let stderr_tx = msg_tx.clone();
        let source_name = self.config.name.clone();
        let parser_type = self.config.parser.clone();
        let ignore = Arc::clone(&compiled_ignore);
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            let mut parser = StreamErrorParser::new(&parser_type, &source_name);

            while let Ok(Some(line)) = lines.next_line().await {
                let output_line = OutputLine {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    stream: OutputStream::Stderr,
                    line: line.clone(),
                };
                let _ = stderr_tx
                    .send(ProcessMessage::OutputLine(output_line))
                    .await;

                let errors = filter_ignored_errors(parser.process_line(&line), &ignore);
                if !errors.is_empty() {
                    let _ = stderr_tx.send(ProcessMessage::Errors(errors)).await;
                }
            }

            let errors = filter_ignored_errors(parser.flush(), &ignore);
            if !errors.is_empty() {
                let _ = stderr_tx.send(ProcessMessage::Errors(errors)).await;
            }
        });

        self.task_handles.push(stdout_handle);
        self.task_handles.push(stderr_handle);

        // Spawn exit monitor task
        let exit_tx = msg_tx;
        let exit_handle = tokio::spawn(async move {
            let status = child.wait().await;
            let code = status.ok().and_then(|s| s.code());
            let _ = exit_tx.send(ProcessMessage::Exited { code }).await;
        });
        self.task_handles.push(exit_handle);

        // Spawn health check task if port configured
        if let Some(_port) = self.config.health_port {
            // Health polling is handled by the manager's event loop
        }

        Ok(msg_rx)
    }

    /// Stop the process gracefully, then force kill if needed.
    pub async fn stop(&mut self) {
        if self.runtime.state == ProcessState::Stopped {
            return;
        }

        self.runtime.state = ProcessState::Stopping;
        info!("Stopping process '{}'", self.config.name);

        // Cancel health check task
        if let Some(handle) = self.health_task.take() {
            handle.abort();
        }

        // Kill the child process
        if let Some(pid) = self.runtime.pid {
            #[cfg(windows)]
            {
                // On Windows, use taskkill to kill the process tree
                let _ = crate::process_helpers::no_window("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }

            #[cfg(not(windows))]
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        }

        // Wait briefly for tasks to finish
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Abort remaining tasks
        for handle in self.task_handles.drain(..) {
            handle.abort();
        }

        // If health port is configured, ensure it's free
        if let Some(port) = self.config.health_port {
            if !health::wait_for_port_free(port, Duration::from_secs(5)).await {
                warn!(
                    "Port {} still in use after stopping '{}', attempting port kill",
                    port, self.config.name
                );
                health::kill_port_process(port).await;
            }
        }

        self.runtime.state = ProcessState::Stopped;
        self.runtime.pid = None;
        self.runtime.port_healthy = None;
        self.runtime.descendant_pids.clear();
        self.runtime.service_pid = None;

        info!("Process '{}' stopped", self.config.name);
    }

    /// Build the platform-appropriate command.
    fn build_command(&self) -> Command {
        let mut cmd;

        #[cfg(windows)]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;

            let full_command = if self.config.args.is_empty() {
                self.config.command.clone()
            } else {
                format!("{} {}", self.config.command, self.config.args.join(" "))
            };

            cmd = Command::new("cmd");
            cmd.args(["/C", &full_command]);
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }

        #[cfg(not(windows))]
        {
            cmd = crate::process_helpers::tokio_no_window(&self.config.command);
            cmd.args(&self.config.args);
            // Unix analog to the Windows Job Object KILL_ON_JOB_CLOSE: ask
            // the kernel to deliver SIGKILL to this child if our process
            // (the runner) dies. Combined with the descendant-tree reap on
            // graceful shutdown, this keeps orphan dev servers from
            // accumulating across crashes.
            #[cfg(target_os = "linux")]
            unsafe {
                use std::os::unix::process::CommandExt;
                cmd.pre_exec(|| {
                    // PR_SET_PDEATHSIG = 1; SIGKILL = 9.
                    let r = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                    if r != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        cmd.current_dir(&self.config.cwd);
        cmd.envs(&self.config.env);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        cmd
    }

    /// Push a line to the ring buffer, evicting old entries if at capacity.
    pub async fn push_output(&self, line: OutputLine) {
        let mut buf = self.output_buffer.write().await;
        if buf.len() >= self.config.buffer_size {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    /// Get the last N lines of output.
    pub async fn get_output(&self, tail: usize) -> Vec<OutputLine> {
        let buf = self.output_buffer.read().await;
        let start = if buf.len() > tail {
            buf.len() - tail
        } else {
            0
        };
        buf.iter().skip(start).cloned().collect()
    }

    /// Build a ProcessStatus from current runtime state.
    pub fn status(&self) -> ProcessStatus {
        ProcessStatus {
            id: self.config.id.clone(),
            name: self.config.name.clone(),
            state: self.runtime.state,
            pid: self.runtime.pid,
            uptime_secs: self.runtime.started_at.map(|s| s.elapsed().as_secs()),
            port_healthy: self.runtime.port_healthy,
            restart_count: self.runtime.restart_count,
            error_count: self.runtime.error_count,
            category: self.config.category.clone(),
            has_build_command: self.config.rebuild_enabled
                && self
                    .config
                    .build_command
                    .as_ref()
                    .is_some_and(|s| !s.is_empty()),
        }
    }
}
