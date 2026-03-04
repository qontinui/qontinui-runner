//! ClaudeSession - interactive bidirectional session with Claude CLI.
//!
//! Manages the lifecycle of a Claude CLI process using the stream-json protocol.
//! Supports multiple turns, user message queuing, and interrupt.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::Emitter;
use tracing::{debug, error, info, warn};

use crate::claude_protocol::request_id::next_request_id;
use crate::claude_protocol::types::{OutgoingControlRequest, UserInputMessage};
use crate::database::CheckpointDb;
use crate::findings::storage as finding_storage;
use crate::findings::{Finding, FindingParser, ParsedFinding};
use crate::mcp::shared::{emit_ai_output, AiSessionContext, FindingContext, ProgressContext};
use crate::workflow_state::{ParsedProgress, ProgressParser};

use super::dispatcher;
use super::state::{SessionState, SessionStateTracker};
use super::writer::StdinWriter;

#[cfg(target_os = "windows")]
use std::os::windows::io::AsRawHandle;

/// Maximum number of pending user messages in the queue.
const MAX_PENDING_MESSAGES: usize = 10;

/// An interactive Claude CLI session.
pub struct ClaudeSession {
    /// Session identifier (typically the task_run_id + phase suffix).
    session_id: String,
    /// State machine for session lifecycle.
    state_tracker: SessionStateTracker,
    /// Thread-safe stdin writer.
    stdin_writer: Arc<StdinWriter>,
    /// Queue of user messages waiting to be sent (when session is Processing).
    pending_messages: Arc<Mutex<VecDeque<String>>>,
    /// Accumulated text output from the session.
    accumulated_output: Arc<Mutex<String>>,
    /// Whether a user has sent at least one message (for autonomous/interactive switching).
    user_has_interacted: Arc<AtomicBool>,
    /// Child process ID (for stop/kill).
    child_pid: u32,
    /// Last activity timestamp (for timeout monitoring).
    last_activity: Arc<AtomicU64>,
    /// Whether the session has produced output.
    has_output: Arc<AtomicBool>,
    /// Sender to signal the heartbeat thread to stop.
    stop_heartbeat: Option<mpsc::Sender<()>>,
    /// Handle to the stdout reader thread.
    stdout_join: Mutex<Option<thread::JoinHandle<String>>>,
    /// Handle to the stderr reader thread.
    stderr_join: Mutex<Option<thread::JoinHandle<String>>>,
    /// Handle to the heartbeat thread.
    heartbeat_join: Mutex<Option<thread::JoinHandle<()>>>,
    /// Handle to the finding processor thread.
    finding_join: Mutex<Option<thread::JoinHandle<Vec<Finding>>>>,
    /// Handle to the progress processor thread.
    progress_join: Mutex<Option<thread::JoinHandle<u32>>>,
    /// Shared output buffer (for timeout recovery).
    shared_output_buf: Arc<Mutex<String>>,
    /// Raw stdout pipe handle (for Windows pipe close workaround).
    #[cfg(target_os = "windows")]
    stdout_raw_handle: Option<std::os::windows::io::RawHandle>,
    /// PID tracker for external stop support.
    pid_tracker: Option<Arc<Mutex<Vec<u32>>>>,
    /// Sender to signal completion to wait_for_result().
    completion_tx: Mutex<Option<mpsc::Sender<(bool, String)>>>,
    /// Tracks how many bytes of accumulated_output have been persisted to DB.
    persisted_output_len: Arc<AtomicUsize>,
    /// Sender for AI output deltas to persist to DB after each turn.
    turn_persist_tx: Option<mpsc::Sender<String>>,
}

// SAFETY: ClaudeSession contains a raw Windows handle (RawHandle = *mut c_void) for the stdout
// pipe close workaround. This handle is only used in close() to call CloseHandle(), which is
// thread-safe. All other fields are already Send+Sync (Arc<Mutex<...>>, atomics, mpsc senders).
unsafe impl Send for ClaudeSession {}
unsafe impl Sync for ClaudeSession {}

impl ClaudeSession {
    /// Spawn a new interactive Claude CLI session.
    ///
    /// Performs the initialization handshake (sends `initialize` control request,
    /// waits for control response) before returning.
    pub fn spawn(
        working_dir: &str,
        session_id: &str,
        app_handle: &tauri::AppHandle,
        session_ctx: Option<AiSessionContext>,
        finding_ctx: Option<FindingContext>,
        progress_ctx: Option<ProgressContext>,
        pid_tracker: Option<Arc<Mutex<Vec<u32>>>>,
    ) -> Result<Self, String> {
        info!(
            "Spawning interactive Claude session: {} in {}",
            session_id, working_dir
        );

        // Spawn CLI with stream-json input AND output
        let mut cmd = crate::process_helpers::cmd_no_window();
        cmd.args([
            "/c",
            "claude",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(working_dir)
        // Remove CLAUDECODE env var to prevent "nested session" detection.
        // The runner spawns Claude CLI as an automation tool, not as a nested session.
        .env_remove("CLAUDECODE")
        .env("QONTINUI_TRACE_ID", uuid::Uuid::new_v4().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn Claude CLI: {}", e))?;

        // Assign to Windows Job Object for crash safety (auto-kill on runner exit)
        #[cfg(target_os = "windows")]
        crate::job_object::assign_process_to_job(
            child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE
        );

        let child_pid = child.id();
        info!("Claude CLI spawned with PID {}", child_pid);

        // Register PID for external stop support
        if let Some(ref tracker) = pid_tracker {
            if let Ok(mut pids) = tracker.lock() {
                pids.push(child_pid);
                info!("Registered AI process PID {}", child_pid);
            }
        }

        // Take stdin, stdout, stderr
        let stdin = child
            .stdin
            .take()
            .ok_or("Failed to take stdin from Claude CLI")?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Save raw stdout handle for Windows pipe close workaround
        #[cfg(target_os = "windows")]
        let stdout_raw_handle = stdout.as_ref().map(|s| s.as_raw_handle());

        // Create shared state
        let state_tracker = SessionStateTracker::new();
        let stdin_writer = Arc::new(StdinWriter::new(stdin));
        let pending_messages: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let accumulated_output = Arc::new(Mutex::new(String::new()));
        let user_has_interacted = Arc::new(AtomicBool::new(false));
        let shared_output_buf = Arc::new(Mutex::new(String::new()));

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_activity = Arc::new(AtomicU64::new(now_secs));
        let has_output = Arc::new(AtomicBool::new(false));

        // Transition to Initializing
        state_tracker
            .transition(SessionState::Initializing)
            .map_err(|e| format!("State transition failed: {}", e))?;

        // Send initialize control request
        let init_request_id = next_request_id();
        let init_msg = OutgoingControlRequest::initialize(&init_request_id);
        stdin_writer
            .write_message(&init_msg)
            .map_err(|e| format!("Failed to send init request: {}", e))?;
        info!("Sent initialize request (id: {})", init_request_id);

        // AI output persistence tracking
        let persisted_output_len = Arc::new(AtomicUsize::new(0));

        // Create a channel for persisting AI output to DB after each turn.
        // The persister thread receives output deltas and writes them with
        // [AI_RESPONSE] markers via append_task_output_ex.
        let (turn_persist_tx, turn_persist_rx) = mpsc::channel::<String>();
        let turn_persist_tx_option = if session_ctx.is_some() {
            let persist_task_run_id = session_ctx
                .as_ref()
                .map(|c| c.task_run_id().to_string())
                .unwrap_or_default();

            thread::spawn(move || {
                // Open DB connection once for the lifetime of this thread
                let db = match CheckpointDb::new() {
                    Ok(db) => db,
                    Err(e) => {
                        warn!("Failed to open DB for AI output persistence: {}", e);
                        // Drain the channel so senders don't block
                        while turn_persist_rx.recv().is_ok() {}
                        return;
                    }
                };

                while let Ok(delta) = turn_persist_rx.recv() {
                    if delta.is_empty() {
                        continue;
                    }
                    let formatted = format!("\n[AI_RESPONSE]\n{}\n[/AI_RESPONSE]\n", delta);
                    if let Err(e) =
                        db.append_task_output_ex(&persist_task_run_id, &formatted, false, false)
                    {
                        warn!("Failed to persist AI response to output_log: {}", e);
                    } else {
                        debug!(
                            "Persisted AI response ({} chars) for task_run_id={}",
                            delta.len(),
                            persist_task_run_id
                        );
                    }
                }
                debug!("AI output persister thread exiting");
            });

            Some(turn_persist_tx)
        } else {
            // No session context — drop the receiver so channel is closed
            drop(turn_persist_rx);
            None
        };

        // Channels for findings and progress
        let (finding_tx, finding_rx) = mpsc::channel::<ParsedFinding>();
        let (progress_tx, progress_rx) = mpsc::channel::<ParsedProgress>();

        // Heartbeat thread
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let app_handle_heartbeat = app_handle.clone();
        let session_id_heartbeat = session_id.to_string();
        let session_ctx_heartbeat = session_ctx.clone();
        let has_output_heartbeat = has_output.clone();
        let start_time = Instant::now();

        let heartbeat_handle = thread::spawn(move || {
            let mut last_update = 0u64;
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));

                if has_output_heartbeat.load(Ordering::Relaxed) {
                    continue;
                }

                let elapsed_secs = start_time.elapsed().as_secs();
                if elapsed_secs > 0
                    && elapsed_secs.is_multiple_of(30)
                    && elapsed_secs != last_update
                {
                    last_update = elapsed_secs;
                    let mins = elapsed_secs / 60;
                    let secs = elapsed_secs % 60;
                    let msg = if mins > 0 {
                        format!(
                            "Session {} processing... ({}m {}s)",
                            session_id_heartbeat, mins, secs
                        )
                    } else {
                        format!("Session {} processing... ({}s)", session_id_heartbeat, secs)
                    };
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        emit_ai_output(
                            &app_handle_heartbeat,
                            &msg,
                            "status",
                            None,
                            session_ctx_heartbeat.as_ref(),
                        );
                    }));
                }
            }
        });

        // Stdout reader thread
        let state_for_stdout = state_tracker.clone();
        let writer_for_stdout = stdin_writer.clone();
        let pending_for_stdout = pending_messages.clone();
        let accumulated_for_stdout = accumulated_output.clone();
        let user_interacted_for_stdout = user_has_interacted.clone();
        let app_handle_stdout = app_handle.clone();
        let session_ctx_stdout = session_ctx.clone();
        let has_output_stdout = has_output.clone();
        let last_activity_stdout = last_activity.clone();
        let shared_output_for_thread = shared_output_buf.clone();
        let finding_ctx_for_stdout = finding_ctx.clone();
        let progress_ctx_for_stdout = progress_ctx.clone();
        let persist_tx_for_stdout = turn_persist_tx_option.clone();
        let persisted_len_for_stdout = persisted_output_len.clone();

        let stdout_handle = thread::spawn(move || {
            let mut all_text = String::new();
            let mut line_buffer = String::new();

            let mut finding_parser = if finding_ctx_for_stdout.is_some() {
                Some(FindingParser::new())
            } else {
                None
            };
            let mut progress_parser = if progress_ctx_for_stdout.is_some() {
                Some(ProgressParser::new())
            } else {
                None
            };

            if let Some(stdout) = stdout {
                info!("[STDOUT_READER] Thread started, reading lines from Claude CLI stdout");
                let reader = BufReader::new(stdout);
                let mut line_count = 0u64;
                for result in reader.lines() {
                    match result {
                        Ok(line) => {
                            line_count += 1;
                            let preview = crate::claude_protocol::codec::truncate_str(&line, 150);
                            info!("[STDOUT_READER] Line #{}: {}", line_count, preview);

                            // Update activity
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            last_activity_stdout.store(now, Ordering::Relaxed);

                            if let Some(text) = dispatcher::dispatch_line(
                                &line,
                                &app_handle_stdout,
                                session_ctx_stdout.as_ref(),
                                finding_parser.as_mut(),
                                progress_parser.as_mut(),
                                &finding_tx,
                                &progress_tx,
                                &mut line_buffer,
                                &state_for_stdout,
                                &writer_for_stdout,
                                &pending_for_stdout,
                                &accumulated_for_stdout,
                                &user_interacted_for_stdout,
                                &persist_tx_for_stdout,
                                &persisted_len_for_stdout,
                            ) {
                                has_output_stdout.store(true, Ordering::Relaxed);
                                all_text.push_str(&text);

                                // Sync to shared buffer periodically
                                if let Ok(mut buf) = shared_output_for_thread.lock() {
                                    *buf = all_text.clone();
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "[STDOUT_READER] Error reading line after {} lines: {}",
                                line_count, e
                            );
                            break;
                        }
                    }
                }
                info!(
                    "[STDOUT_READER] Loop ended after {} lines (EOF or error)",
                    line_count
                );
            } else {
                warn!("[STDOUT_READER] No stdout handle available!");
            }

            // Flush remaining line buffer
            dispatcher::flush_line_buffer(
                &line_buffer,
                finding_parser.as_mut(),
                progress_parser.as_mut(),
                &finding_tx,
                &progress_tx,
            );

            // Final sync
            if let Ok(mut buf) = shared_output_for_thread.lock() {
                *buf = all_text.clone();
            }

            all_text
        });

        // Finding processor thread (same pattern as existing code)
        let app_handle_findings = app_handle.clone();
        let finding_ctx_for_processor = finding_ctx.clone();
        let session_ctx_for_findings = session_ctx.clone();

        let finding_handle = thread::spawn(move || {
            let mut detected_findings: Vec<Finding> = Vec::new();

            if let Some(ctx) = finding_ctx_for_processor {
                let db = match CheckpointDb::new() {
                    Ok(db) => Some(db),
                    Err(e) => {
                        warn!("Failed to open database for finding storage: {}", e);
                        None
                    }
                };

                while let Ok(parsed_finding) = finding_rx.recv() {
                    info!(
                        "Detected finding: {} ({}:{})",
                        parsed_finding.title,
                        parsed_finding.category.as_str(),
                        parsed_finding.severity.as_str()
                    );

                    if let Some(ref db) = db {
                        let conn = match db.connection() {
                            Ok(c) => c,
                            Err(e) => {
                                warn!("Failed to get database connection: {}", e);
                                continue;
                            }
                        };

                        let is_resolved = parsed_finding.is_resolved;

                        match finding_storage::insert_finding(
                            &conn,
                            &ctx.task_run_id,
                            ctx.session_num,
                            &parsed_finding,
                        ) {
                            Ok(finding) => {
                                let event_name = if is_resolved {
                                    "finding_resolved"
                                } else {
                                    "finding_detected"
                                };

                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        if let Err(e) =
                                            app_handle_findings.emit(event_name, &finding)
                                        {
                                            warn!("Failed to emit {} event: {}", event_name, e);
                                        }
                                        // Also broadcast to WebSocket clients
                                        if let Ok(json) = serde_json::to_value(&finding) {
                                            crate::event_system::broadcast_ws_notification(
                                                &app_handle_findings,
                                                event_name,
                                                &json,
                                            );
                                        }
                                    }));

                                let finding_msg = if is_resolved {
                                    format!(
                                        "Finding resolved: [{}:{}] {}",
                                        finding.category.as_str(),
                                        finding.severity.as_str(),
                                        finding.title
                                    )
                                } else {
                                    format!(
                                        "Finding detected: [{}:{}] {}",
                                        finding.category.as_str(),
                                        finding.severity.as_str(),
                                        finding.title
                                    )
                                };
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_findings,
                                            &finding_msg,
                                            "finding",
                                            None,
                                            session_ctx_for_findings.as_ref(),
                                        );
                                    }));

                                detected_findings.push(finding);
                            }
                            Err(e) => {
                                warn!("Failed to store finding: {}", e);
                            }
                        }
                    }
                }
            } else {
                while finding_rx.recv().is_ok() {}
            }

            detected_findings
        });

        // Progress processor thread (same pattern as existing code)
        let app_handle_progress = app_handle.clone();
        let progress_ctx_for_processor = progress_ctx.clone();
        let session_ctx_for_progress = session_ctx.clone();

        let progress_handle = thread::spawn(move || {
            let mut progress_count: u32 = 0;

            if let Some(ctx) = progress_ctx_for_processor {
                let db = match CheckpointDb::new() {
                    Ok(db) => Some(db),
                    Err(e) => {
                        warn!("Failed to open database for progress storage: {}", e);
                        None
                    }
                };

                while let Ok(parsed_progress) = progress_rx.recv() {
                    debug!(
                        "Detected progress: {} - {}/{}",
                        parsed_progress.marker_type,
                        parsed_progress.current,
                        parsed_progress
                            .total
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| "?".to_string())
                    );

                    if let Some(ref db) = db {
                        let is_step_complete = parsed_progress.marker_type
                            == crate::workflow_state::progress_markers::STEP_COMPLETE;
                        let data_json = if is_step_complete {
                            parsed_progress
                                .sub_step_id
                                .as_ref()
                                .map(|id| serde_json::json!({ "sub_step_id": id }).to_string())
                        } else {
                            None
                        };

                        match db.save_step_progress_marker(
                            &ctx.checkpoint_id,
                            &parsed_progress.marker_type,
                            parsed_progress.current,
                            parsed_progress.total,
                            parsed_progress.description.as_deref(),
                            data_json.as_deref(),
                        ) {
                            Ok(_) => {
                                progress_count += 1;

                                let progress_event = serde_json::json!({
                                    "checkpoint_id": ctx.checkpoint_id,
                                    "task_run_id": ctx.task_run_id,
                                    "marker_type": parsed_progress.marker_type,
                                    "current": parsed_progress.current,
                                    "total": parsed_progress.total,
                                    "percentage": parsed_progress.percentage(),
                                    "description": parsed_progress.description,
                                });

                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        if let Err(e) = app_handle_progress
                                            .emit("step_progress", &progress_event)
                                        {
                                            warn!("Failed to emit step_progress event: {}", e);
                                        }
                                        // Also broadcast to WebSocket clients (channel: "step-progress")
                                        crate::event_system::broadcast_ws_notification(
                                            &app_handle_progress,
                                            "step-progress",
                                            &progress_event,
                                        );
                                    }));

                                let progress_msg = if let Some(total) = parsed_progress.total {
                                    let pct = if total > 0 {
                                        (parsed_progress.current as f64 / total as f64 * 100.0)
                                            as u32
                                    } else {
                                        0
                                    };
                                    format!(
                                        "Progress: {}/{} ({}%) - {}",
                                        parsed_progress.current,
                                        total,
                                        pct,
                                        parsed_progress.marker_type
                                    )
                                } else {
                                    format!(
                                        "Progress: {} - {}",
                                        parsed_progress.current, parsed_progress.marker_type
                                    )
                                };

                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        emit_ai_output(
                                            &app_handle_progress,
                                            &progress_msg,
                                            "progress",
                                            None,
                                            session_ctx_for_progress.as_ref(),
                                        );
                                    }));
                            }
                            Err(e) => {
                                warn!("Failed to store progress marker: {}", e);
                            }
                        }
                    }
                }
            } else {
                while progress_rx.recv().is_ok() {}
            }

            progress_count
        });

        // Stderr reader thread
        let stderr_handle = thread::spawn(move || {
            let mut output = String::new();
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_string(&mut output);
            }
            output
        });

        // Spawn a background thread to wait for child process exit
        // and manage final cleanup / state transitions.
        let state_for_waiter = state_tracker.clone();
        let writer_for_waiter = stdin_writer.clone();
        let session_id_for_waiter = session_id.to_string();
        let pid_tracker_for_waiter = pid_tracker.clone();
        let child_pid_for_waiter = child_pid;

        thread::spawn(move || {
            // Wait for the child process to exit
            Self::wait_for_child(child, &session_id_for_waiter);

            // Remove PID from tracker
            if let Some(ref tracker) = pid_tracker_for_waiter {
                if let Ok(mut pids) = tracker.lock() {
                    pids.retain(|&p| p != child_pid_for_waiter);
                    info!("Unregistered AI process PID {}", child_pid_for_waiter);
                }
            }

            // Close stdin
            writer_for_waiter.close();

            // Transition to Closed
            state_for_waiter.force_close();
            info!(
                "Session {} process exited, state -> Closed",
                session_id_for_waiter
            );
        });

        // Wait briefly for the init handshake to complete
        // The dispatcher handles the ControlResponse and transitions to Ready
        let init_deadline = Instant::now() + Duration::from_secs(60);
        let mut stderr_handle = Some(stderr_handle);
        loop {
            let current_state = state_tracker.get();
            if current_state == SessionState::Ready {
                info!("Session {} initialization complete", session_id);
                break;
            }
            if current_state == SessionState::Closed {
                // Try to capture stderr for diagnostics
                let stderr_output = stderr_handle
                    .take()
                    .and_then(|h| h.join().ok())
                    .unwrap_or_default();
                let stderr_trimmed = stderr_output.trim();
                if stderr_trimmed.is_empty() {
                    return Err("Claude CLI exited during initialization".to_string());
                } else {
                    warn!("Claude CLI stderr: {}", stderr_trimmed);
                    return Err(format!(
                        "Claude CLI exited during initialization: {}",
                        stderr_trimmed
                    ));
                }
            }
            if Instant::now() > init_deadline {
                return Err("Initialization timeout (60s) - Claude CLI did not respond".to_string());
            }
            thread::sleep(Duration::from_millis(50));
        }
        let stderr_handle = stderr_handle.expect("stderr handle consumed unexpectedly");

        Ok(Self {
            session_id: session_id.to_string(),
            state_tracker,
            stdin_writer,
            pending_messages,
            accumulated_output,
            user_has_interacted,
            child_pid,
            last_activity,
            has_output,
            stop_heartbeat: Some(stop_tx),
            stdout_join: Mutex::new(Some(stdout_handle)),
            stderr_join: Mutex::new(Some(stderr_handle)),
            heartbeat_join: Mutex::new(Some(heartbeat_handle)),
            finding_join: Mutex::new(Some(finding_handle)),
            progress_join: Mutex::new(Some(progress_handle)),
            shared_output_buf,
            #[cfg(target_os = "windows")]
            stdout_raw_handle,
            pid_tracker,
            completion_tx: Mutex::new(None),
            persisted_output_len,
            turn_persist_tx: turn_persist_tx_option,
        })
    }

    /// Get the current session state.
    pub fn state(&self) -> SessionState {
        self.state_tracker.get()
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Get the child process PID.
    pub fn pid(&self) -> u32 {
        self.child_pid
    }

    /// Get the last-activity tracker Arc (for Doctor health monitoring).
    pub fn last_activity_tracker(&self) -> Arc<AtomicU64> {
        self.last_activity.clone()
    }

    /// Whether a user has interacted with this session.
    pub fn has_user_interacted(&self) -> bool {
        self.user_has_interacted.load(Ordering::Relaxed)
    }

    /// Send the initial prompt to start the first turn.
    /// Transitions from Ready to Processing.
    pub fn send_initial_prompt(&self, prompt: &str) -> Result<(), String> {
        let state = self.state_tracker.get();
        if state != SessionState::Ready {
            return Err(format!("Cannot send initial prompt in state: {}", state));
        }

        let user_msg = UserInputMessage::new(prompt, "default");
        self.stdin_writer.write_message(&user_msg)?;

        self.state_tracker
            .transition(SessionState::Processing)
            .map_err(|e| format!("State transition failed: {}", e))?;

        info!(
            "Sent initial prompt ({} chars), session {} -> Processing",
            prompt.len(),
            self.session_id
        );

        Ok(())
    }

    /// Send a user message. If Ready, sends immediately. If Processing, queues it.
    /// Returns true if sent immediately, false if queued.
    pub fn send_user_message(&self, message: &str) -> Result<bool, String> {
        let state = self.state_tracker.get();

        if !state.can_send_message() {
            return Err(format!("Cannot send message in state: {}", state));
        }

        if state == SessionState::Ready {
            // Send immediately
            let user_msg = UserInputMessage::new(message, "default");
            self.stdin_writer.write_message(&user_msg)?;
            self.user_has_interacted.store(true, Ordering::Relaxed);

            self.state_tracker
                .transition(SessionState::Processing)
                .map_err(|e| format!("State transition failed: {}", e))?;

            info!(
                "Sent user message immediately ({} chars), session {} -> Processing",
                message.len(),
                self.session_id
            );
            Ok(true)
        } else {
            // Queue the message (Processing state)
            let mut queue = self
                .pending_messages
                .lock()
                .map_err(|e| format!("Failed to lock message queue: {}", e))?;

            if queue.len() >= MAX_PENDING_MESSAGES {
                return Err(format!(
                    "Message queue full ({} messages). Wait for the current turn to complete.",
                    MAX_PENDING_MESSAGES
                ));
            }

            queue.push_back(message.to_string());
            info!(
                "Queued user message ({} chars, {} in queue)",
                message.len(),
                queue.len()
            );
            Ok(false)
        }
    }

    /// Send an interrupt request.
    pub fn interrupt(&self) -> Result<(), String> {
        let state = self.state_tracker.get();
        if !state.can_interrupt() {
            return Err(format!("Cannot interrupt in state: {}", state));
        }

        let request_id = next_request_id();
        let interrupt_msg = OutgoingControlRequest::interrupt(&request_id);
        self.stdin_writer.write_message(&interrupt_msg)?;

        self.state_tracker
            .transition(SessionState::Interrupting)
            .map_err(|e| format!("State transition failed: {}", e))?;

        info!(
            "Sent interrupt request (id: {}), session {} -> Interrupting",
            request_id, self.session_id
        );
        Ok(())
    }

    /// Get the accumulated output text.
    pub fn get_output(&self) -> String {
        self.accumulated_output
            .lock()
            .map(|buf| buf.clone())
            .unwrap_or_default()
    }

    /// Wait for the session to reach a terminal state.
    /// Returns (success, accumulated_output).
    pub fn wait_for_completion(&self, timeout: Option<Duration>) -> Result<(bool, String), String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut was_processing = self.state_tracker.get() == SessionState::Processing;

        loop {
            let state = self.state_tracker.get();
            if state == SessionState::Closed {
                let output = self.get_output();
                // Consider it successful if we got output
                let success = !output.is_empty();
                return Ok((success, output));
            }

            // In interactive mode, Claude CLI stays alive after sending a result
            // (state goes Processing -> Ready). If we were processing and now we're
            // Ready with no pending messages and no user interaction, close stdin to
            // signal Claude to exit. The child waiter thread will then set Closed.
            if was_processing && state == SessionState::Ready {
                let has_pending = self
                    .pending_messages
                    .lock()
                    .map(|q| !q.is_empty())
                    .unwrap_or(false);
                let user_interacted = self.user_has_interacted.load(Ordering::Relaxed);

                if !has_pending && !user_interacted {
                    info!(
                        "Session {} turn completed (Processing -> Ready), closing stdin to exit CLI",
                        self.session_id
                    );
                    self.stdin_writer.close();
                    // Reset flag and continue waiting for Closed state
                    was_processing = false;
                }
            }

            if state == SessionState::Processing {
                was_processing = true;
            }

            if let Some(deadline) = deadline {
                if Instant::now() > deadline {
                    warn!(
                        "Session {} wait timeout, current state: {}",
                        self.session_id, state
                    );
                    // Return what we have
                    let output = self
                        .shared_output_buf
                        .lock()
                        .map(|s| s.clone())
                        .unwrap_or_default();
                    return Ok((false, output));
                }
            }

            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Gracefully close the session.
    pub fn close(&self) -> Result<(), String> {
        info!("Closing session {}", self.session_id);

        // Stop heartbeat
        if let Some(ref tx) = self.stop_heartbeat {
            let _ = tx.send(());
        }

        // Flush any remaining unpersisted output before closing.
        // Set persisted_output_len to MAX first to prevent the dispatcher from
        // also sending a delta for the same content (race with stdout thread).
        if let Some(ref tx) = self.turn_persist_tx {
            if let Ok(buf) = self.accumulated_output.lock() {
                let persisted = self.persisted_output_len.swap(usize::MAX, Ordering::SeqCst);
                if buf.len() > persisted {
                    let delta = buf[persisted..].to_string();
                    if !delta.trim().is_empty() {
                        let _ = tx.send(delta);
                    }
                }
            }
            // Note: the sender is dropped when the ClaudeSession struct is dropped,
            // which terminates the persister thread after it processes pending messages.
        }

        // Close stdin (sends EOF to CLI)
        self.stdin_writer.close();

        // Force state to Closed
        self.state_tracker.force_close();

        // Close stdout pipe on Windows to unblock reader thread
        #[cfg(target_os = "windows")]
        if let Some(raw_handle) = self.stdout_raw_handle {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(
                    raw_handle as windows_sys::Win32::Foundation::HANDLE,
                );
            }
        }

        info!("Session {} closed", self.session_id);
        Ok(())
    }

    /// Internal: wait for child process to exit.
    fn wait_for_child(mut child: Child, session_id: &str) {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    info!("Session {} child process exited: {:?}", session_id, status);
                    return;
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    error!("Session {} error waiting for child: {}", session_id, e);
                    return;
                }
            }
        }
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        // Ensure cleanup on drop
        if self.state_tracker.get() != SessionState::Closed {
            debug!(
                "ClaudeSession::drop - forcing close for {}",
                self.session_id
            );
            let _ = self.close();
        }

        // Remove PID from tracker
        if let Some(ref tracker) = self.pid_tracker {
            if let Ok(mut pids) = tracker.lock() {
                pids.retain(|&p| p != self.child_pid);
            }
        }
    }
}

impl std::fmt::Debug for ClaudeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeSession")
            .field("session_id", &self.session_id)
            .field("state", &self.state_tracker.get())
            .field("pid", &self.child_pid)
            .field("user_interacted", &self.has_user_interacted())
            .finish()
    }
}
