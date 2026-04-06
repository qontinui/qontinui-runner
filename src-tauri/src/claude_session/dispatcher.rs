//! Message dispatcher for Claude CLI stdout output.
//!
//! Parses NDJSON lines from Claude CLI's stdout and routes them to the appropriate
//! handlers: text extraction, finding parsing, progress parsing, control request
//! handling, and state transitions.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use tauri::{Emitter, Manager};
use tracing::{debug, info, trace, warn};

use crate::claude_protocol::codec::decode_message;
use crate::claude_protocol::types::OutgoingControlResponse;
use crate::commands::ai_session::emit_session_state;
use crate::findings::{FindingParser, ParsedFinding};
use crate::mcp::shared::{emit_ai_output, AiSessionContext};
use crate::str_utils::truncate_str;
use crate::workflow_state::{ParsedProgress, ProgressParser};

use super::state::{SessionState, SessionStateTracker};
use super::writer::StdinWriter;

/// Emit a session state event if we have enough context.
fn emit_state_if_possible(
    app_handle: &tauri::AppHandle,
    session_ctx: Option<&AiSessionContext>,
    state: SessionState,
) {
    if let Some(ctx) = session_ctx {
        emit_session_state(app_handle, ctx.task_run_id(), &ctx.session_id, state);
    }
}

/// Configuration for the dispatcher.
pub struct DispatcherConfig {
    /// App handle for emitting events.
    pub app_handle: tauri::AppHandle,
    /// Session context for event emission.
    pub session_ctx: Option<AiSessionContext>,
    /// Whether to parse findings from output.
    pub parse_findings: bool,
    /// Whether to parse progress markers from output.
    pub parse_progress: bool,
}

/// Dispatcher result after processing all stdout.
pub struct DispatcherResult {
    /// All accumulated text output.
    pub all_text: String,
    /// Whether the last result was successful.
    pub success: bool,
}

/// Process a single NDJSON line from Claude CLI stdout.
///
/// This function handles:
/// - Text extraction and event emission
/// - Finding parsing
/// - Progress parsing
/// - Control request auto-approval
/// - State transitions on Result messages
///
/// Returns the extracted text (if any).
pub fn dispatch_line(
    line: &str,
    app_handle: &tauri::AppHandle,
    session_ctx: Option<&AiSessionContext>,
    mut finding_parser: Option<&mut FindingParser>,
    mut progress_parser: Option<&mut ProgressParser>,
    finding_tx: &Sender<ParsedFinding>,
    progress_tx: &Sender<ParsedProgress>,
    line_buffer: &mut String,
    state_tracker: &SessionStateTracker,
    stdin_writer: &Arc<StdinWriter>,
    pending_messages: &Arc<std::sync::Mutex<VecDeque<String>>>,
    accumulated_output: &Arc<std::sync::Mutex<String>>,
    user_has_interacted: &std::sync::atomic::AtomicBool,
    turn_persist_tx: &Option<Sender<String>>,
    persisted_output_len: &AtomicUsize,
    // Fallback session ID for file locking when session_ctx is None
    fallback_session_id: Option<&str>,
) -> Option<String> {
    // Decode the NDJSON line
    let msg = match decode_message(line) {
        Ok(m) => m,
        Err(e) => {
            debug!("Skipping non-parseable line: {}", e);
            return None;
        }
    };

    // Emit tool activity from assistant messages with tool_use blocks.
    // This is the primary path for tool activity in bypassPermissions mode,
    // where can_use_tool control requests are never sent by the CLI.
    if let Some((tool_name, data)) = msg.extract_tool_use() {
        let activity = format_tool_activity(tool_name, &data);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit_ai_output(app_handle, &activity, "tool_activity", None, session_ctx);
        }));
        // Auto-register files under active development when Edit/Write tools are used
        auto_register_file(app_handle, session_ctx, fallback_session_id, tool_name, &data);
    }

    // Also extract tool activity from content_block_start messages.
    // In stream-json mode, tool_use blocks arrive as content_block_start events
    // before (or instead of) full assistant messages with tool_use content blocks.
    if let Some((tool_name, data)) = msg.extract_tool_use_from_block_start() {
        let activity = format_tool_activity(&tool_name, &data);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit_ai_output(app_handle, &activity, "tool_activity", None, session_ctx);
        }));
        // Auto-register files under active development when Edit/Write tools are used
        auto_register_file(app_handle, session_ctx, &tool_name, &data);
    }

    // Handle control requests from CLI (auto-approve tool use in bypass mode)
    if let Some(ctrl_req) = msg.as_control_request() {
        // Emit tool activity event so the frontend can show what the AI is doing
        if ctrl_req.request.subtype == "can_use_tool" {
            if let Some(tool_name) = ctrl_req
                .request
                .data
                .get("tool_name")
                .and_then(|v| v.as_str())
            {
                let activity = format_tool_activity(tool_name, &ctrl_req.request.data);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    emit_ai_output(app_handle, &activity, "tool_activity", None, session_ctx);
                }));
            }
        }
        handle_control_request(ctrl_req, stdin_writer);
        return None;
    }

    // Handle control responses (to our init/interrupt requests)
    if msg.as_control_response().is_some() {
        debug!("Received control response from CLI");
        // If we're initializing, transition to Ready
        if state_tracker.get() == SessionState::Initializing {
            match state_tracker.transition(SessionState::Ready) {
                Ok(_) => {
                    info!("Session initialized, transitioning to Ready");
                    emit_state_if_possible(app_handle, session_ctx, SessionState::Ready);
                }
                Err(e) => warn!("Failed to transition to Ready: {}", e),
            }
        }
        return None;
    }

    // Handle result messages (turn completion)
    if msg.is_result() {
        let success = msg.is_success_result();
        info!("Received result message (success={})", success);

        // Transition state: Processing/Interrupting -> Ready
        let current = state_tracker.get();
        if current == SessionState::Processing || current == SessionState::Interrupting {
            match state_tracker.transition(SessionState::Ready) {
                Ok(_) => {
                    info!("Transitioned to Ready after result (was: {})", current);
                    emit_state_if_possible(app_handle, session_ctx, SessionState::Ready);
                }
                Err(e) => warn!("Failed to transition to Ready after result: {}", e),
            }
        } else {
            info!(
                "Result received but state is {} (not Processing/Interrupting), no transition",
                current
            );
        }

        // Persist the AI response delta to DB for chat session resilience.
        // This captures everything the AI said since the last persist point.
        if let Some(ref tx) = turn_persist_tx {
            if let Ok(mut buf) = accumulated_output.lock() {
                let persisted = persisted_output_len.load(Ordering::Relaxed);
                if buf.len() > persisted {
                    let delta = buf[persisted..].to_string();
                    if !delta.trim().is_empty() {
                        let _ = tx.send(delta);
                    }
                }
                // Drain buffer after persistence to prevent unbounded memory growth.
                // The full output is persisted to DB via turn_persist_tx, so the
                // in-memory copy is no longer needed.
                buf.clear();
                persisted_output_len.store(0, Ordering::Relaxed);
            }
        }

        // Check for pending user messages and send the next one
        send_next_pending_message(
            state_tracker,
            stdin_writer,
            pending_messages,
            user_has_interacted,
            app_handle,
            session_ctx,
        );

        // Skip text extraction for result messages — the text was already
        // emitted via streaming content_block_delta events. Extracting text
        // from the result would duplicate the entire response.
        return None;
    }

    // Extract text from the message
    let text = match msg.extract_text() {
        Some(t) if !t.is_empty() => t,
        _ => return None,
    };

    // Emit AI output event
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emit_ai_output(app_handle, &text, "claude", None, session_ctx);
    }));

    // Buffer text for line-based parsing (findings, progress)
    line_buffer.push_str(&text);

    // Process complete lines
    while let Some(newline_pos) = line_buffer.find('\n') {
        let complete_line = line_buffer[..newline_pos].to_string();
        *line_buffer = line_buffer[newline_pos + 1..].to_string();

        // Parse for findings
        if let Some(ref mut parser) = finding_parser {
            if let Some(parsed_finding) = parser.process_line(&complete_line) {
                let _ = finding_tx.send(parsed_finding);
            }
        }

        // Parse for progress markers
        if let Some(ref mut parser) = progress_parser {
            if let Some(parsed_progress) = parser.parse_line(&complete_line) {
                let _ = progress_tx.send(parsed_progress);
            }
        }
    }

    // Accumulate text in the shared output buffer
    if let Ok(mut buf) = accumulated_output.lock() {
        buf.push_str(&text);
    }

    Some(text)
}

/// Handle a control request from the CLI.
/// In bypass permissions mode, we auto-approve everything.
fn handle_control_request(
    ctrl_req: &crate::claude_protocol::types::CliControlRequest,
    stdin_writer: &Arc<StdinWriter>,
) {
    let subtype = &ctrl_req.request.subtype;
    debug!("CLI control request: subtype={}", subtype);

    if let Some(ref request_id) = ctrl_req.request_id {
        // Auto-approve tool use requests (we run in bypassPermissions mode)
        let response = OutgoingControlResponse::allow_tool(request_id);
        if let Err(e) = stdin_writer.write_message(&response) {
            warn!("Failed to send control response: {}", e);
        } else {
            trace!("Auto-approved control request: {}", subtype);
        }
    } else {
        warn!(
            "CLI control request without request_id, cannot respond: {}",
            subtype
        );
    }
}

/// Format a human-readable description of a tool activity.
///
/// Extracts the tool name and key details (file path, command, etc.)
/// to show what the AI is currently doing.
pub(crate) fn format_tool_activity(
    tool_name: &str,
    data: &serde_json::Map<String, serde_json::Value>,
) -> String {
    match tool_name {
        "Read" | "read" => {
            if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
                let short = short_path(path);
                format!("Reading {}", short)
            } else {
                "Reading file...".to_string()
            }
        }
        "Write" | "write" => {
            if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
                let short = short_path(path);
                format!("Writing {}", short)
            } else {
                "Writing file...".to_string()
            }
        }
        "Edit" | "edit" => {
            if let Some(path) = data.get("file_path").and_then(|v| v.as_str()) {
                let short = short_path(path);
                format!("Editing {}", short)
            } else {
                "Editing file...".to_string()
            }
        }
        "Bash" | "bash" => {
            if let Some(cmd) = data.get("command").and_then(|v| v.as_str()) {
                let short_cmd = if cmd.len() > 60 {
                    format!("{}...", truncate_str(cmd, 57))
                } else {
                    cmd.to_string()
                };
                format!("Running: {}", short_cmd)
            } else {
                "Running command...".to_string()
            }
        }
        "Glob" | "glob" => {
            if let Some(pattern) = data.get("pattern").and_then(|v| v.as_str()) {
                format!("Searching for {}", pattern)
            } else {
                "Searching files...".to_string()
            }
        }
        "Grep" | "grep" => {
            if let Some(pattern) = data.get("pattern").and_then(|v| v.as_str()) {
                let short = if pattern.len() > 40 {
                    format!("{}...", truncate_str(pattern, 37))
                } else {
                    pattern.to_string()
                };
                format!("Searching for \"{}\"", short)
            } else {
                "Searching code...".to_string()
            }
        }
        "WebFetch" | "WebSearch" => "Searching the web...".to_string(),
        "Task" => "Running subagent...".to_string(),
        _ => format!("Using {}...", tool_name),
    }
}

/// Acquire an exclusive file lock and register the file in the advisory registry.
///
/// When a session uses Edit or Write tools, this function:
/// 1. Acquires an exclusive file lock — BLOCKS if another session holds it.
///    Blocking the stdout reader thread creates backpressure that pauses Claude Code.
/// 2. Registers the file in the advisory registry for conflict visibility.
/// 3. Emits events for the frontend (conflict banners, waiting indicators).
fn auto_register_file(
    app_handle: &tauri::AppHandle,
    session_ctx: Option<&AiSessionContext>,
    fallback_session_id: Option<&str>,
    tool_name: &str,
    data: &serde_json::Map<String, serde_json::Value>,
) {
    // Only register for file-modifying tools
    match tool_name {
        "Edit" | "edit" | "Write" | "write" => {}
        _ => return,
    }

    let file_path = match data.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => return,
    };

    // For workflow sessions, use the task_run_id. For terminal-launched sessions
    // (no session context), use the fallback session ID (typically the Claude
    // session_id from ClaudeSession::spawn). This ensures file locks are
    // tied to a known identifier that can be cleaned up when the session ends.
    let (task_run_id, holder_name) = match session_ctx {
        Some(ctx) => (ctx.task_run_id().to_string(), ctx.session_name.clone()),
        None => match fallback_session_id {
            Some(id) => (id.to_string(), id.to_string()),
            None => return, // No identifier available — skip file locking
        },
    };

    use crate::commands::AppState;
    if let Some(app_state) = app_handle.try_state::<std::sync::Arc<AppState>>() {
        let lock_manager = app_state.file_lock_manager.clone();
        let registry = app_state.file_registry_manager.clone();
        let event_broadcast = app_state.event_broadcast.clone();
        let handle = app_handle.clone();
        let file_path_clone = file_path.clone();

        // Block the stdout reader thread to acquire the file lock.
        // This creates backpressure that pauses Claude Code when another
        // session holds the file — deterministic, no AI judgment needed.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            // Use block_in_place to run async code on this sync thread
            // without blocking the tokio runtime's thread pool.
            let waited_for = tokio::task::block_in_place(|| {
                rt.block_on(async {
                    // Emit waiting event if the file is held by another session
                    let blocker = lock_manager
                        .is_held_by_other(&file_path_clone, &task_run_id)
                        .await;

                    if let Some(ref blocker_name) = blocker {
                        info!(
                            "Session '{}' waiting for file lock on '{}' (held by '{}')",
                            holder_name, file_path_clone, blocker_name
                        );
                        let wait_data = serde_json::json!({
                            "type": "file-lock-waiting",
                            "file_path": file_path_clone,
                            "task_run_id": task_run_id,
                            "holder_name": holder_name,
                            "blocked_by": blocker_name,
                        });
                        let _ = handle.emit("file-lock-waiting", &wait_data);
                        let _ = event_broadcast.send(wait_data);
                    }

                    // This blocks until the file is available
                    let waited = lock_manager
                        .acquire(&file_path_clone, &task_run_id, &holder_name)
                        .await;

                    if waited.is_some() {
                        info!(
                            "Session '{}' acquired file lock on '{}' (was waiting)",
                            holder_name, file_path_clone
                        );
                        let acquired_data = serde_json::json!({
                            "type": "file-lock-acquired",
                            "file_path": file_path_clone,
                            "task_run_id": task_run_id,
                            "holder_name": holder_name,
                        });
                        let _ = handle.emit("file-lock-acquired", &acquired_data);
                        let _ = event_broadcast.send(acquired_data);
                    }

                    waited
                })
            });

            // Also register in the advisory registry (non-blocking, fire-and-forget)
            let file_path_reg = file_path.clone();
            let task_run_id_reg = task_run_id.clone();
            let holder_name_reg = holder_name.clone();
            rt.spawn(async move {
                let conflicts = registry
                    .register(
                        std::slice::from_ref(&file_path_reg),
                        &task_run_id_reg,
                        &holder_name_reg,
                    )
                    .await;

                if !conflicts.is_empty() {
                    let conflict_data = serde_json::json!({
                        "type": "file-conflict-detected",
                        "file_path": file_path_reg,
                        "task_run_id": task_run_id_reg,
                        "holder_name": holder_name_reg,
                        "conflicts": conflicts.iter().map(|c| serde_json::json!({
                            "file_path": c.file_path,
                            "other_holders": c.other_holders.iter().map(|h| serde_json::json!({
                                "task_run_id": h.task_run_id,
                                "holder_name": h.holder_name,
                            })).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    });

                    let _ = handle.emit("file-conflict-detected", &conflict_data);
                    let _ = event_broadcast.send(conflict_data);
                }
            });

            let _ = waited_for;
        } else {
            warn!(
                "No tokio runtime available for file lock acquire — edit of '{}' proceeding unblocked",
                file_path
            );
        }
    }
}

/// Shorten a file path to just the filename or last two components.
fn short_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    match parts.len() {
        0 => path.to_string(),
        1 => parts[0].to_string(),
        _ => format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]),
    }
}

/// Check pending messages and send the next one if the session is Ready.
fn send_next_pending_message(
    state_tracker: &SessionStateTracker,
    stdin_writer: &Arc<StdinWriter>,
    pending_messages: &Arc<std::sync::Mutex<VecDeque<String>>>,
    user_has_interacted: &std::sync::atomic::AtomicBool,
    app_handle: &tauri::AppHandle,
    session_ctx: Option<&AiSessionContext>,
) {
    if state_tracker.get() != SessionState::Ready {
        return;
    }

    let next_msg = pending_messages.lock().ok().and_then(|mut q| q.pop_front());

    if let Some(message) = next_msg {
        info!("Sending queued user message ({} chars)", message.len());

        // Build the user input message
        let user_msg = crate::claude_protocol::types::UserInputMessage::new(&message, "default");

        match stdin_writer.write_message(&user_msg) {
            Ok(()) => {
                user_has_interacted.store(true, std::sync::atomic::Ordering::Relaxed);
                // Transition to Processing
                match state_tracker.transition(SessionState::Processing) {
                    Ok(_) => {
                        emit_state_if_possible(app_handle, session_ctx, SessionState::Processing);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to transition to Processing after sending queued message: {}",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                warn!("Failed to send queued user message: {}", e);
            }
        }
    }
}

/// Process any remaining text in the line buffer (final line without trailing newline).
pub fn flush_line_buffer(
    line_buffer: &str,
    finding_parser: Option<&mut FindingParser>,
    progress_parser: Option<&mut ProgressParser>,
    finding_tx: &Sender<ParsedFinding>,
    progress_tx: &Sender<ParsedProgress>,
) {
    if line_buffer.is_empty() {
        return;
    }

    if let Some(parser) = finding_parser {
        if let Some(parsed_finding) = parser.process_line(line_buffer) {
            let _ = finding_tx.send(parsed_finding);
        }
    }

    if let Some(parser) = progress_parser {
        if let Some(parsed_progress) = parser.parse_line(line_buffer) {
            let _ = progress_tx.send(parsed_progress);
        }
    }
}
