//! Claude CLI session runner functions.
//!
//! Contains the core functions for running Claude CLI sessions:
//! - `run_claude_session_inline` — one-shot subprocess with stream-json parsing
//! - `run_claude_session_with_retry` — inline with exponential backoff retry
//! - `run_claude_session_interactive` — bidirectional session via `ClaudeSession`
//! - `run_claude_session_interactive_with_retry` — interactive with retry

use std::sync::Arc;
use tauri::Emitter;
use tracing::{debug, info, warn};

use crate::database::CheckpointDb;
use crate::doctor::DoctorHandle;
use crate::findings::storage as finding_storage;
use crate::findings::{Finding, FindingParser, ParsedFinding};
use crate::mcp::shared::{
    emit_ai_output, AiSessionContext, FindingContext, ProgressContext, ReflectionFixContext,
};
use crate::orchestrator::{RetryConfig, RetryService, RetryState};
use crate::reflection::parser::{ParsedReflectionFix, ReflectionFixParser};
use crate::reflection::storage as reflection_storage;
use crate::reflection::types::CreateReflectionFixInput;
use crate::step_executor::ExecutionStepConfig;
use crate::step_injection::parser::InjectedStepParser;
use crate::step_injection::types::StepInjectionContext;
use crate::workflow_state::{ParsedProgress, ProgressParser};

#[cfg(target_os = "windows")]
use std::os::windows::io::AsRawHandle;

/// Extract text from Claude CLI stream-json output line
fn extract_text_from_stream_json(json_line: &str) -> Option<String> {
    // Parse the JSON line
    let parsed: serde_json::Value = serde_json::from_str(json_line).ok()?;

    // Handle different message types
    match parsed.get("type")?.as_str()? {
        "assistant" => {
            // Extract text from assistant message content
            let content = parsed.get("message")?.get("content")?.as_array()?;
            let mut text_parts = Vec::new();
            for item in content {
                // Use and_then to gracefully skip items without a "type" field
                // instead of ? which would abort the entire function
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
            }
            if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            }
        }
        "content_block_delta" => {
            // Handle streaming deltas (partial text)
            parsed
                .get("delta")?
                .get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        }
        "result" => {
            // Claude Code stream-json returns result as a plain string
            if let Some(text) = parsed.get("result").and_then(|r| r.as_str()) {
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            } else if let Some(content) = parsed.get("result").and_then(|r| r.get("content")).and_then(|c| c.as_array()) {
                // Fallback: handle object format with content array
                let mut text_parts = Vec::new();
                for item in content {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    }
                }
                if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join(""))
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Run a Claude CLI session inline (as a child process) and wait for completion.
/// Returns the session output when complete.
///
/// This is the new "in-runner" execution model that replaces independent process spawning.
/// Claude runs as a child process, we wait for completion, then check checkpoint to continue.
///
/// If `finding_ctx` is provided, the function will parse AI output for [FINDING:...] markers
/// and store detected findings in the database, emitting events for each finding.
///
/// If `progress_ctx` is provided, the function will parse AI output for progress markers
/// and store them in the database, emitting events for each progress update.
///
/// If `pid_tracker` is provided, the child process PID will be stored there so it can be
/// killed by the stop_ai_analysis endpoint.
#[allow(clippy::too_many_arguments)]
fn run_claude_session_inline(
    working_dir: &str,
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    session_ctx: Option<AiSessionContext>,
    finding_ctx: Option<FindingContext>,
    progress_ctx: Option<ProgressContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    doctor_handle: Option<&DoctorHandle>,
    reflection_fix_ctx: Option<ReflectionFixContext>,
    step_injection_ctx: Option<StepInjectionContext>,
) -> Result<(bool, String, Vec<ExecutionStepConfig>), String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    info!("Running Claude session inline: {}", session_id);

    // Write prompt to temp file
    let temp_dir = std::env::temp_dir();
    let prompt_file = temp_dir.join(format!("claude_session_{}.txt", session_id));
    std::fs::write(&prompt_file, prompt)
        .map_err(|e| format!("Failed to write prompt file: {}", e))?;

    let prompt_content =
        std::fs::read(&prompt_file).map_err(|e| format!("Failed to read prompt file: {}", e))?;

    // Spawn Claude CLI with stream-json output
    //
    // SECURITY NOTE: bypassPermissions mode rationale
    // ------------------------------------------------
    // We use --permission-mode bypassPermissions because:
    // 1. The qontinui-runner is an AUTOMATION tool that programmatically invokes Claude
    // 2. Interactive permission prompts would block automation (no user to click "Allow")
    // 3. The user has already consented to automation by configuring and running workflows
    // 4. The runner itself provides the security boundary - it controls what prompts are sent
    //
    // Security implications:
    // - Claude can execute any action without per-action confirmation
    // - The runner's workflow configuration is the trust boundary
    // - Users should only run trusted workflow configurations
    //
    // Alternative considered: Using "acceptEdits" mode would still require user interaction
    // for bash commands, which breaks automation. Full bypass is necessary for autonomous operation.
    let mut child = std::process::Command::new("cmd.exe")
        .args([
            "/c",
            "claude",
            "--output-format",
            "stream-json",
            "--verbose",
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(working_dir)
        // Remove CLAUDECODE env var to prevent "nested session" detection.
        // The runner spawns Claude CLI as an automation tool, not as a nested session.
        .env_remove("CLAUDECODE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn Claude CLI: {}", e))?;

    // Store child PID for stop functionality
    let child_pid = child.id();
    if let Some(ref tracker) = pid_tracker {
        if let Ok(mut pids) = tracker.lock() {
            pids.push(child_pid);
            info!(
                "Registered AI process PID {} for session {}",
                child_pid, session_id
            );
        }
    }

    // Track activity for timeout (created early so Doctor registration can use it)
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_activity = Arc::new(AtomicU64::new(now_secs));
    let last_activity_stdout = last_activity.clone();

    // Register with Doctor health monitoring
    if let Some(handle) = doctor_handle {
        let reg = crate::doctor::ProcessRegistration {
            pid: child_pid,
            process_type: crate::doctor::ProcessType::SessionStreaming,
            label: format!("Claude session: {}", session_id),
            last_activity: Some(last_activity.clone()),
        };
        if let Err(e) = handle.register_blocking(reg) {
            warn!("Failed to register session with Doctor: {}", e);
        }
    }

    // Helper to remove PID from tracker when we're done
    let remove_pid = |tracker: &Option<Arc<std::sync::Mutex<Vec<u32>>>>| {
        if let Some(ref t) = tracker {
            if let Ok(mut pids) = t.lock() {
                pids.retain(|&p| p != child_pid);
                info!("Unregistered AI process PID {}", child_pid);
            }
        }
        // Also unregister from Doctor
        if let Some(handle) = doctor_handle {
            let _ = handle.unregister_blocking(child_pid);
        }
    };

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&prompt_content)
            .map_err(|e| format!("Failed to write to Claude stdin: {}", e))?;
    }

    let has_output = Arc::new(AtomicBool::new(false));
    let has_output_heartbeat = has_output.clone();

    // Heartbeat thread
    let app_handle_heartbeat = app_handle.clone();
    let session_id_heartbeat = session_id.to_string();
    let session_ctx_heartbeat = session_ctx.clone();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
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
            if elapsed_secs > 0 && elapsed_secs.is_multiple_of(30) && elapsed_secs != last_update {
                last_update = elapsed_secs;
                let mins = elapsed_secs / 60;
                let secs = elapsed_secs % 60;
                let msg = if mins > 0 {
                    format!(
                        "⏳ Session {} processing... ({}m {}s)",
                        session_id_heartbeat, mins, secs
                    )
                } else {
                    format!(
                        "⏳ Session {} processing... ({}s)",
                        session_id_heartbeat, secs
                    )
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

    // Channel for parsed findings (sent from stdout thread to finding processor thread)
    let (finding_tx, finding_rx) = mpsc::channel::<ParsedFinding>();

    // Channel for parsed progress markers (sent from stdout thread to progress processor thread)
    let (progress_tx, progress_rx) = mpsc::channel::<ParsedProgress>();

    // Channel for parsed reflection fixes (sent from stdout thread to reflection fix processor thread)
    let (reflection_fix_tx, reflection_fix_rx) = mpsc::channel::<ParsedReflectionFix>();

    // Channel for injected steps (sent from stdout thread, collected in main)
    let (injected_step_tx, injected_step_rx) = mpsc::channel::<ExecutionStepConfig>();

    // Stdout reader thread
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let has_output_stdout = has_output.clone();
    let session_ctx_stdout = session_ctx.clone();
    let finding_ctx_for_stdout = finding_ctx.clone();
    let progress_ctx_for_stdout = progress_ctx.clone();
    let reflection_fix_ctx_for_stdout = reflection_fix_ctx.clone();
    let step_injection_ctx_for_stdout = step_injection_ctx.clone();

    // On Windows, save the raw pipe handle before moving stdout into the reader thread.
    // After the Claude process exits, child processes may still hold inherited copies of
    // the pipe's write end, preventing EOF. Closing the read end from the main thread
    // unblocks the reader thread's BufReader::lines() immediately.
    #[cfg(target_os = "windows")]
    let stdout_raw_handle = stdout.as_ref().map(|s| s.as_raw_handle());

    // Shared buffer for accumulated text, so we can read it even if the stdout
    // thread hangs (e.g., on Windows when child processes hold pipe handles open).
    let shared_output_buf = Arc::new(std::sync::Mutex::new(String::new()));
    let shared_output_for_thread = shared_output_buf.clone();

    let stdout_handle = thread::spawn(move || {
        let mut all_text = String::new();
        // Create finding parser if we have a finding context
        let mut finding_parser = if finding_ctx_for_stdout.is_some() {
            Some(FindingParser::new())
        } else {
            None
        };

        // Create progress parser if we have a progress context
        let mut progress_parser = if progress_ctx_for_stdout.is_some() {
            Some(ProgressParser::new())
        } else {
            None
        };

        // Create reflection fix parser if we have a reflection fix context
        let mut reflection_fix_parser = if reflection_fix_ctx_for_stdout.is_some() {
            Some(ReflectionFixParser::new())
        } else {
            None
        };

        // Create step injection parser if we have a step injection context
        let mut step_injection_parser = if step_injection_ctx_for_stdout.is_some() {
            Some(InjectedStepParser::new())
        } else {
            None
        };

        // Buffer to accumulate text until we have complete lines for finding/progress parsing.
        // Stream-json sends partial text chunks (content_block_delta), so markers like
        // [FINDING:code_bug:high] or [PROGRESS: 50/100] can be split across multiple events.
        // We need to buffer and only parse complete lines (ending with \n).
        let mut line_buffer = String::new();

        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                // Update activity time
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                last_activity_stdout.store(now, Ordering::Relaxed);

                // Extract text from JSON
                if let Some(text) = extract_text_from_stream_json(&line) {
                    has_output_stdout.store(true, Ordering::Relaxed);
                    if !text.is_empty() {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            emit_ai_output(
                                &app_handle_stdout,
                                &text,
                                "claude",
                                None,
                                session_ctx_stdout.as_ref(),
                            );
                        }));

                        // Buffer text and parse complete lines for findings and progress
                        line_buffer.push_str(&text);

                        // Process complete lines from the buffer
                        while let Some(newline_pos) = line_buffer.find('\n') {
                            let complete_line = line_buffer[..newline_pos].to_string();
                            line_buffer = line_buffer[newline_pos + 1..].to_string();

                            // Parse for findings
                            if let Some(ref mut parser) = finding_parser {
                                if let Some(parsed_finding) = parser.process_line(&complete_line) {
                                    // Send the parsed finding to the processor thread
                                    let _ = finding_tx.send(parsed_finding);
                                }
                            }

                            // Parse for progress markers
                            if let Some(ref mut parser) = progress_parser {
                                if let Some(parsed_progress) = parser.parse_line(&complete_line) {
                                    // Send the parsed progress to the processor thread
                                    let _ = progress_tx.send(parsed_progress);
                                }
                            }

                            // Parse for reflection fix markers
                            if let Some(ref mut parser) = reflection_fix_parser {
                                if let Some(parsed_fix) = parser.process_line(&complete_line) {
                                    let _ = reflection_fix_tx.send(parsed_fix);
                                }
                            }

                            // Parse for step injection markers
                            if let Some(ref mut parser) = step_injection_parser {
                                if let Some(injected_step) = parser.process_line(&complete_line) {
                                    let _ = injected_step_tx.send(injected_step);
                                }
                            }
                        }

                        all_text.push_str(&text);

                        // Sync accumulated text to shared buffer periodically
                        // so it's available even if this thread hangs on pipe read
                        if let Ok(mut buf) = shared_output_for_thread.lock() {
                            *buf = all_text.clone();
                        }
                    }
                }
            }
        }

        // Process any remaining text in the buffer (final line without trailing newline)
        if !line_buffer.is_empty() {
            if let Some(ref mut parser) = finding_parser {
                if let Some(parsed_finding) = parser.process_line(&line_buffer) {
                    let _ = finding_tx.send(parsed_finding);
                }
            }
            if let Some(ref mut parser) = progress_parser {
                if let Some(parsed_progress) = parser.parse_line(&line_buffer) {
                    let _ = progress_tx.send(parsed_progress);
                }
            }
            if let Some(ref mut parser) = reflection_fix_parser {
                if let Some(parsed_fix) = parser.process_line(&line_buffer) {
                    let _ = reflection_fix_tx.send(parsed_fix);
                }
            }
            if let Some(ref mut parser) = step_injection_parser {
                if let Some(injected_step) = parser.process_line(&line_buffer) {
                    let _ = injected_step_tx.send(injected_step);
                }
            }
        }

        // Final sync
        if let Ok(mut buf) = shared_output_for_thread.lock() {
            *buf = all_text.clone();
        }

        all_text
    });

    // Finding processor thread - stores findings in DB and emits events
    let app_handle_findings = app_handle.clone();
    let finding_ctx_for_processor = finding_ctx.clone();
    let session_ctx_for_findings = session_ctx.clone();

    let finding_processor_handle = thread::spawn(move || {
        let mut detected_findings: Vec<Finding> = Vec::new();

        // Only process if we have a finding context
        if let Some(ctx) = finding_ctx_for_processor {
            // Open a database connection for this thread
            let db = match CheckpointDb::new() {
                Ok(db) => Some(db),
                Err(e) => {
                    warn!("Failed to open database for finding storage: {}", e);
                    None
                }
            };

            // Process incoming findings from the channel
            while let Ok(parsed_finding) = finding_rx.recv() {
                // Log the detection
                info!(
                    "Detected finding: {} ({}:{})",
                    parsed_finding.title,
                    parsed_finding.category.as_str(),
                    parsed_finding.severity.as_str()
                );

                // Store in database if connection is available
                if let Some(ref db) = db {
                    // Get a connection from the pool for this operation
                    let conn = match db.connection() {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("Failed to get database connection: {}", e);
                            continue;
                        }
                    };

                    // Check if this is a resolved finding (marked via :resolved modifier)
                    let is_resolved = parsed_finding.is_resolved;

                    match finding_storage::insert_finding(
                        &conn,
                        &ctx.task_run_id,
                        ctx.session_num,
                        &parsed_finding,
                    ) {
                        Ok(finding) => {
                            // Emit appropriate event to frontend based on status
                            let event_name = if is_resolved {
                                "finding_resolved"
                            } else {
                                "finding_detected"
                            };

                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                if let Err(e) = app_handle_findings.emit(event_name, &finding) {
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

                            // Also emit as AI output for visibility in the session log
                            let finding_msg = if is_resolved {
                                format!(
                                    "✅ Finding resolved: [{}:{}] {}",
                                    finding.category.as_str(),
                                    finding.severity.as_str(),
                                    finding.title
                                )
                            } else {
                                format!(
                                    "📋 Finding detected: [{}:{}] {}",
                                    finding.category.as_str(),
                                    finding.severity.as_str(),
                                    finding.title
                                )
                            };
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                            warn!("Failed to store finding '{}': {}", parsed_finding.title, e);
                        }
                    }
                }
            }
        } else {
            // No finding context - just drain the channel to avoid blocking
            while finding_rx.recv().is_ok() {}
        }

        detected_findings
    });

    // Progress processor thread - stores progress markers in DB and emits events
    let app_handle_progress = app_handle.clone();
    let progress_ctx_for_processor = progress_ctx.clone();
    let session_ctx_for_progress = session_ctx.clone();

    let progress_processor_handle = thread::spawn(move || {
        let mut progress_count: u32 = 0;

        // Only process if we have a progress context
        if let Some(ctx) = progress_ctx_for_processor {
            // Open a database connection for this thread
            let db = match CheckpointDb::new() {
                Ok(db) => Some(db),
                Err(e) => {
                    warn!("Failed to open database for progress storage: {}", e);
                    None
                }
            };

            // Process incoming progress markers from the channel
            while let Ok(parsed_progress) = progress_rx.recv() {
                // Log the detection
                debug!(
                    "Detected progress: {} - {}/{}",
                    parsed_progress.marker_type,
                    parsed_progress.current,
                    parsed_progress
                        .total
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "?".to_string())
                );

                // Handle STEP_COMPLETE markers specially for granular checkpointing
                let is_step_complete = parsed_progress.marker_type
                    == crate::workflow_state::progress_markers::STEP_COMPLETE;

                // Build extra JSON data for step_complete markers (includes sub_step_id)
                let data_json = if is_step_complete {
                    parsed_progress
                        .sub_step_id
                        .as_ref()
                        .map(|id| serde_json::json!({ "sub_step_id": id }).to_string())
                } else {
                    None
                };

                // Store in database if connection is available
                if let Some(ref db) = db {
                    match db.save_step_progress_marker(
                        &ctx.checkpoint_id,
                        &parsed_progress.marker_type,
                        parsed_progress.current,
                        parsed_progress.total,
                        parsed_progress.description.as_deref(),
                        data_json.as_deref(),
                    ) {
                        Ok(_marker_id) => {
                            progress_count += 1;

                            // Emit progress event to frontend
                            let progress_event = serde_json::json!({
                                "checkpoint_id": ctx.checkpoint_id,
                                "task_run_id": ctx.task_run_id,
                                "marker_type": parsed_progress.marker_type,
                                "current": parsed_progress.current,
                                "total": parsed_progress.total,
                                "percentage": parsed_progress.percentage(),
                                "description": parsed_progress.description,
                            });

                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                if let Err(e) =
                                    app_handle_progress.emit("step_progress", &progress_event)
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

                            // Emit special event for STEP_COMPLETE markers (sub-step granular checkpointing)
                            if is_step_complete {
                                if let Some(ref sub_step_id) = parsed_progress.sub_step_id {
                                    let sub_step_event = serde_json::json!({
                                        "checkpoint_id": ctx.checkpoint_id,
                                        "task_run_id": ctx.task_run_id,
                                        "sub_step_id": sub_step_id,
                                        "description": parsed_progress.description,
                                    });
                                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                        || {
                                            if let Err(e) = app_handle_progress
                                                .emit("sub_step_complete", &sub_step_event)
                                            {
                                                warn!(
                                                    "Failed to emit sub_step_complete event: {}",
                                                    e
                                                );
                                            }
                                        },
                                    ));
                                    info!(
                                        "Sub-step completed: {} (checkpoint: {})",
                                        sub_step_id, ctx.checkpoint_id
                                    );
                                }
                            }

                            // Emit as AI output for visibility in the session log
                            let progress_msg = if let Some(total) = parsed_progress.total {
                                let pct = if total > 0 {
                                    (parsed_progress.current as f64 / total as f64 * 100.0) as u32
                                } else {
                                    0
                                };
                                format!(
                                    "📊 Progress: {}/{} ({}%) - {}",
                                    parsed_progress.current,
                                    total,
                                    pct,
                                    parsed_progress.marker_type
                                )
                            } else {
                                format!(
                                    "📊 Progress: {} - {}",
                                    parsed_progress.current, parsed_progress.marker_type
                                )
                            };

                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                            warn!(
                                "Failed to store progress marker '{}': {}",
                                parsed_progress.marker_type, e
                            );
                        }
                    }
                }
            }
        } else {
            // No progress context - just drain the channel to avoid blocking
            while progress_rx.recv().is_ok() {}
        }

        progress_count
    });

    // Reflection fix processor thread - stores reflection fixes in DB and emits events
    let app_handle_reflection = app_handle.clone();
    let reflection_fix_ctx_for_processor = reflection_fix_ctx.clone();
    let session_ctx_for_reflection = session_ctx.clone();

    let reflection_fix_processor_handle = thread::spawn(move || {
        let mut fix_count: u32 = 0;

        if let Some(ctx) = reflection_fix_ctx_for_processor {
            let db = match CheckpointDb::new() {
                Ok(db) => Some(db),
                Err(e) => {
                    warn!(
                        "Failed to open database for reflection fix storage: {}",
                        e
                    );
                    None
                }
            };

            while let Ok(parsed_fix) = reflection_fix_rx.recv() {
                info!(
                    "Detected reflection fix: {} (type: {}, confidence: {})",
                    parsed_fix.description, parsed_fix.fix_type, parsed_fix.confidence
                );

                if let Some(ref db) = db {
                    let conn = match db.connection() {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("Failed to get database connection: {}", e);
                            continue;
                        }
                    };

                    let input = CreateReflectionFixInput {
                        source_task_run_id: ctx.source_task_run_id.clone(),
                        reflection_task_run_id: ctx.reflection_task_run_id.clone(),
                        source_finding_id: parsed_fix.source_finding_id,
                        source_knowledge_id: None,
                        fix_type: parsed_fix.fix_type.clone(),
                        fix_description: parsed_fix.description.clone(),
                        file_changed: parsed_fix.file_changed,
                        old_value: parsed_fix.old_value,
                        new_value: parsed_fix.new_value,
                        confidence: parsed_fix.confidence.clone(),
                    };

                    match reflection_storage::insert_fix(&conn, &input) {
                        Ok(fix) => {
                            fix_count += 1;
                            let msg = format!(
                                "Reflection fix recorded: [{}:{}] {}",
                                fix.fix_type, fix.confidence, fix.fix_description
                            );
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                emit_ai_output(
                                    &app_handle_reflection,
                                    &msg,
                                    "reflection_fix",
                                    None,
                                    session_ctx_for_reflection.as_ref(),
                                );
                            }));

                            // Emit event to frontend
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                if let Err(e) =
                                    app_handle_reflection.emit("reflection_fix_recorded", &fix)
                                {
                                    warn!(
                                        "Failed to emit reflection_fix_recorded event: {}",
                                        e
                                    );
                                }
                            }));

                            // Auto-apply safe fix types
                            let should_auto_apply = match (fix.fix_type.as_str(), fix.confidence.as_str()) {
                                ("knowledge_base_update", "high" | "medium") => true,
                                ("context_addition", "high") => true,
                                ("instruction_clarification", "high") => true,
                                ("workflow_step_rewrite", "high") => true,
                                _ => false,
                            };

                            if should_auto_apply {
                                match fix.fix_type.as_str() {
                                    "knowledge_base_update" | "context_addition" => {
                                        // Create task knowledge entry (existing behavior)
                                        let category = match fix.fix_type.as_str() {
                                            "knowledge_base_update" => "recurring_pattern",
                                            "context_addition" => "context",
                                            _ => "observation",
                                        };

                                        let related_files: Vec<String> = fix.file_changed.iter().cloned().collect();

                                        match db.create_task_knowledge(
                                            &ctx.source_task_run_id,
                                            category,
                                            "reflection",
                                            1,
                                            &fix.fix_description,
                                            fix.file_changed.as_deref(),
                                            &fix.confidence,
                                            &related_files,
                                        ) {
                                            Ok(knowledge) => {
                                                info!("Auto-applied reflection fix as knowledge entry: {}", knowledge.id);
                                                let auto_msg = format!(
                                                    "Auto-applied fix as {} knowledge: {}",
                                                    category, fix.fix_description
                                                );
                                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                    emit_ai_output(
                                                        &app_handle_reflection,
                                                        &auto_msg,
                                                        "reflection_auto_apply",
                                                        None,
                                                        session_ctx_for_reflection.as_ref(),
                                                    );
                                                }));
                                            }
                                            Err(e) => warn!("Failed to auto-apply fix as knowledge: {}", e),
                                        }
                                    }
                                    "instruction_clarification" | "workflow_step_rewrite" => {
                                        // Create generation rule entry
                                        use crate::workflow_generation::rules;
                                        let agent = rules::infer_agent_from_fix(&fix.fix_description);
                                        let section = rules::infer_section_from_fix(&fix.fix_description, &agent);
                                        let next_num = rules::next_rule_number(&conn, &agent, &section);
                                        let title = rules::truncate_to_title(&fix.fix_description);

                                        match rules::insert_rule(&conn, &rules::InsertRuleInput {
                                            agent: agent.clone(),
                                            section: section.clone(),
                                            rule_number: next_num,
                                            title: title.clone(),
                                            content: fix.fix_description.clone(),
                                            condition: None,
                                            provenance: "reflection".to_string(),
                                            source_fix_id: Some(fix.id.clone()),
                                        }) {
                                            Ok(rule) => {
                                                info!("Auto-applied reflection fix as generation rule: {} (agent={}, section={})", rule.id, agent, section);
                                                let auto_msg = format!(
                                                    "Auto-applied fix as generation rule [{}/{}]: {}",
                                                    agent, section, title
                                                );
                                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                    emit_ai_output(
                                                        &app_handle_reflection,
                                                        &auto_msg,
                                                        "reflection_auto_apply",
                                                        None,
                                                        session_ctx_for_reflection.as_ref(),
                                                    );
                                                }));
                                            }
                                            Err(e) => warn!("Failed to auto-apply fix as generation rule: {}", e),
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to store reflection fix: {}", e);
                        }
                    }
                }
            }
        } else {
            // No reflection fix context - just drain the channel to avoid blocking
            while reflection_fix_rx.recv().is_ok() {}
        }

        fix_count
    });

    // Stderr reader thread
    let stderr = child.stderr.take();
    let stderr_handle = thread::spawn(move || {
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        output
    });

    // Wait for process to complete.
    // Health monitoring is handled by the Doctor service, which observes
    // process health (CPU, memory, activity) and emits events when stuck.
    // The Doctor never kills processes — it only notifies the frontend.
    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            let _ = stop_tx.send(());
            let _ = heartbeat_handle.join();
            let _ = std::fs::remove_file(&prompt_file);
            remove_pid(&pid_tracker);
            return Err(format!("Failed to wait for Claude: {}", e));
        }
    };

    // Cleanup
    let _ = stop_tx.send(());
    let _ = heartbeat_handle.join();

    // On Windows, close the stdout pipe's read end now that the process has exited.
    // This unblocks the reader thread immediately if child processes are still
    // holding inherited copies of the pipe's write end.
    #[cfg(target_os = "windows")]
    if let Some(raw_handle) = stdout_raw_handle {
        unsafe {
            // SAFETY: The handle was obtained from ChildStdout before it was moved
            // into the reader thread. Closing it here causes the reader's read() to
            // return an error, terminating the BufReader::lines() loop. The reader
            // thread will then exit and join() will return promptly.
            windows_sys::Win32::Foundation::CloseHandle(
                raw_handle as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }

    // Join stdout thread with a bounded wait (fallback for non-Windows or edge cases).
    // On Windows the handle close above should make this return immediately, but
    // the timeout acts as a safety net.
    let all_output = {
        let (join_tx, join_rx) = mpsc::channel::<String>();
        let _ = thread::spawn(move || {
            let result = stdout_handle.join().unwrap_or_default();
            let _ = join_tx.send(result);
        });
        match join_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(text) => text,
            Err(_) => {
                warn!(
                    "Session {}: stdout thread didn't finish within 10s after process exit. \
                     Child process likely holding pipe handle. Using buffered output.",
                    session_id
                );
                // Fall back to the shared buffer which was synced periodically
                shared_output_buf
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default()
            }
        }
    };

    let stderr_output = stderr_handle.join().unwrap_or_default();
    // Wait for the finding processor thread to complete
    // (it will exit when the stdout thread closes the channel sender)
    let detected_findings = finding_processor_handle.join().unwrap_or_default();
    // Wait for the progress processor thread to complete
    let progress_count = progress_processor_handle.join().unwrap_or_default();
    // Wait for the reflection fix processor thread to complete
    let reflection_fix_count = reflection_fix_processor_handle.join().unwrap_or_default();
    // Collect all injected steps from the channel
    let mut injected_steps: Vec<ExecutionStepConfig> = Vec::new();
    while let Ok(step) = injected_step_rx.try_recv() {
        injected_steps.push(step);
    }
    let _ = std::fs::remove_file(&prompt_file);

    // Log summary of detected findings
    if !detected_findings.is_empty() {
        info!(
            "Session {} detected {} findings",
            session_id,
            detected_findings.len()
        );
    }

    // Log summary of progress markers
    if progress_count > 0 {
        info!(
            "Session {} recorded {} progress markers",
            session_id, progress_count
        );
    }

    // Log summary of reflection fixes
    if reflection_fix_count > 0 {
        info!(
            "Session {} recorded {} reflection fixes",
            session_id, reflection_fix_count
        );
    }

    // Log summary of injected steps
    if !injected_steps.is_empty() {
        info!(
            "Session {} collected {} injected verification step(s)",
            session_id,
            injected_steps.len()
        );
    }

    // Emit stderr if any
    if !stderr_output.is_empty() {
        for line in stderr_output.lines() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                emit_ai_output(
                    app_handle,
                    &format!("[stderr] {}", line),
                    "claude",
                    None,
                    session_ctx.as_ref(),
                );
            }));
        }
    }

    let success = status.success();

    info!(
        "Session {} completed: success={}, output_len={}, findings={}, progress_markers={}, reflection_fixes={}, injected_steps={}",
        session_id,
        success,
        all_output.len(),
        detected_findings.len(),
        progress_count,
        reflection_fix_count,
        injected_steps.len()
    );

    // Remove PID from tracker now that session is complete
    remove_pid(&pid_tracker);

    Ok((success, all_output, injected_steps))
}

/// Run Claude session with retry support.
///
/// Wraps `run_claude_session_inline` with exponential backoff retry logic.
/// On transient failures, the error context is injected into the prompt for the retry.
///
/// # Arguments
/// * `retry_config` - Retry configuration (if None, no retry)
/// * All other arguments are passed to `run_claude_session_inline`
///
/// # Returns
/// * `(bool, String, Option<RetryState>)` - (success, output, retry_state if retries occurred)
#[allow(clippy::too_many_arguments)]
pub fn run_claude_session_with_retry(
    working_dir: &str,
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    session_ctx: Option<AiSessionContext>,
    finding_ctx: Option<FindingContext>,
    progress_ctx: Option<ProgressContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    retry_config: Option<&RetryConfig>,
    doctor_handle: Option<&DoctorHandle>,
    reflection_fix_ctx: Option<ReflectionFixContext>,
    step_injection_ctx: Option<StepInjectionContext>,
) -> Result<(bool, String, Option<RetryState>, Vec<ExecutionStepConfig>), String> {
    use std::thread;
    use std::time::Duration;

    // If no retry config or retry disabled, just run once
    let config = match retry_config {
        Some(cfg) if cfg.enabled => cfg,
        _ => {
            let result = run_claude_session_inline(
                working_dir,
                prompt,
                session_id,
                app_handle,
                session_ctx,
                finding_ctx,
                progress_ctx,
                pid_tracker,
                doctor_handle,
                reflection_fix_ctx,
                step_injection_ctx,
            )?;
            return Ok((result.0, result.1, None, result.2));
        }
    };

    let retry_service = RetryService::new(config.clone());
    let mut retry_state = RetryState::new();
    let mut current_prompt = prompt.to_string();

    loop {
        // Clone contexts for each attempt (they might be consumed)
        let mut ctx_clone = session_ctx.clone();
        let finding_ctx_clone = finding_ctx.clone();
        let progress_ctx_clone = progress_ctx.clone();
        let pid_tracker_clone = pid_tracker.clone();
        let reflection_fix_ctx_clone = reflection_fix_ctx.clone();
        let step_injection_ctx_clone = step_injection_ctx.clone();

        // Update retry information on the context if this is a retry attempt
        if retry_state.attempt > 0 {
            if let Some(ref mut ctx) = ctx_clone {
                // Update the inner ExecutionContext with retry information
                ctx.context.retry_attempt = retry_state.attempt;
                // Set retry_of to the session_id to indicate which session is being retried
                ctx.context.retry_of = Some(session_id.to_string());
            }
        }

        // Try to run the session
        let result = run_claude_session_inline(
            working_dir,
            &current_prompt,
            session_id,
            app_handle,
            ctx_clone,
            finding_ctx_clone,
            progress_ctx_clone,
            pid_tracker_clone,
            doctor_handle,
            reflection_fix_ctx_clone,
            step_injection_ctx_clone,
        );

        match result {
            Ok((success, output, injected_steps)) => {
                // Session succeeded (or at least completed without error)
                if retry_state.attempt > 0 {
                    info!(
                        "Session {} succeeded after {} retries",
                        session_id, retry_state.attempt
                    );
                }
                return Ok((success, output, Some(retry_state), injected_steps));
            }
            Err(error) => {
                // Session failed - check if we should retry
                let decision = retry_service.should_retry(&error, &retry_state);

                match decision {
                    crate::orchestrator::RetryDecision::Retry { delay_ms, feedback } => {
                        // Check if feedback will be injected
                        let will_inject_feedback =
                            config.feedback_injection && !feedback.is_empty();

                        // Record this attempt
                        retry_state.record_attempt(&error, delay_ms, will_inject_feedback);

                        warn!(
                            "Session {} failed (attempt {}), retrying in {}ms: {}",
                            session_id, retry_state.attempt, delay_ms, error
                        );

                        // Wait before retry
                        thread::sleep(Duration::from_millis(delay_ms));

                        // If feedback injection is enabled, prepend error context to prompt
                        if will_inject_feedback {
                            current_prompt = format!("{}\n\n---\n\n{}", feedback, prompt);
                            info!(
                                "Injected {} chars of feedback context for retry",
                                feedback.len()
                            );
                        }
                    }
                    crate::orchestrator::RetryDecision::GiveUp { reason } => {
                        // Record final attempt and give up
                        retry_state.record_attempt(&error, 0, false);

                        warn!(
                            "Session {} giving up after {} attempts: {}",
                            session_id, retry_state.attempt, reason
                        );

                        return Err(format!(
                            "Session failed after {} attempts: {} (last error: {})",
                            retry_state.attempt, reason, error
                        ));
                    }
                }
            }
        }
    }
}

/// Run an interactive Claude CLI session using the bidirectional stream-json protocol.
///
/// Unlike `run_claude_session_inline` which sends a single prompt and waits for completion,
/// this function spawns a `ClaudeSession` that supports multiple turns, user message queuing,
/// and interrupt. The session is registered with the `SessionManager` so the frontend can
/// interact with it via the `ai_chat` Tauri commands.
///
/// Returns `(success, output)` after the session completes.
#[allow(clippy::too_many_arguments)]
pub fn run_claude_session_interactive(
    working_dir: &str,
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    session_ctx: Option<AiSessionContext>,
    finding_ctx: Option<FindingContext>,
    progress_ctx: Option<ProgressContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    session_manager: &Arc<crate::claude_session::SessionManager>,
    task_run_id: &str,
    doctor_handle: Option<&DoctorHandle>,
    _reflection_fix_ctx: Option<ReflectionFixContext>,
    _step_injection_ctx: Option<StepInjectionContext>,
) -> Result<(bool, String, Vec<ExecutionStepConfig>), String> {
    use crate::claude_session::ClaudeSession;
    use crate::commands::ai_chat::emit_session_state;
    use std::sync::Arc;

    info!(
        "Starting interactive Claude session: {} (task_run_id: {})",
        session_id, task_run_id
    );

    // Spawn the interactive session (performs init handshake)
    let session = ClaudeSession::spawn(
        working_dir,
        session_id,
        app_handle,
        session_ctx.clone(),
        finding_ctx,
        progress_ctx,
        pid_tracker,
    )?;

    // Register with Doctor health monitoring
    if let Some(handle) = doctor_handle {
        let reg = crate::doctor::ProcessRegistration {
            pid: session.pid(),
            process_type: crate::doctor::ProcessType::SessionStreaming,
            label: format!("Claude interactive session: {}", session_id),
            last_activity: None,
        };
        if let Err(e) = handle.register_blocking(reg) {
            warn!("Failed to register interactive session with Doctor: {}", e);
        }
    }

    let session = Arc::new(session);

    // Register with session manager so frontend can interact
    session_manager.register(task_run_id, session.clone())?;

    // Emit initial "ready" state
    emit_session_state(app_handle, task_run_id, session_id, session.state());

    // Send the initial prompt
    session.send_initial_prompt(prompt)?;

    // Emit "processing" state
    emit_session_state(app_handle, task_run_id, session_id, session.state());

    // Wait for the session to complete (blocks until CLI exits)
    let (success, output) = session.wait_for_completion(None)?;

    // If session didn't reach Closed (e.g. timeout), close it explicitly
    if session.state() != crate::claude_session::SessionState::Closed {
        warn!(
            "Session {} not closed after wait_for_completion (state: {}), closing explicitly",
            session_id,
            session.state()
        );
        let _ = session.close();
    }

    // Emit "closed" state
    emit_session_state(
        app_handle,
        task_run_id,
        session_id,
        crate::claude_session::SessionState::Closed,
    );

    // Unregister from session manager
    session_manager.remove(task_run_id);

    // Unregister from Doctor health monitoring
    if let Some(handle) = doctor_handle {
        let _ = handle.unregister_blocking(session.pid());
    }

    info!(
        "Interactive session {} completed (success={}, output={} chars)",
        session_id,
        success,
        output.len()
    );

    // Note: Step injection not implemented for interactive sessions yet
    Ok((success, output, Vec::new()))
}

/// Run an interactive Claude CLI session with retry support.
///
/// Wraps `run_claude_session_interactive` with the same retry logic as
/// `run_claude_session_with_retry`.
#[allow(clippy::too_many_arguments)]
pub fn run_claude_session_interactive_with_retry(
    working_dir: &str,
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    session_ctx: Option<AiSessionContext>,
    finding_ctx: Option<FindingContext>,
    progress_ctx: Option<ProgressContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    retry_config: Option<&RetryConfig>,
    session_manager: &Arc<crate::claude_session::SessionManager>,
    task_run_id: &str,
    doctor_handle: Option<&DoctorHandle>,
    reflection_fix_ctx: Option<ReflectionFixContext>,
    step_injection_ctx: Option<StepInjectionContext>,
) -> Result<(bool, String, Option<RetryState>, Vec<ExecutionStepConfig>), String> {
    use std::thread;
    use std::time::Duration;

    // If no retry config or retry disabled, just run once
    let config = match retry_config {
        Some(cfg) if cfg.enabled => cfg,
        _ => {
            let result = run_claude_session_interactive(
                working_dir,
                prompt,
                session_id,
                app_handle,
                session_ctx,
                finding_ctx,
                progress_ctx,
                pid_tracker,
                session_manager,
                task_run_id,
                doctor_handle,
                reflection_fix_ctx,
                step_injection_ctx,
            )?;
            return Ok((result.0, result.1, None, result.2));
        }
    };

    let retry_service = RetryService::new(config.clone());
    let mut retry_state = RetryState::new();
    let mut current_prompt = prompt.to_string();

    loop {
        let mut ctx_clone = session_ctx.clone();
        let finding_ctx_clone = finding_ctx.clone();
        let progress_ctx_clone = progress_ctx.clone();
        let pid_tracker_clone = pid_tracker.clone();
        let reflection_fix_ctx_clone = reflection_fix_ctx.clone();
        let step_injection_ctx_clone = step_injection_ctx.clone();

        // Update retry information on the context if this is a retry attempt
        if retry_state.attempt > 0 {
            if let Some(ref mut ctx) = ctx_clone {
                ctx.context.retry_attempt = retry_state.attempt;
                ctx.context.retry_of = Some(session_id.to_string());
            }
        }

        let result = run_claude_session_interactive(
            working_dir,
            &current_prompt,
            session_id,
            app_handle,
            ctx_clone,
            finding_ctx_clone,
            progress_ctx_clone,
            pid_tracker_clone,
            session_manager,
            task_run_id,
            doctor_handle,
            reflection_fix_ctx_clone,
            step_injection_ctx_clone,
        );

        match result {
            Ok((success, output, injected_steps)) => {
                if retry_state.attempt > 0 {
                    info!(
                        "Interactive session {} succeeded after {} retries",
                        session_id, retry_state.attempt
                    );
                }
                return Ok((success, output, Some(retry_state), injected_steps));
            }
            Err(error) => {
                let decision = retry_service.should_retry(&error, &retry_state);

                match decision {
                    crate::orchestrator::RetryDecision::Retry { delay_ms, feedback } => {
                        let will_inject_feedback =
                            config.feedback_injection && !feedback.is_empty();

                        retry_state.record_attempt(&error, delay_ms, will_inject_feedback);

                        warn!(
                            "Interactive session {} failed (attempt {}), retrying in {}ms: {}",
                            session_id, retry_state.attempt, delay_ms, error
                        );

                        thread::sleep(Duration::from_millis(delay_ms));

                        if will_inject_feedback {
                            current_prompt = format!("{}\n\n---\n\n{}", feedback, prompt);
                        }
                    }
                    crate::orchestrator::RetryDecision::GiveUp { reason } => {
                        retry_state.record_attempt(&error, 0, false);

                        warn!(
                            "Interactive session {} giving up after {} attempts: {}",
                            session_id, retry_state.attempt, reason
                        );

                        return Err(format!(
                            "Session failed after {} attempts: {} (last error: {})",
                            retry_state.attempt, reason, error
                        ));
                    }
                }
            }
        }
    }
}
