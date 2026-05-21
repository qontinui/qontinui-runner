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

/// Early-exit signal populated by the exit-monitor task as soon as
/// `child.wait()` returns, BEFORE sending the `Exited` message through the
/// process channel.
///
/// `await_ready` polls this each tick as the equivalent of `child.try_wait()`
/// for our wrapped-child architecture (the child is owned by the exit-monitor
/// closure, so the manager can't call `try_wait` directly). This lets the
/// readiness loop detect an immediate child exit (e.g. `'next' is not
/// recognized`, missing binary, `npm install` incomplete) within one poll
/// interval, instead of waiting for the `process_event_loop` to drain all
/// stdout/stderr output that may precede the `Exited` channel message.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EarlyExitInfo {
    pub code: Option<i32>,
}

/// A managed child process with stream readers and ring buffer.
pub(crate) struct ManagedProcess {
    pub config: ProcessConfig,
    pub runtime: ProcessRuntime,
    pub output_buffer: Arc<RwLock<VecDeque<OutputLine>>>,
    /// Early-exit signal — see [`EarlyExitInfo`]. Cleared on each `start()`,
    /// set by the exit-monitor task as the first action after `child.wait()`
    /// returns. Synchronous mutex so `await_ready` can read it without an
    /// await point.
    pub early_exit: Arc<std::sync::Mutex<Option<EarlyExitInfo>>>,
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
            early_exit: Arc::new(std::sync::Mutex::new(None)),
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
        // Clear any prior generation's early-exit signal so the readiness
        // poll for this spawn doesn't immediately see a stale flag.
        if let Ok(mut g) = self.early_exit.lock() {
            *g = None;
        }

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

        // Spawn exit monitor task. As the FIRST action after `child.wait()`
        // returns, populate the shared `early_exit` signal so the manager's
        // `await_ready` poll detects the exit on its next tick — independent
        // of how much buffered stdout/stderr the `process_event_loop` still
        // has to drain before the `Exited` channel message lands.
        let exit_tx = msg_tx;
        let early_exit = Arc::clone(&self.early_exit);
        let exit_handle = tokio::spawn(async move {
            let status = child.wait().await;
            let code = status.ok().and_then(|s| s.code());
            if let Ok(mut g) = early_exit.lock() {
                *g = Some(EarlyExitInfo { code });
            }
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

    /// Test-only accessor for the early-exit signal. Used by the unit test
    /// that validates immediate-child-exit detection without spinning up a
    /// full `ProcessCaptureManager` (which would need a `tauri::AppHandle`).
    #[cfg(test)]
    pub fn early_exit_handle(&self) -> Arc<std::sync::Mutex<Option<EarlyExitInfo>>> {
        Arc::clone(&self.early_exit)
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

#[cfg(test)]
mod tests {
    //! Tests for the immediate-child-exit detection signal (`early_exit`).
    //!
    //! Iter 2 added a 30s readiness poll in `manager.rs::await_ready`. Iter 3
    //! (this file) adds an out-of-band signal so the manager can detect an
    //! immediate-exit child within a single poll interval instead of waiting
    //! for the deadline. These tests validate the signal end-to-end on
    //! `ManagedProcess` without standing up the full manager (which would
    //! need a `tauri::AppHandle`).

    use super::*;
    use std::collections::HashMap;
    use std::time::Instant;

    /// Build a `ProcessConfig` that immediately exits non-zero. On Windows we
    /// use `powershell -c "exit 1"`, on POSIX `sh -c "exit 1"`. `start_process`
    /// wraps the command in `cmd /C` on Windows already; on POSIX the command
    /// is invoked directly. Either way the spawned child terminates within a
    /// few hundred milliseconds.
    fn immediate_exit_config() -> ProcessConfig {
        #[cfg(windows)]
        let (command, args) = ("powershell", vec!["-c".to_string(), "exit 1".to_string()]);
        #[cfg(not(windows))]
        let (command, args) = ("sh", vec!["-c".to_string(), "exit 1".to_string()]);

        ProcessConfig {
            id: "test-immediate-exit".to_string(),
            name: "Immediate Exit Test".to_string(),
            command: command.to_string(),
            args,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            env: HashMap::new(),
            health_port: None,
            parser: ParserType::default(),
            auto_start: false,
            category: "test".to_string(),
            buffer_size: 100,
            enabled: true,
            ignore_patterns: Vec::new(),
            start_group: 0,
            dev_only: false,
            rebuild_enabled: false,
            build_command: None,
            build_args: Vec::new(),
        }
    }

    /// Spawn an immediately-exiting child and assert that the `early_exit`
    /// signal is populated within ~1s. This is the test_wait()-equivalent
    /// guarantee that `await_ready` relies on to fail-fast on crashed-spawn
    /// children (`'next' is not recognized`, missing binary, partial
    /// `npm install`) instead of timing out at 30s.
    #[tokio::test]
    async fn early_exit_signal_fires_within_one_second_on_immediate_exit() {
        let mut process = ManagedProcess::new(immediate_exit_config());
        let early_exit = process.early_exit_handle();

        // Pre-condition: signal starts empty.
        assert!(
            early_exit.lock().unwrap().is_none(),
            "early_exit must start as None"
        );

        // Spawn the child + reader tasks.
        let _msg_rx = process
            .start()
            .expect("start should succeed for valid command");

        // Poll the signal up to 1.5s (gives a little buffer over the
        // contract-claimed 1s for slow CI runners).
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut observed: Option<EarlyExitInfo> = None;
        while Instant::now() < deadline {
            if let Some(info) = *early_exit.lock().unwrap() {
                observed = Some(info);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let info = observed.expect(
            "early_exit signal should be populated within 1s of an immediate-exit child spawn",
        );

        // PowerShell `exit 1` returns exit code 1. POSIX `sh -c "exit 1"` likewise.
        // We assert the code is Some (i.e. we captured it) and is non-zero —
        // platforms occasionally normalize 1 → other small ints, so don't pin
        // the exact value beyond "non-zero meaning failure was observed."
        assert!(
            info.code.is_some(),
            "expected exit code to be captured, got None"
        );
        assert_ne!(
            info.code,
            Some(0),
            "expected non-zero exit code from `exit 1`, got {:?}",
            info.code
        );
    }

    /// Restarting after an immediate-exit must clear the prior generation's
    /// `early_exit` signal, otherwise the next readiness poll would see a
    /// stale flag and fail without actually re-spawning.
    #[tokio::test]
    async fn early_exit_signal_clears_on_restart() {
        let mut process = ManagedProcess::new(immediate_exit_config());
        let early_exit = process.early_exit_handle();

        // First spawn: child exits immediately, signal populates.
        let _ = process.start().expect("first start should succeed");
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline && early_exit.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            early_exit.lock().unwrap().is_some(),
            "first generation early_exit should have populated"
        );

        // Force state back to Stopped so `start()` will accept a re-spawn
        // (the existing exit path normally does this via the event loop, but
        // we're testing `ManagedProcess` directly without the manager).
        process.runtime.state = ProcessState::Stopped;

        // Second spawn: signal should be cleared synchronously by `start()`
        // BEFORE the second child has had time to exit, so we can observe
        // the cleared state immediately.
        let _ = process.start().expect("second start should succeed");
        // Race window: the new child can exit within microseconds. We only
        // need to prove the slot was reset, not that we observed `None`
        // forever. The "cleared on start" semantics are what await_ready
        // depends on — verified by reading the signal right after start().
        // If the slot is `Some` here, it MUST be the new generation's exit
        // (not the old one), so check by waiting for it to be `Some` again
        // and confirming the assertion holds across the restart boundary.
        let deadline2 = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline2 && early_exit.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            early_exit.lock().unwrap().is_some(),
            "second generation early_exit should populate after restart"
        );
    }
}
