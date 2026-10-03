//! Message dispatcher for Claude CLI stdout output.
//!
//! Parses NDJSON lines from Claude CLI's stdout and routes them to the appropriate
//! handlers: text extraction, finding parsing, progress parsing, control request
//! handling, and state transitions.
//!
//! **What a frame MEANS is decided without Tauri** ([`observe_frame`] over a
//! per-session [`FrameLedger`]); [`dispatch_line`] only applies the
//! [`FrameOutcome`] — emits, persistence, the next queued message. Plan
//! `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`
//! Phase 8 moved the meaning there so every recorded CLI fixture is a test:
//!
//! - a frame type the decoder does not list is counted and named once per
//!   session at `info!` (type and byte length only — a frame can carry the
//!   account's email and organisation), instead of vanishing at `debug!`;
//! - a `rate_limit_event` whose status is not `allowed`, and a `result` that
//!   is not a success (`is_error: true` under `subtype: success` included),
//!   become a `Confirmed` [`FailureSignal::StructuredEvent`] for the one
//!   classifier and recovery table (`session::failure`, `failure_recovery`);
//! - `system:session_state_changed` `idle` drives `Processing → Ready` when
//!   the CLI sends it (it arrives AFTER `result`); a CLI that never sends it
//!   gets the `result`-driven transition, as before.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use tauri::{Emitter, Manager};
use tracing::{debug, info, trace, warn};

use crate::claude_protocol::codec::decode_message;
use crate::claude_protocol::types::{
    CliSessionState, ClaudeOutputMessage, OutgoingControlResponse, SystemSubtype,
};
use crate::commands::ai_session::emit_session_state;
use crate::findings::{FindingParser, ParsedFinding};
use crate::mcp::shared::{emit_ai_output, AiSessionContext};
use crate::session::failure::FailureSignal;
use crate::session::failure_recovery::{self, Evidence, Lane};
use crate::str_utils::truncate_str;
use crate::workflow_state::{ParsedProgress, ProgressParser};

use super::state::{SessionState, SessionStateTracker};
use super::writer::StdinWriter;

// ============================================================================
// Frame meaning (pure — no Tauri)
// ============================================================================

/// What the dispatcher remembers across ONE session's stdout frames. Owned by
/// the session's stdout reader thread; one per CLI process.
#[derive(Debug)]
pub struct FrameLedger {
    /// The session id the failure store keys this session under — the same
    /// key the lane's exit path reports with, which is what lets an errored
    /// turn and the non-zero exit that follows it be one failure.
    session_key: String,
    /// Frames the decoder does not understand, per type (`system:<subtype>`
    /// for an unknown system subtype). The first of each type is logged.
    unrecognized: BTreeMap<String, u64>,
    /// The typed error code an errored turn's synthetic assistant frame
    /// carried; consumed by that turn's `result`.
    pending_error_code: Option<String>,
    /// The CLI's own last-reported turn state, when it reports one.
    cli_state: Option<CliSessionState>,
    /// The CLI has sent at least one `session_state_changed`: from then on
    /// `idle`, not `result`, ends the turn.
    state_events_seen: bool,
}

impl FrameLedger {
    pub fn new(session_key: impl Into<String>) -> Self {
        Self {
            session_key: session_key.into(),
            unrecognized: BTreeMap::new(),
            pending_error_code: None,
            cli_state: None,
            state_events_seen: false,
        }
    }

    /// The failure-store key.
    pub fn session_key(&self) -> &str {
        &self.session_key
    }

    /// The CLI's last-reported turn state (`requires_action` is Phase 9's
    /// "waiting on a permission decision" signal). `None` when the CLI does
    /// not send state events.
    pub fn cli_state(&self) -> Option<&CliSessionState> {
        self.cli_state.as_ref()
    }

    /// Unrecognised frames seen so far, per type.
    pub fn unrecognized_counts(&self) -> &BTreeMap<String, u64> {
        &self.unrecognized
    }

    /// Count one unrecognised frame; `true` when it is the first of its type.
    fn count_unrecognized(&mut self, frame_type: String, bytes: usize) -> bool {
        let count = self.unrecognized.entry(frame_type.clone()).or_insert(0);
        *count += 1;
        let first = *count == 1;
        if first {
            // Type and size only: a frame's body can carry account data.
            info!(
                session = %self.session_key,
                frame_type = %frame_type,
                bytes,
                "stream-json frame type this runner does not interpret (logged once per type per session; further ones are counted)"
            );
        }
        first
    }
}

impl Drop for FrameLedger {
    fn drop(&mut self) {
        if !self.unrecognized.is_empty() {
            info!(
                session = %self.session_key,
                counts = ?self.unrecognized,
                "stream-json frames this runner did not interpret, per type, over the session"
            );
        }
    }
}

/// What one decoded frame asks [`dispatch_line`] to do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FrameOutcome {
    /// A state transition [`observe_frame`] performed on the tracker; the
    /// caller emits it.
    pub transitioned_to: Option<SessionState>,
    /// The frame states that the turn failed (or the account is limited).
    pub failure: Option<FailureSignal>,
    /// The frame is a successful turn's `result` — evidence that clears the
    /// session's active failures.
    pub turn_succeeded: bool,
    /// The frame is a `result`: persist the turn's output.
    pub turn_ended: bool,
    /// The session may be Ready for the next queued user message.
    pub ready_for_next: bool,
    /// The first frame of a type the decoder does not interpret, this session.
    pub first_unrecognized: Option<String>,
}

/// Decide what `msg` means for this session, and perform the state transition
/// it implies on `tracker`. `bytes` is the raw line's length (logged for
/// unrecognised frames instead of the frame itself).
pub fn observe_frame(
    ledger: &mut FrameLedger,
    msg: &ClaudeOutputMessage,
    bytes: usize,
    tracker: &SessionStateTracker,
) -> FrameOutcome {
    let mut out = FrameOutcome::default();
    match msg {
        ClaudeOutputMessage::Other(frame) => {
            if ledger.count_unrecognized(frame.frame_type.clone(), bytes) {
                out.first_unrecognized = Some(frame.frame_type.clone());
            }
        }
        ClaudeOutputMessage::System(sys) => match &sys.subtype {
            Some(SystemSubtype::SessionStateChanged) => {
                ledger.state_events_seen = true;
                let state = sys.state.clone();
                match &state {
                    Some(CliSessionState::Idle) => {
                        out.transitioned_to = turn_end_transition(tracker, "session_state_changed:idle");
                        out.ready_for_next = true;
                    }
                    Some(CliSessionState::RequiresAction) => {
                        info!(
                            session = %ledger.session_key,
                            "CLI reports requires_action (waiting on a decision)"
                        );
                    }
                    Some(CliSessionState::Running) => {}
                    Some(CliSessionState::Unknown(s)) => {
                        let key = format!("system:session_state_changed:{s}");
                        if ledger.count_unrecognized(key.clone(), bytes) {
                            out.first_unrecognized = Some(key);
                        }
                    }
                    None => debug!("session_state_changed without a state"),
                }
                if state.is_some() {
                    ledger.cli_state = state;
                }
            }
            Some(SystemSubtype::Unknown(sub)) => {
                let key = format!("system:{sub}");
                if ledger.count_unrecognized(key.clone(), bytes) {
                    out.first_unrecognized = Some(key);
                }
            }
            Some(SystemSubtype::Init | SystemSubtype::ThinkingTokens) | None => {}
        },
        ClaudeOutputMessage::ControlResponse(_) => {
            debug!("Received control response from CLI");
            // The init handshake's answer: Initializing -> Ready.
            if tracker.get() == SessionState::Initializing {
                match tracker.transition(SessionState::Ready) {
                    Ok(_) => {
                        info!("Session initialized, transitioning to Ready");
                        out.transitioned_to = Some(SessionState::Ready);
                    }
                    Err(e) => warn!("Failed to transition to Ready: {}", e),
                }
            }
        }
        ClaudeOutputMessage::Assistant(a) => {
            if let Some(code) = a.error.as_deref().filter(|c| !c.trim().is_empty()) {
                ledger.pending_error_code = Some(code.to_string());
            }
        }
        ClaudeOutputMessage::RateLimitEvent(ev) => {
            let info = ev.rate_limit_info.as_ref();
            match info.and_then(|i| i.status.as_ref().map(|s| (i, s))) {
                // The per-turn status report; not a failure.
                Some((_, status)) if status.is_allowed() => {}
                Some((info, status)) => {
                    warn!(
                        session = %ledger.session_key,
                        status = status.as_str(),
                        rate_limit_type = info.rate_limit_type.as_deref().unwrap_or("?"),
                        "rate_limit_event reports a non-allowed status"
                    );
                    out.failure = Some(FailureSignal::StructuredEvent {
                        error_code: None,
                        api_status: None,
                        rate_limit_status: Some(status.as_str().to_string()),
                        message: None,
                        reset_at: info.resets_at.and_then(epoch_to_rfc3339),
                    });
                }
                // No status is no statement — not a failure, not an all-clear.
                None => debug!("rate_limit_event without a status"),
            }
        }
        ClaudeOutputMessage::Result(result) => {
            out.turn_ended = true;
            let error_code = ledger.pending_error_code.take();
            let success = result.is_success();
            info!("Received result message (success={})", success);
            if success {
                out.turn_succeeded = true;
            } else {
                out.failure = Some(FailureSignal::StructuredEvent {
                    error_code,
                    api_status: result.api_error_status,
                    rate_limit_status: None,
                    message: result.error_text(),
                    reset_at: None,
                });
            }
            // Fallback for a CLI that does not report its own state: the
            // result ends the turn. When it does, `idle` (after this frame)
            // ends it instead.
            if !ledger.state_events_seen {
                out.transitioned_to = turn_end_transition(tracker, "result");
                out.ready_for_next = true;
            }
        }
        ClaudeOutputMessage::User(_)
        | ClaudeOutputMessage::ContentBlockStart(_)
        | ClaudeOutputMessage::ContentBlockDelta(_)
        | ClaudeOutputMessage::ContentBlockStop(_)
        | ClaudeOutputMessage::ControlRequest(_) => {}
    }
    out
}

/// End-of-turn transition: Processing/Interrupting -> Ready. `None` when the
/// session was in neither state.
fn turn_end_transition(tracker: &SessionStateTracker, cause: &str) -> Option<SessionState> {
    let current = tracker.get();
    if current != SessionState::Processing && current != SessionState::Interrupting {
        info!(
            "Turn end ({}) received but state is {} (not Processing/Interrupting), no transition",
            cause, current
        );
        return None;
    }
    match tracker.transition(SessionState::Ready) {
        Ok(_) => {
            info!("Transitioned to Ready after {} (was: {})", cause, current);
            Some(SessionState::Ready)
        }
        Err(e) => {
            warn!("Failed to transition to Ready after {}: {}", cause, e);
            None
        }
    }
}

/// Epoch seconds -> RFC 3339, the failure record's `reset_at` form.
fn epoch_to_rfc3339(secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339())
}

/// Hand a typed failure frame to the one failure store: classified, recorded
/// as the session's active failure and announced. Recovery is NOT executed
/// here — the CLI process is still running, and every structured-lane recovery
/// restarts it. It runs when the child exits: the lane's exit path reports to
/// `failure_recovery::report`, which folds that exit into this failure rather
/// than recording a second one.
fn report_structured_failure(
    app_handle: &tauri::AppHandle,
    session_ctx: Option<&AiSessionContext>,
    session_key: &str,
    signal: &FailureSignal,
) {
    let provider = qontinui_runner_lib::cli_profile::claude::ID;
    let Some(failure) = failure_recovery::record_only(session_key, Lane::Structured, provider, signal)
    else {
        return;
    };
    warn!(
        session = %session_key,
        kind = ?failure.kind,
        policy = ?failure.recovery_policy,
        "stream-json turn reported a typed failure"
    );
    if let Some(ctx) = session_ctx {
        let text = match &failure.details {
            Some(d) => format!("{}. {}", failure.title, d),
            None => failure.title.clone(),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit_ai_output(app_handle, &text, "status", None, Some(ctx));
        }));
    }
}

// ============================================================================
// Dispatch (applies a FrameOutcome)
// ============================================================================

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
/// - Applying [`observe_frame`]'s outcome: state transitions, typed
///   failures, the next queued message
///
/// Returns the extracted text (if any).
#[allow(clippy::too_many_arguments)]
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
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
    turn_persist_tx: &Option<super::session::TurnPersistSender>,
    persisted_output_len: &AtomicUsize,
    // Fallback session ID for file locking when session_ctx is None
    fallback_session_id: Option<&str>,
    // Worktree ID for the session (Some when session has been promoted into
    // a git worktree). Scopes file-registry entries so two sessions editing
    // the same path in different worktrees do not flag each other as conflicts.
    worktree_id: Option<&str>,
    // This session's frame bookkeeping (unrecognised-type counts, the CLI's
    // reported state, the pending error code of an errored turn).
    ledger: &mut FrameLedger,
) -> Option<String> {
    // Decode the NDJSON line. An unlisted frame type is NOT an error (it
    // decodes as `Other`); only a malformed line lands here.
    let msg = match decode_message(line) {
        Ok(m) => m,
        Err(e) => {
            debug!("Skipping non-parseable line: {}", e);
            return None;
        }
    };

    let outcome = observe_frame(ledger, &msg, line.len(), state_tracker);
    if let Some(state) = outcome.transitioned_to {
        emit_state_if_possible(app_handle, session_ctx, state);
    }
    if let Some(ref signal) = outcome.failure {
        report_structured_failure(app_handle, session_ctx, ledger.session_key(), signal);
    }
    if outcome.turn_succeeded {
        failure_recovery::clear_on_evidence(ledger.session_key(), Evidence::TurnSucceeded);
    }

    // Emit tool activity from assistant messages with tool_use blocks.
    // This is the primary path for tool activity in bypassPermissions mode,
    // where can_use_tool control requests are never sent by the CLI.
    if let Some((tool_name, data)) = msg.extract_tool_use() {
        let activity = format_tool_activity(tool_name, &data);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit_ai_output(app_handle, &activity, "tool_activity", None, session_ctx);
        }));
        // Auto-register files under active development when Edit/Write tools are used
        auto_register_file(
            app_handle,
            session_ctx,
            fallback_session_id,
            tool_name,
            &data,
            worktree_id,
        );
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
        auto_register_file(
            app_handle,
            session_ctx,
            fallback_session_id,
            &tool_name,
            &data,
            worktree_id,
        );
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

    // Control responses (to our init/interrupt requests) carry no output;
    // the init transition was applied by `observe_frame`.
    if msg.as_control_response().is_some() {
        return None;
    }

    // Handle result messages (turn completion). The state transition — or,
    // when the CLI reports its own state, the wait for `idle` — was decided
    // by `observe_frame`.
    if outcome.turn_ended {
        // Persist the AI response delta to DB for chat session resilience.
        // This captures everything the AI said since the last persist point.
        if let Some(ref tx) = turn_persist_tx {
            if let Ok(mut buf) = accumulated_output.lock() {
                let persisted = persisted_output_len.load(Ordering::Relaxed);
                if buf.len() > persisted {
                    let delta = buf[persisted..].to_string();
                    if !delta.trim().is_empty() {
                        tx.send(delta);
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
        if outcome.ready_for_next {
            send_next_pending_message(
                state_tracker,
                stdin_writer,
                pending_messages,
                user_has_interacted,
                app_handle,
                session_ctx,
            );
        }

        // Skip text extraction for result messages — the text was already
        // emitted via streaming content_block_delta events. Extracting text
        // from the result would duplicate the entire response.
        return None;
    }

    // `session_state_changed: idle` ended the turn: the next queued message
    // may go now.
    if outcome.ready_for_next {
        send_next_pending_message(
            state_tracker,
            stdin_writer,
            pending_messages,
            user_has_interacted,
            app_handle,
            session_ctx,
        );
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
    worktree_id: Option<&str>,
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
        let pg_db = app_state.pg_db.clone();
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

                    // Productivity stack §2 / Phase 4 / §9 Q5: capture a
                    // pre-edit snapshot the first time this session
                    // touches this path. Skips the work entirely if a
                    // snapshot already exists for the (session, path)
                    // pair, so a session that edits the same file twice
                    // only writes one blob (the pre-first-edit one,
                    // which is the rollback target).
                    //
                    // Runs synchronously inside the block_on so the
                    // snapshot is on disk + recorded in PG BEFORE
                    // Claude Code's stdout reader unblocks and the edit
                    // proceeds. Errors are logged at warn-level and do
                    // not block the edit (snapshots are advisory; a
                    // missing snapshot just means /rewind-session
                    // can't roll back this file).
                    capture_pre_edit_snapshot(&handle, &pg_db, &task_run_id, &file_path_clone)
                        .await;

                    waited
                })
            });

            // Also durably record the touched file in PG (Commit Progress
            // Phase A). Distinct from the advisory registry below: registry
            // entries are released when the agent finishes, but
            // session_touched_files is append-only so commit-time logic
            // (Phases C/D) can still enumerate every file the agent edited.
            // Fire-and-forget — never blocks the stdout reader, never
            // propagates errors. PG INSERTs are sub-millisecond locally.
            let pg_db_touch = pg_db.clone();
            let file_path_touch = file_path.clone();
            let task_run_id_touch = task_run_id.clone();
            let worktree_id_touch = worktree_id.map(|s| s.to_string());
            rt.spawn(async move {
                if let Err(e) = pg_db_touch
                    .record_file_touched(
                        &task_run_id_touch,
                        &file_path_touch,
                        worktree_id_touch.as_deref(),
                    )
                    .await
                {
                    warn!(
                        "session_touched_files: record_file_touched failed for task_run='{}' file='{}': {}",
                        task_run_id_touch, file_path_touch, e
                    );
                }
            });

            // Broadcast the touch to the Rust deconflicter loop
            // (§4.1 of plans/2026-05-13-coord-as-deconflicter-plan.md).
            // `.send` returns `Err` only when there are no receivers,
            // which is fine — the deconflicter is a soft advisor and
            // missed touches degrade gracefully. The send is non-blocking
            // (broadcast channel) so it can run right here without
            // spawning. Sent AFTER the record_file_touched spawn above
            // so the deconflicter's SQL query (which reads
            // `project.session_touched_files`) is racing the same writer
            // it depends on — but the dedup window plus the 15-minute
            // recent-touch lookback make the order non-load-bearing:
            // a brand-new path will simply have one row when the
            // deconflicter peeks (this session's row), find no
            // *other* recent toucher, and stay silent. A second
            // session editing the same path later sees both rows.
            let _ = app_state
                .touch_events_tx
                .send(crate::deconflict::TouchEvent {
                    task_run_id: task_run_id.clone(),
                    file_path: file_path.clone(),
                });

            // Also register in the advisory registry (non-blocking, fire-and-forget)
            let file_path_reg = file_path.clone();
            let task_run_id_reg = task_run_id.clone();
            let holder_name_reg = holder_name.clone();
            let worktree_id_reg = worktree_id.map(|s| s.to_string());
            rt.spawn(async move {
                let conflicts = registry
                    .register(
                        std::slice::from_ref(&file_path_reg),
                        &task_run_id_reg,
                        &holder_name_reg,
                        worktree_id_reg,
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

            // Phase 2 (terminal traffic-light plan §2): probe + emit
            // `commit-state-changed` so the per-tab traffic light reflects the
            // post-Edit/Write state. Best-effort, fire-and-forget — the helper
            // spawns its own task and applies a 500 ms per-session debounce
            // (so N consecutive Edit/Write hooks in the same turn don't
            // trigger N probes).
            crate::mcp::ai_session::emit_commit_state_for_session(
                app_handle.clone(),
                task_run_id.clone(),
            );

            let _ = waited_for;
        } else {
            warn!(
                "No tokio runtime available for file lock acquire — edit of '{}' proceeding unblocked",
                file_path
            );
        }
    }
}

/// Capture a pre-edit snapshot of `file_path` for `task_run_id`. Called by
/// `auto_register_file` once per (session, path) pair from inside the
/// `block_on` that holds the file lock — so the on-disk + PG state is
/// committed before Claude Code's stdout reader unblocks and the edit
/// proceeds.
///
/// Behaviour:
/// - Cheap pre-check via `has_pre_edit_snapshot`: if a pre-edit snapshot
///   already exists for this pair we exit immediately. This handles the
///   "session edits the same file twice" case (only the first snapshot
///   is the rollback target; subsequent edits would otherwise overwrite
///   the rollback blob).
/// - If the source file doesn't exist (e.g. Claude Code is creating a
///   brand-new file via Write), skip — there's nothing to snapshot, and
///   `/rewind-session` simply leaves the file alone if no snapshot row
///   exists.
/// - Otherwise compute sha256, copy bytes to
///   `<runner_data_dir>/session_snapshots/<session_id>/<sha256>.blob`,
///   and insert the metadata row.
///
/// All errors are logged at warn-level — snapshots are advisory and a
/// failure here MUST NOT block the edit (per Phase 4 §10 mitigation).
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
async fn capture_pre_edit_snapshot(
    app_handle: &tauri::AppHandle,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    session_id: &str,
    file_path: &str,
) {
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    // 1. Cheap pre-check: skip duplicate snapshots within the same session.
    match pg_db.has_pre_edit_snapshot(session_id, file_path).await {
        Ok(true) => {
            trace!(
                "capture_pre_edit_snapshot: snapshot already exists for session={}, path={} — skipping",
                session_id,
                file_path
            );
            return;
        }
        Ok(false) => {}
        Err(e) => {
            warn!(
                "capture_pre_edit_snapshot: has_pre_edit_snapshot lookup failed (session={}, path={}): {} — assuming none and proceeding",
                session_id, file_path, e
            );
        }
    }

    // 2. Read the source file. ENOENT means Claude Code is creating a new
    //    file; nothing to snapshot. All other errors are logged.
    let bytes = match std::fs::read(file_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            trace!(
                "capture_pre_edit_snapshot: source file does not exist (session={}, path={}) — likely new-file Write; skipping snapshot",
                session_id, file_path
            );
            return;
        }
        Err(e) => {
            warn!(
                "capture_pre_edit_snapshot: failed to read '{}' (session={}): {} — edit proceeds without snapshot",
                file_path, session_id, e
            );
            return;
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha = format!("{:x}", hasher.finalize());

    // 3. Compute the on-disk blob destination under runner_data_dir.
    let blob_dir: PathBuf = match app_handle.path().app_data_dir() {
        Ok(base) => base.join("session_snapshots").join(session_id),
        Err(e) => {
            warn!(
                "capture_pre_edit_snapshot: app_data_dir() failed (session={}): {} — skipping",
                session_id, e
            );
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&blob_dir) {
        warn!(
            "capture_pre_edit_snapshot: create_dir_all('{}') failed (session={}): {} — skipping",
            blob_dir.display(),
            session_id,
            e
        );
        return;
    }
    let blob_path = blob_dir.join(format!("{}.blob", sha));

    // 4. Write the blob (idempotent — content-addressed by sha256).
    if !blob_path.exists() {
        if let Err(e) = std::fs::write(&blob_path, &bytes) {
            warn!(
                "capture_pre_edit_snapshot: write blob '{}' failed (session={}): {} — skipping",
                blob_path.display(),
                session_id,
                e
            );
            return;
        }
    }

    // 5. Insert PG metadata row.
    let blob_path_str = blob_path.to_string_lossy().to_string();
    if let Err(e) = pg_db
        .insert_snapshot(session_id, file_path, &blob_path_str, &sha, true)
        .await
    {
        warn!(
            "capture_pre_edit_snapshot: insert_snapshot failed (session={}, path={}): {} — blob orphaned but harmless",
            session_id, file_path, e
        );
        return;
    }

    debug!(
        "capture_pre_edit_snapshot: captured {} bytes for session={}, path={} (sha={})",
        bytes.len(),
        session_id,
        file_path,
        &sha[..8.min(sha.len())]
    );
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

#[cfg(test)]
mod tests {
    //! Every recorded Claude 2.1.285 stream-json fixture (plan Phase 2,
    //! `tests/fixtures/cli_protocol/claude/2.1.285/`) replayed through the
    //! dispatcher's frame logic in the order the CLI wrote it, with the
    //! runner's own side of the turn simulated: the init handshake answer puts
    //! the session in Ready, then the runner sends its user message and moves
    //! to Processing. The same fixtures drive `mock_claude_cli --replay` in the
    //! `cli_probes` integration test.

    use super::*;
    use crate::claude_protocol::types::RateLimitStatus;
    use crate::session::failure::classify;
    use crate::session::failure_recovery::{RecoveryTarget, StructuredRestart};
    use qontinui_types::cli_session::{FailureConfidence, FailureEvidenceSource, FailureKind};

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cli_protocol/claude/2.1.285")
    }

    fn scenario(stem: &str) -> serde_json::Value {
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture_dir().join("manifest.json")).expect("manifest"),
        )
        .expect("manifest json");
        manifest["scenarios"][stem].clone()
    }

    fn profile() -> &'static qontinui_types::cli_session::CliProfile {
        qontinui_runner_lib::cli_profile::profile_for(qontinui_runner_lib::cli_profile::claude::ID)
            .expect("claude profile")
    }

    /// One replayed fixture: each frame's type and outcome, and where the
    /// session ended up.
    struct Replay {
        frames: Vec<(String, FrameOutcome)>,
        ledger: FrameLedger,
        tracker: SessionStateTracker,
    }

    impl Replay {
        fn failures(&self) -> Vec<&FailureSignal> {
            self.frames.iter().filter_map(|(_, o)| o.failure.as_ref()).collect()
        }

        /// The frame type at which the session went Processing -> Ready.
        fn turn_ended_ready_at(&self) -> Vec<String> {
            self.frames
                .iter()
                .skip(1) // the init handshake's own Ready
                .filter(|(_, o)| o.transitioned_to == Some(SessionState::Ready))
                .map(|(t, _)| t.clone())
                .collect()
        }

        fn count(&self, frame_type: &str) -> usize {
            self.frames.iter().filter(|(t, _)| t == frame_type).count()
        }
    }

    fn replay(stem: &str) -> Replay {
        let text = std::fs::read_to_string(fixture_dir().join(format!("{stem}.ndjson")))
            .unwrap_or_else(|e| panic!("{stem}: {e}"));
        let tracker = SessionStateTracker::new();
        tracker.transition(SessionState::Initializing).unwrap();
        let mut ledger = FrameLedger::new(format!("dispatcher-test-{stem}"));
        let mut frames = Vec::new();
        for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            // Not dropped: every recorded frame decodes.
            let msg = decode_message(line).unwrap_or_else(|e| panic!("{stem} line {i}: {e}"));
            let outcome = observe_frame(&mut ledger, &msg, line.len(), &tracker);
            frames.push((frame_label(&msg), outcome));
            if i == 0 {
                assert_eq!(
                    tracker.get(),
                    SessionState::Ready,
                    "{stem}: the first frame is the init handshake answer"
                );
                // The runner sends its user message.
                tracker.transition(SessionState::Processing).unwrap();
            }
        }
        Replay {
            frames,
            ledger,
            tracker,
        }
    }

    fn frame_label(msg: &ClaudeOutputMessage) -> String {
        match msg {
            ClaudeOutputMessage::System(s) => match (&s.subtype, &s.state) {
                (Some(sub), Some(state)) => format!("system:{}:{}", sub.as_str(), state.as_str()),
                (Some(sub), None) => format!("system:{}", sub.as_str()),
                (None, _) => "system".to_string(),
            },
            other => other.frame_type().to_string(),
        }
    }

    /// `plain_turn`: the per-turn `rate_limit_event` is decoded (not dropped)
    /// and, reporting `allowed`, is no failure; with no state events the
    /// `result` ends the turn; the turn succeeded.
    #[test]
    fn fixture_plain_turn() {
        let r = replay("plain_turn");
        assert_eq!(r.count("rate_limit_event"), 1, "decoded, not dropped");
        assert!(r.failures().is_empty());
        assert_eq!(r.turn_ended_ready_at(), vec!["result".to_string()]);
        assert_eq!(r.tracker.get(), SessionState::Ready);
        let result = &r.frames.iter().find(|(t, _)| t == "result").unwrap().1;
        assert!(result.turn_succeeded && result.turn_ended && result.ready_for_next);
        assert!(r.ledger.unrecognized_counts().is_empty());
        assert_eq!(r.ledger.cli_state(), None, "no state events without the env var");
        assert_eq!(scenario("plain_turn")["exit_code"], 0);
    }

    /// `errored_turn_invalid_model`: `subtype: success` + `is_error: true` is
    /// reported FAILED — one Confirmed structured failure carrying the
    /// synthetic assistant frame's `model_not_found` and the 404 — and the
    /// non-zero exit that follows (exit 1, stderr recorded) is the same
    /// failure, not a second one.
    #[test]
    fn fixture_errored_turn_is_reported_failed_and_its_exit_is_one_failure() {
        let r = replay("errored_turn_invalid_model");
        let result = &r.frames.iter().find(|(t, _)| t == "result").unwrap().1;
        assert!(!result.turn_succeeded, "subtype success + is_error true is not a success");
        let failures = r.failures();
        assert_eq!(failures.len(), 1);
        let FailureSignal::StructuredEvent {
            error_code,
            api_status,
            rate_limit_status,
            message,
            ..
        } = failures[0]
        else {
            panic!("expected a structured event, got {:?}", failures[0]);
        };
        assert_eq!(error_code.as_deref(), Some("model_not_found"));
        assert_eq!(*api_status, Some(404));
        assert_eq!(*rate_limit_status, None);
        assert!(message.as_deref().unwrap_or_default().contains("issue with the selected model"));
        let classified = classify(failures[0], profile()).unwrap();
        assert_eq!(classified.kind, FailureKind::BadRequest);
        assert_eq!(classified.evidence.source, FailureEvidenceSource::StructuredEvent);
        assert_eq!(classified.evidence.confidence, FailureConfidence::Confirmed);
        // The session still leaves Processing on the fallback path.
        assert_eq!(r.turn_ended_ready_at(), vec!["result".to_string()]);

        // Frame, then exit: one failure through the real store.
        struct NoRestart;
        impl StructuredRestart for NoRestart {
            fn restart(&self, _rotate: bool) -> Result<(), String> {
                panic!("a bad request is never restarted");
            }
        }
        let sc = scenario("errored_turn_invalid_model");
        let exit_code = sc["exit_code"].as_i64().map(|c| c as i32);
        assert_eq!(exit_code, Some(1));
        let key = r.ledger.session_key().to_string();
        let provider = qontinui_runner_lib::cli_profile::claude::ID;
        let recorded =
            failure_recovery::record_only(&key, Lane::Structured, provider, failures[0]).unwrap();
        let target = || RecoveryTarget::Structured {
            session_id: key.clone(),
            restart: Arc::new(NoRestart),
        };
        let stderr = sc["stderr"].as_str().unwrap().to_string();
        let on_stderr =
            failure_recovery::report(target(), provider, None, FailureSignal::Stderr(stderr));
        let on_exit =
            failure_recovery::report(target(), provider, None, FailureSignal::Exit { code: exit_code });
        assert_eq!(on_stderr.unwrap().id, recorded.id);
        assert_eq!(on_exit.unwrap().id, recorded.id);
        let active = failure_recovery::active(&key);
        assert_eq!(active.len(), 1, "one failure, not two: {active:?}");
        assert_eq!(active[0].kind, FailureKind::BadRequest);
        assert!(failure_recovery::dismiss(&key, &recorded.id));
    }

    /// `can_use_tool_sdk_allow_with_session_state_events`: with
    /// `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1` the CLI's own `idle` — which
    /// arrives AFTER `result` — ends the turn; `result` no longer does.
    /// `requires_action` is recorded; `thinking_tokens` is tolerated.
    #[test]
    fn fixture_session_state_idle_drives_ready() {
        let sc = scenario("can_use_tool_sdk_allow_with_session_state_events");
        assert_eq!(
            sc["extra_env"][crate::claude_protocol::SESSION_STATE_EVENTS_ENV],
            "1",
            "the env var the structured lane now sets"
        );
        let r = replay("can_use_tool_sdk_allow_with_session_state_events");
        assert_eq!(
            r.turn_ended_ready_at(),
            vec!["system:session_state_changed:idle".to_string()]
        );
        let result = &r.frames.iter().find(|(t, _)| t == "result").unwrap().1;
        assert_eq!(result.transitioned_to, None, "result waits for idle");
        assert!(!result.ready_for_next);
        assert!(result.turn_succeeded);
        assert_eq!(r.tracker.get(), SessionState::Ready);
        assert_eq!(r.ledger.cli_state(), Some(&CliSessionState::Idle));
        assert_eq!(r.count("system:session_state_changed:requires_action"), 1);
        assert!(r.count("system:thinking_tokens") >= 1);
        assert!(r.ledger.unrecognized_counts().is_empty(), "{:?}", r.ledger.unrecognized_counts());
        assert_eq!(r.count("rate_limit_event"), 1);
        assert!(r.failures().is_empty());

        // `requires_action` is the state the ledger holds while the
        // permission request is open.
        let text = std::fs::read_to_string(
            fixture_dir().join("can_use_tool_sdk_allow_with_session_state_events.ndjson"),
        )
        .unwrap();
        let tracker = SessionStateTracker::new();
        let mut ledger = FrameLedger::new("dispatcher-test-requires-action");
        for line in text.lines() {
            let msg = decode_message(line).unwrap();
            observe_frame(&mut ledger, &msg, line.len(), &tracker);
            if msg.as_control_request().is_some() {
                assert_eq!(ledger.cli_state(), Some(&CliSessionState::RequiresAction));
            }
        }
    }

    /// `can_use_tool_sdk_deny`: a denied tool is not an errored turn.
    #[test]
    fn fixture_sdk_deny_is_not_a_failure() {
        let r = replay("can_use_tool_sdk_deny");
        assert!(r.failures().is_empty());
        assert_eq!(r.count("rate_limit_event"), 1);
        assert_eq!(r.turn_ended_ready_at(), vec!["result".to_string()]);
        assert!(r.frames.iter().any(|(t, o)| t == "result" && o.turn_succeeded));
        assert_eq!(scenario("can_use_tool_sdk_deny")["exit_code"], 0);
    }

    /// `can_use_tool_runner_shape_ignored`: the CLI ignored the runner's
    /// `{"allowed": true}` and never ended the turn, so the session stays
    /// Processing — no fabricated Ready, no fabricated failure. (The hang is
    /// Phase 9's responder to fix.)
    #[test]
    fn fixture_runner_shape_ignored_never_ends_the_turn() {
        let r = replay("can_use_tool_runner_shape_ignored");
        assert_eq!(r.tracker.get(), SessionState::Processing);
        assert!(r.turn_ended_ready_at().is_empty());
        assert!(r.failures().is_empty());
        assert_eq!(r.count("rate_limit_event"), 1, "decoded, not dropped");
        assert_eq!(r.count("control_request"), 1);
        assert_eq!(
            scenario("can_use_tool_runner_shape_ignored")["killed_by_probe_watchdog"],
            true
        );
    }

    fn observe_one(ledger: &mut FrameLedger, tracker: &SessionStateTracker, line: &str) -> FrameOutcome {
        observe_frame(ledger, &decode_message(line).unwrap(), line.len(), tracker)
    }

    /// A non-allowed rate-limit status is a Confirmed structured failure;
    /// `rejected` (the unified subscription window is spent) is a quota, an
    /// unseen status is `unknown` — never success.
    #[test]
    fn non_allowed_rate_limit_event_is_a_failure() {
        let tracker = SessionStateTracker::new();
        let mut ledger = FrameLedger::new("dispatcher-test-rate-limit");
        let rejected = observe_one(
            &mut ledger,
            &tracker,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1790727000,"rateLimitType":"five_hour"}}"#,
        );
        let signal = rejected.failure.expect("rejected is a failure");
        let FailureSignal::StructuredEvent {
            rate_limit_status,
            reset_at,
            ..
        } = &signal
        else {
            panic!("structured event expected");
        };
        assert_eq!(rate_limit_status.as_deref(), Some(RateLimitStatus::Rejected.as_str()));
        assert_eq!(reset_at.as_deref(), Some("2026-09-30T00:10:00+00:00"));
        let f = classify(&signal, profile()).unwrap();
        assert_eq!(f.kind, FailureKind::QuotaExhausted);
        assert_eq!(f.evidence.confidence, FailureConfidence::Confirmed);

        let unseen = observe_one(
            &mut ledger,
            &tracker,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"some_future_status"}}"#,
        );
        let f = classify(&unseen.failure.expect("an unseen status is not allowed"), profile()).unwrap();
        assert_eq!(f.kind, FailureKind::Unknown);

        for line in [
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{}}"#,
            r#"{"type":"rate_limit_event"}"#,
        ] {
            assert_eq!(observe_one(&mut ledger, &tracker, line).failure, None, "{line}");
        }
    }

    /// Unknown frame types and system subtypes are counted per type and
    /// reported as "first" exactly once per session.
    #[test]
    fn unrecognized_frames_are_counted_and_named_once() {
        let tracker = SessionStateTracker::new();
        let mut ledger = FrameLedger::new("dispatcher-test-unrecognized");
        let first = observe_one(&mut ledger, &tracker, r#"{"type":"stream_event","event":{}}"#);
        assert_eq!(first.first_unrecognized.as_deref(), Some("stream_event"));
        for _ in 0..2 {
            let again = observe_one(&mut ledger, &tracker, r#"{"type":"stream_event"}"#);
            assert_eq!(again.first_unrecognized, None);
        }
        let sys = observe_one(&mut ledger, &tracker, r#"{"type":"system","subtype":"hook_started"}"#);
        assert_eq!(sys.first_unrecognized.as_deref(), Some("system:hook_started"));
        // Known subtypes are not counted.
        observe_one(&mut ledger, &tracker, r#"{"type":"system","subtype":"thinking_tokens"}"#);
        assert_eq!(
            ledger.unrecognized_counts().iter().map(|(k, v)| (k.as_str(), *v)).collect::<Vec<_>>(),
            vec![("stream_event", 3), ("system:hook_started", 1)]
        );
    }

    /// An errored result whose error code came from the preceding assistant
    /// frame consumes it; the next turn does not inherit it.
    #[test]
    fn pending_error_code_belongs_to_one_turn() {
        let tracker = SessionStateTracker::new();
        let mut ledger = FrameLedger::new("dispatcher-test-pending-code");
        observe_one(
            &mut ledger,
            &tracker,
            r#"{"type":"assistant","error":"rate_limit","is_api_error_message":true,"message":{"content":[]}}"#,
        );
        let errored = observe_one(
            &mut ledger,
            &tracker,
            r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429}"#,
        );
        let f = classify(&errored.failure.unwrap(), profile()).unwrap();
        assert_eq!(f.kind, FailureKind::RateLimited);
        let next = observe_one(
            &mut ledger,
            &tracker,
            r#"{"type":"result","subtype":"error_during_execution","errors":["boom"]}"#,
        );
        let FailureSignal::StructuredEvent {
            error_code, message, ..
        } = next.failure.unwrap()
        else {
            panic!("structured event expected");
        };
        assert_eq!(error_code, None);
        assert_eq!(message.as_deref(), Some("boom"));
    }

    /// The `result` fallback still ends an interrupted turn; `idle` while the
    /// session is already Ready transitions nothing.
    #[test]
    fn fallback_and_idempotent_turn_end() {
        let tracker = SessionStateTracker::new();
        tracker.transition(SessionState::Initializing).unwrap();
        tracker.transition(SessionState::Ready).unwrap();
        tracker.transition(SessionState::Processing).unwrap();
        tracker.transition(SessionState::Interrupting).unwrap();
        let mut ledger = FrameLedger::new("dispatcher-test-fallback");
        let r = observe_one(&mut ledger, &tracker, r#"{"type":"result","subtype":"success"}"#);
        assert_eq!(r.transitioned_to, Some(SessionState::Ready));
        assert!(r.turn_succeeded);
        let idle = observe_one(
            &mut ledger,
            &tracker,
            r#"{"type":"system","subtype":"session_state_changed","state":"idle"}"#,
        );
        assert_eq!(idle.transitioned_to, None);
        assert!(idle.ready_for_next, "a queued message may still go on idle");
    }
}
