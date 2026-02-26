//! Managed process: spawn, stop, stream reading.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::error_monitor::types::ErrorEvent;

use super::health;
use super::stream_parser::StreamErrorParser;
use super::types::*;

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
        if self.runtime.state != ProcessState::Stopped && self.runtime.state != ProcessState::Failed
        {
            return Err(format!(
                "Process '{}' is in state {}, cannot start",
                self.config.name, self.runtime.state
            ));
        }

        self.runtime.state = ProcessState::Starting;

        let (msg_tx, msg_rx) = mpsc::channel::<ProcessMessage>(500);

        // Build and spawn the command
        let mut cmd = self.build_command();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn '{}': {}", self.config.name, e))?;

        let pid = child.id();
        info!("Spawned process '{}' (PID: {:?})", self.config.name, pid);

        self.runtime.pid = pid;
        self.runtime.state = ProcessState::Running;
        self.runtime.started_at = Some(std::time::Instant::now());

        // Take stdout and stderr for reading
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;

        // Spawn stdout reader task
        let stdout_tx = msg_tx.clone();
        let source_name = self.config.name.clone();
        let parser_type = self.config.parser.clone();
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

                let errors = parser.process_line(&line);
                if !errors.is_empty() {
                    let _ = stdout_tx.send(ProcessMessage::Errors(errors)).await;
                }
            }

            let errors = parser.flush();
            if !errors.is_empty() {
                let _ = stdout_tx.send(ProcessMessage::Errors(errors)).await;
            }
        });

        // Spawn stderr reader task
        let stderr_tx = msg_tx.clone();
        let source_name = self.config.name.clone();
        let parser_type = self.config.parser.clone();
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

                let errors = parser.process_line(&line);
                if !errors.is_empty() {
                    let _ = stderr_tx.send(ProcessMessage::Errors(errors)).await;
                }
            }

            let errors = parser.flush();
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
                let _ = std::process::Command::new("taskkill")
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

        info!("Process '{}' stopped", self.config.name);
    }

    /// Build the platform-appropriate command.
    fn build_command(&self) -> Command {
        let mut cmd;

        #[cfg(windows)]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

            let full_command = if self.config.args.is_empty() {
                self.config.command.clone()
            } else {
                format!("{} {}", self.config.command, self.config.args.join(" "))
            };

            cmd = Command::new("cmd");
            cmd.args(["/C", &full_command]);
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        #[cfg(not(windows))]
        {
            cmd = Command::new(&self.config.command);
            cmd.args(&self.config.args);
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
        }
    }
}
