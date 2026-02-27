//! Message dispatcher for Claude CLI stdout output.
//!
//! Parses NDJSON lines from Claude CLI's stdout and routes them to the appropriate
//! handlers: text extraction, finding parsing, progress parsing, control request
//! handling, and state transitions.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use tracing::{debug, info, trace, warn};

use crate::claude_protocol::codec::decode_message;
use crate::claude_protocol::types::OutgoingControlResponse;
use crate::commands::ai_chat::emit_session_state;
use crate::findings::{FindingParser, ParsedFinding};
use crate::mcp::shared::{emit_ai_output, AiSessionContext};
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
) -> Option<String> {
    // Decode the NDJSON line
    let msg = match decode_message(line) {
        Ok(m) => m,
        Err(e) => {
            debug!("Skipping non-parseable line: {}", e);
            return None;
        }
    };

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
            if let Ok(buf) = accumulated_output.lock() {
                let persisted = persisted_output_len.load(Ordering::Relaxed);
                if buf.len() > persisted {
                    let delta = buf[persisted..].to_string();
                    if !delta.trim().is_empty() {
                        persisted_output_len.store(buf.len(), Ordering::Relaxed);
                        let _ = tx.send(delta);
                    }
                }
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
fn format_tool_activity(
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
                    format!("{}...", &cmd[..57])
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
                    format!("{}...", &pattern[..37])
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
