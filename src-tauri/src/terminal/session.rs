//! TerminalSession — PTY lifecycle management for a single terminal instance.
//!
//! Spawns a shell via `portable-pty`, manages reader/writer threads,
//! and emits Tauri events for output and exit.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter};
use tracing::{debug, info, warn};

use super::interceptor::OutputInterceptor;
use super::types::{TerminalExitEvent, TerminalId, TerminalInfo, TerminalOutputEvent};

/// Shell integration scripts embedded at compile time.
#[cfg(target_os = "windows")]
const PS1_INTEGRATION: &str = include_str!("../../resources/shell-integration.ps1");
#[cfg(not(target_os = "windows"))]
const BASH_INTEGRATION: &str = include_str!("../../resources/shell-integration.bash");

/// Write a shell integration script to a temp file, returning the path on success.
fn write_integration_script(content: &str, name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, content).ok()?;
    Some(path)
}

/// Maximum scrollback buffer capacity (1 MB).
const SCROLLBACK_CAPACITY: usize = 1_048_576;

/// A single PTY-backed terminal session.
pub struct TerminalSession {
    /// Unique identifier for this terminal.
    id: TerminalId,
    /// Display title.
    title: String,
    /// Working directory the shell was started in.
    working_dir: String,
    /// Which terminal page this session belongs to.
    page_id: String,
    /// Thread-safe writer to PTY stdin.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Handle to the PTY master (needed for resize).
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child process PID.
    child_pid: Option<u32>,
    /// Current terminal dimensions (atomic for lock-free resize from &self).
    cols: AtomicU16,
    rows: AtomicU16,
    /// Whether the shell process is still alive.
    is_alive: Arc<AtomicBool>,
    /// Exit code (set when process exits).
    exit_code: Arc<Mutex<Option<i32>>>,
    /// Handle to the reader thread (for join on cleanup).
    reader_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Handle to the waiter thread (for join on cleanup).
    waiter_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Bytes received by the frontend (for flow control).
    bytes_sent: Arc<AtomicU64>,
    bytes_acked: Arc<AtomicU64>,
    /// Ring buffer of recent raw PTY output for reconnection.
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    /// Monotonic counter of all bytes ever produced by the PTY.
    total_bytes_produced: Arc<AtomicU64>,
    /// Unix timestamp in milliseconds when the session was created.
    created_at: u64,
}

impl TerminalSession {
    /// Spawn a new terminal session with a shell process.
    pub fn spawn(
        id: TerminalId,
        title: String,
        working_dir: String,
        page_id: String,
        cols: u16,
        rows: u16,
        app_handle: AppHandle,
        interceptor: Arc<OutputInterceptor>,
    ) -> Result<Self, String> {
        let pty_system = native_pty_system();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to open PTY: {}", e))?;

        // Build shell command
        let mut cmd = Self::build_shell_command();

        // Set working directory
        let cwd = if working_dir.is_empty() {
            dirs::home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        } else {
            working_dir.clone()
        };
        cmd.cwd(&cwd);

        // Remove CLAUDECODE env var so Claude CLI works inside the terminal
        cmd.env_remove("CLAUDECODE");

        // Set TERM for proper color/capability support.
        // xterm.js is a full xterm-compatible terminal, so use xterm-256color on all
        // platforms. The previous "cygwin" setting on Windows caused issues with tools
        // like Claude Code that check TERM for capability detection.
        cmd.env("TERM", "xterm-256color");

        // Mark this terminal as running inside the Qontinui Runner so that tools
        // (e.g. Claude Code via the shell integration wrapper) can detect the context.
        cmd.env("QONTINUI_RUNNER_TERMINAL", "1");
        cmd.env(
            "QONTINUI_RUNNER_API_PORT",
            crate::mcp::types::get_mcp_api_port().to_string(),
        );

        // Spawn the child process
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("Failed to spawn shell: {}", e))?;

        let child_pid = child.process_id();
        info!(
            terminal_id = %id,
            pid = ?child_pid,
            cwd = %cwd,
            "Terminal session spawned"
        );

        // Assign to Windows Job Object for crash safety
        #[cfg(target_os = "windows")]
        if let Some(pid) = child_pid {
            Self::assign_to_job_object(pid);
        }

        // Get writer and master from the PTY pair
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("Failed to take PTY writer: {}", e))?;
        let writer = Arc::new(Mutex::new(writer));

        let is_alive = Arc::new(AtomicBool::new(true));
        let exit_code: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let bytes_sent = Arc::new(AtomicU64::new(0));
        let bytes_acked = Arc::new(AtomicU64::new(0));
        let scrollback_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(SCROLLBACK_CAPACITY)));
        let total_bytes_produced = Arc::new(AtomicU64::new(0));
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Get a reader from the master PTY
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("Failed to clone PTY reader: {}", e))?;

        // Spawn reader thread: reads PTY output → interceptor → scrollback + Tauri event
        let reader_id = id.clone();
        let reader_app = app_handle.clone();
        let reader_alive = is_alive.clone();
        let reader_bytes_sent = bytes_sent.clone();
        let reader_bytes_acked = bytes_acked.clone();
        let reader_scrollback = scrollback_buffer.clone();
        let reader_total_bytes = total_bytes_produced.clone();
        let reader_handle = thread::Builder::new()
            .name(format!("terminal-reader-{}", &id))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    if !reader_alive.load(Ordering::Relaxed) {
                        break;
                    }

                    // Flow control: pause if frontend is too far behind
                    let sent = reader_bytes_sent.load(Ordering::Relaxed);
                    let acked = reader_bytes_acked.load(Ordering::Relaxed);
                    if sent > acked + 1_048_576 {
                        // 1MB backpressure threshold
                        thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }

                    match reader.read(&mut buf) {
                        Ok(0) => {
                            debug!(terminal_id = %reader_id, "PTY reader got EOF");
                            break;
                        }
                        Ok(n) => {
                            let data = interceptor.process(&reader_id, &buf[..n]);

                            // Tee processed output into scrollback ring buffer
                            if let Ok(mut sb) = reader_scrollback.lock() {
                                for &byte in &data {
                                    if sb.len() >= SCROLLBACK_CAPACITY {
                                        sb.pop_front();
                                    }
                                    sb.push_back(byte);
                                }
                            }
                            reader_total_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);

                            let encoded = STANDARD.encode(&data);
                            let event = TerminalOutputEvent {
                                terminal_id: reader_id.clone(),
                                data: encoded,
                            };
                            if let Err(e) = reader_app.emit("terminal-output", &event) {
                                warn!(
                                    terminal_id = %reader_id,
                                    error = %e,
                                    "Failed to emit terminal output event"
                                );
                            }
                            reader_bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            // On Windows, the PTY reader returns an error when the child exits
                            debug!(terminal_id = %reader_id, error = %e, "PTY read error (likely process exit)");
                            break;
                        }
                    }
                }
                debug!(terminal_id = %reader_id, "Reader thread exiting");
            })
            .map_err(|e| format!("Failed to spawn reader thread: {}", e))?;

        // Spawn waiter thread: detects process exit
        let waiter_id = id.clone();
        let waiter_alive = is_alive.clone();
        let waiter_exit = exit_code.clone();
        let waiter_app = app_handle;
        let waiter_handle = thread::Builder::new()
            .name(format!("terminal-waiter-{}", &id))
            .spawn(move || {
                // portable-pty's child is not Send, so we must wait in the thread that has it
                let mut child = child;
                let status = child.wait();
                let code = match status {
                    Ok(exit) => {
                        // ExitStatus doesn't expose the code directly on all platforms
                        // via portable-pty. Use success() check.
                        if exit.success() {
                            Some(0)
                        } else {
                            // Try to get the exit code; fall back to 1 for non-zero
                            Some(1)
                        }
                    }
                    Err(e) => {
                        warn!(terminal_id = %waiter_id, error = %e, "Failed to wait on child process");
                        None
                    }
                };

                waiter_alive.store(false, Ordering::Relaxed);
                if let Ok(mut ec) = waiter_exit.lock() {
                    *ec = code;
                }

                info!(terminal_id = %waiter_id, exit_code = ?code, "Terminal process exited");

                let event = TerminalExitEvent {
                    terminal_id: waiter_id.clone(),
                    exit_code: code,
                };
                if let Err(e) = waiter_app.emit("terminal-exit", &event) {
                    warn!(
                        terminal_id = %waiter_id,
                        error = %e,
                        "Failed to emit terminal exit event"
                    );
                }
            })
            .map_err(|e| format!("Failed to spawn waiter thread: {}", e))?;

        // Store the master for resize operations
        let master: Box<dyn MasterPty + Send> = pair.master;

        Ok(Self {
            id,
            title,
            working_dir: cwd,
            page_id,
            writer,
            master: Arc::new(Mutex::new(master)),
            child_pid,
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            is_alive,
            exit_code,
            reader_join: Mutex::new(Some(reader_handle)),
            waiter_join: Mutex::new(Some(waiter_handle)),
            bytes_sent,
            bytes_acked,
            scrollback_buffer,
            total_bytes_produced,
            created_at,
        })
    }

    /// Build the platform-appropriate shell command, injecting shell integration if possible.
    fn build_shell_command() -> CommandBuilder {
        #[cfg(target_os = "windows")]
        {
            let mut cmd = CommandBuilder::new("powershell.exe");
            // Try to write and source the integration script. Fall back to plain -NoExit on failure.
            if let Some(script_path) =
                write_integration_script(PS1_INTEGRATION, "qontinui-shell-integration.ps1")
            {
                // -Command mode: dot-source the script then keep shell alive.
                // The script itself sources $PROFILE and sets up OSC 633 hooks.
                let source_cmd = format!(". '{}'", script_path.display());
                cmd.args(["-NoLogo", "-NoExit", "-Command", source_cmd.as_str()]);
            } else {
                cmd.args(["-NoLogo", "-NoExit"]);
            }
            cmd
        }
        #[cfg(not(target_os = "windows"))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let mut cmd = CommandBuilder::new(&shell);
            // Use --rcfile to source our integration script (which re-sources ~/.bashrc internally).
            // Fall back to --login if the script can't be written.
            if let Some(script_path) =
                write_integration_script(BASH_INTEGRATION, "qontinui-shell-integration.bash")
            {
                let rcfile = script_path.to_string_lossy().into_owned();
                cmd.arg("--rcfile");
                cmd.arg(rcfile.as_str());
            } else {
                cmd.arg("--login");
            }
            cmd
        }
    }

    /// Assign a process to the Windows Job Object for crash safety.
    #[cfg(target_os = "windows")]
    fn assign_to_job_object(pid: u32) {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

        unsafe {
            let handle = OpenProcess(PROCESS_ALL_ACCESS, 0, pid);
            if !handle.is_null() && !std::ptr::eq(handle, INVALID_HANDLE_VALUE as *mut _) {
                crate::job_object::assign_process_to_job(handle as _);
                CloseHandle(handle as _);
            }
        }
    }

    /// Write data (keystrokes) to the PTY stdin.
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| format!("Writer lock poisoned: {}", e))?;
        writer
            .write_all(data)
            .map_err(|e| format!("Failed to write to PTY: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("Failed to flush PTY: {}", e))?;
        Ok(())
    }

    /// Resize the PTY dimensions.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let master = self
            .master
            .lock()
            .map_err(|e| format!("Master lock poisoned: {}", e))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Failed to resize PTY: {}", e))?;
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        Ok(())
    }

    /// Acknowledge bytes received by the frontend (flow control).
    pub fn ack(&self, bytes: u64) {
        self.bytes_acked.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Get terminal info for the frontend.
    pub fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id.clone(),
            title: self.title.clone(),
            pid: self.child_pid,
            cols: self.cols.load(Ordering::Relaxed),
            rows: self.rows.load(Ordering::Relaxed),
            working_dir: self.working_dir.clone(),
            is_alive: self.is_alive.load(Ordering::Relaxed),
            exit_code: self.exit_code.lock().ok().and_then(|ec| *ec),
            created_at: self.created_at,
            total_bytes_produced: self.total_bytes_produced.load(Ordering::Relaxed),
            page_id: self.page_id.clone(),
        }
    }

    /// Get the scrollback buffer contents and the byte offset where the data starts.
    /// Returns `(data, start_offset)` where `start_offset = total_bytes_produced - data.len()`.
    pub fn get_scrollback_buffer(&self) -> (Vec<u8>, u64) {
        let total = self.total_bytes_produced.load(Ordering::Relaxed);
        let data = match self.scrollback_buffer.lock() {
            Ok(sb) => sb.iter().copied().collect::<Vec<u8>>(),
            Err(_) => Vec::new(),
        };
        let start_offset = total.saturating_sub(data.len() as u64);
        (data, start_offset)
    }

    /// Reset flow control counters so a reconnecting frontend doesn't hit backpressure.
    pub fn reset_flow_control(&self) {
        let sent = self.bytes_sent.load(Ordering::Relaxed);
        self.bytes_acked.store(sent, Ordering::Relaxed);
    }

    /// Check if the shell process is still alive.
    pub fn is_alive(&self) -> bool {
        self.is_alive.load(Ordering::Relaxed)
    }

    /// Kill the shell process and clean up threads.
    pub fn close(&self) {
        info!(terminal_id = %self.id, "Closing terminal session");
        self.is_alive.store(false, Ordering::Relaxed);

        // Kill the child process via PID if still alive
        if let Some(pid) = self.child_pid {
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
        }

        // Drop the writer to signal EOF on stdin
        if let Ok(mut writer) = self.writer.lock() {
            drop(writer.flush());
        }

        // Drop the master PTY handle — this closes the OS pipe and unblocks the
        // reader thread which may be stuck in a blocking read() call.
        if let Ok(mut master) = self.master.lock() {
            // Replace with a placeholder so the Drop actually runs now.
            // MasterPty is trait-object-boxed, so we swap it out.
            let _dropped = std::mem::replace(&mut *master, create_noop_master());
        }

        // Join threads with a timeout so we never hang the UI
        if let Ok(mut handle) = self.reader_join.lock() {
            if let Some(h) = handle.take() {
                join_with_timeout(h, "reader", &self.id);
            }
        }
        if let Ok(mut handle) = self.waiter_join.lock() {
            if let Some(h) = handle.take() {
                join_with_timeout(h, "waiter", &self.id);
            }
        }

        info!(terminal_id = %self.id, "Terminal session closed");
    }
}

/// Join a thread with a timeout, logging a warning if it doesn't finish in time.
fn join_with_timeout(handle: thread::JoinHandle<()>, name: &str, terminal_id: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_name = name.to_string();
    let tid = terminal_id.to_string();

    // Spawn a helper thread that joins and signals completion
    let _ = thread::Builder::new()
        .name(format!("join-{}-{}", thread_name, tid))
        .spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });

    // Wait up to 2 seconds
    if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
        warn!(
            terminal_id = %terminal_id,
            thread = %name,
            "Thread did not finish within 2s timeout — detaching"
        );
    }
}

/// Create a no-op MasterPty placeholder used when dropping the real master during close.
fn create_noop_master() -> Box<dyn MasterPty + Send> {
    Box::new(NoopMaster)
}

/// Minimal MasterPty that does nothing — used as a swap target during close().
struct NoopMaster;

impl MasterPty for NoopMaster {
    fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
        Ok(())
    }
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize {
            rows: 0,
            cols: 0,
            pixel_width: 0,
            pixel_height: 0,
        })
    }
    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }
    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }
    #[cfg(unix)]
    fn process_group_leader(&self) {}
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.is_alive.load(Ordering::Relaxed) {
            self.close();
        }
    }
}
