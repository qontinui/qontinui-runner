//! MCP API Server
//!
//! Provides an HTTP API for the MCP server to communicate with the runner.
//! This allows Claude Code (running in WSL) to control the Windows runner.
//!
//! # Multi-Monitor Coordinate System
//!
//! Windows uses a "virtual desktop" coordinate system where all monitors are combined
//! into one large coordinate space. The primary monitor is usually at (0, 0), and other
//! monitors can have negative coordinates if positioned to the left or above.
//!
//! ## Example 3-Monitor Setup:
//! ```text
//!     Left Monitor        Primary Monitor       Right Monitor
//!     (-1920, 702)        (0, 0)                (3840, 702)
//!     1920x1080           3840x2160             1920x1080
//!
//!     Virtual Desktop Origin: (-1920, 0) - the minimum X and Y across all monitors
//!     Virtual Desktop Size: 7680x2160
//! ```
//!
//! ## Key Insight: FIND vs CLICK Coordinates
//!
//! When the FIND action captures a screenshot, it captures the **entire virtual desktop**
//! (all monitors combined). The coordinates returned by FIND are relative to the
//! **virtual desktop origin** (the minimum X, minimum Y point across all monitors).
//!
//! When a CLICK action targets the FIND result, pyautogui needs **absolute virtual
//! desktop coordinates** to position the mouse correctly.

#![allow(dead_code)]
//!
//! ## The Offset Calculation
//!
//! The `monitor_offset_x` and `monitor_offset_y` values passed to Python represent
//! the **virtual desktop origin** - NOT a specific monitor's position.
//!
//! ```text
//! Example: User clicks on left monitor at FIND result (65, 1372)
//!
//! Virtual desktop origin: (-1920, 0)  ← minimum X and Y across all monitors
//! FIND result (relative to screenshot): (65, 1372)
//! Final absolute coordinates: (65 + -1920, 1372 + 0) = (-1855, 1372)
//!
//! This correctly places the click on the left monitor!
//! ```
//!
//! ## Common Pitfall (Fixed)
//!
//! Previously, the code incorrectly used the **specific monitor's position** as the offset.
//! For the left monitor at (-1920, 702), this added 702 to the Y coordinate, causing clicks
//! to land on the wrong monitor (702 pixels too low).
//!
//! The fix: Always calculate the virtual desktop origin (min X, min Y across all monitors)
//! regardless of which monitor is specified, because FIND always captures the full virtual desktop.

use crate::safe_eprintln;
use crate::safe_lock::safe_lock_or_recover;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tracing::{debug, error, info, warn};

use regex::Regex;

use crate::action_service::UnifiedActionService;
use crate::commands::rag::{send_embeddings_to_web, RAGState};
use crate::commands::AppState;
use crate::config::ConfigLoader;
use crate::config_storage::{ConfigMetadata, ConfigStorage, StoredConfig};
use crate::context;
use crate::dom_capture::{
    DomCapture, DomCaptureLogger, DomCaptureSource, DomCaptureTrigger, ReceiveExtensionDomRequest,
};
use crate::executor::with_default_bridge;
use crate::findings::storage as finding_storage;
use crate::findings::{Finding, FindingParser, ParsedFinding};
use crate::mcp::awas::{
    awas_check_support, awas_discover, awas_execute, awas_extract_elements, awas_list_actions,
};
use crate::mcp::types::{GoToStateRequest, GoToStateResult};
use crate::orchestrator::{
    DeterministicVerifier, OrchestratorState, RetryConfig, RetryService, RetryState, WorkerSignal,
};
use crate::rag::{ImportResult, QontinuiConfig, RAGConfigSummary};
use crate::scriptlets;
use crate::settings;
use crate::step_event_builder::categorize_steps;
use crate::summary_generator;
use crate::task_recorder::{TaskConfig, TaskRecorder};
use crate::tiered_info::{self, RunDetails};
use crate::timeout_config::Timeouts;
use crate::workflow_generation;
use crate::workflow_state::{ParsedProgress, ProgressParser};
// WorkflowManager import removed - using unified SessionManager instead
use axum::routing::{delete, put};
use tauri::{Emitter, Manager};

// Windows-specific imports for process creation flags
#[cfg(target_os = "windows")]
use std::os::windows::io::AsRawHandle;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

// Windows constants for process creation
#[cfg(target_os = "windows")]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

/// Re-fetch execution steps from unified workflow for a task.
/// For unified workflow tasks, this fetches fresh step definitions from the database
/// to ensure fields like check_type, command, etc. are present (which may be missing
/// from old cached execution_steps_json that only stored type/name).
fn refetch_unified_workflow_steps(
    task_id: &str,
    cached_steps_json: Option<String>,
    db: &crate::database::CheckpointDb,
) -> Option<String> {
    // Check if this is a unified workflow task
    if !task_id.starts_with("unified-workflow-") {
        return cached_steps_json;
    }

    // Extract workflow ID from task ID (format: unified-workflow-{uuid}-{timestamp})
    let parts: Vec<&str> = task_id.split('-').collect();
    if parts.len() < 7 {
        return cached_steps_json;
    }

    let workflow_id = format!(
        "{}-{}-{}-{}-{}",
        parts[2], parts[3], parts[4], parts[5], parts[6]
    );

    info!(
        "Re-fetching unified workflow steps for task {} from workflow {}",
        task_id, workflow_id
    );

    // Fetch workflow from database
    match db.get_unified_workflow(&workflow_id) {
        Ok(Some(workflow)) => {
            use crate::step_executor::ExecutionStepConfig;
            let monitor = 0;
            let mut all_steps: Vec<ExecutionStepConfig> = Vec::new();

            // Helper closure to convert step
            let convert_step = |step: &serde_json::Value| -> Option<ExecutionStepConfig> {
                // Debug: log the raw step JSON
                info!(
                    "refetch_unified_workflow_steps: converting step: {}",
                    serde_json::to_string(step).unwrap_or_else(|_| "ERROR".to_string())
                );

                // Try direct deserialization first
                if let Ok(mut config) = serde_json::from_value::<ExecutionStepConfig>(step.clone())
                {
                    info!(
                        "refetch_unified_workflow_steps: serde succeeded, check_type={:?}",
                        config.check_type
                    );
                    if config.monitor_index.is_none() {
                        config.monitor_index = Some(monitor);
                    }
                    return Some(config);
                }

                // Debug: log that serde failed
                info!("refetch_unified_workflow_steps: serde failed, using manual extraction");

                // Fall back to manual extraction
                let step_type = step.get("type").and_then(|t| t.as_str())?;
                let name = step
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());

                // Helper to get string from either snake_case or camelCase
                let get_str = |keys: &[&str]| -> Option<String> {
                    keys.iter()
                        .find_map(|k| step.get(*k).and_then(|v| v.as_str()))
                        .map(|s| s.to_string())
                };
                let get_bool = |keys: &[&str]| -> Option<bool> {
                    keys.iter()
                        .find_map(|k| step.get(*k).and_then(|v| v.as_bool()))
                };

                Some(ExecutionStepConfig {
                    step_type: step_type.to_string(),
                    name,
                    monitor_index: Some(monitor),
                    check_type: get_str(&["check_type", "checkType"]),
                    check_command: get_str(&["command", "check_command", "checkCommand"]),
                    check_working_directory: get_str(&[
                        "working_directory",
                        "workingDirectory",
                        "check_working_directory",
                        "checkWorkingDirectory",
                    ]),
                    check_auto_fix: get_bool(&[
                        "auto_fix",
                        "autoFix",
                        "check_auto_fix",
                        "checkAutoFix",
                    ]),
                    test_id: get_str(&["test_id", "testId"]),
                    test_type: get_str(&["test_type", "testType"]),
                    test_is_critical: get_bool(&["is_critical", "isCritical"]),
                    shell_command: get_str(&["command", "shell_command", "shellCommand"]),
                    shell_command_working_directory: get_str(&[
                        "working_directory",
                        "workingDirectory",
                        "shell_command_working_directory",
                        "shellCommandWorkingDirectory",
                    ]),
                    shell_command_fail_on_error: get_bool(&[
                        "fail_on_error",
                        "failOnError",
                        "shell_command_fail_on_error",
                        "shellCommandFailOnError",
                    ]),
                    prompt_content: get_str(&["content", "prompt_content", "promptContent"]),
                    is_setup: get_bool(&["isSetup", "is_setup"]),
                    ..Default::default()
                })
            };

            // Add setup steps (mark as setup phase)
            for step in &workflow.setup_steps {
                if let Some(mut config) = convert_step(step) {
                    config.is_setup = Some(true);
                    config.phase = Some("setup".to_string());
                    all_steps.push(config);
                }
            }

            // Add verification steps
            for step in &workflow.verification_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("verification".to_string());
                    all_steps.push(config);
                }
            }

            // Add agentic steps
            for step in &workflow.agentic_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("agentic".to_string());
                    all_steps.push(config);
                }
            }

            // Add completion steps (mark as completion phase)
            for step in &workflow.completion_steps {
                if let Some(mut config) = convert_step(step) {
                    config.phase = Some("completion".to_string());
                    all_steps.push(config);
                }
            }

            info!(
                "Re-fetched {} steps from unified workflow definition",
                all_steps.len()
            );

            // Update the task_run with the correct execution_steps_json
            if let Ok(new_json) = serde_json::to_string(&all_steps) {
                if let Err(e) =
                    db.update_task_run_execution_steps(task_id, Some(new_json.clone()), None)
                {
                    warn!(
                        "Failed to update execution_steps_json for task {}: {}",
                        task_id, e
                    );
                }
                Some(new_json)
            } else {
                cached_steps_json
            }
        }
        Ok(None) => {
            warn!(
                "Unified workflow {} not found, using cached execution_steps_json",
                workflow_id
            );
            cached_steps_json
        }
        Err(e) => {
            warn!(
                "Failed to fetch unified workflow {}: {}, using cached execution_steps_json",
                workflow_id, e
            );
            cached_steps_json
        }
    }
}

/// Generate a stable ID from a file path using a hash.
fn generate_id_from_path(path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("file-{:x}", hasher.finish())
}

/// Extract a human-readable name from a file path.
fn path_to_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unnamed Config")
        .to_string()
}

/// Spawn Python script with proper console on Windows.
/// Claude CLI requires a console window to function properly.
fn spawn_python_with_console(
    python_path: &str,
    args: &[&std::ffi::OsStr],
    working_dir: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    let mut cmd = std::process::Command::new(python_path);
    cmd.args(args).current_dir(working_dir);

    #[cfg(target_os = "windows")]
    {
        // CREATE_NEW_CONSOLE: Creates a new console window (required for Claude CLI)
        // Note: CREATE_BREAKAWAY_FROM_JOB requires special permissions so we don't use it here.
        // The Python spawn script handles job breakaway internally via subprocess.Popen flags.
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }

    cmd.spawn()
}

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
                if item.get("type")?.as_str()? == "text" {
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
            // Final result - extract text from content blocks
            let content = parsed.get("result")?.get("content")?.as_array()?;
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
        }
        _ => None,
    }
}

/// Structured finding output instructions with few-shot examples.
/// This is injected into prompts so the AI outputs findings in a parseable format.
pub const FINDING_INSTRUCTIONS: &str = r#"
---

## MANDATORY: Structured Finding Output Format

**YOU MUST USE THIS FORMAT for ALL issues, bugs, fixes, and observations you discover.**

The qontinui-runner parses these markers to display findings in the Monitor tab. If you don't use this format, your findings will NOT be tracked.

### Format

```
[FINDING:category:severity]
Title: Brief descriptive title
Description: What was found and why it matters
File: path/to/file.ext (if applicable)
Line: 42 (if applicable)
Resolution: What you did to fix it (if fixed)
[/FINDING]
```

### Categories
- `code_bug` - Code bugs (auto-fixable)
- `security` - Security vulnerabilities (auto-fixable)
- `test_issue` - Test problems (auto-fixable)
- `documentation` - Doc issues (auto-fixable)
- `todo` - TODOs needing user input
- `enhancement` - Improvements needing user input
- `performance` - Performance issues needing user input
- `config_issue` - Config problems (manual fix)
- `already_fixed` - Fixed in this/previous session
- `warning` - Things to be aware of

### Severity Levels
- `critical` - System-breaking, security vulnerabilities, data loss
- `high` - Major functionality broken
- `medium` - Should address soon
- `low` - Minor issues
- `info` - Informational

### Few-Shot Examples

**Example 1: Bug you fixed**
```
[FINDING:code_bug:high]
Title: Null pointer exception in user authentication
Description: The login handler didn't check if user was null before accessing properties, causing crashes for deleted users.
File: src/auth/login.ts
Line: 45
Resolution: Added null check before accessing user.email property
[/FINDING]
```

**Example 2: Security issue you fixed**
```
[FINDING:security:critical]
Title: SQL injection vulnerability in search endpoint
Description: User input was directly interpolated into SQL query without sanitization.
File: src/api/search.py
Line: 89
Resolution: Replaced string interpolation with parameterized query
[/FINDING]
```

**Example 3: Type error you fixed**
```
[FINDING:code_bug:medium]
Title: Type mismatch in API response handler
Description: Function expected string but received number from JSON parse.
File: src/handlers/response.ts
Line: 23
Resolution: Added type coercion and validation
[/FINDING]
```

**Example 4: Issue needing user input**
```
[FINDING:enhancement:medium:needs_input]
Title: Caching strategy decision needed
Description: Multiple valid caching approaches are possible for this endpoint.
Question: Which caching strategy should we use?
Options: Redis (distributed) | In-memory (simple) | Hybrid
File: src/api/cache.ts
[/FINDING]
```

**Example 5: Warning (informational)**
```
[FINDING:warning:info]
Title: Deprecated API usage detected
Description: Using deprecated fetch API that will be removed in v3.0
File: src/utils/http.ts
Line: 12
[/FINDING]
```

**OUTPUT FINDINGS AS YOU WORK.** Don't save them all for the end. Each time you find or fix something, output a [FINDING:...] block immediately.
"#;

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
    timeout_seconds: Option<u64>,
    session_ctx: Option<AiSessionContext>,
    finding_ctx: Option<FindingContext>,
    progress_ctx: Option<ProgressContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
) -> Result<(bool, String), String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    let timeout_msg = match timeout_seconds {
        Some(t) => format!("timeout: {}s", t),
        None => "no timeout".to_string(),
    };
    info!(
        "Running Claude session inline: {} ({})",
        session_id, timeout_msg
    );

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

    // Helper to remove PID from tracker when we're done
    let remove_pid = |tracker: &Option<Arc<std::sync::Mutex<Vec<u32>>>>| {
        if let Some(ref t) = tracker {
            if let Ok(mut pids) = t.lock() {
                pids.retain(|&p| p != child_pid);
                info!("Unregistered AI process PID {}", child_pid);
            }
        }
    };

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&prompt_content)
            .map_err(|e| format!("Failed to write to Claude stdin: {}", e))?;
    }

    // Track activity for timeout
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let last_activity = Arc::new(AtomicU64::new(now_secs));
    let last_activity_stdout = last_activity.clone();

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
            if elapsed_secs > 0 && elapsed_secs % 30 == 0 && elapsed_secs != last_update {
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

    // Stdout reader thread
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let has_output_stdout = has_output.clone();
    let session_ctx_stdout = session_ctx.clone();
    let finding_ctx_for_stdout = finding_ctx.clone();
    let progress_ctx_for_stdout = progress_ctx.clone();

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

    // Stderr reader thread
    let stderr = child.stderr.take();
    let stderr_handle = thread::spawn(move || {
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        output
    });

    // Wait for process with optional inactivity timeout
    // Use a safety fallback of 600s (10 min) when no timeout is configured,
    // to prevent hanging indefinitely if the process doesn't exit cleanly.
    let effective_timeout = timeout_seconds.unwrap_or(600);
    let status = loop {
        {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last_activity_secs = last_activity.load(Ordering::Relaxed);
            let inactive_secs = now_secs.saturating_sub(last_activity_secs);

            if inactive_secs > effective_timeout {
                warn!(
                    "Session {} timed out after {}s of inactivity (configured: {:?}, effective: {}s)",
                    session_id, inactive_secs, timeout_seconds, effective_timeout
                );
                let _ = child.kill();
                thread::sleep(Duration::from_millis(500));
                // Break with the exit status so the normal cleanup path runs
                // and collects whatever output was accumulated before the timeout.
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    _ => {
                        // Process didn't exit cleanly after kill — use normal cleanup
                        // to recover accumulated output from the shared buffer
                        let _ = stop_tx.send(());
                        let _ = heartbeat_handle.join();
                        let _ = std::fs::remove_file(&prompt_file);
                        // Recover output from shared buffer
                        let buffered = shared_output_buf
                            .lock()
                            .map(|s| s.clone())
                            .unwrap_or_default();
                        remove_pid(&pid_tracker);
                        if buffered.is_empty() {
                            return Err(format!(
                                "Session timed out after {}s of inactivity (no output captured)",
                                inactive_secs
                            ));
                        }
                        // Return the buffered output as a failed session (not an Err)
                        // so the caller gets the output text
                        return Ok((false, buffered));
                    }
                }
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = stop_tx.send(());
                let _ = heartbeat_handle.join();
                let _ = std::fs::remove_file(&prompt_file);
                return Err(format!("Failed to wait for Claude: {}", e));
            }
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
        "Session {} completed: success={}, output_len={}, findings={}, progress_markers={}",
        session_id,
        success,
        all_output.len(),
        detected_findings.len(),
        progress_count
    );

    // Remove PID from tracker now that session is complete
    remove_pid(&pid_tracker);

    Ok((success, all_output))
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
    timeout_seconds: Option<u64>,
    session_ctx: Option<AiSessionContext>,
    finding_ctx: Option<FindingContext>,
    progress_ctx: Option<ProgressContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
    retry_config: Option<&RetryConfig>,
) -> Result<(bool, String, Option<RetryState>), String> {
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
                timeout_seconds,
                session_ctx,
                finding_ctx,
                progress_ctx,
                pid_tracker,
            )?;
            return Ok((result.0, result.1, None));
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
            timeout_seconds,
            ctx_clone,
            finding_ctx_clone,
            progress_ctx_clone,
            pid_tracker_clone,
        );

        match result {
            Ok((success, output)) => {
                // Session succeeded (or at least completed without error)
                if retry_state.attempt > 0 {
                    info!(
                        "Session {} succeeded after {} retries",
                        session_id, retry_state.attempt
                    );
                }
                return Ok((success, output, Some(retry_state)));
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

/// Default port for the MCP API server
pub const MCP_API_PORT: u16 = 9876;

/// Shared state for the API server
pub struct ApiState {
    pub app_state: Arc<AppState>,
    pub rag_state: Arc<RAGState>,
    pub app_handle: tauri::AppHandle,
    /// Currently loaded config ID (for tracking which config is active)
    pub current_config_id: std::sync::Mutex<Option<String>>,
    /// Persistent storage for configurations
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    /// Unified action service for deterministic execution
    pub action_service: Arc<UnifiedActionService>,
    /// Currently running AI process PIDs (for stopping)
    pub current_ai_pids: Arc<std::sync::Mutex<Vec<u32>>>,
    /// Pending UI Bridge requests waiting for frontend response
    pub ui_bridge_pending: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    /// Chrome extension WebSocket connection sender
    pub extension_ws_sender:
        Arc<tokio::sync::Mutex<Option<futures_util::stream::SplitSink<WebSocket, Message>>>>,
    /// Pending extension command requests (requestId -> oneshot sender for response)
    pub extension_pending_requests: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    /// Extension connection status
    pub extension_connected: Arc<std::sync::atomic::AtomicBool>,
    /// Timestamp of last pong received from extension (epoch millis)
    pub extension_last_pong: Arc<std::sync::atomic::AtomicI64>,
    /// Timestamp when extension connected (epoch millis, 0 if not connected)
    pub extension_connected_since: Arc<std::sync::atomic::AtomicI64>,
    /// Number of extension reconnections since runner start
    pub extension_reconnect_count: Arc<std::sync::atomic::AtomicU64>,
    /// Orchestrator states by task_run_id (for agentic task orchestration)
    pub orchestrator_states:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, OrchestratorState>>>,
    /// Web extraction state tracking
    pub extraction_state: Arc<ExtractionState>,
    /// Task IDs currently being resumed (to prevent duplicate resumes)
    pub resuming_task_ids: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

/// Response for API endpoints
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Create an error response
pub fn api_error(message: impl Into<String>) -> ApiResponse<()> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.into()),
    }
}

/// Save the current config back to the original file
/// This is used when project contexts are modified
fn save_current_config_to_file(app_state: &Arc<crate::AppState>) -> Result<(), String> {
    // Get the path to the current config file
    let config_path = settings::get_last_config_path()
        .ok_or_else(|| "No config file path available. Load a configuration first.".to_string())?;

    // Get the current config
    let config_lock = app_state
        .current_config
        .lock()
        .map_err(|e| format!("Failed to lock config: {}", e))?;

    let config = config_lock
        .as_ref()
        .ok_or_else(|| "No configuration loaded".to_string())?;

    // Serialize and save
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(&config_path, json)
        .map_err(|e| format!("Failed to write config file: {}", e))?;

    info!("Saved config with contexts to: {}", config_path);
    Ok(())
}

/// Status response
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub executor_running: bool,
    pub executor_state: String,
    pub config_loaded: bool,
    pub config_path: Option<String>,
    /// Whether an AI analysis is currently in progress
    pub ai_analysis_running: bool,
}

/// Tool version response for MCP caching
#[derive(Debug, Serialize)]
pub struct ToolVersionResponse {
    /// Version hash for cache invalidation (based on config + test count)
    pub version: String,
    /// Number of base tools available
    pub tool_count: usize,
    /// Number of tests that can be executed
    pub test_count: usize,
    /// Last update timestamp
    pub last_updated: String,
}

/// Load config request
#[derive(Debug, Deserialize)]
pub struct LoadConfigRequest {
    pub config_path: String,
}

/// Run workflow request
#[derive(Debug, Deserialize)]
pub struct RunWorkflowRequest {
    pub workflow_name: String,
    #[serde(default)]
    pub monitor_index: Option<i32>,
    /// Timeout in seconds for execution completion (None = disabled, no timeout)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// Execute single action request
#[derive(Debug, Deserialize)]
pub struct ExecuteActionRequest {
    /// Action type: "click", "double_click", "right_click", "type", "hotkey", etc.
    pub action_type: String,
    /// Image ID from the loaded config (required for click actions)
    #[serde(default)]
    pub image_id: String,
    /// Optional monitor index
    #[serde(default)]
    pub monitor_index: Option<i32>,
    /// Timeout in seconds for action completion (None = disabled, no timeout)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Text to type (for "type" action)
    #[serde(default)]
    pub text_input: Option<String>,
    /// Hotkey combination (for "hotkey" action), e.g., "ctrl+c"
    #[serde(default)]
    pub hotkey: Option<String>,
}

/// Execute action result
#[derive(Debug, Serialize)]
pub struct ExecuteActionResult {
    pub success: bool,
    pub action_type: String,
    pub image_id: String,
    pub error: Option<String>,
}

/// Execute Python command request (generic command forwarding to Python executor)
///
/// This is used by the accessibility service and other features that need
/// to send commands directly to the Python executor via HTTP.
#[derive(Debug, Deserialize)]
pub struct ExecutePythonCommandRequest {
    /// Command type (e.g., "capture_accessibility", "auto_connect_accessibility")
    pub cmd_type: String,
    /// Command parameters as JSON
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Execute Python command response
#[derive(Debug, Serialize)]
pub struct ExecutePythonCommandResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Workflow execution result (for /workflow/run endpoint)
#[derive(Debug, Serialize)]
pub struct WorkflowExecutionResult {
    pub success: bool,
    pub workflow_name: String,
    pub error: Option<String>,
}

/// Capture screenshot request for AI Automation Builder
#[derive(Debug, Deserialize)]
pub struct CaptureScreenshotRequest {
    /// Monitor index (0-based), None for all monitors combined
    #[serde(default)]
    pub monitor: Option<i32>,
    /// Delay in seconds before capture (0-30)
    #[serde(default)]
    pub delay_seconds: Option<f64>,
    /// Task/run identifier for filename (e.g., "ai-task-abc123")
    #[serde(default)]
    pub task_id: Option<String>,
    /// Step index for filename ordering
    #[serde(default)]
    pub step_index: Option<u32>,
}

/// Capture screenshot response
#[derive(Debug, Serialize)]
pub struct CaptureScreenshotResponse {
    pub success: bool,
    /// Relative path to screenshot in .dev-logs/screenshots/
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    /// Absolute path to screenshot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absolute_path: Option<String>,
    /// Screenshot width in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    /// Screenshot height in pixels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
    /// Monitor that was captured (None = all monitors)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<i32>,
    /// Error message if capture failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Monitor info for the API response.
/// Matches the Monitor type from qontinui-schemas/geometry.
#[derive(Debug, Serialize)]
pub struct MonitorInfoResponse {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Spatial position: "left", "center", or "right"
    pub position: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_primary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable description (runner-specific extension)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Monitors response
#[derive(Debug, Serialize)]
pub struct MonitorsResponse {
    pub count: usize,
    pub monitors: Vec<MonitorInfoResponse>,
    pub available_descriptors: Vec<String>,
}

/// WebSocket handler for streaming execution events
///
/// Clients connect to /ws/events to receive real-time execution events including:
/// - Image recognition results with found coordinates
/// - Tree events (state activation/deactivation)
/// - Workflow execution progress
///
/// This enables the web frontend to display live perception of automation state.
async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_events(socket, state))
}

/// Handle WebSocket connection for event streaming
async fn handle_ws_events(socket: WebSocket, state: Arc<ApiState>) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to the broadcast channel
    let mut event_rx = state.app_state.event_broadcast.subscribe();

    info!("WebSocket client connected for event streaming");

    // Spawn task to forward broadcast events to WebSocket
    let send_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    // Serialize event to JSON string
                    match serde_json::to_string(&event) {
                        Ok(json_str) => {
                            if sender.send(Message::Text(json_str)).await.is_err() {
                                debug!("WebSocket client disconnected");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Failed to serialize event for WebSocket: {}", e);
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WebSocket client lagged, skipped {} events", n);
                    // Continue receiving - client can handle gaps
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("Event broadcast channel closed");
                    break;
                }
            }
        }
    });

    // Handle incoming messages (for ping/pong or future commands)
    while let Some(result) = receiver.next().await {
        match result {
            Ok(Message::Ping(data)) => {
                // Ping/pong is handled automatically by axum
                debug!("Received ping from WebSocket client");
                let _ = data; // Acknowledge we received it
            }
            Ok(Message::Close(_)) => {
                debug!("WebSocket client sent close");
                break;
            }
            Ok(_) => {
                // Ignore other message types for now
            }
            Err(e) => {
                warn!("WebSocket receive error: {}", e);
                break;
            }
        }
    }

    // Clean up send task
    send_task.abort();
    info!("WebSocket client disconnected from event streaming");
}

// =============================================================================
// Chrome Extension WebSocket Handlers
// =============================================================================

/// WebSocket handler for Chrome extension connection
///
/// The Chrome extension connects to /ws/extension to enable bidirectional
/// communication for UI Bridge exploration. The runner can send exploration
/// commands to the extension, which forwards them to the active browser tab.
async fn ws_extension_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_extension_ws(socket, state))
}

/// Handle WebSocket connection from Chrome extension (or offscreen document).
///
/// Includes a server-side ping loop that sends PING frames every 20 seconds
/// to detect dead connections early and keep the connection alive at the TCP level.
async fn handle_extension_ws(socket: WebSocket, state: Arc<ApiState>) {
    use std::sync::atomic::Ordering;

    let (sender, mut receiver) = socket.split();

    // Store the sender for sending commands to extension
    {
        let mut ws_sender = state.extension_ws_sender.lock().await;
        *ws_sender = Some(sender);
    }

    // Update connection tracking
    let now_ms = chrono::Utc::now().timestamp_millis();
    state.extension_connected.store(true, Ordering::SeqCst);
    state.extension_last_pong.store(now_ms, Ordering::SeqCst);
    state
        .extension_connected_since
        .store(now_ms, Ordering::SeqCst);
    let reconnect_num = state
        .extension_reconnect_count
        .fetch_add(1, Ordering::SeqCst);

    info!(
        "Chrome extension WebSocket connected (connection #{})",
        reconnect_num + 1
    );

    // Server-side ping interval: sends PING every 20 seconds
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(20));
    ping_interval.tick().await; // First tick completes immediately

    // Main event loop: process messages and send pings concurrently
    loop {
        tokio::select! {
            // Handle incoming messages from extension
            result = receiver.next() => {
                match result {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(msg) => {
                                handle_extension_message(msg, state.clone()).await;
                            }
                            Err(e) => {
                                warn!("Failed to parse extension message: {}", e);
                            }
                        }
                    }
                    Some(Ok(Message::Ping(_))) => {
                        debug!("Received WebSocket ping from extension");
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // Update last pong timestamp for health tracking
                        state.extension_last_pong.store(
                            chrono::Utc::now().timestamp_millis(),
                            Ordering::SeqCst,
                        );
                        debug!("Received pong from extension");
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("Extension WebSocket closed");
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("Extension WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        // Stream ended
                        info!("Extension WebSocket stream ended");
                        break;
                    }
                }
            }
            // Send periodic PING frames to detect dead connections
            _ = ping_interval.tick() => {
                let mut ws_sender = state.extension_ws_sender.lock().await;
                if let Some(ref mut sender) = *ws_sender {
                    if let Err(e) = sender.send(Message::Ping(vec![])).await {
                        warn!("Failed to send ping to extension: {}", e);
                        break;
                    }
                    debug!("Sent ping to extension");
                } else {
                    break;
                }
            }
        }
    }

    // Clean up on disconnect
    {
        let mut ws_sender = state.extension_ws_sender.lock().await;
        *ws_sender = None;
    }
    state.extension_connected.store(false, Ordering::SeqCst);
    state.extension_connected_since.store(0, Ordering::SeqCst);

    // Reject all pending requests
    {
        let mut pending = state.extension_pending_requests.lock().await;
        for (request_id, sender) in pending.drain() {
            let _ = sender.send(serde_json::json!({
                "success": false,
                "error": "Extension disconnected"
            }));
            debug!(
                "Rejected pending extension request {} due to disconnect",
                request_id
            );
        }
    }

    info!("Chrome extension WebSocket disconnected");
}

/// Handle a message from the Chrome extension
async fn handle_extension_message(msg: serde_json::Value, state: Arc<ApiState>) {
    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let request_id = msg.get("requestId").and_then(|r| r.as_str()).unwrap_or("");

    match msg_type {
        "EXPLORATION_RESPONSE" => {
            // Response to a command we sent to the extension
            let mut pending = state.extension_pending_requests.lock().await;
            if let Some(sender) = pending.remove(request_id) {
                let _ = sender.send(msg.clone());
                debug!("Delivered extension response for request {}", request_id);
            } else {
                warn!("No pending request found for response {}", request_id);
            }
        }
        "RECORDING_SNAPSHOT" => {
            // Snapshot from the recorder during a recording session
            handle_recording_snapshot(msg, state).await;
        }
        "PING" => {
            // Extension sent an application-level ping — respond with PONG
            // so the extension knows the connection is alive.
            let mut ws_sender = state.extension_ws_sender.lock().await;
            if let Some(ref mut sender) = *ws_sender {
                let pong = serde_json::json!({ "type": "PONG" });
                if let Ok(json_str) = serde_json::to_string(&pong) {
                    let _ = sender.send(Message::Text(json_str)).await;
                }
            }
            // Also update last pong tracking (bidirectional health)
            state.extension_last_pong.store(
                chrono::Utc::now().timestamp_millis(),
                std::sync::atomic::Ordering::SeqCst,
            );
            debug!("Responded to application-level ping from extension");
        }
        "PONG" => {
            // Update last pong timestamp (application-level pong)
            state.extension_last_pong.store(
                chrono::Utc::now().timestamp_millis(),
                std::sync::atomic::Ordering::SeqCst,
            );
            debug!("Received application-level pong from extension");
        }
        "EXTENSION_REQUEST" => {
            // Extension is sending a request to the runner (future use)
            debug!("Received extension request: {:?}", msg);
        }
        "FULL_PAGE_CAPTURE_PROGRESS" => {
            // Progress update from full-page screenshot capture
            if let Some(progress) = msg.get("progress") {
                debug!("Full-page capture progress: {:?}", progress);
                // Note: Tauri event emission removed - AppState doesn't have app_handle
                // Progress is logged for debugging purposes
            }
        }
        _ => {
            debug!("Unknown extension message type: {}", msg_type);
        }
    }
}

/// Handle a recording snapshot from the browser extension
async fn handle_recording_snapshot(msg: serde_json::Value, state: Arc<ApiState>) {
    // Extract snapshot data
    let snapshot = match msg.get("snapshot") {
        Some(s) => s,
        None => {
            warn!("RECORDING_SNAPSHOT missing snapshot field");
            return;
        }
    };

    // Parse the snapshot
    let parsed: Result<crate::recording::RecordingSnapshot, _> =
        serde_json::from_value(snapshot.clone());

    match parsed {
        Ok(snapshot_data) => {
            debug!(
                "Received recording snapshot: trigger={}, url={}, elements={}",
                snapshot_data.trigger, snapshot_data.url, snapshot_data.element_count
            );

            // If the snapshot has enhanced action capture data, we can auto-add to active recording
            if let Some(action) = &snapshot_data.action {
                // Look for an active recording that matches this tab
                let tab_id = msg
                    .get("sessionTabId")
                    .and_then(|t| t.as_i64())
                    .map(|t| t as i32);

                if let Some(tab_id) = tab_id {
                    // Try to find an active recording for this tab
                    let storage = crate::recording::RecordingStorage::new(
                        state.app_state.checkpoint_db.clone(),
                    );

                    match storage.list_recordings(
                        Some(crate::recording::RecordingStatus::Recording),
                        Some(10),
                    ) {
                        Ok(recordings) => {
                            // Find recording for this tab
                            if let Some(recording) =
                                recordings.iter().find(|r| r.tab_id == Some(tab_id))
                            {
                                // Parse action type
                                let action_type: Result<crate::recording::ActionType, _> =
                                    action.action_type.parse();

                                if let Ok(action_type) = action_type {
                                    // Build action data based on type
                                    let action_data = match action_type {
                                        crate::recording::ActionType::Click => {
                                            action.click.as_ref().map(|c| {
                                                serde_json::to_value(c)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Type => {
                                            action.type_data.as_ref().map(|t| {
                                                serde_json::to_value(t)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Navigate => {
                                            action.navigate.as_ref().map(|n| {
                                                serde_json::to_value(n)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Select => {
                                            action.select.as_ref().map(|s| {
                                                serde_json::to_value(s)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Scroll => {
                                            action.scroll.as_ref().map(|s| {
                                                serde_json::to_value(s)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Keypress => {
                                            action.keypress.as_ref().map(|k| {
                                                serde_json::to_value(k)
                                                    .unwrap_or(serde_json::Value::Null)
                                            })
                                        }
                                        crate::recording::ActionType::Hover => None,
                                    };

                                    let input = crate::recording::AddActionInput {
                                        action_type,
                                        url: action.url.clone(),
                                        page_title: snapshot_data.title.clone(),
                                        target: action.target.clone(),
                                        action_data,
                                        timestamp: action.timestamp.clone(),
                                        duration_ms: None,
                                    };

                                    match storage.add_action(&recording.id, input) {
                                        Ok(recorded_action) => {
                                            info!(
                                                "Auto-recorded action {} for recording {}",
                                                recorded_action.sequence_number, recording.id
                                            );

                                            // Emit event to frontend
                                            if let Err(e) = state.app_handle.emit(
                                                "recording-action-added",
                                                serde_json::json!({
                                                    "recording_id": recording.id,
                                                    "action": recorded_action,
                                                }),
                                            ) {
                                                warn!("Failed to emit recording-action-added event: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Failed to auto-record action: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to list active recordings: {}", e);
                        }
                    }
                }
            }

            // Broadcast the snapshot to connected clients (for real-time monitoring)
            let _ = state.app_state.event_broadcast.send(serde_json::json!({
                "type": "recording_snapshot",
                "snapshot": snapshot_data,
                "tab_id": msg.get("sessionTabId"),
                "total_snapshots": msg.get("totalSnapshots"),
            }));
        }
        Err(e) => {
            warn!("Failed to parse recording snapshot: {}", e);
            debug!("Raw snapshot: {:?}", snapshot);
        }
    }
}

/// Send a command to the extension and wait for response
async fn send_extension_command(
    state: Arc<ApiState>,
    action: &str,
    params: serde_json::Value,
    timeout_secs: u64,
) -> Result<serde_json::Value, String> {
    use std::sync::atomic::Ordering;

    // Check if extension is connected
    if !state.extension_connected.load(Ordering::SeqCst) {
        return Err("Chrome extension not connected".to_string());
    }

    // Generate request ID
    let request_id = format!(
        "runner-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    );

    // Create command message
    let command = serde_json::json!({
        "type": "EXPLORATION_COMMAND",
        "requestId": request_id,
        "action": action,
        "params": params
    });

    // Create oneshot channel for response
    let (tx, rx) = tokio::sync::oneshot::channel();

    // Register pending request
    {
        let mut pending = state.extension_pending_requests.lock().await;
        pending.insert(request_id.clone(), tx);
    }

    // Send command to extension
    {
        let mut ws_sender = state.extension_ws_sender.lock().await;
        if let Some(ref mut sender) = *ws_sender {
            match serde_json::to_string(&command) {
                Ok(json_str) => {
                    if let Err(e) = sender.send(Message::Text(json_str)).await {
                        // Clean up pending request
                        let mut pending = state.extension_pending_requests.lock().await;
                        pending.remove(&request_id);
                        return Err(format!("Failed to send command to extension: {}", e));
                    }
                }
                Err(e) => {
                    let mut pending = state.extension_pending_requests.lock().await;
                    pending.remove(&request_id);
                    return Err(format!("Failed to serialize command: {}", e));
                }
            }
        } else {
            let mut pending = state.extension_pending_requests.lock().await;
            pending.remove(&request_id);
            return Err("Extension WebSocket sender not available".to_string());
        }
    }

    // Wait for response with timeout (0 = default 30s, not infinite)
    let effective_timeout = if timeout_secs == 0 { 30 } else { timeout_secs };
    match tokio::time::timeout(std::time::Duration::from_secs(effective_timeout), rx).await {
        Ok(Ok(response)) => {
            let success = response
                .get("success")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            if success {
                Ok(response
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            } else {
                let error = response
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                Err(error.to_string())
            }
        }
        Ok(Err(_)) => Err("Response channel closed".to_string()),
        Err(_) => {
            // Timeout - clean up pending request
            let mut pending = state.extension_pending_requests.lock().await;
            pending.remove(&request_id);
            Err(format!(
                "Extension command timed out after {}s",
                effective_timeout
            ))
        }
    }
}

// =============================================================================
// Extension HTTP Endpoints (for Python bridge)
// =============================================================================

/// Request body for extension command
#[derive(Debug, Deserialize)]
struct ExtensionCommandRequest {
    action: String,
    #[serde(default)]
    params: serde_json::Value,
    #[serde(default = "default_extension_timeout")]
    timeout_secs: u64,
}

/// Default timeout for extension commands.
/// Returns 0 to indicate no timeout (run until completion).
fn default_extension_timeout() -> u64 {
    0 // No timeout - run until completion
}

/// Get extension connection status with health details
async fn get_extension_status(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    use std::sync::atomic::Ordering;

    let connected = state.extension_connected.load(Ordering::SeqCst);
    let last_pong_ms = state.extension_last_pong.load(Ordering::SeqCst);
    let connected_since_ms = state.extension_connected_since.load(Ordering::SeqCst);
    let reconnect_count = state.extension_reconnect_count.load(Ordering::SeqCst);

    let now_ms = chrono::Utc::now().timestamp_millis();
    let last_pong_ago_sec = if last_pong_ms > 0 {
        Some((now_ms - last_pong_ms) / 1000)
    } else {
        None
    };
    let connection_age_sec = if connected_since_ms > 0 {
        Some((now_ms - connected_since_ms) / 1000)
    } else {
        None
    };

    Json(ApiResponse::success(serde_json::json!({
        "connected": connected,
        "websocket_url": "ws://localhost:9876/ws/extension",
        "last_pong_ago_sec": last_pong_ago_sec,
        "connection_age_sec": connection_age_sec,
        "reconnect_count": reconnect_count
    })))
}

/// Send a command to the extension and wait for response
async fn send_extension_command_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExtensionCommandRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!(
        "Extension command request: action={}, params={:?}",
        request.action, request.params
    );

    match send_extension_command(state, &request.action, request.params, request.timeout_secs).await
    {
        Ok(data) => Json(ApiResponse::success(data)),
        Err(e) => {
            warn!("Extension command failed: {}", e);
            Json(ApiResponse {
                success: false,
                data: None::<serde_json::Value>,
                error: Some(e.to_string()),
            })
        }
    }
}

/// SSE events handler for MCP notification streaming
///
/// Provides Server-Sent Events (SSE) stream of runner events.
/// More compatible with HTTP-based MCP clients than WebSocket.
///
/// Events emitted:
/// - qontinui/execution_started - Workflow begins
/// - qontinui/execution_progress - Step completion
/// - qontinui/execution_completed - Workflow ends
/// - qontinui/test_started - Test begins
/// - qontinui/test_completed - Test ends
/// - qontinui/image_recognition - Match found/failed
/// - qontinui/error - Error occurs
async fn sse_events_handler(
    State(state): State<Arc<ApiState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    use futures_util::StreamExt as FuturesStreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    // Subscribe to the broadcast channel
    let event_rx = state.app_state.event_broadcast.subscribe();

    info!("SSE client connected for event streaming");

    // Convert broadcast receiver to SSE stream
    // Use futures_util::StreamExt::filter_map for compatibility with Sse
    let stream =
        FuturesStreamExt::filter_map(BroadcastStream::new(event_rx), |result| async move {
            match result {
                Ok(event) => {
                    // Serialize event to JSON and wrap in SSE Event
                    match serde_json::to_string(&event) {
                        Ok(json_str) => {
                            // Determine event type from the event data
                            let event_type = event
                                .get("event_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("message");

                            Some(Ok(Event::default()
                                .event(format!("qontinui/{}", event_type))
                                .data(json_str)))
                        }
                        Err(e) => {
                            warn!("Failed to serialize event for SSE: {}", e);
                            None
                        }
                    }
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    warn!("SSE client lagged, skipped {} events", n);
                    // Send a notification about skipped events
                    Some(Ok(Event::default().event("qontinui/warning").data(
                        format!("{{\"message\": \"Skipped {} events due to lag\"}}", n),
                    )))
                }
            }
        });

    // Return SSE response with keep-alive
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Health check endpoint
async fn health() -> Json<ApiResponse<String>> {
    Json(ApiResponse::success("ok".to_string()))
}

// ============================================================================
// Bridge Management Endpoints
// ============================================================================

use crate::executor::{BridgeInfo, BridgeMode, CreateBridgeResult, GuiLockInfo};

/// Request body for creating a new bridge
#[derive(Debug, Deserialize)]
struct CreateBridgeRequest {
    /// Operating mode: "gui" or "headless"
    #[serde(default)]
    mode: BridgeMode,
    /// Optional task run ID to associate with this bridge
    run_id: Option<String>,
    /// Monitor indices for GUI mode (default: [0])
    #[serde(default)]
    monitor_indices: Vec<i32>,
    /// Force acquire GUI lock even if held by another bridge
    #[serde(default)]
    force_gui_lock: bool,
}

/// Request body for running a workflow on a specific bridge
#[derive(Debug, Deserialize)]
struct BridgeWorkflowRequest {
    /// Workflow name to run
    workflow_name: Option<String>,
    /// Config path to load (optional if already loaded)
    config_path: Option<String>,
    /// Workflow parameters
    params: Option<serde_json::Value>,
}

/// List all active bridges
async fn list_bridges(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<Vec<BridgeInfo>>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let bridges = bridge_manager.list_bridges().await;
        Json(ApiResponse::success(bridges))
    } else {
        Json(ApiResponse::<Vec<BridgeInfo>>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Create a new bridge
async fn create_bridge(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateBridgeRequest>,
) -> Json<ApiResponse<CreateBridgeResult>> {
    info!(
        "Creating new bridge: mode={:?}, run_id={:?}",
        request.mode, request.run_id
    );

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let monitor_indices = if request.monitor_indices.is_empty() {
            vec![0]
        } else {
            request.monitor_indices
        };

        match bridge_manager
            .create_bridge(
                request.mode,
                request.run_id,
                monitor_indices,
                request.force_gui_lock,
            )
            .await
        {
            Ok(result) => Json(ApiResponse::success(result)),
            Err(e) => Json(ApiResponse::<CreateBridgeResult>::error(&e)),
        }
    } else {
        Json(ApiResponse::<CreateBridgeResult>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Get info for a specific bridge
async fn get_bridge(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
) -> Json<ApiResponse<Option<BridgeInfo>>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let info = bridge_manager.get_bridge_info(&bridge_id).await;
        Json(ApiResponse::success(info))
    } else {
        Json(ApiResponse::<Option<BridgeInfo>>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Remove a bridge
async fn remove_bridge(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
) -> Json<ApiResponse<()>> {
    info!("Removing bridge: {}", bridge_id);

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        match bridge_manager.remove_bridge(&bridge_id).await {
            Ok(()) => Json(ApiResponse::success(())),
            Err(e) => Json(ApiResponse::<()>::error(&e)),
        }
    } else {
        Json(ApiResponse::<()>::error("Bridge manager not initialized"))
    }
}

/// Run a workflow on a specific bridge
async fn run_bridge_workflow(
    State(state): State<Arc<ApiState>>,
    Path(bridge_id): Path<String>,
    Json(request): Json<BridgeWorkflowRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    info!(
        "Running workflow on bridge {}: {:?}",
        bridge_id, request.workflow_name
    );

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        // Load config if provided
        if let Some(config_path) = request.config_path {
            let load_result = bridge_manager
                .with_bridge(&bridge_id, |bridge| bridge.load_configuration(&config_path));

            if let Err(e) = load_result {
                return Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to access bridge: {}",
                    e
                )));
            }

            if let Ok(Err(e)) = load_result {
                return Json(ApiResponse::<serde_json::Value>::error(format!(
                    "Failed to load config: {}",
                    e
                )));
            }
        }

        // Build execution params
        let params = if request.workflow_name.is_some() || request.params.is_some() {
            Some(serde_json::json!({
                "workflow_name": request.workflow_name,
                "params": request.params,
            }))
        } else {
            None
        };

        // Start execution
        let start_result = bridge_manager.with_bridge(&bridge_id, |bridge| {
            bridge.start_execution_with_params(params)
        });

        match start_result {
            Ok(Ok(())) => Json(ApiResponse::success(serde_json::json!({
                "message": "Workflow started",
                "bridge_id": bridge_id,
            }))),
            Ok(Err(e)) => Json(ApiResponse::<serde_json::Value>::error(format!(
                "Failed to start workflow: {}",
                e
            ))),
            Err(e) => Json(ApiResponse::<serde_json::Value>::error(e)),
        }
    } else {
        Json(ApiResponse::<serde_json::Value>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Get current GUI lock holder
async fn get_gui_lock(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<GuiLockInfo>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let info = bridge_manager.get_gui_lock_info().await;
        Json(ApiResponse::success(info))
    } else {
        Json(ApiResponse::<GuiLockInfo>::error(
            "Bridge manager not initialized",
        ))
    }
}

// ============================================================================
// Headless-Only Mode Endpoints
// ============================================================================

/// Response for headless-only mode status
#[derive(Debug, Serialize)]
struct HeadlessOnlyResponse {
    /// Whether headless-only mode is enabled
    enabled: bool,
    /// Description of what this mode does
    description: String,
}

/// Request body for setting headless-only mode
#[derive(Debug, Deserialize)]
struct SetHeadlessOnlyRequest {
    /// Whether to enable headless-only mode
    enabled: bool,
}

/// Get headless-only mode status
///
/// When headless-only mode is enabled, GUI bridges cannot be created.
/// This is intended for server deployments without GUI access.
async fn get_headless_only(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<HeadlessOnlyResponse>> {
    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        let enabled = bridge_manager.is_headless_only();
        Json(ApiResponse::success(HeadlessOnlyResponse {
            enabled,
            description: if enabled {
                "Headless-only mode is ENABLED. All bridges must be headless. \
                GUI mode bridges cannot be created. This is intended for server deployments."
                    .to_string()
            } else {
                "Headless-only mode is DISABLED. Both GUI and headless bridges can be created."
                    .to_string()
            },
        }))
    } else {
        Json(ApiResponse::<HeadlessOnlyResponse>::error(
            "Bridge manager not initialized",
        ))
    }
}

/// Set headless-only mode
///
/// When enabled, all bridges must be created in headless mode.
/// GUI mode requests will be rejected with an error.
async fn set_headless_only(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetHeadlessOnlyRequest>,
) -> Json<ApiResponse<HeadlessOnlyResponse>> {
    info!("Setting headless-only mode to: {}", request.enabled);

    let bridge_manager_guard = state.app_state.bridge_manager.lock().await;

    if let Some(ref bridge_manager) = *bridge_manager_guard {
        bridge_manager.set_headless_only(request.enabled);

        Json(ApiResponse::success(HeadlessOnlyResponse {
            enabled: request.enabled,
            description: if request.enabled {
                "Headless-only mode is now ENABLED. All bridges must be headless. \
                GUI mode bridges cannot be created."
                    .to_string()
            } else {
                "Headless-only mode is now DISABLED. Both GUI and headless bridges can be created."
                    .to_string()
            },
        }))
    } else {
        Json(ApiResponse::<HeadlessOnlyResponse>::error(
            "Bridge manager not initialized",
        ))
    }
}

// ============================================================================
// Debug Endpoints
// ============================================================================

/// A parsed error entry from log files
#[derive(Debug, Clone, Serialize)]
struct DebugError {
    /// Timestamp of the error
    timestamp: String,
    /// Service that generated the error (backend, frontend, api, runner)
    service: String,
    /// Log level (error, warning)
    level: String,
    /// Error message
    message: String,
    /// Optional stack trace or additional context
    context: Option<String>,
}

/// Summary of errors by category
#[derive(Debug, Clone, Serialize)]
struct DebugErrorSummary {
    /// Total errors found
    total: usize,
    /// Errors by service
    by_service: std::collections::HashMap<String, usize>,
    /// Errors by level
    by_level: std::collections::HashMap<String, usize>,
}

/// Response from /debug/app/errors endpoint
#[derive(Debug, Clone, Serialize)]
struct DebugErrorsResponse {
    /// Summary statistics
    summary: DebugErrorSummary,
    /// Individual errors (most recent first)
    errors: Vec<DebugError>,
}

/// Query parameters for /debug/app/errors
#[derive(Debug, Deserialize)]
struct DebugErrorsQuery {
    /// Maximum number of errors to return (default: 50)
    limit: Option<usize>,
    /// Filter by service (backend, frontend, api, runner)
    service: Option<String>,
    /// Filter by level (error, warning)
    level: Option<String>,
}

/// Get application errors from dev-logs
///
/// Parses log files from .dev-logs/ and returns structured error information.
async fn get_debug_errors(
    axum::extract::Query(query): axum::extract::Query<DebugErrorsQuery>,
) -> Json<ApiResponse<DebugErrorsResponse>> {
    use std::io::{BufRead, BufReader};

    let dev_logs_path = crate::paths::get_dev_logs_dir();

    if !dev_logs_path.exists() {
        return Json(ApiResponse::success(DebugErrorsResponse {
            summary: DebugErrorSummary {
                total: 0,
                by_service: std::collections::HashMap::new(),
                by_level: std::collections::HashMap::new(),
            },
            errors: vec![],
        }));
    }

    let limit = query.limit.unwrap_or(50);
    let mut all_errors: Vec<DebugError> = Vec::new();

    // Build log file list from global settings
    let global_settings = crate::settings::get_global_log_source_settings();
    let log_files: Vec<(String, String)> = if global_settings.sources.is_empty() {
        // Fallback if no sources configured
        vec![
            ("backend.log".to_string(), "backend".to_string()),
            ("frontend.log".to_string(), "frontend".to_string()),
            ("runner-tauri.log".to_string(), "runner".to_string()),
            (
                "runner-actions.jsonl".to_string(),
                "runner-actions".to_string(),
            ),
        ]
    } else {
        global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| {
                let filename = s.path.clone();
                let service = s.name.to_lowercase().replace(' ', "-");
                (filename, service)
            })
            .collect()
    };

    // Regex patterns for error detection (compiled once)
    static RE_ERROR_1: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_ERROR_2: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_WARNING: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_TIMESTAMP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_DATE_PREFIX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    let re_error_1 =
        RE_ERROR_1.get_or_init(|| Regex::new(r"(?i)(error|exception|traceback|failed)").unwrap());
    let re_error_2 =
        RE_ERROR_2.get_or_init(|| Regex::new(r"(?i)(ERROR|error:|\[error\])").unwrap());
    let re_warning = RE_WARNING.get_or_init(|| Regex::new(r"(?i)(warning|warn|\[warn\])").unwrap());
    let re_timestamp = RE_TIMESTAMP
        .get_or_init(|| Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})").unwrap());
    let re_date_prefix = RE_DATE_PREFIX.get_or_init(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap());

    let error_patterns: &[(&Regex, &str)] = &[
        // Python/FastAPI errors
        (re_error_1, "error"),
        // TypeScript/Next.js errors
        (re_error_2, "error"),
        // Warnings
        (re_warning, "warning"),
    ];

    for (filename, service) in &log_files {
        // Apply service filter if specified
        if let Some(ref svc_filter) = query.service {
            if !service.eq_ignore_ascii_case(svc_filter) {
                continue;
            }
        }

        // If the path is absolute, use it directly; otherwise join with dev_logs_path
        let source_path = std::path::Path::new(filename);
        let file_path = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            dev_logs_path.join(filename)
        };
        if !file_path.exists() {
            continue;
        }

        if let Ok(file) = std::fs::File::open(&file_path) {
            let reader = BufReader::new(file);
            let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

            // Process from end (most recent) to beginning
            let mut i = lines.len();
            while i > 0 {
                i -= 1;
                let line = &lines[i];

                // Determine log level
                let mut level = None;
                for (re, lvl) in error_patterns {
                    if re.is_match(line) {
                        level = Some(*lvl);
                        break;
                    }
                }

                if let Some(lvl) = level {
                    // Apply level filter if specified
                    if let Some(ref lvl_filter) = query.level {
                        if !lvl.eq_ignore_ascii_case(lvl_filter) {
                            continue;
                        }
                    }

                    // Extract timestamp if present (various formats)
                    let timestamp = re_timestamp
                        .captures(line)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default();

                    // Collect context (surrounding lines for stack traces)
                    let mut context_lines = Vec::new();
                    let mut j = i + 1;
                    while j < lines.len() && j < i + 10 {
                        let ctx_line = &lines[j];
                        // Stop at next log entry (has timestamp or is empty)
                        if ctx_line.is_empty() || re_date_prefix.is_match(ctx_line) {
                            break;
                        }
                        context_lines.push(ctx_line.clone());
                        j += 1;
                    }

                    let context = if context_lines.is_empty() {
                        None
                    } else {
                        Some(context_lines.join("\n"))
                    };

                    all_errors.push(DebugError {
                        timestamp,
                        service: service.to_string(),
                        level: lvl.to_string(),
                        message: line.clone(),
                        context,
                    });
                }
            }
        }
    }

    // Sort by timestamp (most recent first)
    all_errors.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Build summary before truncating
    let mut by_service: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut by_level: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for err in &all_errors {
        *by_service.entry(err.service.clone()).or_insert(0) += 1;
        *by_level.entry(err.level.clone()).or_insert(0) += 1;
    }

    let total = all_errors.len();

    // Truncate to limit
    all_errors.truncate(limit);

    Json(ApiResponse::success(DebugErrorsResponse {
        summary: DebugErrorSummary {
            total,
            by_service,
            by_level,
        },
        errors: all_errors,
    }))
}

/// Get findings summary from database
///
/// Returns a summary of issues detected in previous sessions.
/// If task_run_id query parameter is provided, returns findings only for that task run.
/// Otherwise returns findings from the most recent task runs.
async fn get_findings_summary(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    // Get optional task_run_id filter
    let task_run_id_filter = params.get("task_run_id").cloned();

    // Get the database path using the same pattern as context.rs
    let app_data_dir = match dirs::config_dir() {
        Some(config_dir) => config_dir.join("com.qontinui.runner"),
        None => {
            return Json(ApiResponse::success(serde_json::json!({
                "total_findings": 0,
                "code_related_findings": 0,
                "by_severity": {},
                "findings": [],
                "error": "Could not find config directory"
            })));
        }
    };
    let db_path = app_data_dir.join("runner.db");

    let db = match rusqlite::Connection::open(&db_path) {
        Ok(conn) => conn,
        Err(e) => {
            return Json(ApiResponse::success(serde_json::json!({
                "total_findings": 0,
                "code_related_findings": 0,
                "by_severity": {},
                "findings": [],
                "error": format!("Failed to open database: {}", e)
            })));
        }
    };

    // Get findings based on filter
    let mut all_findings = Vec::new();
    if let Some(task_run_id) = task_run_id_filter {
        // Filter to specific task run
        if let Ok(findings) = finding_storage::get_findings_for_task(&db, &task_run_id) {
            all_findings.extend(findings);
        }
    } else {
        // Get recent task run IDs (fallback for when no specific run is requested)
        let task_run_ids: Vec<String> =
            match db.prepare("SELECT id FROM task_runs ORDER BY created_at DESC LIMIT 5") {
                Ok(mut stmt) => stmt
                    .query_map([], |row| row.get(0))
                    .ok()
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default(),
                Err(_) => vec![],
            };

        for task_run_id in &task_run_ids {
            if let Ok(findings) = finding_storage::get_findings_for_task(&db, task_run_id) {
                all_findings.extend(findings);
            }
        }
    }

    let total = all_findings.len();

    // Count by severity
    let mut by_severity: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut code_related = 0;

    for finding in &all_findings {
        *by_severity
            .entry(finding.severity.as_str().to_string())
            .or_insert(0) += 1;
        if finding
            .code_context
            .as_ref()
            .and_then(|c| c.file.as_ref())
            .is_some()
        {
            code_related += 1;
        }
    }

    let response = serde_json::json!({
        "total_findings": total,
        "code_related_findings": code_related,
        "by_severity": by_severity,
        "findings": all_findings.iter().take(20).collect::<Vec<_>>()
    });

    Json(ApiResponse::success(response))
}

/// Launch Chrome with remote debugging enabled
async fn launch_debug_chrome() -> Json<ApiResponse<String>> {
    use std::process::Command;

    // Common Chrome paths on Windows
    let chrome_paths = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ];

    let chrome_path = chrome_paths
        .iter()
        .find(|p| std::path::Path::new(p).exists());

    match chrome_path {
        Some(path) => {
            // First, kill all existing Chrome processes
            // The debug port only works on the FIRST Chrome instance
            info!("Killing existing Chrome processes...");
            let _ = Command::new("taskkill")
                .args(["/F", "/IM", "chrome.exe"])
                .output();

            // Wait a moment for processes to terminate
            std::thread::sleep(std::time::Duration::from_millis(1000));

            // Now launch Chrome with debug flag and separate profile
            // Using a separate user-data-dir ensures the debug port works
            // even if Chrome would normally restore a previous session
            match Command::new(path)
                .args([
                    "--remote-debugging-port=9222",
                    "--user-data-dir=C:\\temp\\chrome-debug-profile",
                ])
                .spawn()
            {
                Ok(_) => {
                    info!("Launched Chrome with remote debugging on port 9222");
                    Json(ApiResponse::success(
                        "Chrome launched with debugging enabled".to_string(),
                    ))
                }
                Err(e) => {
                    error!("Failed to launch Chrome: {}", e);
                    Json(ApiResponse {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to launch Chrome: {}", e)),
                    })
                }
            }
        }
        None => {
            error!("Chrome not found at expected paths");
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some("Chrome not found. Please close Chrome and launch it manually with: chrome.exe --remote-debugging-port=9222".to_string()),
            })
        }
    }
}

/// Get available monitors with position information
async fn get_monitors(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<MonitorsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_handle = state.app_handle.clone();

    let window = app_handle.get_webview_window("main").ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Failed to get main window")),
        )
    })?;

    let monitors = window.available_monitors().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get monitors: {}", e))),
        )
    })?;

    let primary_monitor = window.current_monitor().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get current monitor: {}", e))),
        )
    })?;

    // Collect x positions for determining spatial layout
    let x_positions: Vec<i32> = monitors.iter().map(|m| m.position().x).collect();
    let min_x = x_positions.iter().min().copied().unwrap_or(0);
    let max_x = x_positions.iter().max().copied().unwrap_or(0);

    // Build monitor info with positions matching qontinui-schemas/geometry
    let monitor_infos: Vec<MonitorInfoResponse> = monitors
        .iter()
        .enumerate()
        .map(|(idx, monitor)| {
            let mon_position = monitor.position();
            let mon_size = monitor.size();
            let scale_factor = monitor.scale_factor();
            let name = monitor.name().map(|n| n.to_string());

            let is_primary = match &primary_monitor {
                Some(current) => {
                    let current_pos = current.position();
                    let current_size = current.size();
                    mon_position.x == current_pos.x
                        && mon_position.y == current_pos.y
                        && mon_size.width == current_size.width
                        && mon_size.height == current_size.height
                }
                None => idx == 0,
            };

            // Determine position based on x coordinate (matches schema: "left", "center", "right")
            let position = if monitors.len() == 1 {
                "center".to_string()
            } else if mon_position.x == min_x {
                "left".to_string()
            } else if mon_position.x == max_x {
                "right".to_string()
            } else {
                "center".to_string()
            };

            // Build description
            let mut desc_parts = vec![format!("Monitor {}", idx)];
            if is_primary {
                desc_parts.push("primary".to_string());
            }
            desc_parts.push(position.clone());
            desc_parts.push(format!("{}x{}", mon_size.width, mon_size.height));
            let description = format!("{} ({})", desc_parts[0], desc_parts[1..].join(", "));

            MonitorInfoResponse {
                index: idx,
                x: mon_position.x,
                y: mon_position.y,
                width: mon_size.width,
                height: mon_size.height,
                position,
                is_primary: Some(is_primary),
                scale_factor: Some(scale_factor),
                name,
                description: Some(description),
            }
        })
        .collect();

    // Build available descriptors
    let mut descriptors = vec!["primary".to_string()];
    for m in &monitor_infos {
        if !descriptors.contains(&m.position) {
            descriptors.push(m.position.clone());
        }
    }
    for m in &monitor_infos {
        descriptors.push(m.index.to_string());
    }

    Ok(Json(ApiResponse::success(MonitorsResponse {
        count: monitor_infos.len(),
        monitors: monitor_infos,
        available_descriptors: descriptors,
    })))
}

/// Get executor status
async fn get_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<StatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Clone Arc for use in spawn_blocking
    let app_state = state.app_state.clone();

    // Run blocking operations in a separate thread to avoid blocking the async runtime
    let result = tokio::task::spawn_blocking(move || {
        // Use with_default_bridge helper for bridge access
        let (executor_running, executor_state) = match with_default_bridge(&app_state, |bridge| {
            (bridge.is_running(), bridge.get_state().name().to_string())
        }) {
            Ok(result) => result,
            Err(_) => (false, "not_started".to_string()),
        };

        // Use unwrap_or_else to recover from poisoned mutex
        let config_lock = app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        });
        let config_loaded = config_lock.is_some();
        drop(config_lock);

        let config_path = crate::settings::get_last_config_path();

        (executor_running, executor_state, config_loaded, config_path)
    })
    .await
    .map_err(|e| {
        error!("Failed to get status: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    // Check AI analysis status using async version to avoid blocking
    let ai_running = has_running_ai_tasks_async(state.app_state.checkpoint_db.clone()).await;

    Ok(Json(ApiResponse::success(StatusResponse {
        executor_running: result.0,
        executor_state: result.1,
        config_loaded: result.2,
        config_path: result.3,
        ai_analysis_running: ai_running,
    })))
}

/// Get tool version for MCP caching
///
/// Returns a version hash based on:
/// - Current config ID (if loaded)
/// - Number of tests in the database
///
/// MCP clients can use this to invalidate their tool cache when
/// the available tools change (e.g., new tests added).
async fn get_tool_version(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ToolVersionResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use sha2::{Digest, Sha256};

    // Get current config ID
    let config_id = state
        .current_config_id
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| "none".to_string());

    // Get test count from database using list_verification_tests
    let db = state.app_state.checkpoint_db.clone();
    let test_count = tokio::task::spawn_blocking(move || {
        db.list_verification_tests(false, None, None)
            .map(|tests| tests.len())
            .unwrap_or(0)
    })
    .await
    .unwrap_or(0);

    // Base tool count (from qontinui-mcp server.py TOOLS list)
    // This should be kept in sync with the actual tool count
    const BASE_TOOL_COUNT: usize = 35;

    // Create version hash from config_id and test_count
    let version_input = format!("{}:{}", config_id, test_count);
    let mut hasher = Sha256::new();
    hasher.update(version_input.as_bytes());
    let hash = hasher.finalize();
    let version = format!("{:x}", hash)[..8].to_string();

    let last_updated = chrono::Utc::now().to_rfc3339();

    Ok(Json(ApiResponse::success(ToolVersionResponse {
        version,
        tool_count: BASE_TOOL_COUNT,
        test_count,
        last_updated,
    })))
}

/// Internal helper to load a configuration file synchronously.
/// Used by resume_active_workflow_on_startup to load config before resuming.
///
/// This performs the core config loading logic:
/// 1. Loads and validates the configuration file
/// 2. Stores it in the app state (current_config)
/// 3. Sends debug settings to the Python executor
/// 4. Sends the configuration to the Python executor
pub fn load_config_internal(
    app_state: &Arc<crate::AppState>,
    config_path: &str,
) -> Result<String, String> {
    // Step 1: Load and validate the configuration file
    let config = ConfigLoader::load_from_file(config_path).map_err(|e| {
        error!(
            "load_config_internal: Failed to load configuration from {}: {}",
            config_path, e
        );
        format!("Failed to load configuration: {}", e)
    })?;

    let summary = config.summary();
    info!("load_config_internal: Configuration validated: {}", summary);

    // Set project context for runtime environment
    crate::runtime_env::set_project_context(crate::runtime_env::ProjectContext {
        project_id: config.metadata.project_id.clone(),
        workspace_id: None, // TODO: Add workspace_id to ConfigMetadata if needed
        name: Some(config.metadata.name.clone()),
        triggered_by: None, // Set dynamically when task is triggered via API
    });

    // Step 2: Store the configuration in app state
    *app_state.current_config.lock().unwrap_or_else(|poisoned| {
        warn!("load_config_internal: current_config mutex was poisoned, recovering");
        poisoned.into_inner()
    }) = Some(config);
    info!("load_config_internal: Configuration stored in app state");

    // Step 3: Send debug settings and configuration to Python bridge
    let config_path_owned = config_path.to_string();
    let summary_clone = summary.clone();
    match with_default_bridge(app_state, |bridge| {
        if !bridge.is_running() {
            warn!("load_config_internal: Python executor not running, config stored but not sent to executor");
            return Ok(summary_clone.clone());
        }

        // Send debug settings first (before config execution)
        let debug_settings = settings::get_debug_settings();
        if let Err(e) = bridge.set_debug_settings(
            debug_settings.enable_image_debug,
            debug_settings.top_matches_count,
        ) {
            warn!("load_config_internal: Failed to send debug settings: {}", e);
        } else {
            info!(
                "load_config_internal: Debug settings sent: enable={}, top_matches={}",
                debug_settings.enable_image_debug, debug_settings.top_matches_count
            );
        }

        // Send configuration to Python
        bridge.load_configuration(&config_path_owned).map_err(|e| {
            error!(
                "load_config_internal: Failed to send configuration to Python: {}",
                e
            );
            format!("Failed to send configuration to Python: {}", e)
        })?;

        info!("load_config_internal: Configuration sent to Python executor");
        Ok(summary_clone.clone())
    }) {
        Ok(result) => result,
        Err(_) => {
            warn!(
                "load_config_internal: Python executor not initialized, config stored but not sent"
            );
            Ok(summary)
        }
    }
}

/// Load a configuration file
///
/// This mirrors the behavior from commands/config.rs:
/// 1. Loads and validates the configuration file
/// 2. Stores it in the app state (current_config)
/// 3. Saves the path for auto-load functionality
/// 4. Sends debug settings to the Python executor
/// 5. Sends the configuration to the Python executor
#[tracing::instrument(
    name = "api.request.load_config",
    skip(state, request),
    fields(
        endpoint = "/load-config",
        method = "POST",
        config_path = %request.config_path
    )
)]
async fn load_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<LoadConfigRequest>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading config: {}", request.config_path);

    let app_state = state.app_state.clone();
    let config_path = request.config_path.clone();
    let config_path_for_event = request.config_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Step 1: Load and validate the configuration file
        let config = ConfigLoader::load_from_file(&config_path).map_err(|e| {
            error!(
                "MCP API: Failed to load configuration from {}: {}",
                config_path, e
            );
            format!("Failed to load configuration: {}", e)
        })?;

        let summary = config.summary();
        info!("MCP API: Configuration validated: {}", summary);

        // Create config data for event emission (including metadata for projectId)
        let config_data = serde_json::json!({
            "metadata": config.metadata.clone(),
            "workflows": config.workflows.clone(),
            "states": config.states.clone(),
            "transitions": config.transitions.clone(),
            "images": config.images.clone()
        });

        // Step 2: Store the configuration in app state
        *app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        }) = Some(config);
        info!("MCP API: Configuration stored in app state");

        // Step 3: Save the path as the last loaded config
        if let Err(e) = settings::save_last_config_path(&config_path) {
            warn!("MCP API: Failed to save last config path: {}", e);
        }

        // Step 4 & 5: Send debug settings and configuration to Python bridge
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send debug settings first (before config execution)
            let debug_settings = settings::get_debug_settings();
            if let Err(e) = bridge.set_debug_settings(
                debug_settings.enable_image_debug,
                debug_settings.top_matches_count,
            ) {
                warn!("MCP API: Failed to send debug settings: {}", e);
            } else {
                info!(
                    "MCP API: Debug settings sent: enable={}, top_matches={}",
                    debug_settings.enable_image_debug, debug_settings.top_matches_count
                );
            }

            // Send configuration to Python
            bridge.load_configuration(&config_path).map_err(|e| {
                error!("MCP API: Failed to send configuration to Python: {}", e);
                format!("Failed to send configuration to Python: {}", e)
            })?;

            info!("MCP API: Configuration sent to Python executor");
            Ok((summary.clone(), config_data.clone()))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok((summary, config_data)) => {
            info!("MCP API: Config loaded successfully");

            // Debug: Log metadata being sent
            if let Some(metadata) = config_data.get("metadata") {
                info!("MCP API: Config metadata being emitted: {:?}", metadata);
            } else {
                warn!("MCP API: No metadata in config_data!");
            }

            // Auto-add to ConfigStorage (database)
            // Generate ID from project_id in config metadata, or from file path
            let config_id = config_data
                .get("metadata")
                .and_then(|m| m.get("projectId"))
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| generate_id_from_path(&config_path_for_event));

            let config_name = config_data
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| path_to_name(&config_path_for_event));

            // Upsert: update if exists, insert if new
            if let Err(e) = state.app_state.checkpoint_db.save_config_with_id(
                &config_id,
                config_data.clone(),
                &config_name,
                "file",
                Some(&config_path_for_event),
            ) {
                warn!(
                    "MCP API: Failed to auto-store config in ConfigStorage: {}",
                    e
                );
            } else {
                info!(
                    "MCP API: Auto-stored config '{}' with id '{}' in ConfigStorage",
                    config_name, config_id
                );
                // Store current config ID
                if let Ok(mut current_id) = state.current_config_id.lock() {
                    *current_id = Some(config_id);
                }
            }

            // Emit event to notify frontend of config load
            let event_payload = serde_json::json!({
                "event": "config_loaded",
                "data": {
                    "path": config_path_for_event,
                    "config": config_data
                }
            });

            if let Err(e) = state.app_handle.emit("executor-event", &event_payload) {
                warn!("MCP API: Failed to emit config_loaded event: {}", e);
            } else {
                info!("MCP API: Emitted config_loaded event to frontend");
            }

            Ok(Json(ApiResponse::success(summary)))
        }
        Err(e) => {
            error!("MCP API: Failed to load config: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Run a workflow by name and wait for completion
///
/// Uses the UnifiedActionService for deterministic execution, ensuring both
/// manual API calls and AI task execution use the same code path.
///
/// Creates a TaskRun record (task_type='automation') to ensure all automation
/// runs are tracked in the unified TaskRun system.
#[tracing::instrument(
    name = "api.request.run_workflow",
    skip(state, request),
    fields(
        endpoint = "/run-workflow",
        method = "POST",
        workflow_name = %request.workflow_name,
        monitor_index = ?request.monitor_index,
        timeout_seconds = ?request.timeout_seconds
    )
)]
async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowRequest>,
) -> Result<Json<ApiResponse<WorkflowExecutionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running workflow: {} (timeout: {:?})",
        request.workflow_name, request.timeout_seconds
    );
    safe_eprintln!(
        "[MCP_API] run_workflow received: workflow={}, monitor_index={:?}",
        request.workflow_name,
        request.monitor_index
    );

    // Get current config_id for linking automation to config
    let config_id = state
        .current_config_id
        .lock()
        .ok()
        .and_then(|guard| guard.clone());

    // Create a TaskRun for this automation execution
    // This ensures ALL automation runs go through the unified TaskRun system
    let task_recorder = TaskRecorder::new(state.app_state.checkpoint_db.clone());
    let task_config = TaskConfig::automation_task(
        &format!("Workflow: {}", request.workflow_name),
        config_id.as_deref().unwrap_or("unknown"),
        Some(&request.workflow_name),
    );

    let task_handle = match task_recorder.start_task(task_config) {
        Ok(handle) => {
            info!(
                "MCP API: Created TaskRun {} for workflow {}",
                handle.id(),
                request.workflow_name
            );
            handle
        }
        Err(e) => {
            error!("MCP API: Failed to create TaskRun: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create task run: {}", e))),
            ));
        }
    };

    // Link the run_recording_handler to this task run
    // This ensures automation metrics go to task_run_automation table
    // session_num=1 for the initial run
    state
        .app_state
        .run_recording_handler
        .set_task_run(task_handle.id().to_string(), 1, 1)
        .await;

    // Use UnifiedActionService for deterministic execution
    let action_service = state.action_service.clone();

    let result = action_service
        .run_workflow(
            &request.workflow_name,
            None, // No additional config
            request.monitor_index,
            request.timeout_seconds,
            None, // No initial state override from MCP API
        )
        .await;

    // Clear the task_run link after execution
    state.app_state.run_recording_handler.clear_task_run().await;

    match result {
        Ok(workflow_result) => {
            info!(
                "MCP API: Workflow completed via UnifiedActionService: success={}, error={:?}",
                workflow_result.success, workflow_result.error
            );

            // Update task run status based on workflow result
            if workflow_result.success {
                if let Err(e) = task_handle.complete() {
                    warn!("MCP API: Failed to complete task run: {}", e);
                }
            } else {
                let error_msg = workflow_result
                    .error
                    .as_deref()
                    .unwrap_or("Workflow failed");
                if let Err(e) = task_handle.fail(error_msg) {
                    warn!("MCP API: Failed to mark task run as failed: {}", e);
                }
            }

            Ok(Json(ApiResponse::success(WorkflowExecutionResult {
                success: workflow_result.success,
                workflow_name: workflow_result.workflow_name,
                error: workflow_result.error,
            })))
        }
        Err(e) => {
            error!("MCP API: Workflow execution failed: {}", e);

            // Mark task run as failed
            if let Err(fail_err) = task_handle.fail(&e.to_string()) {
                warn!("MCP API: Failed to mark task run as failed: {}", fail_err);
            }

            match e {
                crate::action_service::ActionError::Timeout(seconds) => Err((
                    StatusCode::REQUEST_TIMEOUT,
                    Json(api_error(format!(
                        "Workflow execution timed out after {} seconds",
                        seconds
                    ))),
                )),
                crate::action_service::ActionError::ExecutorNotRunning => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not running")),
                )),
                crate::action_service::ActionError::ExecutorNotInitialized => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not initialized")),
                )),
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )),
            }
        }
    }
}

// ============================================================================
// Execute Steps Endpoint - Unified Step Execution
// ============================================================================

/// Request to execute a list of steps
#[derive(Debug, Deserialize)]
struct ExecuteStepsRequest {
    /// Steps to execute
    steps: Vec<crate::step_executor::ExecutionStepConfig>,
    /// Optional execution ID (generated if not provided)
    #[serde(default)]
    execution_id: Option<String>,
    /// Log sources to capture during execution
    #[serde(default)]
    log_sources: Vec<crate::step_executor::LogSourceConfig>,
    /// Optional task run ID for database logging (AWAS steps, etc.)
    #[serde(default)]
    task_run_id: Option<String>,
}

/// Execute a list of steps and return results
///
/// This is the unified execution endpoint used by:
/// - Run page (single workflow step)
/// - AI Builder (multi-step before AI session)
/// - MCP API (direct step execution)
///
/// Running a single workflow from the Run page is just:
/// `{ "steps": [{ "type": "workflow", "name": "MyWorkflow" }] }`
#[tracing::instrument(
    name = "api.request.execute_steps",
    skip(state, request),
    fields(
        endpoint = "/execute-steps",
        method = "POST",
        step_count = %request.steps.len(),
        execution_id = ?request.execution_id
    )
)]
async fn execute_steps(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteStepsRequest>,
) -> Result<
    Json<ApiResponse<crate::step_executor::ExecutionResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let execution_id = request.execution_id.unwrap_or_else(|| {
        format!(
            "exec-{}-{}",
            chrono::Utc::now().format("%Y%m%d%H%M%S"),
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("0000")
        )
    });

    info!(
        "MCP API: Executing {} steps (execution_id: {})",
        request.steps.len(),
        execution_id
    );

    // Create step executor with app handle for frontend event emission
    let mut executor = crate::step_executor::StepExecutor::with_app_handle(
        state.app_state.clone(),
        state.config_storage.clone(),
        state.app_handle.clone(),
    );

    // Add task_run_id if provided (enables AWAS step result logging to database)
    if let Some(task_run_id) = request.task_run_id {
        executor = executor.with_task_run_id(task_run_id);
    }

    // Execute all steps with log source capture
    let result = executor
        .execute_steps_with_log_sources(&request.steps, &execution_id, &request.log_sources)
        .await;

    info!(
        "MCP API: Execution complete - {} of {} steps succeeded",
        result.successful_steps, result.total_steps
    );

    Ok(Json(ApiResponse::success(result)))
}

/// Response for load-last-config endpoint
#[derive(Debug, Serialize)]
pub struct LoadLastConfigResponse {
    pub config_path: String,
    pub workflow_id: Option<String>,
    pub monitor_index: Option<i32>,
    pub summary: String,
}

/// Load the last used configuration, workflow, and monitor from settings
///
/// This is useful after a runner restart to restore the previous state.
/// It reads saved settings and loads the configuration just like load_config.
async fn load_last_config(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<LoadLastConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading last configuration from settings");

    // First, get the saved settings
    let config_path = match settings::get_last_config_path() {
        Some(path) => path,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error("No last configuration found")),
            ))
        }
    };

    let workflow_id = settings::get_last_workflow_id();
    let monitor_index = settings::get_last_monitor_index();

    info!(
        "MCP API: Found last config: path={}, workflow={:?}, monitor={:?}",
        config_path, workflow_id, monitor_index
    );

    // Check if the file still exists
    if !std::path::Path::new(&config_path).exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Last configuration file no longer exists: {}",
                config_path
            ))),
        ));
    }

    let app_state = state.app_state.clone();
    let config_path_clone = config_path.clone();
    let config_path_for_event = config_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Load and validate the configuration file
        let config = ConfigLoader::load_from_file(&config_path_clone).map_err(|e| {
            error!(
                "MCP API: Failed to load configuration from {}: {}",
                config_path_clone, e
            );
            format!("Failed to load configuration: {}", e)
        })?;

        let summary = config.summary();
        info!("MCP API: Configuration validated: {}", summary);

        // Create config data for event emission (including metadata for projectId)
        let config_data = serde_json::json!({
            "metadata": config.metadata.clone(),
            "workflows": config.workflows.clone(),
            "states": config.states.clone(),
            "transitions": config.transitions.clone(),
            "images": config.images.clone()
        });

        // Store the configuration in app state
        *app_state.current_config.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: current_config mutex was poisoned, recovering");
            poisoned.into_inner()
        }) = Some(config);
        info!("MCP API: Configuration stored in app state");

        // Send debug settings and configuration to Python bridge
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send debug settings first
            let debug_settings = settings::get_debug_settings();
            if let Err(e) = bridge.set_debug_settings(
                debug_settings.enable_image_debug,
                debug_settings.top_matches_count,
            ) {
                warn!("MCP API: Failed to send debug settings: {}", e);
            }

            // Send configuration to Python
            bridge.load_configuration(&config_path_clone).map_err(|e| {
                error!("MCP API: Failed to send configuration to Python: {}", e);
                format!("Failed to send configuration to Python: {}", e)
            })?;

            info!("MCP API: Configuration sent to Python executor");
            Ok((summary.clone(), config_data.clone()))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok((summary, config_data)) => {
            info!("MCP API: Last config loaded successfully");

            // Emit event to notify frontend of config load
            let event_payload = serde_json::json!({
                "event": "config_loaded",
                "data": {
                    "path": config_path_for_event,
                    "config": config_data,
                    "workflow_id": workflow_id,
                    "monitor_index": monitor_index
                }
            });

            if let Err(e) = state.app_handle.emit("executor-event", &event_payload) {
                warn!("MCP API: Failed to emit config_loaded event: {}", e);
            } else {
                info!("MCP API: Emitted config_loaded event to frontend");
            }

            Ok(Json(ApiResponse::success(LoadLastConfigResponse {
                config_path,
                workflow_id,
                monitor_index,
                summary,
            })))
        }
        Err(e) => {
            error!("MCP API: Failed to load last config: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Stop the current execution
async fn stop_execution(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping execution");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| match bridge.stop_execution() {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Failed to stop execution: {}", e)),
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Execution stopped");
            Ok(Json(ApiResponse::success("Execution stopped".to_string())))
        }
        Err(e) => {
            error!("MCP API: Failed to stop execution: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute a Python command via the executor bridge
///
/// This endpoint forwards commands to the Python executor and returns the result.
/// Used by the accessibility service and other features that need to communicate
/// with the Python executor via HTTP (e.g., for frontend components that can't
/// use Tauri IPC directly).
async fn execute_python_command(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecutePythonCommandRequest>,
) -> Result<Json<ApiResponse<ExecutePythonCommandResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Executing Python command: {} with params: {:?}",
        request.cmd_type, request.params
    );

    let app_state = state.app_state.clone();
    let cmd_type = request.cmd_type.clone();
    let cmd_type_for_log = cmd_type.clone(); // Clone for logging after closure
    let params = request.params.clone();

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Convert params to Option<Value> (None if params is null or empty object)
            let params_option = if params.is_null()
                || (params.is_object() && params.as_object().is_none_or(|o| o.is_empty()))
            {
                None
            } else {
                Some(params)
            };

            // Use configurable timeout (default: disabled)
            // Falls back to 1 hour to prevent infinite IPC hangs
            let timeout_duration =
                Timeouts::python_command().unwrap_or_else(|| std::time::Duration::from_secs(3600));
            bridge.send_command_and_wait(&cmd_type, params_option, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            info!(
                "MCP API: Python command {} completed, success={}",
                cmd_type_for_log, response.success
            );
            Ok(Json(ApiResponse::success(ExecutePythonCommandResponse {
                success: response.success,
                error: response.error,
                data: response.data,
            })))
        }
        Err(e) => {
            error!("MCP API: Python command {} failed: {}", cmd_type_for_log, e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute a single action (e.g., click on an image, type text, press hotkey)
///
/// This endpoint allows executing individual GUI actions without running a full workflow.
/// Uses the UnifiedActionService for deterministic execution, ensuring both
/// manual API calls and AI task execution use the same code path.
#[tracing::instrument(
    name = "api.request.execute_action",
    skip(state, request),
    fields(
        endpoint = "/execute-action",
        method = "POST",
        action_type = %request.action_type,
        image_id = %request.image_id,
        monitor_index = ?request.monitor_index
    )
)]
async fn execute_action(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteActionRequest>,
) -> Result<Json<ApiResponse<ExecuteActionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Executing action: {} (image: {}, text: {:?}, hotkey: {:?}, timeout: {:?})",
        request.action_type,
        request.image_id,
        request.text_input,
        request.hotkey,
        request.timeout_seconds
    );

    // Use UnifiedActionService for deterministic execution
    let action_service = state.action_service.clone();
    let action_type = request.action_type.clone();
    let image_id = request.image_id.clone();

    // Build config for TYPE and HOTKEY actions
    let config = if let Some(ref text) = request.text_input {
        Some(serde_json::json!({ "text": text }))
    } else {
        request
            .hotkey
            .as_ref()
            .map(|hotkey| serde_json::json!({ "hotkey": hotkey }))
    };

    match action_service
        .execute_action(
            &request.action_type,
            &request.image_id,
            config.as_ref(),
            request.monitor_index,
        )
        .await
    {
        Ok(result) => {
            let action_result = ExecuteActionResult {
                success: result.success,
                action_type: action_type.clone(),
                image_id: image_id.clone(),
                error: if result.success { None } else { result.message },
            };

            if action_result.success {
                info!(
                    "MCP API: Action {} on image {} succeeded via UnifiedActionService",
                    action_result.action_type, action_result.image_id
                );
            } else {
                warn!(
                    "MCP API: Action {} on image {} failed: {:?}",
                    action_result.action_type, action_result.image_id, action_result.error
                );
            }
            Ok(Json(ApiResponse::success(action_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to execute action: {}", e);
            match e {
                crate::action_service::ActionError::ExecutorNotRunning => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not running")),
                )),
                crate::action_service::ActionError::ExecutorNotInitialized => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not initialized")),
                )),
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )),
            }
        }
    }
}

// ============================================================================
// State Navigation API Endpoint
// ============================================================================

/// Navigate to a target state using pathfinding
///
/// This endpoint uses the state machine to find and execute the path
/// from the current state to the target state.
#[tracing::instrument(
    name = "api.request.go_to_state",
    skip(state, request),
    fields(
        endpoint = "/go-to-state",
        method = "POST",
        state_id = %request.state_id,
        timeout_seconds = %request.timeout_seconds
    )
)]
async fn go_to_state(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GoToStateRequest>,
) -> Result<Json<ApiResponse<GoToStateResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Navigating to state: {} (timeout: {}s)",
        request.state_id, request.timeout_seconds
    );

    // Use UnifiedActionService for deterministic execution
    let action_service = state.action_service.clone();
    let state_id = request.state_id.clone();

    match action_service
        .go_to_state(
            &request.state_id,
            None, // No additional config
            request.monitor_index,
            Some(request.timeout_seconds),
        )
        .await
    {
        Ok(result) => {
            let nav_result = GoToStateResult {
                success: result.success,
                state_id: state_id.clone(),
                error: result.error,
            };

            if nav_result.success {
                info!(
                    "MCP API: Successfully navigated to state {} via UnifiedActionService",
                    nav_result.state_id
                );
            } else {
                warn!(
                    "MCP API: Failed to navigate to state {}: {:?}",
                    nav_result.state_id, nav_result.error
                );
            }
            Ok(Json(ApiResponse::success(nav_result)))
        }
        Err(e) => {
            error!("MCP API: Failed to navigate to state: {}", e);
            match e {
                crate::action_service::ActionError::ExecutorNotRunning => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not running")),
                )),
                crate::action_service::ActionError::ExecutorNotInitialized => Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error("Python executor not initialized")),
                )),
                _ => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(e.to_string())),
                )),
            }
        }
    }
}

// ============================================================================
// Screenshot Capture API Endpoint
// ============================================================================

/// Capture a screenshot and save it to .dev-logs/screenshots/ with task-identifiable naming.
/// Also logs the screenshot to ai-output.jsonl for AI analysis.
///
/// This endpoint is used by both:
/// 1. Dedicated screenshot actions in the AI Automation Builder
/// 2. Post-step screenshots (takeScreenshot toggle on other step types)
#[tracing::instrument(
    name = "api.request.capture_screenshot",
    skip(state, request),
    fields(
        endpoint = "/capture-screenshot",
        method = "POST",
        monitor = ?request.monitor,
        delay_seconds = ?request.delay_seconds
    )
)]
async fn capture_screenshot_step(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CaptureScreenshotRequest>,
) -> Result<Json<ApiResponse<CaptureScreenshotResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use base64::Engine;
    use std::fs;
    use std::io::Write;

    info!(
        "MCP API: Capturing screenshot (monitor: {:?}, delay: {:?}s, task: {:?}, step: {:?})",
        request.monitor, request.delay_seconds, request.task_id, request.step_index
    );

    // Apply delay if specified (clamped to 0-30 seconds)
    if let Some(delay) = request.delay_seconds {
        if delay > 0.0 {
            let clamped_delay = delay.clamp(0.0, 30.0);
            info!(
                "MCP API: Waiting {}s before screenshot capture",
                clamped_delay
            );
            tokio::time::sleep(tokio::time::Duration::from_secs_f64(clamped_delay)).await;
        }
    }

    // Capture screenshot via Python IPC
    let capture_response =
        match capture_screenshot_ipc(state.app_state.clone(), request.monitor, "png").await {
            Ok(response) => response,
            Err(e) => {
                error!("MCP API: Failed to capture screenshot via IPC: {}", e);
                return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                    success: false,
                    screenshot_path: None,
                    absolute_path: None,
                    width: None,
                    height: None,
                    monitor: request.monitor,
                    error: Some(format!("IPC error: {}", e)),
                })));
            }
        };

    let screenshot_base64 = match capture_response
        .get("screenshot_base64")
        .and_then(|s| s.as_str())
    {
        Some(s) => s.to_string(),
        None => {
            error!("MCP API: No screenshot_base64 in IPC response");
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some("No screenshot data in response".to_string()),
            })));
        }
    };

    let width = capture_response
        .get("width")
        .and_then(|w| w.as_i64())
        .map(|w| w as i32);
    let height = capture_response
        .get("height")
        .and_then(|h| h.as_i64())
        .map(|h| h as i32);

    // Decode base64 to bytes
    let image_bytes = match base64::engine::general_purpose::STANDARD.decode(&screenshot_base64) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!("MCP API: Failed to decode screenshot base64: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Failed to decode screenshot: {}", e)),
            })));
        }
    };

    // Generate filename with task/run identification
    let timestamp = chrono::Utc::now().timestamp_millis();
    let task_id = request.task_id.as_deref().unwrap_or("manual");
    let step_part = request
        .step_index
        .map(|i| format!("step{:02}", i))
        .unwrap_or_else(|| "step00".to_string());
    let monitor_part = request
        .monitor
        .map(|m| format!("m{}", m))
        .unwrap_or_else(|| "all".to_string());
    let filename = format!(
        "screenshot-{}-{}-{}-{}.png",
        task_id, step_part, timestamp, monitor_part
    );

    // Save to .dev-logs/screenshots/
    let screenshots_dir = crate::paths::get_screenshots_dir();
    if let Err(e) = fs::create_dir_all(&screenshots_dir) {
        error!("MCP API: Failed to create screenshots directory: {}", e);
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Failed to create directory: {}", e)),
        })));
    }

    let screenshot_path = screenshots_dir.join(&filename);
    let mut file = match fs::File::create(&screenshot_path) {
        Ok(f) => f,
        Err(e) => {
            error!("MCP API: Failed to create screenshot file: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Failed to create file: {}", e)),
            })));
        }
    };

    if let Err(e) = file.write_all(&image_bytes) {
        error!("MCP API: Failed to write screenshot file: {}", e);
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Failed to write file: {}", e)),
        })));
    }

    let relative_path = format!("screenshots/{}", filename);
    let absolute_path_str = screenshot_path.to_string_lossy().to_string();

    info!(
        "MCP API: Screenshot saved: {} ({}x{})",
        relative_path,
        width.unwrap_or(0),
        height.unwrap_or(0)
    );

    // Log to ai-output.jsonl
    let ai_output_entry = crate::commands::logging::AiOutputEntry {
        id: format!("ss-{}-{}", timestamp, rand::random::<u32>()),
        timestamp,
        line: format!(
            "[SCREENSHOT] {} ({}x{})",
            filename,
            width.unwrap_or(0),
            height.unwrap_or(0)
        ),
        source: "runner".to_string(),
        action_id: Some(format!("screenshot-{}", step_part)),
        task_run_id: None,
        session_id: None,
        session_name: None,
        phase: None,
        phase_iteration: None,
        screenshot_path: Some(relative_path.clone()),
        screenshot_width: width,
        screenshot_height: height,
    };

    // Append to ai-output.jsonl
    let _ = crate::commands::logging::append_ai_output_log(ai_output_entry);

    // Also emit to frontend for real-time display
    emit_ai_output(
        &state.app_handle,
        &format!(
            "[SCREENSHOT] Captured: {} ({}x{})",
            filename,
            width.unwrap_or(0),
            height.unwrap_or(0)
        ),
        "runner",
        Some(&format!("screenshot-{}", step_part)),
        None,
    );

    Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
        success: true,
        screenshot_path: Some(relative_path),
        absolute_path: Some(absolute_path_str),
        width,
        height,
        monitor: request.monitor,
        error: None,
    })))
}

// ============================================================================
// Screenshot List API Endpoint
// ============================================================================

/// List all screenshots from dev-logs directories.
/// Returns screenshots from:
/// - `.dev-logs/screenshots/` - Annotated screenshots from image recognition
/// - `.dev-logs/playwright-screenshots/` - Screenshots from Playwright test failures
async fn list_screenshots_endpoint() -> Json<crate::commands::screenshots::ScreenshotsResponse> {
    info!("MCP API: Listing screenshots from dev-logs directories");
    Json(crate::commands::screenshots::list_screenshots().await)
}

// ============================================================================
// Action Log View API Endpoint
// ============================================================================

/// Get the action log view data.
/// Returns the same data as the Tauri command `get_action_log_view`.
/// This provides a single source of truth for both the Actions page and GUI Automation widget.
async fn get_action_log_view_endpoint(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting action log view");

    let processor = state.app_state.display_processor.lock().await;

    match processor.get_view("action_log") {
        Ok(view_data) => {
            info!("MCP API: Action log view retrieved successfully");
            Ok(Json(view_data))
        }
        Err(e) => {
            error!("MCP API: Failed to get action log view: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get action log view: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Render Logging API Endpoints (for UI Testing)
// ============================================================================

/// Get all render log entries
/// Used by Python tests to verify component rendering
async fn get_render_log() -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting render log");

    let result = crate::commands::logging::load_render_log();

    if result.success {
        Ok(Json(serde_json::json!({
            "success": true,
            "data": result.data
        })))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )),
        ))
    }
}

/// Clear the render log file
async fn clear_render_log_handler(
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Clearing render log");

    let result = crate::commands::logging::clear_render_log();

    if result.success {
        Ok(Json(ApiResponse::success(())))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )),
        ))
    }
}

/// Get the path to the render log file
async fn get_render_log_path(
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting render log path");

    let result = crate::commands::logging::get_render_log_path_cmd();

    if result.success {
        Ok(Json(serde_json::json!({
            "success": true,
            "data": result.data
        })))
    } else {
        Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string()),
            )),
        ))
    }
}

// ============================================================================
// Navigation API Endpoints (for UI Testing)
// ============================================================================

/// Request to navigate to a page
#[derive(Debug, Deserialize)]
pub struct NavigateRequest {
    /// Target page/tab ID (e.g., "run-recap", "run", "active", "library")
    pub page: String,
    /// Optional: task run ID when navigating to run-recap
    #[serde(default)]
    pub task_run_id: Option<i64>,
    /// Optional: select a specific run when navigating
    #[serde(default)]
    pub select_run: Option<i64>,
}

/// Navigate to a specific page in the runner UI.
/// Used by Python tests to trigger page renders for testing.
async fn navigate_to_page(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<NavigateRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Navigating to page: {} (task_run_id: {:?}, select_run: {:?})",
        request.page, request.task_run_id, request.select_run
    );

    // Emit navigation event to frontend
    let event_payload = serde_json::json!({
        "type": "navigate",
        "page": request.page,
        "task_run_id": request.task_run_id,
        "select_run": request.select_run,
    });

    if let Err(e) = state.app_handle.emit("test-navigation", &event_payload) {
        error!("MCP API: Failed to emit navigation event: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to emit navigation event: {}", e))),
        ));
    }

    // Give the UI a moment to process the navigation
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    Ok(Json(ApiResponse::success(())))
}

// ============================================================================
// UI Bridge API Endpoints
// ============================================================================

/// Request to execute an action on an element
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UIBridgeActionRequest {
    action: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
    #[serde(default)]
    wait_options: Option<serde_json::Value>,
}

/// Request to execute an action on a component
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UIBridgeComponentActionRequest {
    #[serde(default)]
    params: Option<serde_json::Value>,
}

/// Discovery options request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UIBridgeDiscoveryRequest {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    interactive_only: Option<bool>,
    #[serde(default)]
    include_hidden: Option<bool>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default)]
    selector: Option<String>,
}

/// UI Bridge timeout is fetched from centralized config
/// This needs a reasonable timeout since it's synchronous communication with the frontend.
fn get_ui_bridge_timeout_ms() -> u64 {
    Timeouts::ui_bridge_ipc().as_millis() as u64
}

/// Send a UI Bridge request and wait for the response synchronously.
///
/// This creates a oneshot channel, stores the sender in the pending map,
/// emits the request to the frontend, and waits for the response with a timeout.
async fn ui_bridge_request_sync(
    state: &Arc<ApiState>,
    request_type: &str,
    additional_payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Create the full event payload
    let mut event_payload = serde_json::json!({
        "requestId": request_id,
        "type": request_type
    });

    // Merge additional payload fields
    if let (Some(base), Some(additional)) = (
        event_payload.as_object_mut(),
        additional_payload.as_object(),
    ) {
        for (key, value) in additional {
            base.insert(key.clone(), value.clone());
        }
    }

    // Create oneshot channel for the response
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Store the sender in the pending map
    {
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.insert(request_id.clone(), tx);
    }

    // Emit request to React frontend
    if let Err(e) = state.app_handle.emit("ui-bridge-request", &event_payload) {
        // Clean up the pending entry
        let mut pending = state.ui_bridge_pending.lock().await;
        pending.remove(&request_id);
        return Err(format!("Failed to emit UI Bridge request: {}", e));
    }

    // Wait for response with timeout
    let timeout_duration = std::time::Duration::from_millis(get_ui_bridge_timeout_ms());
    match tokio::time::timeout(timeout_duration, rx).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => {
            // Channel was closed without sending
            Err("UI Bridge request channel closed unexpectedly".to_string())
        }
        Err(_) => {
            // Timeout - clean up the pending entry
            let mut pending = state.ui_bridge_pending.lock().await;
            pending.remove(&request_id);
            Err(format!(
                "UI Bridge request timed out after {}ms. Is the frontend running?",
                get_ui_bridge_timeout_ms()
            ))
        }
    }
}

/// Handle incoming UI Bridge response from the frontend.
///
/// This is called by the Tauri event listener set up in create_router.
pub async fn handle_ui_bridge_response(
    pending: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
    response: serde_json::Value,
) {
    let request_id = response
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(request_id) = request_id {
        let mut pending_map = pending.lock().await;
        if let Some(sender) = pending_map.remove(&request_id) {
            // Extract the data portion of the response
            let data = response.get("data").cloned().unwrap_or(response.clone());
            if sender.send(data).is_err() {
                warn!(
                    "UI Bridge: Failed to send response, receiver dropped for request {}",
                    request_id
                );
            } else {
                debug!("UI Bridge: Delivered response for request {}", request_id);
            }
        } else {
            warn!(
                "UI Bridge: No pending request found for response {}",
                request_id
            );
        }
    } else {
        warn!("UI Bridge: Response missing requestId: {:?}", response);
    }
}

/// Get all registered UI elements from the React UI Bridge.
///
/// This endpoint emits an event to the React frontend via Tauri IPC
/// and waits for the response synchronously.
async fn ui_bridge_get_elements_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all elements");

    match ui_bridge_request_sync(&state, "get_elements", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific element by ID.
async fn ui_bridge_get_element_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting element {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_element",
        serde_json::json!({ "elementId": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action on an element.
async fn ui_bridge_execute_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<UIBridgeActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on element {}",
        request.action, id
    );

    let payload = serde_json::json!({
        "elementId": id,
        "action": {
            "action": request.action,
            "params": request.params,
            "waitOptions": request.wait_options
        }
    });

    match ui_bridge_request_sync(&state, "execute_action", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get all registered components.
async fn ui_bridge_get_components_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all components");

    match ui_bridge_request_sync(&state, "get_components", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific component by ID.
async fn ui_bridge_get_component_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting component {}", id);

    match ui_bridge_request_sync(
        &state,
        "get_component",
        serde_json::json!({ "componentId": id }),
    )
    .await
    {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Execute an action on a component.
async fn ui_bridge_execute_component_action_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, action_id)): Path<(String, String)>,
    Json(request): Json<UIBridgeComponentActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "UI Bridge API: Executing action {} on component {}",
        action_id, id
    );

    let payload = serde_json::json!({
        "componentId": id,
        "actionId": action_id,
        "params": request.params
    });

    match ui_bridge_request_sync(&state, "execute_component_action", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Discover controllable elements in the UI.
async fn ui_bridge_discover_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<UIBridgeDiscoveryRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Discovering elements");

    let payload = serde_json::json!({
        "options": {
            "root": request.root,
            "interactiveOnly": request.interactive_only,
            "includeHidden": request.include_hidden,
            "limit": request.limit,
            "types": request.types,
            "selector": request.selector
        }
    });

    match ui_bridge_request_sync(&state, "discover", payload).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a full snapshot of the UI Bridge state.
async fn ui_bridge_get_snapshot_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting snapshot");

    match ui_bridge_request_sync(&state, "get_snapshot", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get all loaded specs from the SpecStore.
async fn ui_bridge_get_specs_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting all specs");

    match ui_bridge_request_sync(&state, "get_specs", serde_json::json!({})).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get a specific spec by ID from the SpecStore.
async fn ui_bridge_get_spec_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("UI Bridge API: Getting spec {}", id);

    match ui_bridge_request_sync(&state, "get_spec", serde_json::json!({ "specId": id })).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("UI Bridge API: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Web Extraction API Endpoints
// ============================================================================

/// Request to start web extraction
#[derive(Debug, Deserialize)]
pub struct StartExtractionRequest {
    /// URLs to extract from
    pub urls: Vec<String>,
    /// Viewport sizes as [width, height] pairs
    #[serde(default)]
    pub viewports: Vec<(u32, u32)>,
    /// Whether to capture hover states
    #[serde(default = "default_true")]
    pub capture_hover_states: bool,
    /// Whether to capture focus states
    #[serde(default = "default_true")]
    pub capture_focus_states: bool,
    /// Maximum crawl depth
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum pages to crawl
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
    /// Backend session ID to update with progress
    #[serde(default)]
    pub session_id: Option<String>,
    /// Backend API URL for progress updates
    #[serde(default)]
    pub backend_url: Option<String>,
    /// Auth token for backend API calls
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Enable comprehensive extraction pipeline (captures ALL visible elements)
    #[serde(default)]
    pub use_comprehensive_extraction: bool,
}

fn default_true() -> bool {
    true
}

fn default_max_depth() -> u32 {
    5
}

fn default_max_pages() -> u32 {
    100
}

/// Response from extraction status endpoint
#[derive(Debug, Serialize)]
pub struct ExtractionStatusResponse {
    pub is_running: bool,
    pub extraction_id: Option<String>,
    pub stats: Option<ExtractionStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtractionStats {
    pub states_found: u32,
    pub transitions_found: u32,
    pub pages_extracted: u32,
    pub warnings: u32,
    pub errors: u32,
}

/// Tracks the current state of web extraction
/// Thread-safe wrapper for extraction status tracking
#[derive(Debug, Default)]
pub struct ExtractionState {
    inner: std::sync::Mutex<ExtractionStateInner>,
}

#[derive(Debug, Default)]
struct ExtractionStateInner {
    is_running: bool,
    extraction_id: Option<String>,
    stats: ExtractionStats,
}

impl ExtractionState {
    /// Create a new extraction state tracker
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(ExtractionStateInner::default()),
        }
    }

    /// Mark extraction as started with the given ID
    pub fn start(&self, extraction_id: Option<String>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.is_running = true;
        inner.extraction_id = extraction_id;
        inner.stats = ExtractionStats::default();
    }

    /// Mark extraction as stopped
    pub fn stop(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.is_running = false;
    }

    /// Mark extraction as complete with final stats
    pub fn complete(&self, stats: ExtractionStats) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.is_running = false;
        inner.stats = stats;
    }

    /// Update the extraction stats
    pub fn update_stats(&self, stats: ExtractionStats) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.stats = stats;
    }

    /// Get the current extraction status
    pub fn get_status(&self) -> ExtractionStatusResponse {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ExtractionStatusResponse {
            is_running: inner.is_running,
            extraction_id: inner.extraction_id.clone(),
            stats: if inner.is_running
                || inner.stats.states_found > 0
                || inner.stats.pages_extracted > 0
            {
                Some(inner.stats.clone())
            } else {
                None
            },
        }
    }
}

// =============================================================================
// UI-TARS Extraction Types
// =============================================================================

/// Request to start UI-TARS extraction
#[derive(Debug, Deserialize)]
pub struct StartUITarsExtractionRequest {
    /// Target type: "web" or "desktop"
    #[serde(default = "default_desktop")]
    pub target_type: String,
    /// Target URL (for web) or application name (for desktop)
    pub target: String,
    /// Exploration goal (what to discover)
    #[serde(default = "default_uitars_goal")]
    pub goal: String,
    /// Provider: "local_transformers", "local_vllm", or "cloud"
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model size: "2B", "7B", or "72B"
    #[serde(default = "default_model_size")]
    pub model_size: String,
    /// Quantization: "none", "int8", or "int4"
    #[serde(default = "default_quantization")]
    pub quantization: String,
    /// Maximum exploration steps
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// Timeout in seconds
    #[serde(default = "default_uitars_timeout")]
    pub timeout_seconds: u32,
    /// Whether to save screenshots
    #[serde(default = "default_true")]
    pub save_screenshots: bool,
    /// HuggingFace endpoint (for cloud provider)
    #[serde(default)]
    pub huggingface_endpoint: Option<String>,
    /// HuggingFace API token (for cloud provider)
    #[serde(default)]
    pub huggingface_api_token: Option<String>,
    /// vLLM server URL (for local_vllm provider)
    #[serde(default)]
    pub vllm_server_url: Option<String>,
    /// Monitor index for desktop extraction
    #[serde(default)]
    pub monitor_index: u32,
}

fn default_desktop() -> String {
    "desktop".to_string()
}

fn default_uitars_goal() -> String {
    "Explore the application and discover all clickable UI elements including buttons, links, menu items, and interactive controls. Identify distinct application states and the actions that transition between them.".to_string()
}

fn default_provider() -> String {
    "local_transformers".to_string()
}

fn default_model_size() -> String {
    "2B".to_string()
}

fn default_quantization() -> String {
    "int4".to_string()
}

fn default_max_steps() -> u32 {
    50
}

/// Default timeout for UI-TARS extraction.
/// Returns 0 to indicate no timeout (run until completion).
fn default_uitars_timeout() -> u32 {
    0 // No timeout - run until completion
}

/// Response from UI-TARS extraction status endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsExtractionStatusResponse {
    pub status: String,
    pub current_step: u32,
    pub max_steps: u32,
    pub elapsed_seconds: f64,
    pub last_thought: Option<String>,
    pub last_action: Option<String>,
    pub states_discovered: u32,
    pub transitions_discovered: u32,
    pub error_message: Option<String>,
    pub uitars_available: bool,
}

/// Response from UI-TARS extraction results endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsExtractionResultsResponse {
    pub states: Vec<UITarsDiscoveredState>,
    pub transitions: Vec<UITarsDiscoveredTransition>,
    pub total_steps: u32,
    pub total_screenshots: u32,
    pub exploration_time_seconds: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsDiscoveredState {
    pub id: String,
    pub name: String,
    pub description: String,
    pub screenshot_path: String,
    pub elements: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UITarsDiscoveredTransition {
    pub id: String,
    pub from_state_id: String,
    pub to_state_id: String,
    pub action_type: String,
    pub action_description: String,
    pub coordinates: Option<(i32, i32)>,
}

/// Start web extraction
#[tracing::instrument(
    name = "api.request.start_web_extraction",
    skip(state, request),
    fields(
        endpoint = "/extraction/start",
        method = "POST",
        url_count = %request.urls.len()
    )
)]
async fn start_web_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting web extraction for {} URLs",
        request.urls.len()
    );

    // Generate an extraction ID from session_id or create a new one
    let extraction_id = request
        .session_id
        .clone()
        .unwrap_or_else(|| format!("extraction_{}", chrono::Utc::now().timestamp_millis()));

    // Build extraction params
    let params = serde_json::json!({
        "urls": request.urls,
        "viewports": request.viewports,
        "capture_hover_states": request.capture_hover_states,
        "capture_focus_states": request.capture_focus_states,
        "max_depth": request.max_depth,
        "max_pages": request.max_pages,
        "session_id": request.session_id,
        "backend_url": request.backend_url,
        "auth_token": request.auth_token,
        "use_comprehensive_extraction": request.use_comprehensive_extraction,
    });

    let app_state = state.app_state.clone();
    let extraction_state = state.extraction_state.clone();
    let extraction_id_for_state = extraction_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("start_web_extraction", Some(params))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            // Mark extraction as running
            extraction_state.start(Some(extraction_id_for_state.clone()));
            info!(
                "MCP API: Web extraction started with ID: {}",
                extraction_id_for_state
            );
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "extraction_id": extraction_id_for_state,
                "message": "Web extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start web extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Request to start vision extraction
#[derive(Debug, Deserialize)]
struct StartVisionExtractionRequest {
    /// Base64-encoded screenshot or file path
    screenshot: String,
    /// Techniques to run: ["edge", "sam3", "ocr"]
    #[serde(default = "default_vision_techniques")]
    techniques: Vec<String>,
    /// Edge detection: Canny low threshold
    #[serde(default = "default_canny_low")]
    canny_low: i32,
    /// Edge detection: Canny high threshold
    #[serde(default = "default_canny_high")]
    canny_high: i32,
    /// Edge detection: minimum contour area
    #[serde(default = "default_min_contour_area")]
    min_contour_area: i32,
    /// SAM3: points per side
    #[serde(default = "default_points_per_side")]
    points_per_side: i32,
    /// SAM3: predicted IoU threshold
    #[serde(default = "default_pred_iou_thresh")]
    pred_iou_thresh: f64,
    /// SAM3: stability score threshold
    #[serde(default = "default_stability_score_thresh")]
    stability_score_thresh: f64,
    /// OCR: engine ("easyocr" or "tesseract")
    #[serde(default = "default_ocr_engine")]
    ocr_engine: String,
    /// OCR: languages
    #[serde(default = "default_ocr_languages")]
    ocr_languages: Vec<String>,
    /// OCR: confidence threshold
    #[serde(default = "default_ocr_confidence")]
    ocr_confidence_threshold: f64,
    /// Fusion: IoU threshold for deduplication
    #[serde(default = "default_iou_threshold")]
    iou_threshold: f64,
}

fn default_vision_techniques() -> Vec<String> {
    vec!["edge".to_string(), "ocr".to_string()]
}

fn default_canny_low() -> i32 {
    50
}

fn default_canny_high() -> i32 {
    150
}

fn default_min_contour_area() -> i32 {
    100
}

fn default_points_per_side() -> i32 {
    32
}

fn default_pred_iou_thresh() -> f64 {
    0.88
}

fn default_stability_score_thresh() -> f64 {
    0.95
}

fn default_ocr_engine() -> String {
    "easyocr".to_string()
}

fn default_ocr_languages() -> Vec<String> {
    vec!["en".to_string()]
}

fn default_ocr_confidence() -> f64 {
    0.6
}

fn default_iou_threshold() -> f64 {
    0.5
}

/// Start vision extraction
async fn start_vision_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartVisionExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Starting vision extraction");

    // Build extraction params
    let params = serde_json::json!({
        "config": {
            "screenshot": request.screenshot,
            "techniques": request.techniques,
            "canny_low": request.canny_low,
            "canny_high": request.canny_high,
            "min_contour_area": request.min_contour_area,
            "points_per_side": request.points_per_side,
            "pred_iou_thresh": request.pred_iou_thresh,
            "stability_score_thresh": request.stability_score_thresh,
            "ocr_engine": request.ocr_engine,
            "ocr_languages": request.ocr_languages,
            "ocr_confidence_threshold": request.ocr_confidence_threshold,
            "iou_threshold": request.iou_threshold,
        }
    });

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("run_vision_extraction", Some(params))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Vision extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "Vision extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start vision extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop web extraction
async fn stop_web_extraction(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping web extraction");

    let app_state = state.app_state.clone();
    let extraction_state = state.extraction_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_web_extraction", None)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            // Mark extraction as stopped
            extraction_state.stop();
            info!("MCP API: Web extraction stopped");
            Ok(Json(ApiResponse::success(
                "Web extraction stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop web extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get extraction status
async fn get_extraction_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<ExtractionStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Return the tracked extraction state
    let status = state.extraction_state.get_status();
    debug!(
        "MCP API: Extraction status - is_running: {}, extraction_id: {:?}",
        status.is_running, status.extraction_id
    );
    Ok(Json(ApiResponse::success(status)))
}

/// Update extraction stats
///
/// Called by the Python extraction process to report progress.
async fn update_extraction_stats(
    State(state): State<Arc<ApiState>>,
    Json(stats): Json<ExtractionStats>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    debug!(
        "MCP API: Updating extraction stats - states: {}, transitions: {}, pages: {}, errors: {}",
        stats.states_found, stats.transitions_found, stats.pages_extracted, stats.errors
    );
    state.extraction_state.update_stats(stats);
    Ok(Json(ApiResponse::success("Stats updated".to_string())))
}

/// Mark extraction as complete
///
/// Called by the Python extraction process when extraction finishes.
async fn complete_extraction(
    State(state): State<Arc<ApiState>>,
    Json(stats): Json<ExtractionStats>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Extraction complete - states: {}, transitions: {}, pages: {}, errors: {}",
        stats.states_found, stats.transitions_found, stats.pages_extracted, stats.errors
    );
    state.extraction_state.complete(stats);
    Ok(Json(ApiResponse::success(
        "Extraction completed".to_string(),
    )))
}

/// Get extraction screenshot
///
/// Serves a screenshot image from a web extraction session.
/// The screenshot is stored locally on the runner machine.
async fn get_extraction_screenshot(
    axum::extract::Path((extraction_id, screenshot_id)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use axum::body::Body;
    use axum::http::header;

    // Build path to screenshot file
    let home_dir = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let screenshot_path = home_dir
        .join(".qontinui")
        .join("extraction")
        .join(&extraction_id)
        .join("screenshots")
        .join(format!("{}.png", screenshot_id));

    info!(
        "MCP API: Serving extraction screenshot: {} from {:?}",
        screenshot_id, screenshot_path
    );

    // Check if file exists and read it
    match tokio::fs::read(&screenshot_path).await {
        Ok(data) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            Body::from(data),
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!("Screenshot not found: {:?}", screenshot_path);
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/json")],
                Body::from(r#"{"error": "Screenshot not found"}"#),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to read screenshot file: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/json")],
                Body::from(format!(
                    r#"{{"error": "Failed to read screenshot: {}"}}"#,
                    e
                )),
            )
                .into_response()
        }
    }
}

// ============================================================================
// UI-TARS Extraction Endpoints
// ============================================================================

/// Start UI-TARS extraction
async fn start_uitars_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartUITarsExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting UI-TARS extraction for target: {}",
        request.target
    );

    // Build extraction params
    let params = serde_json::json!({
        "target_type": request.target_type,
        "target": request.target,
        "goal": request.goal,
        "provider": request.provider,
        "model_size": request.model_size,
        "quantization": request.quantization,
        "max_steps": request.max_steps,
        "timeout_seconds": request.timeout_seconds,
        "save_screenshots": request.save_screenshots,
        "huggingface_endpoint": request.huggingface_endpoint,
        "huggingface_api_token": request.huggingface_api_token,
        "vllm_server_url": request.vllm_server_url,
        "monitor_index": request.monitor_index,
    });

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("start_uitars_extraction", Some(params))
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: UI-TARS extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "UI-TARS extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start UI-TARS extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop UI-TARS extraction
async fn stop_uitars_extraction(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping UI-TARS extraction");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_uitars_extraction", None)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: UI-TARS extraction stopped");
            Ok(Json(ApiResponse::success(
                "UI-TARS extraction stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop UI-TARS extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI-TARS extraction status
async fn get_uitars_extraction_status(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            // 60 second timeout for status check
            let timeout = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait("get_uitars_extraction_status", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            // Return the actual status from Python
            if response.success {
                Ok(Json(ApiResponse::success(response.data.unwrap_or(
                    serde_json::json!({
                        "status": "idle",
                        "current_step": 0,
                        "max_steps": 0,
                        "elapsed_seconds": 0.0,
                        "states_discovered": 0,
                        "transitions_discovered": 0,
                        "uitars_available": false
                    }),
                ))))
            } else {
                error!(
                    "MCP API: UI-TARS extraction status command failed: {:?}",
                    response.error
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(
                        response
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    )),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI-TARS extraction status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI-TARS extraction results
async fn get_uitars_extraction_results(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            // 5 minute timeout for results fetch (may involve processing)
            let timeout = std::time::Duration::from_secs(300);
            bridge.send_command_and_wait("get_uitars_extraction_results", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            // Return the actual results from Python
            if response.success {
                Ok(Json(ApiResponse::success(response.data.unwrap_or(
                    serde_json::json!({
                        "states": [],
                        "transitions": [],
                        "total_steps": 0,
                        "total_screenshots": 0,
                        "exploration_time_seconds": 0.0
                    }),
                ))))
            } else {
                error!(
                    "MCP API: UI-TARS extraction results command failed: {:?}",
                    response.error
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(
                        response
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string()),
                    )),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI-TARS extraction results: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// RAG API Endpoints
// ============================================================================

/// Request to import a RAG configuration
///
/// Accepts the full QontinuiConfig format directly from the frontend.
/// The runner extracts what it needs (images, states, patterns) internally.
/// This eliminates the need for frontend transformation code.
#[derive(Debug, Deserialize)]
pub struct ImportRAGRequest {
    /// Full QontinuiConfig - the canonical format from TypeScript/Python
    pub config: QontinuiConfig,
    /// Optional project_id override (defaults to derived from metadata.name)
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Import a RAG configuration
///
/// Accepts the full QontinuiConfig format directly from the frontend.
/// Saves the complete config and extracts images for embedding generation.
async fn import_rag(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportRAGRequest>,
) -> Result<Json<ApiResponse<ImportResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Use provided project_id or derive from metadata.name
    let project_id = request
        .project_id
        .clone()
        .unwrap_or_else(|| request.config.project_id());

    let image_count = request.config.images.len();
    let state_image_count = request.config.state_image_count();
    let pattern_count = request.config.pattern_count();

    info!(
        "MCP API: Importing QontinuiConfig: project_id={}, name={}, images={}, states={}, stateImages={}, patterns={}",
        project_id,
        request.config.metadata.name,
        image_count,
        request.config.states.len(),
        state_image_count,
        pattern_count
    );

    // Save the full QontinuiConfig
    let storage = state.rag_state.storage.lock().await;
    let storage_path = storage
        .save_qontinui_config(&project_id, &request.config)
        .map_err(|e| {
            error!("Failed to save QontinuiConfig: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save config: {}", e))),
            )
        })?;

    // Extract and save images from config.images[]
    // Only save images that are referenced by patterns
    let referenced_ids = request.config.referenced_image_ids();
    let saved_image_count = storage
        .save_images_from_config(&project_id, &request.config.images, &referenced_ids)
        .map_err(|e| {
            error!("Failed to save images: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save images: {}", e))),
            )
        })?;

    let storage_path_str = storage_path.to_string_lossy().to_string();
    drop(storage);

    // Trigger embedding generation in background
    info!(
        "MCP API: Starting background embedding generation for project_id={}",
        project_id
    );
    let embedding_generator = state.rag_state.embedding_generator.lock().await;
    let mut progress_rx = embedding_generator.generate_embeddings_async(project_id.clone());
    drop(embedding_generator);

    // Spawn background task to sync embeddings to web backend when complete
    let project_id_for_sync = project_id.clone();
    tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            match progress.status {
                crate::rag::EmbeddingStatus::Completed => {
                    info!(
                        "MCP API: Embedding generation completed for project_id={}, syncing to web backend",
                        project_id_for_sync
                    );
                    match send_embeddings_to_web(&project_id_for_sync).await {
                        Ok(()) => {
                            info!(
                                "MCP API: Successfully synced embeddings to web for project_id={}",
                                project_id_for_sync
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "MCP API: Failed to sync embeddings to web for project_id={}: {}",
                                project_id_for_sync,
                                e
                            );
                        }
                    }
                    break;
                }
                crate::rag::EmbeddingStatus::Failed(ref err) => {
                    tracing::warn!(
                        "MCP API: Embedding generation failed for project_id={}: {}",
                        project_id_for_sync,
                        err
                    );
                    break;
                }
                _ => {
                    // Continue polling for in-progress updates
                }
            }
        }
    });

    // Auto-add to ConfigStorage (database)
    let config_name = request.config.metadata.name.clone();
    if let Ok(config_json) = serde_json::to_value(&request.config) {
        if let Err(e) = state.app_state.checkpoint_db.save_config_with_id(
            &project_id,
            config_json,
            &config_name,
            "web",
            None,
        ) {
            warn!(
                "MCP API: Failed to auto-store config in ConfigStorage: {}",
                e
            );
        } else {
            info!(
                "MCP API: Auto-stored config '{}' with id '{}' in ConfigStorage",
                config_name, project_id
            );
            // Store current config ID
            if let Ok(mut current_id) = state.current_config_id.lock() {
                *current_id = Some(project_id.clone());
            }
        }
    }

    let result = ImportResult {
        success: true,
        project_id: project_id.clone(),
        message: format!(
            "Successfully imported QontinuiConfig '{}' with {} images ({} saved for RAG) and {} patterns. Embedding generation started.",
            request.config.metadata.name, image_count, saved_image_count, pattern_count
        ),
        screenshot_count: saved_image_count,
        element_count: pattern_count,
        storage_path: storage_path_str,
    };

    Ok(Json(ApiResponse::success(result)))
}

/// List all RAG configurations
async fn list_rag_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<RAGConfigSummary>>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Listing RAG configurations");

    let storage = state.rag_state.storage.lock().await;
    let summaries = storage.list_configs().map_err(|e| {
        error!("Failed to list RAG configs: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list configs: {}", e))),
        )
    })?;

    info!("MCP API: Found {} RAG configurations", summaries.len());

    Ok(Json(ApiResponse::success(summaries)))
}

/// Get RAG embedding status for a project
async fn get_rag_status(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting RAG status for project_id={}", project_id);

    let embedding_generator = state.rag_state.embedding_generator.lock().await;

    // Get progress from state if available (includes in-progress tracking)
    if let Some(progress) = embedding_generator.get_progress(&project_id) {
        let status_str = match &progress.status {
            crate::rag::EmbeddingStatus::NotStarted => "not_started",
            crate::rag::EmbeddingStatus::InProgress(_) => "in_progress",
            crate::rag::EmbeddingStatus::Completed => "completed",
            crate::rag::EmbeddingStatus::Failed(_) => "failed",
        };

        let mut data = serde_json::json!({
            "status": status_str,
            "message": progress.message,
        });

        // Add optional fields if present
        if let Some(percent) = progress.percent {
            data["percent"] = serde_json::json!(percent);
        }
        if let Some(elements_processed) = progress.elements_processed {
            data["elements_processed"] = serde_json::json!(elements_processed);
        }
        if let Some(total_elements) = progress.total_elements {
            data["total_elements"] = serde_json::json!(total_elements);
        }

        return Ok(Json(ApiResponse::success(data)));
    }

    // Fallback to file-based check (for completed/not started)
    let status = embedding_generator.check_status(&project_id);

    let status_str = match &status {
        crate::rag::EmbeddingStatus::NotStarted => "not_started",
        crate::rag::EmbeddingStatus::InProgress(pct) => {
            return Ok(Json(ApiResponse::success(serde_json::json!({
                "status": "in_progress",
                "percent": pct
            }))));
        }
        crate::rag::EmbeddingStatus::Completed => "completed",
        crate::rag::EmbeddingStatus::Failed(_) => "failed",
    };

    let message = match &status {
        crate::rag::EmbeddingStatus::Failed(err) => {
            Some(format!("Embedding generation failed: {}", err))
        }
        crate::rag::EmbeddingStatus::Completed => {
            Some("Embeddings generated successfully".to_string())
        }
        _ => None,
    };

    let mut data = serde_json::json!({
        "status": status_str
    });
    if let Some(msg) = message {
        data["message"] = serde_json::json!(msg);
    }

    Ok(Json(ApiResponse::success(data)))
}

/// Delete a RAG configuration
async fn delete_rag_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Deleting RAG config for project_id={}", project_id);

    let storage = state.rag_state.storage.lock().await;
    storage.delete_config(&project_id).map_err(|e| {
        error!("Failed to delete RAG config: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete config: {}", e))),
        )
    })?;

    info!(
        "MCP API: Successfully deleted RAG config for project_id={}",
        project_id
    );

    Ok(Json(ApiResponse::success(format!(
        "Successfully deleted RAG config: {}",
        project_id
    ))))
}

/// Load a RAG project into the executor
async fn load_rag_project(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(project_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Loading RAG project for project_id={}", project_id);

    // Load QontinuiConfig
    let storage = state.rag_state.storage.lock().await;

    match storage.load_qontinui_config(&project_id) {
        Ok(config) => {
            drop(storage);

            // TODO: Load into executor if needed
            Ok(Json(ApiResponse::success(serde_json::json!({
                "project_id": project_id,
                "name": config.metadata.name,
                "states": config.states.len(),
                "patterns": config.pattern_count(),
                "loaded": true
            }))))
        }
        Err(_) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Project not found: {}", project_id))),
        )),
    }
}

/// Get RAG availability (ML models status)
async fn get_rag_availability(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Checking RAG availability");

    // Check CLIP availability via embedding generator
    // The embedding generator uses CLIP for image embeddings
    let embedding_generator = state.rag_state.embedding_generator.lock().await;
    let clip_available = !embedding_generator.is_degraded();
    drop(embedding_generator);

    // Check text/OCR availability via semantic search
    // The semantic search uses text embeddings for search
    let semantic_search = state.rag_state.semantic_search.lock().await;
    let text_available = !semantic_search.is_degraded();
    let ocr_available = text_available; // OCR is part of the same text pipeline
    drop(semantic_search);

    // Check SAM availability via Python bridge
    // SAM3 runs via the Python executor's segment_screenshot command
    let sam_available = {
        let manager_guard = state.app_state.bridge_manager.lock().await;
        if let Some(ref manager) = *manager_guard {
            manager.is_default_bridge_running()
        } else {
            false
        }
    };

    // Overall availability: at least one model must be available
    let available = clip_available || text_available || sam_available;

    info!(
        "MCP API: RAG availability - clip={}, text={}, ocr={}, sam={}, overall={}",
        clip_available, text_available, ocr_available, sam_available, available
    );

    Ok(Json(ApiResponse::success(serde_json::json!({
        "available": available,
        "models": {
            "clip": clip_available,
            "text": text_available,
            "ocr": ocr_available,
            "sam": sam_available
        }
    }))))
}

/// Request to segment a screenshot using SAM3
#[derive(Debug, Deserialize)]
pub struct SegmentScreenshotRequest {
    /// Base64-encoded screenshot image (PNG or JPEG)
    pub screenshot_base64: String,
    /// Optional minimum segment area in pixels
    #[serde(default)]
    pub min_area: Option<i32>,
    /// Optional SAM model to use (e.g., "sam2_hiera_tiny")
    #[serde(default)]
    pub model: Option<String>,
}

/// Segment in the response
#[derive(Debug, Serialize)]
pub struct SegmentInfo {
    /// Unique segment ID
    pub id: String,
    /// Bounding box [x, y, width, height]
    pub bbox: Vec<i32>,
    /// Segment area in pixels
    pub area: i32,
    /// Base64-encoded cropped image of the segment
    pub image_base64: Option<String>,
}

/// Response from screenshot segmentation
#[derive(Debug, Serialize)]
pub struct SegmentScreenshotResponse {
    /// Whether segmentation was successful
    pub success: bool,
    /// List of detected segments
    pub segments: Vec<SegmentInfo>,
    /// Error message if failed
    pub error: Option<String>,
    /// Processing time in milliseconds
    pub processing_time_ms: Option<i64>,
}

/// Segment a screenshot using SAM3 (Segment Anything Model 3)
///
/// This endpoint receives a base64-encoded screenshot and returns
/// the detected segments with their bounding boxes and cropped images.
/// SAM3 runs locally on the user's machine via the Python executor.
#[tracing::instrument(
    name = "api.request.segment_screenshot",
    skip(state, request),
    fields(
        endpoint = "/segment-screenshot",
        method = "POST",
        screenshot_size = %request.screenshot_base64.len(),
        min_area = ?request.min_area,
        model = ?request.model
    )
)]
async fn segment_screenshot(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SegmentScreenshotRequest>,
) -> Result<Json<ApiResponse<SegmentScreenshotResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Segmenting screenshot ({} bytes base64)",
        request.screenshot_base64.len()
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot_base64": request.screenshot_base64,
        "min_area": request.min_area,
        "model": request.model,
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Use configurable timeout for vision analysis (default: disabled)
            // Falls back to 1 hour to prevent infinite IPC hangs
            let timeout_duration =
                Timeouts::vision_analysis().unwrap_or_else(|| std::time::Duration::from_secs(3600));
            bridge.send_command_and_wait("segment_screenshot", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                // Parse segments from response data
                let segments: Vec<SegmentInfo> = if let Some(data) = response.data {
                    if let Some(segments_arr) = data.get("segments").and_then(|s| s.as_array()) {
                        segments_arr
                            .iter()
                            .filter_map(|seg| {
                                let id = seg.get("id")?.as_str()?.to_string();
                                let bbox = seg.get("bbox")?.as_array()?;
                                let bbox_vec: Vec<i32> = bbox
                                    .iter()
                                    .filter_map(|v| v.as_i64().map(|n| n as i32))
                                    .collect();
                                if bbox_vec.len() != 4 {
                                    return None;
                                }
                                let area =
                                    seg.get("area").and_then(|a| a.as_i64()).unwrap_or(0) as i32;
                                let image_base64 = seg
                                    .get("image_base64")
                                    .and_then(|i| i.as_str())
                                    .map(|s| s.to_string());

                                Some(SegmentInfo {
                                    id,
                                    bbox: bbox_vec,
                                    area,
                                    image_base64,
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                info!(
                    "MCP API: Segmentation completed with {} segments in {}ms",
                    segments.len(),
                    elapsed.as_millis()
                );

                Ok(Json(ApiResponse::success(SegmentScreenshotResponse {
                    success: true,
                    segments,
                    error: None,
                    processing_time_ms: Some(elapsed.as_millis() as i64),
                })))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Segmentation failed".to_string());
                error!("MCP API: Segmentation failed: {}", error_msg);

                Ok(Json(ApiResponse::success(SegmentScreenshotResponse {
                    success: false,
                    segments: Vec::new(),
                    error: Some(error_msg),
                    processing_time_ms: Some(elapsed.as_millis() as i64),
                })))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to segment screenshot: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Vision Extraction Types and Handler
// ============================================================================

/// Request for vision extraction
#[derive(Debug, Deserialize)]
pub struct VisionExtractionRequest {
    /// Base64-encoded screenshot image
    pub screenshot: String,
    /// Techniques to run: "edge", "sam3", "ocr"
    #[serde(default = "default_vision_techniques")]
    pub techniques: Vec<String>,
    /// Edge detection: lower Canny threshold
    #[serde(default = "default_canny_low")]
    pub canny_low: i32,
    /// Edge detection: upper Canny threshold
    #[serde(default = "default_canny_high")]
    pub canny_high: i32,
    /// Edge detection: minimum contour area
    #[serde(default = "default_min_contour_area")]
    pub min_contour_area: i32,
    /// SAM3: points per side for mask generation
    #[serde(default = "default_points_per_side")]
    pub points_per_side: i32,
    /// SAM3: predicted IoU threshold
    #[serde(default = "default_pred_iou_thresh")]
    pub pred_iou_thresh: f64,
    /// SAM3: stability score threshold
    #[serde(default = "default_stability_score_thresh")]
    pub stability_score_thresh: f64,
    /// OCR: engine to use ("easyocr" or "pytesseract")
    #[serde(default = "default_ocr_engine")]
    pub ocr_engine: String,
    /// OCR: languages to detect
    #[serde(default = "default_ocr_languages")]
    pub ocr_languages: Vec<String>,
    /// OCR: confidence threshold
    #[serde(default = "default_ocr_confidence")]
    pub ocr_confidence_threshold: f64,
    /// Fusion: IoU threshold for merging results
    #[serde(default = "default_iou_threshold")]
    pub iou_threshold: f64,
}

/// Run vision extraction on a screenshot
///
/// This endpoint receives a base64-encoded screenshot and runs computer vision
/// algorithms (Edge Detection, SAM3 segmentation, OCR) on the user's machine.
async fn run_vision_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<VisionExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running vision extraction ({} bytes base64, techniques: {:?})",
        request.screenshot.len(),
        request.techniques
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot": request.screenshot,
        "techniques": request.techniques,
        "canny_low": request.canny_low,
        "canny_high": request.canny_high,
        "min_contour_area": request.min_contour_area,
        "points_per_side": request.points_per_side,
        "pred_iou_thresh": request.pred_iou_thresh,
        "stability_score_thresh": request.stability_score_thresh,
        "ocr_engine": request.ocr_engine,
        "ocr_languages": request.ocr_languages,
        "ocr_confidence_threshold": request.ocr_confidence_threshold,
        "iou_threshold": request.iou_threshold,
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (3 minute timeout for vision processing)
            let timeout_duration = std::time::Duration::from_secs(180);
            bridge.send_command_and_wait("run_vision_extraction", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                info!(
                    "MCP API: Vision extraction completed in {}ms",
                    elapsed.as_millis()
                );

                // Return the full response data from Python
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "processing_time_ms": elapsed.as_millis() as i64
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Vision extraction failed".to_string());
                error!("MCP API: Vision extraction failed: {}", error_msg);

                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg,
                    "processing_time_ms": elapsed.as_millis() as i64
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run vision extraction: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Pattern Matching API
// ============================================================================

/// Search region for pattern matching
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SearchRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Request for pattern matching
#[derive(Debug, Deserialize)]
pub struct PatternMatchRequest {
    /// Base64 encoded screenshot or file path
    pub screenshot: String,
    /// Base64 encoded template image or file path
    pub template: String,
    /// Minimum similarity threshold (0.0 to 1.0, default: 0.8)
    #[serde(default = "default_similarity")]
    pub similarity: f32,
    /// Optional search region
    #[serde(default)]
    pub search_region: Option<SearchRegion>,
    /// Maximum matches for find_all (default: 100)
    #[serde(default = "default_max_matches")]
    pub max_matches: Option<i32>,
}

fn default_similarity() -> f32 {
    0.8
}

fn default_max_matches() -> Option<i32> {
    Some(100)
}

/// Match result from pattern matching
#[derive(Debug, Serialize)]
pub struct PatternMatch {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub similarity: f32,
    pub center_x: i32,
    pub center_y: i32,
}

/// Response from pattern matching
#[derive(Debug, Serialize)]
pub struct PatternMatchResponse {
    pub success: bool,
    pub matches: Vec<PatternMatch>,
    pub search_time_ms: f32,
    pub screenshot_width: i32,
    pub screenshot_height: i32,
    pub template_width: i32,
    pub template_height: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Find best match of template in screenshot
#[tracing::instrument(
    name = "api.request.pattern_find",
    skip(state, request),
    fields(
        endpoint = "/pattern/find",
        method = "POST",
        screenshot_size = %request.screenshot.len(),
        template_size = %request.template.len(),
        similarity = %request.similarity
    )
)]
async fn pattern_find(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PatternMatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Pattern find (screenshot: {} bytes, template: {} bytes, similarity: {})",
        request.screenshot.len(),
        request.template.len(),
        request.similarity
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot": request.screenshot,
        "template": request.template,
        "similarity": request.similarity,
        "search_region": request.search_region,
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (30 second timeout)
            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("pattern_find", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                info!(
                    "MCP API: Pattern find completed in {}ms",
                    elapsed.as_millis()
                );

                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "matches": [],
                        "search_time_ms": elapsed.as_millis() as f32
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Pattern find failed".to_string());
                error!("MCP API: Pattern find failed: {}", error_msg);

                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg,
                    "matches": [],
                    "search_time_ms": elapsed.as_millis() as f32
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run pattern find: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Find all matches of template in screenshot
#[tracing::instrument(
    name = "api.request.pattern_find_all",
    skip(state, request),
    fields(
        endpoint = "/pattern/find-all",
        method = "POST",
        screenshot_size = %request.screenshot.len(),
        template_size = %request.template.len(),
        similarity = %request.similarity,
        max_matches = ?request.max_matches
    )
)]
async fn pattern_find_all(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<PatternMatchRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Pattern find all (screenshot: {} bytes, template: {} bytes, similarity: {}, max_matches: {:?})",
        request.screenshot.len(),
        request.template.len(),
        request.similarity,
        request.max_matches
    );

    let start_time = std::time::Instant::now();
    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "screenshot": request.screenshot,
        "template": request.template,
        "similarity": request.similarity,
        "search_region": request.search_region,
        "max_matches": request.max_matches.unwrap_or(100),
    });

    // Use spawn_blocking for the synchronous bridge operation
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (30 second timeout)
            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("pattern_find_all", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    let elapsed = start_time.elapsed();

    match result {
        Ok(response) => {
            if response.success {
                info!(
                    "MCP API: Pattern find all completed in {}ms",
                    elapsed.as_millis()
                );

                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "matches": [],
                        "search_time_ms": elapsed.as_millis() as f32
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Pattern find all failed".to_string());
                error!("MCP API: Pattern find all failed: {}", error_msg);

                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg,
                    "matches": [],
                    "search_time_ms": elapsed.as_millis() as f32
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to run pattern find all: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// IPC-Based Screenshot Capture
// ============================================================================

/// Capture a screenshot via Python IPC (physical pixel resolution)
async fn capture_screenshot_ipc(
    app_state: Arc<crate::AppState>,
    monitor: Option<i32>,
    format: &str,
) -> Result<serde_json::Value, String> {
    let params = serde_json::json!({
        "monitor": monitor,
        "format": format,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("capture_screenshot", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(response
                    .data
                    .unwrap_or(serde_json::json!({"success": true})))
            } else {
                Err(response
                    .error
                    .unwrap_or_else(|| "Screenshot capture failed".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Get monitors via Python IPC (physical pixel coordinates)
async fn get_monitors_ipc(app_state: Arc<crate::AppState>) -> Result<serde_json::Value, String> {
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("get_monitors", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?;

    match result {
        Ok(response) => {
            if response.success {
                Ok(response
                    .data
                    .unwrap_or(serde_json::json!({"success": true, "monitors": [], "count": 0})))
            } else {
                Err(response
                    .error
                    .unwrap_or_else(|| "Get monitors failed".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// HTTP endpoint to get monitors via IPC (physical pixel coordinates)
async fn get_screenshot_monitors_ipc(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Get monitors via IPC");

    match get_monitors_ipc(state.app_state.clone()).await {
        Ok(data) => Ok(Json(ApiResponse::success(data))),
        Err(e) => {
            error!("MCP API: Failed to get monitors via IPC: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Model Management API
// ============================================================================

/// Request to download a model
#[derive(Debug, Deserialize)]
pub struct ModelDownloadRequest {
    /// Model identifier (e.g., "sam3", "clip_vit_b32")
    pub model_id: String,
    /// Force re-download even if already available
    #[serde(default)]
    pub force: bool,
}

/// Request to delete a model
#[derive(Debug, Deserialize)]
pub struct ModelDeleteRequest {
    /// Model identifier
    pub model_id: String,
}

/// Request to get model status
#[derive(Debug, Deserialize)]
pub struct ModelStatusRequest {
    /// Model identifier
    pub model_id: String,
}

/// List all available models with their download status
async fn list_models(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: List models");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_list", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "models": []
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "List models failed".to_string());
                error!("MCP API: List models failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to list models: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Download a model (returns when download completes)
async fn download_model(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ModelDownloadRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Download model {} (force: {})",
        request.model_id, request.force
    );

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "model_id": request.model_id,
        "force": request.force,
    });

    // Model downloads can take a long time, use 10 minute timeout
    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Use configurable timeout for python commands (default: disabled)
            // Falls back to 1 hour to prevent infinite IPC hangs
            let timeout_duration =
                Timeouts::python_command().unwrap_or_else(|| std::time::Duration::from_secs(3600));
            bridge.send_command_and_wait("models_download", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: Model download completed");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Model download failed".to_string());
                error!("MCP API: Model download failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to download model: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Delete a downloaded model
async fn delete_model(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ModelDeleteRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Delete model {}", request.model_id);

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "model_id": request.model_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_delete", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: Model deleted");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Model delete failed".to_string());
                error!("MCP API: Model delete failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to delete model: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get status of a specific model
async fn get_model_status(
    State(state): State<Arc<ApiState>>,
    Path(model_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Get model status for {}", model_id);

    let app_state = state.app_state.clone();
    let params = serde_json::json!({
        "model_id": model_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_status", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "available": false
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Get model status failed".to_string());
                error!("MCP API: Get model status failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get model status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get disk usage for all downloaded models
async fn get_models_disk_usage(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Get models disk usage");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("models_disk_usage", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true,
                        "total_bytes": 0,
                        "models": {}
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Get disk usage failed".to_string());
                error!("MCP API: Get disk usage failed: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get disk usage: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Integration Testing API
// ============================================================================

/// Request to start an integration test run
#[derive(Debug, Deserialize)]
pub struct StartIntegrationTestRequest {
    /// Name of the test run
    pub name: String,
    /// Configuration path being tested (optional)
    #[serde(default)]
    pub config_path: Option<String>,
    /// Test cases to execute
    #[serde(default)]
    pub test_cases: Vec<IntegrationTestCase>,
    /// Additional metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// A test case for integration testing
#[derive(Debug, Deserialize, Serialize)]
pub struct IntegrationTestCase {
    /// Test ID
    #[serde(default)]
    pub test_id: Option<String>,
    /// Test name
    pub name: String,
    /// Test description
    #[serde(default)]
    pub description: Option<String>,
    /// Assertions to run
    #[serde(default)]
    pub assertions: Vec<IntegrationTestAssertion>,
    /// Setup actions
    #[serde(default)]
    pub setup_actions: Vec<serde_json::Value>,
    /// Teardown actions
    #[serde(default)]
    pub teardown_actions: Vec<serde_json::Value>,
}

/// An assertion for integration testing
#[derive(Debug, Deserialize, Serialize)]
pub struct IntegrationTestAssertion {
    /// Assertion type: state_reached, element_found, action_performed, etc.
    #[serde(rename = "type")]
    pub assertion_type: String,
    /// Target to verify
    pub target: String,
    /// Expected value (optional)
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Timeout in seconds
    #[serde(default = "default_assertion_timeout")]
    pub timeout_seconds: f64,
}

fn default_assertion_timeout() -> f64 {
    30.0
}

/// Request to mock a GUI action
#[derive(Debug, Deserialize)]
pub struct MockGuiActionRequest {
    /// Action type: click, type, screenshot
    pub action_type: String,
    /// X coordinate (for click)
    #[serde(default)]
    pub x: Option<i32>,
    /// Y coordinate (for click)
    #[serde(default)]
    pub y: Option<i32>,
    /// Mouse button (for click)
    #[serde(default)]
    pub button: Option<String>,
    /// Click count (for click)
    #[serde(default)]
    pub clicks: Option<i32>,
    /// Text to type (for type action)
    #[serde(default)]
    pub text: Option<String>,
    /// Delay between keystrokes in ms (for type action)
    #[serde(default)]
    pub delay_ms: Option<i32>,
    /// Monitor index (for screenshot)
    #[serde(default)]
    pub monitor_index: Option<i32>,
}

/// Request to find path between states
#[derive(Debug, Deserialize)]
pub struct FindPathRequest {
    /// Source state name or ID
    pub from_state: String,
    /// Target state name or ID
    pub to_state: String,
}

/// Request to traverse to a state
#[derive(Debug, Deserialize)]
pub struct TraverseToStateRequest {
    /// Target state name or ID
    pub target_state: String,
    /// Whether to execute the traversal (false for dry run)
    #[serde(default = "default_execute")]
    pub execute: bool,
}

fn default_execute() -> bool {
    true
}

/// Request to set mock mode
#[derive(Debug, Deserialize)]
pub struct SetMockModeRequest {
    /// Mode: disabled, record, playback
    pub mode: String,
}

/// Request to run an assertion
#[derive(Debug, Deserialize)]
pub struct RunAssertionRequest {
    /// Assertion type
    pub assertion_type: String,
    /// Target to verify
    pub target: String,
    /// Expected value
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Timeout in seconds
    #[serde(default = "default_assertion_timeout")]
    pub timeout_seconds: f64,
}

/// Start an integration test run
async fn start_integration_test(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartIntegrationTestRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Starting integration test run: {}", request.name);

    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "name": request.name,
        "config_path": request.config_path,
        "metadata": request.metadata,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_start_run", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: Integration test run started successfully");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start test run".to_string());
                error!("MCP API: Failed to start test run: {}", error_msg);
                Err((StatusCode::BAD_REQUEST, Json(api_error(error_msg))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start integration test: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get test run status
async fn get_test_run_status(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "run_id": run_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_status", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "status": "unknown"
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get test results
async fn get_integration_test_results(
    State(state): State<Arc<ApiState>>,
    Path(run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "run_id": run_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_get_results", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "results": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// List test runs
async fn list_integration_test_runs(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let limit: i32 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let params = serde_json::json!({
        "limit": limit,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_list_runs", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "runs": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Mock a GUI action
async fn mock_gui_action(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<MockGuiActionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let (command, params) = match request.action_type.as_str() {
        "click" => {
            let params = serde_json::json!({
                "x": request.x.unwrap_or(0),
                "y": request.y.unwrap_or(0),
                "button": request.button.unwrap_or_else(|| "left".to_string()),
                "clicks": request.clicks.unwrap_or(1),
            });
            ("testing_mock_click", params)
        }
        "type" => {
            let params = serde_json::json!({
                "text": request.text.unwrap_or_default(),
                "delay_ms": request.delay_ms.unwrap_or(50),
            });
            ("testing_mock_type", params)
        }
        "screenshot" => {
            let params = serde_json::json!({
                "monitor_index": request.monitor_index,
            });
            ("testing_mock_screenshot", params)
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Unknown action type: {}",
                    request.action_type
                ))),
            ));
        }
    };

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait(command, Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true,
                    "mocked": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get all states for testing
async fn get_testing_states(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_states", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "states": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get all transitions for testing
async fn get_testing_transitions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_transitions", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "transitions": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Find path between states
async fn find_testing_path(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<FindPathRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "from_state": request.from_state,
        "to_state": request.to_state,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_find_path", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": "No path found"
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Traverse to a state
async fn traverse_to_state(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<TraverseToStateRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "target_state": request.target_state,
        "execute": request.execute,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(120);
            bridge.send_command_and_wait(
                "testing_traverse_to_state",
                Some(params),
                timeout_duration,
            )
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get active states
async fn get_testing_active_states(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_active_states", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "active_states": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Set mock mode
async fn set_testing_mock_mode(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<SetMockModeRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "mode": request.mode,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_set_mock_mode", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get mocked actions
async fn get_mocked_actions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_get_mocked_actions", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "actions": []
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Clear mocked actions
async fn clear_mocked_actions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(10);
            bridge.send_command_and_wait("testing_clear_mocked_actions", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Run an assertion
async fn run_testing_assertion(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunAssertionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "assertion_type": request.assertion_type,
        "target": request.target,
        "expected": request.expected,
        "timeout_seconds": request.timeout_seconds,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait("testing_run_assertion", Some(params), timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// End an integration test run
async fn end_integration_test(
    State(state): State<Arc<ApiState>>,
    Path(_run_id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            let timeout_duration = std::time::Duration::from_secs(30);
            bridge.send_command_and_wait("testing_end_run", None, timeout_duration)
        })?
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if let Some(data) = response.data {
                Ok(Json(ApiResponse::success(data)))
            } else {
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": true
                }))))
            }
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

// ============================================================================
// Playwright State Collector API
// ============================================================================

/// Request to start Playwright state collection
#[derive(Debug, Deserialize)]
pub struct StartPlaywrightCollectionRequest {
    /// Target URL to collect from
    pub url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 50)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Risk level: "safe", "caution", or "dry_run" (default: "safe")
    #[serde(default)]
    pub max_risk_level: Option<String>,
    /// Skip clicking elements (default: false)
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Verify extractions with pattern matching (default: true)
    #[serde(default)]
    pub verify_extractions: Option<bool>,
    /// Verification similarity threshold (default: 0.85)
    #[serde(default)]
    pub verification_threshold: Option<f32>,
    /// Additional keywords to block
    #[serde(default)]
    pub additional_blocked_keywords: Option<Vec<String>>,
    /// Additional keywords to allow
    #[serde(default)]
    pub additional_safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
}

/// Response for Playwright collection status
#[derive(Debug, Serialize)]
pub struct PlaywrightCollectionStatusResponse {
    pub job_id: Option<String>,
    pub status: String,
    pub url: Option<String>,
    pub progress_message: Option<String>,
    pub progress_percent: Option<i32>,
    pub error: Option<String>,
    pub has_results: Option<bool>,
}

// =============================================================================
// UI Bridge Exploration
// =============================================================================

/// Request to start UI Bridge exploration
#[derive(Debug, Deserialize)]
pub struct StartUIBridgeExplorationRequest {
    /// Target type: "web", "desktop", or "mobile"
    #[serde(default = "default_target_type")]
    pub target_type: String,
    /// Connection URL for the target application
    pub connection_url: String,
    /// Maximum navigation depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<i32>,
    /// Maximum elements per page (default: 20)
    #[serde(default)]
    pub max_elements_per_page: Option<i32>,
    /// Maximum total elements to explore (default: 100)
    #[serde(default)]
    pub max_total_elements: Option<i32>,
    /// Delay between actions in milliseconds (default: 500)
    #[serde(default)]
    pub action_delay_ms: Option<i32>,
    /// Keywords in element text/id to skip
    #[serde(default)]
    pub blocked_keywords: Option<Vec<String>>,
    /// Keywords that are always safe to interact with
    #[serde(default)]
    pub safe_keywords: Option<Vec<String>>,
    /// CSS selectors to skip
    #[serde(default)]
    pub blocked_selectors: Option<Vec<String>>,
    /// Whether to capture screenshots (default: false)
    #[serde(default)]
    pub capture_screenshots: Option<bool>,
    /// Whether to run state discovery on results (default: true)
    #[serde(default)]
    pub run_state_discovery: Option<bool>,
}

fn default_target_type() -> String {
    "web".to_string()
}

/// Start UI Bridge exploration using qontinui library
/// Returns a job_id that can be used to poll for status and results
async fn start_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartUIBridgeExplorationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting UI Bridge exploration for URL: {} (type: {})",
        request.connection_url, request.target_type
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "target_type": request.target_type,
        "connection_url": request.connection_url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(20),
        "max_total_elements": request.max_total_elements.unwrap_or(100),
        "action_delay_ms": request.action_delay_ms.unwrap_or(500),
        "blocked_keywords": request.blocked_keywords.clone().unwrap_or_default(),
        "safe_keywords": request.safe_keywords.clone().unwrap_or_default(),
        "blocked_selectors": request.blocked_selectors.clone().unwrap_or_default(),
        "capture_screenshots": request.capture_screenshots.unwrap_or(false),
        "run_state_discovery": request.run_state_discovery.unwrap_or(true),
    });

    // Short timeout since this just starts the background job
    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_ui_bridge_exploration", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: UI Bridge exploration job started");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start UI Bridge exploration".to_string());
                error!(
                    "MCP API: Failed to start UI Bridge exploration: {}",
                    error_msg
                );
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start UI Bridge exploration: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Request for getting UI Bridge exploration status
#[derive(Debug, Deserialize)]
pub struct UIBridgeExplorationStatusRequest {
    pub job_id: Option<String>,
}

/// Get UI Bridge exploration status
async fn get_ui_bridge_exploration_status(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_status", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "status": "unknown"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration status".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get UI Bridge exploration status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get UI Bridge exploration results
async fn get_ui_bridge_exploration_results(
    State(state): State<Arc<ApiState>>,
    Query(request): Query<UIBridgeExplorationStatusRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();

    let params = serde_json::json!({
        "job_id": request.job_id,
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("get_ui_bridge_exploration_results", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "data": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get exploration results".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get UI Bridge exploration results: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop UI Bridge exploration
async fn stop_ui_bridge_exploration(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping UI Bridge exploration");

    let app_state = state.app_state.clone();

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("stop_ui_bridge_exploration", None, timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: UI Bridge exploration stop requested");
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "message": "Stop requested"
                }))))
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to stop exploration".to_string());
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to stop UI Bridge exploration: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Request for discovering states from render logs
#[derive(Debug, Deserialize)]
pub struct DiscoverStatesRequest {
    /// Array of DOM snapshot render log entries
    pub render_logs: Vec<serde_json::Value>,
}

/// Discover states from render logs using co-occurrence analysis
/// This endpoint runs state discovery on existing render logs without exploration
async fn discover_states_from_renders(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<DiscoverStatesRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Discovering states from {} render logs",
        request.render_logs.len()
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "render_logs": request.render_logs,
    });

    // Allow more time for analysis of large render logs
    let timeout = std::time::Duration::from_secs(60);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("discover_states_from_renders", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: State discovery completed successfully");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "states": [],
                        "elements": [],
                        "elementToRenders": {},
                        "renderCount": 0,
                        "uniqueElementCount": 0
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to discover states from renders".to_string());
                error!("MCP API: Failed to discover states: {}", error_msg);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(error_msg)),
                ))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to discover states from renders: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Error Monitor Endpoints (Application Log Error Detection)
// ============================================================================

/// Get errors from the error monitor
async fn get_error_monitor_errors(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<
    Json<ApiResponse<Vec<crate::error_monitor::StoredErrorEvent>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let task_run_id = query.get("task_run_id").cloned();
    let limit = query
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(100);

    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let errors = crate::error_monitor::ErrorEventStorage::get_unresolved(
        &conn,
        task_run_id.as_deref(),
        limit,
    )
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get errors: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(errors)))
}

/// Get error summary from the error monitor
async fn get_error_monitor_summary(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<
    Json<ApiResponse<crate::error_monitor::ErrorSummary>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let task_run_id = query.get("task_run_id").cloned();

    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let summary =
        crate::error_monitor::ErrorEventStorage::get_summary(&conn, task_run_id.as_deref())
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to get summary: {}", e))),
                )
            })?;

    Ok(Json(ApiResponse::success(summary)))
}

/// Get curated debug context for AI
async fn get_error_debug_context(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let task_run_id = query.get("task_run_id").cloned();

    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let curator = crate::error_monitor::DebugContextCurator::new();
    let context = curator
        .build_context(&conn, task_run_id.as_deref())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to build debug context: {}", e))),
            )
        })?;

    let formatted = curator.format_for_ai(&context);
    Ok(Json(ApiResponse::success(formatted)))
}

/// Request body for resolving an error
#[derive(Debug, Deserialize)]
struct ResolveErrorRequest {
    resolution_notes: Option<String>,
    resolved_by_task_run_id: Option<String>,
}

/// Resolve an error (mark as fixed)
async fn resolve_error_monitor_error(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
    Json(request): Json<ResolveErrorRequest>,
) -> Json<ApiResponse<()>> {
    match state.app_state.checkpoint_db.connection() {
        Ok(conn) => {
            let result = if let Some(ref task_run_id) = request.resolved_by_task_run_id {
                crate::error_monitor::ErrorEventStorage::mark_resolved_by_task(
                    &conn,
                    id,
                    task_run_id,
                    request.resolution_notes.as_deref(),
                )
            } else {
                crate::error_monitor::ErrorEventStorage::update_status(
                    &conn,
                    id,
                    crate::error_monitor::ErrorStatus::Resolved,
                    request.resolution_notes.as_deref(),
                )
            };
            match result {
                Ok(()) => Json(ApiResponse::success(())),
                Err(e) => Json(api_error(format!("Failed to resolve error: {}", e))),
            }
        }
        Err(e) => Json(api_error(format!("Database error: {}", e))),
    }
}

/// Acknowledge an error (mark as seen)
async fn acknowledge_error_monitor_error(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<i64>,
) -> Json<ApiResponse<()>> {
    match state.app_state.checkpoint_db.connection() {
        Ok(conn) => {
            let result = crate::error_monitor::ErrorEventStorage::update_status(
                &conn,
                id,
                crate::error_monitor::ErrorStatus::Acknowledged,
                None,
            );
            match result {
                Ok(()) => Json(ApiResponse::success(())),
                Err(e) => Json(api_error(format!("Failed to acknowledge error: {}", e))),
            }
        }
        Err(e) => Json(api_error(format!("Database error: {}", e))),
    }
}

/// Request body for generating fix workflow
#[derive(Debug, Deserialize)]
struct GenerateFixWorkflowRequest {
    task_run_id: Option<String>,
    max_iterations: Option<u32>,
}

/// Generate a workflow to fix detected errors
async fn generate_fix_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<GenerateFixWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::error_monitor::GeneratedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Database error: {}", e))),
        )
    })?;

    let config = crate::error_monitor::ErrorFixWorkflowConfig {
        task_run_id: request.task_run_id,
        max_iterations: request.max_iterations.unwrap_or(10),
        ..Default::default()
    };
    let generator = crate::error_monitor::ErrorFixWorkflowGenerator::with_config(config);
    let workflow = generator.generate(&conn).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to generate workflow: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(workflow)))
}

/// Start Playwright state collection
async fn start_playwright_collection(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartPlaywrightCollectionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting Playwright collection for URL: {}",
        request.url
    );

    let app_state = state.app_state.clone();

    // Build parameters for Python command
    let params = serde_json::json!({
        "url": request.url,
        "max_depth": request.max_depth.unwrap_or(2),
        "max_elements_per_page": request.max_elements_per_page.unwrap_or(50),
        "max_risk_level": request.max_risk_level.clone().unwrap_or_else(|| "safe".to_string()),
        "dry_run": request.dry_run.unwrap_or(false),
        "verify_extractions": request.verify_extractions.unwrap_or(true),
        "verification_threshold": request.verification_threshold.unwrap_or(0.85),
        "additional_blocked_keywords": request.additional_blocked_keywords.clone(),
        "additional_safe_keywords": request.additional_safe_keywords.clone(),
        "blocked_selectors": request.blocked_selectors.clone(),
    });

    let timeout = std::time::Duration::from_secs(30);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }
            bridge.send_command_and_wait("start_playwright_collection", Some(params), timeout)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                info!("MCP API: Playwright collection started");
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": true
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to start Playwright collection".to_string());
                error!(
                    "MCP API: Playwright collection failed to start: {}",
                    error_msg
                );
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to start Playwright collection: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get Playwright collection status
async fn get_playwright_collection_status(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let job_id = params.get("job_id").cloned();

    let cmd_params = serde_json::json!({
        "job_id": job_id,
    });

    let timeout = std::time::Duration::from_secs(10);

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command_and_wait(
                "get_playwright_collection_status",
                Some(cmd_params),
                timeout,
            )
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "status": "idle",
                        "job_id": null
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());
                error!("MCP API: Playwright collection status error: {}", error_msg);
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "status": "error",
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!("MCP API: Failed to get Playwright collection status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get Playwright collection results
async fn get_playwright_collection_results(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_state = state.app_state.clone();
    let job_id = params.get("job_id").cloned();

    let cmd_params = serde_json::json!({
        "job_id": job_id,
    });

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            // Use longer timeout for getting results (may include large screenshots)
            let timeout = std::time::Duration::from_secs(60);
            bridge.send_command_and_wait(
                "get_playwright_collection_results",
                Some(cmd_params),
                timeout,
            )
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(response) => {
            if response.success {
                if let Some(data) = response.data {
                    Ok(Json(ApiResponse::success(data)))
                } else {
                    Ok(Json(ApiResponse::success(serde_json::json!({
                        "success": false,
                        "error": "No results available"
                    }))))
                }
            } else {
                let error_msg = response
                    .error
                    .unwrap_or_else(|| "Failed to get results".to_string());
                Ok(Json(ApiResponse::success(serde_json::json!({
                    "success": false,
                    "error": error_msg
                }))))
            }
        }
        Err(e) => {
            error!(
                "MCP API: Failed to get Playwright collection results: {}",
                e
            );
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Stop Playwright collection
async fn stop_playwright_collection(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stopping Playwright collection");

    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        with_default_bridge(&app_state, |bridge| {
            bridge.send_command("stop_playwright_collection", None)
        })?
    })
    .await
    .map_err(|e| {
        error!("MCP API: spawn_blocking error: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Internal error: {}", e))),
        )
    })?;

    match result {
        Ok(_) => {
            info!("MCP API: Playwright collection stopped");
            Ok(Json(ApiResponse::success(
                "Playwright collection stopped".to_string(),
            )))
        }
        Err(e) => {
            error!("MCP API: Failed to stop Playwright collection: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Request to restart the runner (for AI self-healing workflow)
#[derive(Debug, Deserialize)]
pub struct RestartRunnerRequest {
    /// Reason for restart (logged for debugging)
    pub reason: String,
    /// Delay before restart in seconds (default: 3)
    #[serde(default)]
    pub delay_seconds: Option<u64>,
}

// ============================================================================
// Prompt Library Request/Response Types
// ============================================================================

/// Request to create a new prompt
#[derive(Debug, Deserialize)]
pub struct CreatePromptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum number of sessions (null = unlimited)
    #[serde(default)]
    pub max_sessions: Option<u32>,
    /// AI provider (e.g., "anthropic", "openai")
    #[serde(default)]
    pub provider: Option<String>,
    /// AI model to use
    #[serde(default)]
    pub model: Option<String>,
    /// Whether this prompt requires the orchestrator for planning and verification
    /// This is a system-level configuration set at prompt creation time
    #[serde(default)]
    pub requires_orchestrator: bool,
    /// Goal description for the orchestrator (used for planning)
    #[serde(default)]
    pub orchestrator_goal: Option<String>,
    /// Maximum iterations for the orchestrator (default: 10)
    #[serde(default)]
    pub orchestrator_max_iterations: Option<u32>,
}

/// Request to update an existing prompt
#[derive(Debug, Deserialize)]
pub struct UpdatePromptRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Maximum number of sessions (null = unlimited)
    #[serde(default)]
    pub max_sessions: Option<Option<u32>>,
    /// AI provider (e.g., "anthropic", "openai")
    #[serde(default)]
    pub provider: Option<Option<String>>,
    /// AI model to use
    #[serde(default)]
    pub model: Option<Option<String>>,
    /// Whether this prompt requires the orchestrator for planning and verification
    #[serde(default)]
    pub requires_orchestrator: Option<bool>,
    /// Goal description for the orchestrator (used for planning)
    #[serde(default)]
    pub orchestrator_goal: Option<Option<String>>,
    /// Maximum iterations for the orchestrator
    #[serde(default)]
    pub orchestrator_max_iterations: Option<Option<u32>>,
}

/// Request to run a prompt
#[derive(Debug, Deserialize)]
pub struct RunPromptRequest {
    // Mode 1: Lookup prompt from database
    /// Prompt ID to lookup from database (mutually exclusive with name+content)
    #[serde(default)]
    pub prompt_id: Option<String>,

    // Mode 2: Ad-hoc prompt (used by qontinui-web)
    /// Task name for display (required for ad-hoc mode)
    #[serde(default)]
    pub name: Option<String>,
    /// Prompt content (required for ad-hoc mode)
    #[serde(default)]
    pub content: Option<String>,

    // Common options
    /// Optional session_id override (auto-generated if not provided)
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional max_sessions override (uses prompt's setting if not provided)
    #[serde(default)]
    pub max_sessions: Option<u32>,

    // Image analysis options (for multimodal analysis)
    /// Image paths to include (screenshots, etc.) - for multimodal analysis
    #[serde(default)]
    pub image_paths: Option<Vec<String>>,
    /// Video paths to extract frames from
    #[serde(default)]
    pub video_paths: Option<Vec<String>>,
    /// Path to Playwright trace ZIP file (will extract timeline and screenshots)
    #[serde(default)]
    pub trace_path: Option<String>,
    /// Maximum number of frames to extract from each video (default: 3)
    #[serde(default)]
    pub max_video_frames: Option<usize>,
    /// Maximum number of screenshots to extract from trace (default: 5)
    #[serde(default)]
    pub max_trace_screenshots: Option<usize>,

    // Context injection options
    /// Context IDs to explicitly include in the prompt
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    /// Whether to auto-detect and include relevant contexts (default: false)
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,
}

/// Response from running a prompt
#[derive(Debug, Serialize)]
pub struct RunPromptResponse {
    pub task_run_id: String,
    pub session_id: String,
    /// Backward compatibility alias for task_run_id
    pub action_id: String,
    pub state_file: String,
    pub log_file: String,
    pub pid: Option<u32>,
}

/// Request to import prompts
#[derive(Debug, Deserialize)]
pub struct ImportPromptsRequest {
    /// JSON array of prompts to import
    pub prompts_json: String,
}

/// Request to duplicate a prompt
#[derive(Debug, Deserialize)]
pub struct DuplicatePromptRequest {
    /// Optional new name (defaults to "Original Name (Copy)")
    #[serde(default)]
    pub new_name: Option<String>,
}

// ============================================================================
// Macro Request/Response Types
// ============================================================================

use crate::macros;

/// Request to create a new macro
#[derive(Debug, Deserialize)]
pub struct CreateMacroRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<macros::MacroStep>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Request to update an existing macro
#[derive(Debug, Deserialize)]
pub struct UpdateMacroRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<macros::MacroStep>>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ============================================================================
// Workflow Request/Response Types
// ============================================================================

/// AI output event payload (emitted to frontend)
#[derive(Debug, Clone, Serialize)]
pub struct AiOutputEvent {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String, // "prompt" or "claude"
    #[serde(rename = "actionId")]
    pub action_id: Option<String>, // Unique ID per AI loop/action within a session
    /// Parent task run ID (matches task_runs.id in database)
    #[serde(rename = "taskRunId")]
    pub task_run_id: Option<String>,
    /// Session ID for grouping output (may include phase suffix like "-agentic-1")
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "sessionName")]
    pub session_name: Option<String>, // Human-readable session name
    /// Workflow phase: setup, verification, agentic, or completion
    pub phase: Option<String>,
    /// Iteration number within the phase (1, 2, 3...)
    #[serde(rename = "phaseIteration")]
    pub phase_iteration: Option<u32>,
}

// Re-export AiSessionContext from the canonical location
pub use crate::execution_context::AiSessionContext;
use crate::runtime_env::{AiSessionContextExt, ExecutionContextExt};

/// Context for finding detection during AI sessions.
/// Contains the information needed to store findings in the database.
#[derive(Debug, Clone)]
pub struct FindingContext {
    /// The task_run_id for storing findings (same as session_id in most cases)
    pub task_run_id: String,
    /// The current session/phase number within the task run
    pub session_num: u32,
}

/// Context for progress marker detection during AI sessions.
/// Contains the information needed to store progress markers in the database.
#[derive(Debug, Clone)]
pub struct ProgressContext {
    /// The checkpoint_id for storing progress markers.
    /// This links progress markers to a specific step checkpoint.
    pub checkpoint_id: String,
    /// The task_run_id for context (used in event emission)
    pub task_run_id: String,
}

// ============================================================================
// Playwright Script Request Types
// ============================================================================

use crate::playwright::{self, DisplayMode};

/// Request to create a new Playwright script
#[derive(Debug, Deserialize)]
pub struct CreatePlaywrightScriptRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ai_instructions: Option<String>,
    #[serde(default)]
    pub target_url: String,
    pub script_content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_playwright_timeout")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub display_mode: DisplayMode,
    #[serde(default = "default_playwright_browser")]
    pub browser: String,
}

fn default_playwright_timeout() -> u32 {
    60
}

fn default_playwright_browser() -> String {
    "chromium".to_string()
}

/// Request to update an existing Playwright script
#[derive(Debug, Deserialize)]
pub struct UpdatePlaywrightScriptRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub ai_instructions: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub script_content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    #[serde(default)]
    pub display_mode: Option<DisplayMode>,
    #[serde(default)]
    pub browser: Option<String>,
}

/// Request to run a Playwright script
#[derive(Debug, Deserialize)]
pub struct RunPlaywrightScriptRequest {
    /// Optional URL override for this run
    #[serde(default)]
    pub target_url_override: Option<String>,
}

/// Request to import Playwright scripts
#[derive(Debug, Deserialize)]
pub struct ImportPlaywrightScriptsRequest {
    /// JSON array of scripts to import
    pub scripts_json: String,
}

/// Request to duplicate a Playwright script
#[derive(Debug, Deserialize)]
pub struct DuplicatePlaywrightScriptRequest {
    /// Optional new name (defaults to "Original Name (Copy)")
    #[serde(default)]
    pub new_name: Option<String>,
}

// ============================================================================
// Scriptlet Request Types
// ============================================================================

/// Request to create a new scriptlet
#[derive(Debug, Deserialize)]
pub struct CreateScriptletRequest {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source_log_ids: Option<Vec<String>>,
}

/// Request to update an existing scriptlet
#[derive(Debug, Deserialize)]
pub struct UpdateScriptletRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

// ============================================================================
// Inline Python Execution Types
// ============================================================================

/// Request to execute inline Python code
#[derive(Debug, Deserialize)]
pub struct InlinePythonRequest {
    /// Python code to execute
    pub code: String,
    /// Optional pip packages to install (uses uvx for isolation)
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    /// Execution timeout in seconds (default: 30)
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Working directory for execution (default: temp dir)
    #[serde(default)]
    pub working_directory: Option<String>,
}

/// Response from inline Python execution
#[derive(Debug, Serialize)]
pub struct InlinePythonResponse {
    /// Whether execution succeeded (exit code 0)
    pub success: bool,
    /// Stdout from the script
    pub stdout: String,
    /// Stderr from the script
    pub stderr: String,
    /// Return value if the script returned JSON via __QONTINUI_RETURN__ marker
    pub return_value: Option<serde_json::Value>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

/// Emit AI output event to frontend
pub fn emit_ai_output(
    app_handle: &tauri::AppHandle,
    line: &str,
    source: &str,
    action_id: Option<&str>,
    session_ctx: Option<&AiSessionContext>,
) {
    let event = AiOutputEvent {
        id: format!(
            "ai-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            rand::random::<u32>()
        ),
        timestamp: chrono::Utc::now().timestamp_millis(),
        line: line.to_string(),
        source: source.to_string(),
        action_id: action_id.map(|s| s.to_string()),
        task_run_id: session_ctx.map(|ctx| ctx.task_run_id().to_string()),
        session_id: session_ctx.map(|ctx| ctx.session_id.clone()),
        session_name: session_ctx.map(|ctx| ctx.session_name.clone()),
        phase: session_ctx.map(|ctx| ctx.phase().as_str().to_string()),
        phase_iteration: session_ctx.and_then(|ctx| ctx.iteration()),
    };

    if let Err(e) = app_handle.emit("ai-output", &event) {
        warn!("Failed to emit AI output event: {}", e);
    }
}

/// Write AI debug log to file
#[allow(dead_code)]
fn write_ai_debug_log(message: &str) {
    use std::io::Write;

    // Get the .dev-logs directory
    let log_dir = if let Ok(exe_path) = std::env::current_exe() {
        exe_path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(|p| p.join(".dev-logs"))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    } else {
        std::path::PathBuf::from(".")
    };

    let log_file = log_dir.join("ai_execution_debug.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{}] {}", timestamp, message);
    }
}

/// Stop the currently running AI analysis
///
/// This endpoint stops all running tasks by:
/// 1. Killing all tracked AI process PIDs (the actual Claude CLI processes)
/// 2. Getting running task runs from the database
/// 3. Stopping monitoring for each task
/// 4. Marking tasks as stopped in the database
async fn stop_ai_analysis(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Stop AI analysis requested");

    // First, kill all tracked AI processes immediately
    // This is the key fix - previously we only stopped monitoring, not the actual processes
    let pids_to_kill: Vec<u32> = {
        let mut pids = safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
        let pids_copy = pids.clone();
        pids.clear(); // Clear the tracker
        pids_copy
    };

    let mut killed_count = 0;
    for pid in &pids_to_kill {
        info!("MCP API: Killing AI process PID {}", pid);
        // Use taskkill with /T to kill the entire process tree (cmd.exe spawns node.exe for claude)
        // /F forces termination, /T terminates child processes
        let result = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    info!("MCP API: Successfully killed process tree for PID {}", pid);
                    killed_count += 1;
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        "MCP API: taskkill for PID {} returned error: {}",
                        pid, stderr
                    );
                    // Process may have already exited, which is fine
                    killed_count += 1;
                }
            }
            Err(e) => {
                error!("MCP API: Failed to execute taskkill for PID {}: {}", pid, e);
            }
        }
    }

    if !pids_to_kill.is_empty() {
        emit_ai_output(
            &state.app_handle,
            &format!("⛔ Killed {} AI process(es)", killed_count),
            "status",
            None,
            None,
        );
    }

    // Get running tasks from the database
    let db = match CheckpointDb::new() {
        Ok(db) => db,
        Err(e) => {
            error!("MCP API: Failed to open database: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to open database: {}", e))),
            ));
        }
    };

    let running_tasks = match db.get_running_task_runs() {
        Ok(tasks) => tasks,
        Err(e) => {
            error!("MCP API: Failed to get running tasks: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get running tasks: {}", e))),
            ));
        }
    };

    if running_tasks.is_empty() && pids_to_kill.is_empty() {
        info!("MCP API: No running tasks to stop");
        return Ok(Json(ApiResponse::success(())));
    }

    // Stop each running task
    for task in &running_tasks {
        // Mark as stopped in database
        if let Err(e) = db.stop_task_run(&task.id) {
            warn!("MCP API: Failed to stop task run {}: {}", task.id, e);
        }

        info!("MCP API: Stopped task run: {}", task.id);
    }

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!(
            "Stopped {} running task(s), killed {} process(es)",
            running_tasks.len(),
            killed_count
        ),
        "status",
        None,
        None,
    );

    info!(
        "MCP API: Stopped {} AI analysis task(s)",
        running_tasks.len()
    );
    Ok(Json(ApiResponse::success(())))
}

/// Restart the runner (for AI self-healing workflow)
///
/// This endpoint allows the AI to trigger a runner restart after applying fixes.
/// The restart is delayed to allow the response to be sent first.
async fn restart_runner(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RestartRunnerRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let delay_secs = request.delay_seconds.unwrap_or(3);

    info!(
        "MCP API: Runner restart requested - reason: {}, delay: {}s",
        request.reason, delay_secs
    );

    // Emit status to frontend so user knows what's happening
    emit_ai_output(
        &state.app_handle,
        &format!(
            "🔄 Restarting runner in {} seconds: {}",
            delay_secs, request.reason
        ),
        "status",
        None, // No action_id for restart status
        None, // No session context for restart status
    );

    // Spawn a task to exit after delay
    // The Tauri dev server will automatically restart the app
    let delay = std::time::Duration::from_secs(delay_secs);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        info!("MCP API: Exiting for restart...");
        std::process::exit(0);
    });

    Ok(Json(ApiResponse::success(())))
}

// ============================================================================
// AI Developer (Persistent Mode) HTTP Endpoints
// ============================================================================

/// Check if any AI analysis tasks are currently running (sync version).
/// Uses the provided database to check for running task runs.
/// NOTE: This is a synchronous function that blocks. For async contexts,
/// use has_running_ai_tasks_async() or wrap this in spawn_blocking.
#[allow(dead_code)]
pub fn has_running_ai_tasks(db: &Arc<CheckpointDb>) -> bool {
    match db.get_running_task_runs() {
        Ok(tasks) => !tasks.is_empty(),
        Err(e) => {
            warn!("Failed to check running tasks: {}", e);
            false
        }
    }
}

/// Check if any AI analysis tasks are currently running (async version).
/// Uses spawn_blocking to avoid blocking the async runtime.
async fn has_running_ai_tasks_async(db: Arc<CheckpointDb>) -> bool {
    match tokio::task::spawn_blocking(move || db.get_running_task_runs()).await {
        Ok(Ok(tasks)) => !tasks.is_empty(),
        Ok(Err(e)) => {
            warn!("Failed to check running tasks: {}", e);
            false
        }
        Err(e) => {
            warn!("spawn_blocking error checking running tasks: {}", e);
            false
        }
    }
}

/// Migrate JSONL logs to SQLite for a completed task run.
/// This should be called after a task completes (success or failure) to persist logs.
async fn migrate_logs_for_task(
    db: Arc<CheckpointDb>,
    task_id: &str,
    workflow_name: Option<String>,
) {
    let task_id_owned = task_id.to_string();

    // Get the dev-logs directory path
    let dev_logs_dir = match std::env::current_exe() {
        Ok(exe_path) => {
            // Navigate up to find the parent directory containing .dev-logs
            let mut current = exe_path.as_path();
            loop {
                if let Some(parent) = current.parent() {
                    let dev_logs = parent.join(".dev-logs");
                    if dev_logs.exists() {
                        break dev_logs;
                    }
                    // Also check parent's parent (for qontinui_parent_directory)
                    if let Some(grandparent) = parent.parent() {
                        let dev_logs = grandparent.join(".dev-logs");
                        if dev_logs.exists() {
                            break dev_logs;
                        }
                    }
                    current = parent;
                } else {
                    // Fallback to a reasonable default
                    warn!("Could not find .dev-logs directory, skipping log migration");
                    return;
                }
            }
        }
        Err(e) => {
            warn!("Failed to get executable path for log migration: {}", e);
            return;
        }
    };

    info!(
        "Migrating JSONL logs to SQLite for task run: {}",
        task_id_owned
    );

    let result = tokio::task::spawn_blocking(move || {
        crate::log_migration::migrate_logs_to_sqlite(
            &db,
            &task_id_owned,
            &dev_logs_dir,
            workflow_name.as_deref(),
        )
    })
    .await;

    match result {
        Ok(Ok(migration_result)) => {
            info!(
                "Log migration complete for task {}: {} general, {} actions, {} image recognition, {} screenshots, {} playwright",
                task_id,
                migration_result.general_events,
                migration_result.action_events,
                migration_result.image_recognition_events,
                migration_result.screenshots,
                migration_result.playwright_results
            );
            if !migration_result.errors.is_empty() {
                warn!(
                    "Log migration had {} errors: {:?}",
                    migration_result.errors.len(),
                    migration_result.errors
                );
            }
        }
        Ok(Err(e)) => {
            warn!("Failed to migrate logs for task {}: {}", task_id, e);
        }
        Err(e) => {
            warn!(
                "spawn_blocking error during log migration for task {}: {}",
                task_id, e
            );
        }
    }
}

/// Helper function to mark a task run as complete with retry logic.
/// Retries up to 3 times with exponential backoff (100ms, 200ms, 400ms).
/// Returns true if successfully marked complete, false otherwise.
/// Also triggers log migration to persist JSONL logs to SQLite.
///
/// Uses gated function - unified workflows have status managed by LoopController only.
async fn complete_task_run_with_retry(db: Arc<CheckpointDb>, task_id: &str) -> bool {
    let task_id_owned = task_id.to_string();
    let max_retries = 3;

    // Get workflow name before completion for log migration context
    let workflow_name = db
        .get_task_run(&task_id_owned)
        .ok()
        .flatten()
        .and_then(|t| t.workflow_name);

    for retry in 0..max_retries {
        let db_clone = db.clone();
        let id = task_id_owned.clone();

        // Use gated function - unified workflows have status managed by LoopController
        match tokio::task::spawn_blocking(move || {
            db_clone.complete_task_run_if_allowed(&id, "complete_task_run_with_retry")
        })
        .await
        {
            Ok(Ok(true)) => {
                // Successfully marked complete
                if retry > 0 {
                    info!(
                        "Task run {} marked complete after {} retries",
                        task_id_owned, retry
                    );
                }

                // Migrate logs to SQLite after successful completion
                migrate_logs_for_task(db.clone(), &task_id_owned, workflow_name).await;

                return true;
            }
            Ok(Ok(false)) => {
                // Unified workflow - status managed by LoopController, not an error
                return true;
            }
            Ok(Err(e)) => {
                if retry < max_retries - 1 {
                    let delay_ms = 100 * (1 << retry); // 100, 200, 400ms
                    warn!(
                        "Retry {}/{} marking task_run {} complete (waiting {}ms): {}",
                        retry + 1,
                        max_retries,
                        task_id_owned,
                        delay_ms,
                        e
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                } else {
                    error!(
                        "Failed to mark task_run {} as complete after {} retries: {}",
                        task_id_owned, max_retries, e
                    );
                }
            }
            Err(e) => {
                error!(
                    "spawn_blocking error marking task_run {} complete: {}",
                    task_id_owned, e
                );
                return false;
            }
        }
    }

    false
}

/// Helper function to get workspace paths (reused from config.rs pattern)
pub fn get_workspace_paths_internal(
) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf), String> {
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

    let mut current = exe_path.as_path();
    let runner_dir = loop {
        if let Some(parent) = current.parent() {
            if parent.join("src-tauri").exists()
                || parent.file_name().is_some_and(|n| n == "qontinui-runner")
            {
                break parent.to_path_buf();
            }
            current = parent;
        } else {
            let cwd = std::env::current_dir()
                .map_err(|e| format!("Failed to get current directory: {}", e))?;
            break cwd;
        }
    };

    let workspace_root = runner_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| runner_dir.clone());
    let dev_logs_path = workspace_root.join(".dev-logs");
    let scripts_path = workspace_root
        .join("qontinui-claude-config")
        .join("scripts");

    Ok((workspace_root, dev_logs_path, scripts_path))
}

/// Generate MCP tool context documentation for AI sessions.
///
/// This function creates a markdown documentation string describing the available
/// MCP tools for GUI automation, including the specific workflows, states, and
/// images available in the loaded configuration.
fn generate_mcp_tool_context(config: &crate::config::QontinuiConfig) -> String {
    let mut context = String::from(
        r#"
## Available GUI Automation Tools

The following MCP tools are available for deterministic GUI automation.
All actions execute through the unified action service with the pre-loaded config.

### Tools

"#,
    );

    // Tool: run_workflow
    let workflows: Vec<String> = config
        .workflows
        .iter()
        .filter_map(|w| w.get("name").and_then(|n| n.as_str()))
        .map(|n| format!("- {}", n))
        .collect();

    context.push_str(&format!(
        r#"
#### run_workflow
Run a workflow by name from the loaded configuration.

**Available Workflows:**
{}

**Usage:**
```json
{{"tool": "mcp__qontinui__run_workflow", "workflow_name": "WorkflowName", "monitor": "primary"}}
```
"#,
        if workflows.is_empty() {
            "- (none loaded)".to_string()
        } else {
            workflows.join("\n")
        }
    ));

    // Tool: go_to_state
    let states: Vec<String> = config
        .states
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
        .map(|n| format!("- {}", n))
        .collect();

    context.push_str(&format!(
        r#"
#### go_to_state
Navigate to a specific state using pathfinding.

**Available States:**
{}

**Usage:**
```json
{{"tool": "mcp__qontinui__go_to_state", "state_id": "StateName"}}
```
"#,
        if states.is_empty() {
            "- (none loaded)".to_string()
        } else {
            states.join("\n")
        }
    ));

    // Tool: execute_action
    let images: Vec<String> = config
        .images
        .iter()
        .take(20) // Limit to avoid context overflow
        .filter_map(|i| i.get("id").and_then(|id| id.as_str()))
        .map(|id| format!("- {}", id))
        .collect();

    context.push_str(&format!(
        r#"
#### execute_action
Execute a single action (click, type, etc.) on a target image.

**Available Images (first 20):**
{}

**Action Types:** click, double_click, right_click, type

**Usage:**
```json
{{"tool": "mcp__qontinui__execute_action", "action_type": "click", "image_id": "image-123"}}
```
"#,
        if images.is_empty() {
            "- (none loaded)".to_string()
        } else {
            images.join("\n")
        }
    ));

    // Tool: capture_screenshot
    context.push_str(
        r#"
#### capture_screenshot
Capture a screenshot from a specified monitor.

**Usage:**
```json
{"tool": "mcp__qontinui__capture_screenshot", "monitor": 0, "delay_seconds": 1.0}
```
"#,
    );

    context
}

// ============================================================================
// Prompt Library HTTP Endpoints
// ============================================================================

use crate::backup;
use crate::prompts;

/// List all prompts
async fn list_prompts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let prompts = prompts::get_all_prompts();
    Ok(Json(ApiResponse::success(prompts)))
}

/// Get a single prompt by ID
async fn get_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::get_prompt(&id) {
        Some(prompt) => Ok(Json(ApiResponse::success(prompt))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Prompt not found: {}", id))),
        )),
    }
}

/// Create a new prompt
async fn create_prompt(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::create_prompt(
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_sessions,
        request.provider,
        request.model,
        request.requires_orchestrator,
        request.orchestrator_goal,
        request.orchestrator_max_iterations,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing prompt
async fn update_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::update_prompt(
        &id,
        request.name,
        request.description,
        request.content,
        request.category,
        request.tags,
        request.max_sessions,
        request.provider,
        request.model,
        request.requires_orchestrator,
        request.orchestrator_goal,
        request.orchestrator_max_iterations,
    ) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a prompt
async fn delete_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::delete_prompt(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Run a prompt by spawning a Claude session
///
/// Supports two modes:
/// 1. Lookup prompt from database: provide `prompt_id`
/// 2. Ad-hoc prompt: provide `name` and `content`
///
/// Optional image analysis: provide `image_paths`, `video_paths`, or `trace_path`
/// to enhance the prompt with visual analysis data.
async fn run_prompt(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunPromptRequest>,
) -> Result<Json<ApiResponse<RunPromptResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Determine mode and get prompt name + content + orchestrator config
    // Orchestrator config is extracted from saved prompts (system-level setting, not user-controllable)
    let (
        prompt_name,
        prompt_content,
        prompt_id,
        prompt_max_sessions,
        requires_orchestrator,
        _orchestrator_goal,
        _orchestrator_max_iterations,
        _orchestrator_verification_first,
    ) = if let Some(ref id) = request.prompt_id {
        // Mode 1: Lookup from database
        let prompt = prompts::get_prompt(id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Prompt not found: {}", id))),
            )
        })?;
        (
            prompt.name.clone(),
            prompt.content.clone(),
            Some(prompt.id.clone()),
            prompt.max_sessions,
            prompt.requires_orchestrator,
            prompt.orchestrator_goal.clone(),
            prompt.orchestrator_max_iterations,
            prompt.orchestrator_verification_first,
        )
    } else if let (Some(name), Some(content)) = (&request.name, &request.content) {
        // Mode 2: Ad-hoc prompt (no orchestrator by default)
        (
            name.clone(),
            content.clone(),
            None,
            None,
            false,
            None,
            None,
            None,
        )
    } else {
        // Invalid: neither mode satisfied
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "Must provide either prompt_id OR (name AND content)",
            )),
        ));
    };

    // Generate session_id if not provided
    let session_id = request.session_id.unwrap_or_else(|| {
        format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            rand::random::<u16>()
        )
    });

    // Use override or prompt's setting (None = unlimited sessions)
    let max_sessions = request.max_sessions.or(prompt_max_sessions);

    // Use session_id as task_run_id (they are the same)
    let task_run_id = session_id.clone();

    // Auto-load last config if not already loaded and auto_load_last_config is enabled
    // This ensures GUI automation tasks have access to workflows
    let config_was_loaded = {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        config_lock.is_some()
    };

    let mut config_info: Option<(String, Option<String>, Option<i32>)> = None;
    if !config_was_loaded && settings::get_auto_load_last_config() {
        if let Some(config_path) = settings::get_last_config_path() {
            if std::path::Path::new(&config_path).exists() {
                info!(
                    "MCP API: Auto-loading last config for prompt execution: {}",
                    config_path
                );

                // Load the config
                match crate::config::ConfigLoader::load_from_file(&config_path) {
                    Ok(config) => {
                        // Store the config
                        let mut config_lock =
                            safe_lock_or_recover(&state.app_state.current_config, "current_config");
                        *config_lock = Some(config);

                        let workflow_id = settings::get_last_workflow_id();
                        let monitor_index = settings::get_last_monitor_index();
                        config_info = Some((config_path.clone(), workflow_id, monitor_index));

                        info!(
                            "MCP API: Auto-loaded config: {:?}, workflow: {:?}, monitor: {:?}",
                            config_path,
                            config_info.as_ref().map(|c| &c.1),
                            config_info.as_ref().map(|c| &c.2)
                        );
                    }
                    Err(e) => {
                        warn!("MCP API: Failed to auto-load config: {}", e);
                    }
                }
            }
        }
    }

    // Collect images for analysis if provided
    let image_paths = request.image_paths.unwrap_or_default();
    let video_paths = request.video_paths.unwrap_or_default();
    let max_video_frames = request.max_video_frames.unwrap_or(3) as u32;
    let max_trace_screenshots = request.max_trace_screenshots.unwrap_or(5) as u32;

    let (all_images, trace_timeline) = collect_images_for_analysis(
        &image_paths,
        &video_paths,
        request.trace_path.as_deref(),
        max_video_frames,
        max_trace_screenshots,
    );

    // Build enhanced prompt with trace timeline and image references if available
    let mut enhanced_prompt = prompt_content.clone();

    // Inject contexts into the prompt if requested
    let context_ids = request.context_ids.unwrap_or_default();
    let auto_include_contexts = request.auto_include_contexts.unwrap_or(false);

    // Extract action types from loaded config for auto-detection
    let action_types: Vec<String> = {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        if let Some(ref config) = *config_lock {
            // Extract action types from workflows
            config
                .workflows
                .iter()
                .flat_map(|w| {
                    w.get("actions")
                        .and_then(|a| a.as_array())
                        .map(|actions| {
                            actions
                                .iter()
                                .filter_map(|action| {
                                    action
                                        .get("type")
                                        .and_then(|t| t.as_str())
                                        .map(String::from)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .collect()
        } else {
            Vec::new()
        }
    };

    // For now, we pass an empty error list for auto-detection
    // In the future, this could be populated from recent log errors
    let recent_errors: Vec<String> = Vec::new();

    // Inject contexts and track which ones were used
    let (prompt_with_contexts, used_context_ids) =
        if !context_ids.is_empty() || auto_include_contexts {
            let (enhanced, used_ids) = context::inject_contexts(
                &enhanced_prompt,
                &context_ids,
                auto_include_contexts,
                &prompt_content, // Use original prompt for auto-detection matching
                &action_types,
                &recent_errors,
            );

            if !used_ids.is_empty() {
                info!(
                    "MCP API: Injected {} contexts into prompt: {:?}",
                    used_ids.len(),
                    used_ids
                );
            }

            (enhanced, used_ids)
        } else {
            (enhanced_prompt.clone(), Vec::new())
        };
    enhanced_prompt = prompt_with_contexts;

    // Prepend runner-triggered context and supervisor instructions
    // This tells the AI session how to safely restart the runner if needed
    let supervisor_available = check_supervisor_available();
    let runner_context = if supervisor_available {
        r#"## IMPORTANT: Runner-Triggered Session Context

You are being run BY the qontinui-runner. You are a child process of the runner.

**CRITICAL RULES:**
1. Do NOT restart the qontinui-runner directly - it will kill your session
2. You CAN restart backend and frontend without issues
3. If the runner needs to be restarted, USE THE SUPERVISOR API

**Restarting Runner via Supervisor (SAFE):**
```powershell
# Simple restart (no rebuild)
Invoke-RestMethod -Uri "http://localhost:9875/runner/restart" -Method Post -ContentType "application/json" -Body '{"trigger_auto_continue": true}'

# Restart with REBUILD (use after modifying runner Rust code)
Invoke-RestMethod -Uri "http://localhost:9875/runner/restart" -Method Post -ContentType "application/json" -Body '{"rebuild": true, "trigger_auto_continue": true}'
```

**Supervisor API (port 9875):**
- GET /health - Check if supervisor is running
- POST /runner/stop - Stop the runner
- POST /runner/restart - Restart runner (options: rebuild, trigger_auto_continue, wait_timeout_seconds)

**IMPORTANT:** If you modified qontinui-runner Rust code, use `"rebuild": true` to recompile before restart.

---

"#
    } else {
        r#"## IMPORTANT: Runner-Triggered Session Context

You are being run BY the qontinui-runner. You are a child process of the runner.

**CRITICAL RULES:**
1. Do NOT restart the qontinui-runner directly - it will kill your session
2. You CAN restart backend and frontend without issues
3. The supervisor is NOT currently running - if runner restart is needed, inform the user

**If runner restart is needed:**
Tell the user: "The qontinui-runner needs to be restarted manually to apply changes."

---

"#
    };

    enhanced_prompt = format!("{}{}", runner_context, enhanced_prompt);

    // Inject Multi-Step Task Guide context (user override takes precedence)
    let multi_step_guide = context::get_multi_step_guide();
    let multi_step_section = format!(
        "## Multi-Session Task Context\n\n{}\n\n---\n\n",
        context::format_single_context(&multi_step_guide)
    );
    enhanced_prompt = format!("{}{}", multi_step_section, enhanced_prompt);

    // Inject Service Restart Commands context (user override takes precedence)
    // Replace {{WORKSPACE}} placeholder with actual workspace path
    let service_restart = context::get_service_restart_commands();
    let workspace_path = get_workspace_paths_internal()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
        .unwrap_or_else(|_| "{{WORKSPACE}}".to_string());
    let service_restart_content = service_restart
        .content
        .replace("{{WORKSPACE}}", &workspace_path);
    let mut service_restart_with_path = service_restart.clone();
    service_restart_with_path.content = service_restart_content;
    let service_restart_section = format!(
        "{}\n\n---\n\n",
        context::format_single_context(&service_restart_with_path)
    );
    enhanced_prompt = format!("{}{}", service_restart_section, enhanced_prompt);

    // Inject configured log sources from global settings
    // This tells the AI where to find logs for debugging
    {
        let global_settings = crate::settings::get_global_log_source_settings();
        let enabled_sources: Vec<_> = global_settings
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| format!("- **{}**: `{}`", s.name, s.path))
            .collect();

        if !enabled_sources.is_empty() {
            let log_sources_section = format!(
                r#"## Configured Log Sources

The following log files have been configured for monitoring. Use these paths to check for errors:

{}

---

"#,
                enabled_sources.join("\n")
            );
            enhanced_prompt = format!("{}{}", log_sources_section, enhanced_prompt);
        }
    }

    // Add GUI automation context if config was auto-loaded
    if let Some((config_path, workflow_id, monitor_index)) = &config_info {
        let workflow_info = workflow_id
            .as_ref()
            .map(|w| format!("- Last workflow: {}", w))
            .unwrap_or_else(|| "- No last workflow saved".to_string());
        let monitor_info = monitor_index
            .map(|m| format!("- Last monitor index: {}", m))
            .unwrap_or_else(|| "- No last monitor index saved".to_string());

        let gui_context = format!(
            r#"
## GUI Automation Available

A workflow configuration has been auto-loaded:
- Config path: {}
{}
{}

**Runner MCP API (port 9876):**
- GET /status - Check runner and config status
- POST /run-workflow - Run a workflow by name
  Example: `Invoke-RestMethod -Uri "http://localhost:9876/run-workflow" -Method Post -ContentType "application/json" -Body '{{"workflow_id": "workflow-name", "monitor_index": 0}}'`
- GET /monitors - List available monitors

If your task requires running visual automation, use the Runner API to execute workflows.

---

"#,
            config_path, workflow_info, monitor_info
        );

        enhanced_prompt = format!("{}{}", gui_context, enhanced_prompt);
    }

    // Add MCP tool context if config is loaded (either pre-loaded or auto-loaded)
    {
        let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
        if let Some(config) = config_lock.as_ref() {
            let tool_context = generate_mcp_tool_context(config);
            enhanced_prompt = format!("{}\n{}", enhanced_prompt, tool_context);
        }
    }

    if let Some(timeline) = &trace_timeline {
        enhanced_prompt = format!("{}\n\n{}", enhanced_prompt, timeline);
    }

    // Add image paths to prompt if there are any
    if !all_images.is_empty() {
        enhanced_prompt = format!(
            "{}\n\n## Images for Analysis\n\nThe following images are available for analysis. Use the Read tool to view them:\n{}",
            enhanced_prompt,
            all_images.iter().map(|p| format!("- {}", p)).collect::<Vec<_>>().join("\n")
        );
    }

    // Add structured finding output instructions
    enhanced_prompt = format!("{}{}", enhanced_prompt, FINDING_INSTRUCTIONS);

    info!(
        "MCP API: Running prompt '{}' (session: {}, max_sessions: {:?}, requires_orchestrator: {}, images: {})",
        prompt_name,
        session_id,
        max_sessions,
        requires_orchestrator,
        all_images.len()
    );

    // Create TaskRun record in database
    let db = CheckpointDb::new().map_err(|e| {
        error!("MCP API: Failed to open database: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to open database: {}", e))),
        )
    })?;

    {
        let mut input = CreateTaskRunInput::new(&task_run_id, &prompt_name)
            .with_prompt(&enhanced_prompt)
            .with_task_type("task");
        if let Some(ms) = max_sessions {
            input = input.with_max_sessions(ms);
        }
        db.create_task_run(&input)
    }
    .map_err(|e| {
        error!("MCP API: Failed to create task run: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create task run: {}", e))),
        )
    })?;

    info!("MCP API: Created task run with ID: {}", task_run_id);

    // Create session context for AI output events so frontend can display the task name
    // This is the first turn (iteration 1), so turn_count = 1
    let session_ctx = AiSessionContext::agentic(&task_run_id, &prompt_name, 1)
        .with_runtime_env()
        .with_new_trace()
        .with_ai_settings()
        .with_turn_count(1);

    // Emit prompt to frontend (use original prompt content for display)
    emit_ai_output(
        &state.app_handle,
        &prompt_content,
        "prompt",
        Some(&task_run_id),
        Some(&session_ctx),
    );

    // Emit status indicator
    emit_ai_output(
        &state.app_handle,
        "AI session spawned - check task runs for status",
        "status",
        Some(&task_run_id),
        Some(&session_ctx),
    );

    // Record context usage now that the session is starting
    if !used_context_ids.is_empty() {
        context::record_contexts_used(&used_context_ids);
    }

    // =========================================================================
    // EXECUTION PATH ROUTING
    // =========================================================================
    // When requires_orchestrator is true, route through the unified session API
    // which has full orchestrator support (planning, verification, feedback loops).
    // When false, use the simpler direct spawn path.
    // =========================================================================

    // NOTE: The orchestrator path was removed when run_unified_session_loop was deleted.
    // All paths now use the direct spawn path. Orchestrator functionality will be
    // re-integrated via LoopController in a future update.
    if requires_orchestrator {
        warn!(
            "MCP API: Orchestrator path requested but session loop was removed. Falling through to direct spawn path for prompt '{}' (session: {})",
            prompt_name, session_id
        );
    }

    // Always use the direct spawn path for now
    {
        // =====================================================================
        // DIRECT SPAWN PATH
        // =====================================================================
        // DIRECT SPAWN PATH
        // =====================================================================
        // Use the simpler direct spawn path.
        // Orchestrator functionality will be re-integrated via LoopController.
        // =====================================================================

        info!(
            "MCP API: Using direct spawn path for prompt '{}' (session: {})",
            prompt_name, session_id
        );

        let prompt_name_for_state = prompt_name.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (workspace_root, dev_logs_path, scripts_path) = get_workspace_paths_internal()?;
            let spawn_script = scripts_path.join("spawn-independent-claude.py");
            let state_file = dev_logs_path.join(format!("ai-developer-{}.json", session_id));
            let prompt_file = dev_logs_path.join(format!("ai-developer-{}-prompt.txt", session_id));
            let log_file = dev_logs_path.join(format!("claude-session-{}.log", session_id));

            // Ensure .dev-logs directory exists
            std::fs::create_dir_all(&dev_logs_path)
                .map_err(|e| format!("Failed to create dev-logs directory: {}", e))?;

            // Create initial state file
            let initial_state = serde_json::json!({
                "session_id": session_id,
                "task_run_id": session_id,
                "prompt_id": prompt_id,
                "prompt_name": prompt_name_for_state,
                "session_count": 1,
                "max_sessions": max_sessions,
                "status": "starting",
                "started_at": chrono::Utc::now().to_rfc3339(),
                "stop_requested": false,
                "current_action": "Initializing",
                "errors_fixed": [],
                "errors_remaining": [],
                "activity_log": [],
                // Orchestrator not used in direct spawn path
                "requires_orchestrator": false,
                "orchestrator_goal": null,
                "orchestrator_max_iterations": null
            });

            let state_json = serde_json::to_string_pretty(&initial_state)
                .map_err(|e| format!("Failed to serialize state: {}", e))?;
            std::fs::write(&state_file, state_json)
                .map_err(|e| format!("Failed to write state file: {}", e))?;

            // Write enhanced prompt content to file
            std::fs::write(&prompt_file, &enhanced_prompt)
                .map_err(|e| format!("Failed to write prompt file: {}", e))?;

            info!("MCP API: State file created: {:?}", state_file);
            info!("MCP API: Prompt file created: {:?}", prompt_file);

            // Spawn Claude independently using the spawn script
            // Use spawn_python_with_console to ensure Claude CLI gets a console window
            let spawn_result = spawn_python_with_console(
                "python",
                &[
                    spawn_script.as_os_str(),
                    std::ffi::OsStr::new("--file"),
                    prompt_file.as_os_str(),
                    std::ffi::OsStr::new("--session-id"),
                    std::ffi::OsStr::new(&session_id),
                ],
                &workspace_root,
            );

            match spawn_result {
                Ok(child) => {
                    info!(
                        "MCP API: AI Developer spawned with PID: {} for prompt '{}'",
                        child.id(),
                        prompt_name_for_state
                    );
                    Ok((
                        RunPromptResponse {
                            task_run_id: session_id.clone(),
                            action_id: session_id.clone(), // Backward compatibility
                            session_id,
                            state_file: state_file.to_string_lossy().to_string(),
                            log_file: log_file.to_string_lossy().to_string(),
                            pid: Some(child.id()),
                        },
                        log_file,
                        dev_logs_path,
                    ))
                }
                Err(e) => {
                    error!("MCP API: Failed to spawn AI Developer: {}", e);
                    Err(format!("Failed to spawn AI Developer: {}", e))
                }
            }
        })
        .await
        .map_err(|e| {
            error!("MCP API: spawn_blocking error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?;

        match result {
            Ok((response, _log_file, _dev_logs_path)) => {
                // NOTE: TaskMonitor was removed - task completion is now tracked by LoopController
                Ok(Json(ApiResponse::success(response)))
            }
            Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
        }
    }
}

/// Get all categories
async fn get_prompt_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = prompts::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all tags
async fn get_prompt_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = prompts::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Import prompts from JSON
async fn import_prompts(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ImportPromptsRequest>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::import_prompts(&request.prompts_json) {
        Ok(imported) => Ok(Json(ApiResponse::success(imported))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export all prompts as JSON
async fn export_prompts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::export_prompts() {
        Ok(json) => Ok(Json(ApiResponse::success(json))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Duplicate a prompt
async fn duplicate_prompt(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<DuplicatePromptRequest>,
) -> Result<Json<ApiResponse<prompts::SavedPrompt>>, (StatusCode, Json<ApiResponse<()>>)> {
    match prompts::duplicate_prompt(&id, request.new_name) {
        Ok(prompt) => Ok(Json(ApiResponse::success(prompt))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search prompts by query
async fn search_prompts(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<prompts::SavedPrompt>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = prompts::search_prompts(query);
    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// Macro Handlers
// ============================================================================

/// List all macros
async fn list_macros(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<macros::Macro>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let category = params.get("category").map(|s| s.as_str());
    let macro_list = macros::list_macros(category);
    Ok(Json(ApiResponse::success(macro_list)))
}

/// Get a single macro by ID
async fn get_macro(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<macros::Macro>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::get_macro(&id) {
        Some(macro_item) => Ok(Json(ApiResponse::success(macro_item))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Macro not found: {}", id))),
        )),
    }
}

/// Create a new macro
async fn create_macro(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateMacroRequest>,
) -> Result<Json<ApiResponse<macros::Macro>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::create_macro(
        request.name,
        request.description,
        request.steps,
        request.category,
        request.tags,
    ) {
        Ok(macro_item) => Ok(Json(ApiResponse::success(macro_item))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing macro
async fn update_macro(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateMacroRequest>,
) -> Result<Json<ApiResponse<macros::Macro>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::update_macro(
        &id,
        request.name,
        request.description,
        request.steps,
        request.category,
        request.tags,
    ) {
        Ok(macro_item) => Ok(Json(ApiResponse::success(macro_item))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a macro
async fn delete_macro(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match macros::delete_macro(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search macros by query
async fn search_macros(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<macros::Macro>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = macros::search_macros(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Get all macro categories
async fn get_macro_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = macros::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all macro tags
async fn get_macro_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = macros::get_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Run a macro by ID
async fn run_macro(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the macro
    let macro_item = match macros::get_macro(&id) {
        Some(m) => m,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Macro not found: {}", id))),
            ));
        }
    };

    // Increment run count
    if let Err(e) = macros::increment_run_count(&id) {
        tracing::warn!("Failed to increment run count for macro {}: {}", id, e);
    }

    let start_time = std::time::Instant::now();
    let mut step_results: Vec<serde_json::Value> = Vec::new();
    let mut successful_steps = 0;
    let mut failed_steps = 0;

    // Execute each step using the action service
    let action_service = state.action_service.clone();

    for (idx, step) in macro_item.steps.iter().enumerate() {
        let step_start = std::time::Instant::now();
        let mut step_success = false;
        let mut step_error: Option<String> = None;

        match step.action_type.as_str() {
            "click" | "double_click" | "right_click" => {
                if let Some(ref image_ids) = step.target_image_ids {
                    if let Some(first_image_id) = image_ids.first() {
                        // Execute click action
                        match action_service
                            .execute_action(
                                &step.action_type,
                                first_image_id,
                                None,
                                step.monitor_index,
                            )
                            .await
                        {
                            Ok(_result) => {
                                step_success = true;
                            }
                            Err(e) => {
                                step_error = Some(format!("{:?}", e));
                            }
                        }
                    } else {
                        step_error = Some("No target image specified".to_string());
                    }
                } else {
                    step_error = Some("No target image IDs specified".to_string());
                }
            }
            "type" => {
                if let Some(ref text) = step.text_input {
                    // Use the type action via action service
                    let config = serde_json::json!({
                        "text": text
                    });
                    match action_service
                        .execute_action("TYPE", "", Some(&config), step.monitor_index)
                        .await
                    {
                        Ok(_result) => {
                            step_success = true;
                        }
                        Err(e) => {
                            step_error = Some(format!("{:?}", e));
                        }
                    }
                } else {
                    step_error = Some("No text specified for type action".to_string());
                }
            }
            "hotkey" => {
                if let Some(ref hotkey) = step.hotkey {
                    // Use the hotkey action via action service
                    let config = serde_json::json!({
                        "hotkey": hotkey
                    });
                    match action_service
                        .execute_action("HOTKEY", "", Some(&config), step.monitor_index)
                        .await
                    {
                        Ok(_result) => {
                            step_success = true;
                        }
                        Err(e) => {
                            step_error = Some(format!("{:?}", e));
                        }
                    }
                } else {
                    step_error = Some("No hotkey specified".to_string());
                }
            }
            "go_to_state" => {
                if let Some(ref state_ids) = step.target_state_ids {
                    if let Some(first_state_id) = state_ids.first() {
                        // Use go_to_state via action service
                        // Timeouts are disabled by default
                        let timeout = step.timeout_seconds;
                        match action_service
                            .go_to_state(first_state_id, None, step.monitor_index, timeout)
                            .await
                        {
                            Ok(_result) => {
                                step_success = true;
                            }
                            Err(e) => {
                                step_error = Some(format!("{:?}", e));
                            }
                        }
                    } else {
                        step_error = Some("No target state specified".to_string());
                    }
                } else {
                    step_error = Some("No target state IDs specified".to_string());
                }
            }
            _ => {
                step_error = Some(format!("Unknown action type: {}", step.action_type));
            }
        }

        let step_duration = step_start.elapsed().as_millis() as u64;

        // Apply pause_after_ms if specified
        if let Some(pause_ms) = step.pause_after_ms {
            tokio::time::sleep(tokio::time::Duration::from_millis(pause_ms as u64)).await;
        }

        if step_success {
            successful_steps += 1;
        } else {
            failed_steps += 1;
        }

        step_results.push(serde_json::json!({
            "step_index": idx,
            "step_name": step.name,
            "action_type": step.action_type,
            "success": step_success,
            "error": step_error,
            "duration_ms": step_duration,
        }));
    }

    let total_duration = start_time.elapsed().as_millis() as u64;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "macro_id": id,
        "macro_name": macro_item.name,
        "total_steps": macro_item.steps.len(),
        "successful_steps": successful_steps,
        "failed_steps": failed_steps,
        "duration_ms": total_duration,
        "step_results": step_results,
    }))))
}

// ============================================================================
// Playwright Script Handlers
// ============================================================================

/// List all Playwright scripts
async fn list_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let scripts = playwright::get_all_scripts();
    Ok(Json(ApiResponse::success(scripts)))
}

/// Get a single Playwright script by ID
async fn get_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::get_script(&id) {
        Some(script) => Ok(Json(ApiResponse::success(script))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Playwright script not found: {}", id))),
        )),
    }
}

/// Create a new Playwright script
async fn create_playwright_script(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::create_script(
        request.name,
        request.description,
        request.ai_instructions,
        request.target_url,
        request.script_content,
        request.category,
        request.tags,
        request.timeout_seconds,
        request.display_mode,
        request.browser,
    ) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing Playwright script
async fn update_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::update_script(
        &id,
        request.name,
        request.description,
        request.ai_instructions,
        request.target_url,
        request.script_content,
        request.category,
        request.tags,
        request.timeout_seconds,
        request.display_mode,
        request.browser,
    ) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a Playwright script
async fn delete_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::delete_script(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Run a Playwright script
async fn run_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<RunPlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let target_url_override = request.target_url_override;

    // Run in spawn_blocking since it's a blocking operation
    let result =
        tokio::task::spawn_blocking(move || playwright::run_script(&id, target_url_override))
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Task error: {}", e))),
                )
            })?;

    match result {
        Ok(play_result) => Ok(Json(ApiResponse::success(play_result))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Get Playwright script categories
async fn get_playwright_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = playwright::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get Playwright script tags
async fn get_playwright_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = playwright::get_all_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Search Playwright scripts
async fn search_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = playwright::search_scripts(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Import Playwright scripts
async fn import_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ImportPlaywrightScriptsRequest>,
) -> Result<Json<ApiResponse<Vec<playwright::PlaywrightScript>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    match playwright::import_scripts(&request.scripts_json) {
        Ok(scripts) => Ok(Json(ApiResponse::success(scripts))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(api_error(e)))),
    }
}

/// Export all Playwright scripts
async fn export_playwright_scripts(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::export_scripts() {
        Ok(json) => Ok(Json(ApiResponse::success(json))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Duplicate a Playwright script
async fn duplicate_playwright_script(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<DuplicatePlaywrightScriptRequest>,
) -> Result<Json<ApiResponse<playwright::PlaywrightScript>>, (StatusCode, Json<ApiResponse<()>>)> {
    match playwright::duplicate_script(&id, request.new_name) {
        Ok(script) => Ok(Json(ApiResponse::success(script))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

// ============================================================================
// Scriptlet HTTP Endpoints
// ============================================================================

/// List all scriptlets
async fn list_scriptlets(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<scriptlets::Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let scriptlets = scriptlets::get_all_scriptlets();
    Ok(Json(ApiResponse::success(scriptlets)))
}

/// Get a single scriptlet by ID
async fn get_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::get_scriptlet(&id) {
        Some(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Scriptlet not found: {}", id))),
        )),
    }
}

/// Create a new scriptlet
async fn create_scriptlet(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateScriptletRequest>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::create_scriptlet(
        request.name,
        request.content,
        request.category,
        request.tags,
        request.source_log_ids,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing scriptlet
async fn update_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateScriptletRequest>,
) -> Result<Json<ApiResponse<scriptlets::Scriptlet>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::update_scriptlet(
        &id,
        request.name,
        request.content,
        request.category,
        request.tags,
    ) {
        Ok(scriptlet) => Ok(Json(ApiResponse::success(scriptlet))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a scriptlet
async fn delete_scriptlet(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scriptlets::delete_scriptlet(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Get all scriptlet categories
async fn get_scriptlet_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = scriptlets::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

// ============================================================================
// Inline Python Execution
// ============================================================================

/// Execute inline Python code
///
/// This handler allows executing arbitrary Python code with optional dependency
/// isolation via uvx. The code is wrapped to capture return values if the
/// script returns a JSON-serializable value.
async fn execute_inline_python(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<InlinePythonRequest>,
) -> Result<Json<ApiResponse<InlinePythonResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    use std::time::Instant;
    use tokio::process::Command;
    use tokio::time::timeout;

    let start = Instant::now();
    // Timeouts are disabled by default
    let timeout_secs = request.timeout_seconds;

    // Determine working directory
    let working_dir = request
        .working_directory
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    // Create a temporary script file
    let script_id = uuid::Uuid::new_v4();
    let script_path = std::env::temp_dir().join(format!("qontinui_inline_{}.py", script_id));

    // Wrap the code to capture return value
    // The user's code becomes the body of a __main__ function
    // If the function returns a value, it's printed with a special marker
    let indented_code = request
        .code
        .lines()
        .map(|line| format!("    {}", line))
        .collect::<Vec<_>>()
        .join("\n");

    let wrapped_code = format!(
        r#"import json
import sys

def __qontinui_main__():
{indented_code}

if __name__ == "__main__":
    try:
        result = __qontinui_main__()
        if result is not None:
            print("__QONTINUI_RETURN__:" + json.dumps(result))
    except Exception as e:
        print(f"Error: {{e}}", file=sys.stderr)
        sys.exit(1)
"#,
        indented_code = indented_code
    );

    // Write the script to the temp file
    if let Err(e) = std::fs::write(&script_path, &wrapped_code) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to write script: {}", e))),
        ));
    }

    // Build the command - use uvx if dependencies are specified
    // Helper to run with or without timeout
    async fn run_with_optional_timeout(
        mut cmd: tokio::process::Command,
        timeout_secs: Option<u64>,
    ) -> Result<Result<std::process::Output, std::io::Error>, tokio::time::error::Elapsed> {
        if let Some(secs) = timeout_secs {
            timeout(std::time::Duration::from_secs(secs), cmd.output()).await
        } else {
            // No timeout - wrap in Ok to match the return type
            Ok(cmd.output().await)
        }
    }

    let output_result = if let Some(deps) = &request.dependencies {
        if !deps.is_empty() {
            // Use uvx for dependency isolation
            let deps_str = deps.join(",");
            let mut cmd = Command::new("uvx");
            cmd.args(["--with", &deps_str, "python", script_path.to_str().unwrap()])
                .current_dir(&working_dir)
                .kill_on_drop(true);

            run_with_optional_timeout(cmd, timeout_secs).await
        } else {
            // No dependencies, use python directly
            let mut cmd = Command::new("python");
            cmd.arg(script_path.to_str().unwrap())
                .current_dir(&working_dir)
                .kill_on_drop(true);

            run_with_optional_timeout(cmd, timeout_secs).await
        }
    } else {
        // No dependencies, use python directly
        let mut cmd = Command::new("python");
        cmd.arg(script_path.to_str().unwrap())
            .current_dir(&working_dir)
            .kill_on_drop(true);

        run_with_optional_timeout(cmd, timeout_secs).await
    };

    // Cleanup the temp script
    let _ = std::fs::remove_file(&script_path);

    let duration_ms = start.elapsed().as_millis() as u64;

    match output_result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Parse return value from stdout if present
            let (stdout_clean, return_value) = if let Some(idx) =
                stdout.find("__QONTINUI_RETURN__:")
            {
                let (before, after) = stdout.split_at(idx);
                let json_str = after.trim_start_matches("__QONTINUI_RETURN__:");
                let parsed: Option<serde_json::Value> = serde_json::from_str(json_str.trim()).ok();
                (before.to_string(), parsed)
            } else {
                (stdout, None)
            };

            Ok(Json(ApiResponse::success(InlinePythonResponse {
                success: output.status.success(),
                stdout: stdout_clean,
                stderr,
                return_value,
                duration_ms,
            })))
        }
        Ok(Err(e)) => {
            // Command failed to execute
            Ok(Json(ApiResponse::success(InlinePythonResponse {
                success: false,
                stdout: String::new(),
                stderr: format!("Failed to execute: {}", e),
                return_value: None,
                duration_ms,
            })))
        }
        Err(_) => {
            // Timeout
            let timeout_msg = timeout_secs
                .map(|t| format!("Execution timed out after {} seconds", t))
                .unwrap_or_else(|| "Execution timed out".to_string());
            Ok(Json(ApiResponse::success(InlinePythonResponse {
                success: false,
                stdout: String::new(),
                stderr: timeout_msg,
                return_value: None,
                duration_ms,
            })))
        }
    }
}

// ============================================================================
/// Search scriptlets
async fn search_scriptlets(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<scriptlets::Scriptlet>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = scriptlets::search_scriptlets(query);
    Ok(Json(ApiResponse::success(results)))
}

// ============================================================================
// Backup and Restore HTTP Endpoints
// ============================================================================

/// Response for backup creation
#[derive(Debug, Serialize)]
struct BackupResponse {
    /// Base64-encoded ZIP file data
    data: String,
    /// Original filename suggestion
    filename: String,
    /// Backup result with details
    result: backup::BackupResult,
}

/// Request for restore operation
#[derive(Debug, Deserialize)]
struct RestoreRequest {
    /// Base64-encoded ZIP file data
    data: String,
}

/// Create a backup of all user data
///
/// Returns the backup as base64-encoded ZIP data along with metadata.
async fn create_backup_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<BackupResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Creating backup");

    match backup::create_backup() {
        Ok((zip_data, result)) => {
            // Encode ZIP data as base64
            let base64_data =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &zip_data);

            // Generate filename with timestamp
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let filename = format!("qontinui_backup_{}.zip", timestamp);

            info!(
                "MCP API: Backup created successfully ({} bytes, {} files)",
                zip_data.len(),
                result.files_backed_up.len()
            );

            Ok(Json(ApiResponse::success(BackupResponse {
                data: base64_data,
                filename,
                result,
            })))
        }
        Err(e) => {
            error!("MCP API: Backup failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get information about a backup without restoring it
async fn get_backup_info_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<ApiResponse<backup::BackupManifest>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Getting backup info");

    // Decode base64 data
    let zip_data =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &request.data) {
            Ok(data) => data,
            Err(e) => {
                error!("MCP API: Failed to decode backup data: {}", e);
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!("Invalid base64 data: {}", e))),
                ));
            }
        };

    match backup::get_backup_info(&zip_data) {
        Ok(manifest) => {
            info!(
                "MCP API: Backup info retrieved - version {}, {} files",
                manifest.version,
                manifest.files.len()
            );
            Ok(Json(ApiResponse::success(manifest)))
        }
        Err(e) => {
            error!("MCP API: Failed to get backup info: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Restore user data from a backup
///
/// Accepts base64-encoded ZIP data and restores all files to their original locations.
async fn restore_backup_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<RestoreRequest>,
) -> Result<Json<ApiResponse<backup::RestoreResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Restoring from backup");

    // Decode base64 data
    let zip_data =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &request.data) {
            Ok(data) => data,
            Err(e) => {
                error!("MCP API: Failed to decode backup data: {}", e);
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!("Invalid base64 data: {}", e))),
                ));
            }
        };

    match backup::restore_backup(&zip_data) {
        Ok(result) => {
            if result.success {
                info!(
                    "MCP API: Restore completed successfully - {} files restored",
                    result.files_restored.len()
                );
            } else {
                warn!(
                    "MCP API: Restore completed with errors: {:?}",
                    result.errors
                );
            }
            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            error!("MCP API: Restore failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

// ============================================================================
// Trace and Video Extraction Utilities
// ============================================================================

/// Extract action timeline and screenshots from a Playwright trace ZIP file
fn extract_trace_data(
    trace_path: &str,
    max_screenshots: u32,
) -> Result<(String, Vec<String>), String> {
    use std::io::Read;

    info!("Extracting trace data from: {}", trace_path);

    let file =
        std::fs::File::open(trace_path).map_err(|e| format!("Failed to open trace file: {}", e))?;

    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read trace ZIP: {}", e))?;

    let mut timeline = String::new();
    let mut screenshot_paths = Vec::new();

    // Create temp directory for extracted screenshots
    let temp_dir = std::env::temp_dir().join(format!("trace_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();

        // Extract action log for timeline
        if name.contains("actions") && name.ends_with(".json") {
            let mut contents = String::new();
            file.read_to_string(&mut contents).ok();
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                timeline = format_trace_timeline(&json);
            }
        }

        // Extract screenshots (limited by max_screenshots)
        if name.ends_with(".png") && screenshot_paths.len() < max_screenshots as usize {
            let out_path = temp_dir.join(format!("trace_screenshot_{}.png", i));
            if let Ok(mut out_file) = std::fs::File::create(&out_path) {
                if std::io::copy(&mut file, &mut out_file).is_ok() {
                    screenshot_paths.push(out_path.to_string_lossy().to_string());
                }
            }
        }
    }

    info!(
        "Extracted trace: {} chars timeline, {} screenshots",
        timeline.len(),
        screenshot_paths.len()
    );

    Ok((timeline, screenshot_paths))
}

/// Format trace events into a human-readable timeline
fn format_trace_timeline(json: &serde_json::Value) -> String {
    let mut timeline = String::from("## Action Timeline from Trace\n\n");

    if let Some(events) = json.as_array() {
        for (i, event) in events.iter().enumerate() {
            let action_type = event
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            let selector = event.get("selector").and_then(|s| s.as_str()).unwrap_or("");
            let value = event.get("value").and_then(|v| v.as_str()).unwrap_or("");

            if !selector.is_empty() {
                timeline.push_str(&format!(
                    "{}. {} on `{}` {}\n",
                    i + 1,
                    action_type,
                    selector,
                    if !value.is_empty() {
                        format!("with value '{}'", value)
                    } else {
                        String::new()
                    }
                ));
            } else {
                timeline.push_str(&format!("{}. {}\n", i + 1, action_type));
            }
        }
    }

    timeline
}

/// Extract key frames from a video file using ffmpeg
fn extract_video_frames(video_path: &str, max_frames: u32) -> Result<Vec<String>, String> {
    info!(
        "Extracting {} frames from video: {}",
        max_frames, video_path
    );

    // Create temp directory for frames
    let temp_dir = std::env::temp_dir().join(format!("video_frames_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let output_pattern = temp_dir
        .join("frame_%03d.png")
        .to_string_lossy()
        .to_string();

    // Use ffmpeg to extract frames evenly distributed throughout the video
    // -vf "select='not(mod(n,X))'" extracts every Xth frame
    // For 3 frames from a 30 fps, 10 sec video (300 frames), we'd extract every 100th frame
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y", // Overwrite output
            "-i",
            video_path,
            "-vf",
            &format!("select='lt(n\\,{})',setpts=N/FRAME_RATE/TB", max_frames),
            "-vsync",
            "vfr",
            "-frames:v",
            &max_frames.to_string(),
            &output_pattern,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            // Collect extracted frames
            let mut frames = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&temp_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "png") {
                        frames.push(entry.path().to_string_lossy().to_string());
                    }
                }
            }
            frames.sort(); // Ensure frames are in order
            info!("Extracted {} video frames", frames.len());
            Ok(frames)
        }
        Ok(_) => Err(
            "ffmpeg failed to extract frames. Ensure ffmpeg is installed and in PATH.".to_string(),
        ),
        Err(e) => Err(format!(
            "Failed to run ffmpeg: {}. Ensure ffmpeg is installed and in PATH.",
            e
        )),
    }
}

/// Collect all images for AI analysis (screenshots, trace screenshots, video frames)
fn collect_images_for_analysis(
    image_paths: &[String],
    video_paths: &[String],
    trace_path: Option<&str>,
    max_video_frames: u32,
    max_trace_screenshots: u32,
) -> (Vec<String>, Option<String>) {
    let mut all_images = image_paths.to_vec();
    let mut trace_timeline = None;

    // Extract trace data if provided
    if let Some(tp) = trace_path {
        match extract_trace_data(tp, max_trace_screenshots) {
            Ok((timeline, screenshots)) => {
                trace_timeline = Some(timeline);
                all_images.extend(screenshots);
            }
            Err(e) => {
                warn!("Failed to extract trace data: {}", e);
            }
        }
    }

    // Extract video frames if provided
    for video_path in video_paths {
        match extract_video_frames(video_path, max_video_frames) {
            Ok(frames) => {
                all_images.extend(frames);
            }
            Err(e) => {
                warn!("Failed to extract video frames: {}", e);
            }
        }
    }

    (all_images, trace_timeline)
}

// NOTE: execute_claude_cli, execute_windows_native, execute_via_wsl, execute_native,
// and execute_claude_api functions were removed. They implemented synchronous execution
// which has been replaced by the TaskRun model using spawn-independent-claude.py.

// NOTE: Unified Session API and start_session, stop_session, delete_session functions removed
// These are now handled by the LoopController

/// Result of parsing worker output for signals
#[derive(Debug, Clone)]
enum WorkerOutputSignal {
    /// Worker signals work is complete, ready for verification
    WorkComplete { reason: Option<String> },
    /// Worker requests replan
    NeedReplan { reason: String },
    /// Legacy task complete marker (deprecated but still supported)
    TaskComplete,
    /// No signal found, worker continues
    Continue,
}

/// Parse worker output for orchestrator signals
/// This is the primary signal detection used by the orchestrator architecture
fn parse_worker_output_signal(output: &str) -> WorkerOutputSignal {
    // First check for the new orchestrator signals
    if let Some(signal) = WorkerSignal::parse_from_output(output) {
        match signal {
            WorkerSignal::WorkComplete { reason } => {
                info!("Worker signal detected: [WORK_COMPLETE]");
                return WorkerOutputSignal::WorkComplete { reason };
            }
            WorkerSignal::NeedReplan { reason } => {
                info!("Worker signal detected: [NEED_REPLAN] - {}", reason);
                return WorkerOutputSignal::NeedReplan { reason };
            }
            WorkerSignal::Finding(_) => {
                // Findings don't terminate the loop, they're just recorded
            }
            WorkerSignal::Continue => {}
        }
    }

    // Check for legacy TASK_COMPLETE marker (deprecated but supported for backward compatibility)
    let output_upper = output.to_uppercase();
    let legacy_markers = [
        "[TASK_COMPLETE]",
        "[GOAL_COMPLETE]",
        "[GOAL_ACHIEVED]",
        "[STOP_SESSION]",
        "[SESSION_COMPLETE]",
    ];

    for marker in &legacy_markers {
        if output_upper.contains(marker) {
            info!(
                "Legacy completion marker detected: {} - treating as WORK_COMPLETE",
                marker
            );
            // Treat legacy markers as WORK_COMPLETE so verification still runs
            return WorkerOutputSignal::WorkComplete {
                reason: Some(format!("Legacy marker: {}", marker)),
            };
        }
    }

    // Check for structured completion patterns
    let completion_patterns = [
        r#""goal_achieved":\s*true"#,
        r#""goal_achieved": true"#,
        r#"goal_achieved:\s*true"#,
        r#""status":\s*"complete""#,
        r#"status:\s*"complete""#,
    ];

    for pattern in &completion_patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(output) {
                info!(
                    "Goal completion pattern detected: {} - treating as WORK_COMPLETE",
                    pattern
                );
                return WorkerOutputSignal::WorkComplete {
                    reason: Some(format!("Pattern match: {}", pattern)),
                };
            }
        }
    }

    WorkerOutputSignal::Continue
}

/// Check if AI output contains goal completion markers
/// Returns true if any marker indicates the goal has been achieved
/// NOTE: This is kept for backward compatibility but parse_worker_output_signal should be preferred
fn check_goal_completion_markers(output: &str) -> bool {
    matches!(
        parse_worker_output_signal(output),
        WorkerOutputSignal::WorkComplete { .. } | WorkerOutputSignal::TaskComplete
    )
}

/// Result of running deterministic verification
#[derive(Debug, Clone)]
struct DeterministicVerificationResult {
    /// Whether all CRITICAL checks passed (non-critical failures are informational)
    all_passed: bool,
    /// Summary of what was checked
    checks_run: Vec<String>,
    /// Details of CRITICAL failures (these block completion)
    critical_failures: Vec<String>,
    /// Details of non-critical failures (informational only)
    non_critical_failures: Vec<String>,
    /// Raw output from checks
    raw_output: String,
}

/// Run the workflow's actual verification steps (if defined) instead of just build checks
///
/// This function:
/// 1. Gets the task_run from database
/// 2. Extracts verification_steps from execution_steps_json
/// 3. If verification_steps exist, runs them through StepExecutor
/// 4. Otherwise falls back to basic deterministic verification
async fn run_workflow_verification_for_task(
    app_state: &std::sync::Arc<crate::AppState>,
    config_storage: &std::sync::Arc<tokio::sync::Mutex<ConfigStorage>>,
    db_task_id: &str,
    workspace_root: &str,
) -> DeterministicVerificationResult {
    use crate::step_executor::{ExecutionStepConfig, StepExecutor};

    // Try to get verification steps from the task's execution_steps_json
    let verification_steps: Vec<ExecutionStepConfig> =
        match app_state.checkpoint_db.get_task_run(db_task_id) {
            Ok(Some(task)) => task
                .execution_steps_json
                .as_ref()
                .and_then(|json| serde_json::from_str::<Vec<ExecutionStepConfig>>(json).ok())
                .map(|steps| {
                    steps
                        .into_iter()
                        .filter(|s| s.phase.as_deref() == Some("verification"))
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };

    // If no verification steps defined, fall back to basic deterministic verification
    if verification_steps.is_empty() {
        info!(
            "WORKFLOW-VERIFICATION: No verification_steps found for task {} - falling back to basic build checks",
            db_task_id
        );
        return run_deterministic_verification(workspace_root, None).await;
    }

    info!(
        "WORKFLOW-VERIFICATION: Running {} verification_steps for task {}",
        verification_steps.len(),
        db_task_id
    );

    // Create a StepExecutor to run the verification steps
    let executor = StepExecutor::new(app_state.clone(), config_storage.clone());

    // Run verification steps
    let verification_result = executor
        .execute_verification_steps(&verification_steps, db_task_id, 1)
        .await;

    // Log the result
    info!(
        "WORKFLOW-VERIFICATION: Result for task {}: all_passed={}, passed={}/{}, failed={}",
        db_task_id,
        verification_result.all_passed,
        verification_result.passed_steps,
        verification_result.total_steps,
        verification_result.failed_steps
    );

    // Convert to DeterministicVerificationResult format
    let mut checks_run = Vec::new();
    let mut critical_failures = Vec::new();
    let mut raw_output = String::new();

    for result in &verification_result.step_results {
        let check_name = format!("{} ({})", result.step_name, result.step_type);
        checks_run.push(check_name.clone());

        if !result.success {
            let failure_msg = if let Some(ref error) = result.error {
                format!("{}: {}", check_name, error)
            } else {
                format!("{}: failed", check_name)
            };
            critical_failures.push(failure_msg);

            // Add detailed output if available
            if let Some(ref details) = result.verification_details {
                if let Some(ref stdout) = details.stdout {
                    raw_output.push_str(&format!("=== {} ===\n{}\n\n", check_name, stdout));
                }
            }
        }
    }

    DeterministicVerificationResult {
        all_passed: verification_result.all_passed,
        checks_run,
        critical_failures,
        non_critical_failures: Vec::new(),
        raw_output,
    }
}

/// Run deterministic verification for a task
/// This runs build, tests, type checks, etc. before allowing task completion
///
/// IMPORTANT: Tests have an `is_critical` flag. Only critical test failures
/// block task completion. Non-critical failures are reported but don't fail verification.
async fn run_deterministic_verification(
    workspace_root: &str,
    _verification_config: Option<&serde_json::Value>,
) -> DeterministicVerificationResult {
    let _verifier = DeterministicVerifier::new(workspace_root.to_string());
    let mut checks_run = Vec::new();
    let mut critical_failures = Vec::new();
    let non_critical_failures: Vec<String> = Vec::new();
    let mut raw_output = String::new();

    // For Phase 1: Run basic build checks
    // Build checks are always CRITICAL - if the code doesn't compile, verification fails
    let workspace = std::path::Path::new(workspace_root);

    // Check for npm project
    if workspace.join("package.json").exists() {
        checks_run.push("npm build (critical)".to_string());
        info!("Running npm build verification in {}", workspace_root);

        let output = if cfg!(target_os = "windows") {
            std::process::Command::new("cmd")
                .args(["/C", "npm run build"])
                .current_dir(workspace_root)
                .output()
        } else {
            std::process::Command::new("sh")
                .args(["-c", "npm run build"])
                .current_dir(workspace_root)
                .output()
        };

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                raw_output.push_str(&format!(
                    "=== npm build (CRITICAL) ===\nExit: {}\nStdout:\n{}\nStderr:\n{}\n\n",
                    result.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                ));

                if !result.status.success() {
                    critical_failures.push(format!(
                        "npm build failed with exit code {}",
                        result.status.code().unwrap_or(-1)
                    ));
                    // Extract error lines
                    for line in stderr.lines().chain(stdout.lines()) {
                        let lower = line.to_lowercase();
                        if lower.contains("error") || lower.contains("failed") {
                            critical_failures.push(line.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                critical_failures.push(format!("Failed to run npm build: {}", e));
            }
        }
    }

    // Check for Cargo project
    if workspace.join("Cargo.toml").exists() {
        checks_run.push("cargo check (critical)".to_string());
        info!("Running cargo check verification in {}", workspace_root);

        let output = std::process::Command::new("cargo")
            .args(["check"])
            .current_dir(workspace_root)
            .output();

        match output {
            Ok(result) => {
                let stdout = String::from_utf8_lossy(&result.stdout);
                let stderr = String::from_utf8_lossy(&result.stderr);
                raw_output.push_str(&format!(
                    "=== cargo check (CRITICAL) ===\nExit: {}\nStdout:\n{}\nStderr:\n{}\n\n",
                    result.status.code().unwrap_or(-1),
                    stdout,
                    stderr
                ));

                if !result.status.success() {
                    critical_failures.push(format!(
                        "cargo check failed with exit code {}",
                        result.status.code().unwrap_or(-1)
                    ));
                    // Extract error lines
                    for line in stderr.lines() {
                        if line.contains("error[E") || line.starts_with("error:") {
                            critical_failures.push(line.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                critical_failures.push(format!("Failed to run cargo check: {}", e));
            }
        }
    }

    // If no build system found, verification passes by default
    if checks_run.is_empty() {
        checks_run.push("(no build system detected)".to_string());
        raw_output.push_str("No package.json or Cargo.toml found. Skipping build verification.\n");
    }

    // TODO Phase 2: Run verification_tests from database using execute_tests_for_trigger
    // This will:
    // 1. Get tests associated with the config/task
    // 2. Execute each test
    // 3. Track is_critical flag - only critical failures block completion
    // 4. Non-critical failures are reported but don't fail verification
    //
    // Example integration:
    // let test_results = execute_tests_for_trigger(db, config_id, &TriggerPoint::AfterWorkflow, Some(task_run_id));
    // if test_results.critical_failure {
    //     critical_failures.extend(test_results.results.iter()
    //         .filter(|r| !matches!(r.status, TestStatus::Passed) && /* is_critical */)
    //         .map(|r| format!("Test '{}' failed: {}", r.name, r.error.as_deref().unwrap_or("Unknown"))));
    // }

    DeterministicVerificationResult {
        // Only CRITICAL failures block completion
        all_passed: critical_failures.is_empty(),
        checks_run,
        critical_failures,
        non_critical_failures,
        raw_output,
    }
}

/// Generate feedback for failed verification to include in next iteration prompt
fn generate_verification_feedback(result: &DeterministicVerificationResult) -> String {
    let mut feedback = String::new();

    if !result.all_passed {
        feedback.push_str("## ⚠️ Deterministic Verification Failed\n\n");
        feedback.push_str(
            "The system ran verification after your [WORK_COMPLETE] signal and found issues:\n\n",
        );

        feedback.push_str("### Checks Run\n");
        for check in &result.checks_run {
            feedback.push_str(&format!("- {}\n", check));
        }

        if !result.critical_failures.is_empty() {
            feedback.push_str("\n### ❌ Critical Failures (blocking)\n");
            feedback.push_str("These MUST be fixed before the task can complete:\n");
            for failure in &result.critical_failures {
                feedback.push_str(&format!("- {}\n", failure));
            }
        }

        if !result.non_critical_failures.is_empty() {
            feedback.push_str("\n### ⚠️ Non-Critical Failures (informational)\n");
            feedback.push_str("These don't block completion but should be reviewed:\n");
            for failure in &result.non_critical_failures {
                feedback.push_str(&format!("- {}\n", failure));
            }
        }

        feedback.push_str("\n### Action Required\n");
        feedback.push_str(
            "Please fix the CRITICAL issues above and signal [WORK_COMPLETE] again when ready.\n",
        );
        feedback.push_str("The task will NOT be marked complete until all critical checks pass.\n");
    }

    feedback
}

/// Information about a single active session
#[derive(Debug, Clone, Serialize)]
struct ActiveSessionInfo {
    /// Unique session ID
    id: String,
    /// Display name
    name: String,
    /// Current status (running, waiting_for_continuation, etc.)
    status: String,
    /// When the session started
    started_at: String,
    /// Whether this session uses GUI automation (blocks other GUI sessions)
    uses_gui: bool,
}

// NOTE: ResumableWorkflowInfo, get_resumable_workflow, ResumeWorkflowResponse, resume_workflow,
// ForceContinueRequest, ForceContinueResponse, force_continue_session, force_continue_simple
// functions removed - these are now handled by the LoopController

/// Response for auto-continue setting
#[derive(Debug, Serialize)]
struct AutoContinueSettingResponse {
    enabled: bool,
}

/// Get the auto-continue AI workflow setting
async fn get_auto_continue_setting() -> Json<ApiResponse<AutoContinueSettingResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();
    Json(ApiResponse::success(AutoContinueSettingResponse {
        enabled,
    }))
}

/// Request body for setting auto-continue
#[derive(Debug, Deserialize)]
struct SetAutoContinueRequest {
    enabled: bool,
}

/// Set the auto-continue AI workflow setting
async fn set_auto_continue_setting(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<AutoContinueSettingResponse>> {
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!(
                "Auto-continue AI workflow setting updated to: {}",
                body.enabled
            );
            Json(ApiResponse::success(AutoContinueSettingResponse {
                enabled: body.enabled,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to save setting: {}", e)),
        }),
    }
}

/// Response for per-workflow auto-continue setting
#[derive(Debug, Serialize)]
struct WorkflowAutoContinueResponse {
    enabled: bool,
    workflow_name: Option<String>,
}

/// Get the auto-continue setting for the active workflow
/// Now uses global setting and checks for running tasks in database
async fn get_workflow_auto_continue() -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    let enabled = settings::get_auto_continue_ai_workflow();

    // Check if there are any running tasks
    let workflow_name = if let Ok(db) = CheckpointDb::new() {
        db.get_running_task_runs()
            .ok()
            .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
    } else {
        None
    };

    Json(ApiResponse::success(WorkflowAutoContinueResponse {
        enabled,
        workflow_name,
    }))
}

/// Set the auto-continue setting for the active workflow
/// Now just updates the global setting
async fn set_workflow_auto_continue(
    Json(body): Json<SetAutoContinueRequest>,
) -> Json<ApiResponse<WorkflowAutoContinueResponse>> {
    // Update the global setting
    match settings::save_auto_continue_ai_workflow(body.enabled) {
        Ok(_) => {
            info!("Auto-continue setting updated to: {}", body.enabled);

            // Get the active workflow name if any
            let workflow_name = if let Ok(db) = CheckpointDb::new() {
                db.get_running_task_runs()
                    .ok()
                    .and_then(|tasks| tasks.first().map(|t| t.task_name.clone()))
            } else {
                None
            };

            Json(ApiResponse::success(WorkflowAutoContinueResponse {
                enabled: body.enabled,
                workflow_name,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to update auto-continue setting: {}", e)),
        }),
    }
}

/// Check if the supervisor is available on port 9875.
/// Used to determine what restart instructions to give AI sessions.
fn check_supervisor_available() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;

    // Try to connect to supervisor health endpoint
    TcpStream::connect_timeout(
        &"127.0.0.1:9875".parse().unwrap(),
        Duration::from_millis(500),
    )
    .is_ok()
}

// NOTE: resume_all_running_tasks_on_startup function removed - now handled by LoopController

// ============================================================================
// Checkpoint HTTP API Handlers
// ============================================================================

use crate::database::{CheckpointData, CheckpointDb, CreateTaskRunInput, SessionEvent, TaskRun};

/// List all active (non-completed) checkpoints.
async fn list_checkpoints(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<CheckpointData>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .list_active_checkpoints()
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get a checkpoint by workflow name.
async fn get_checkpoint(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<Option<CheckpointData>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_checkpoint(&name)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for saving a checkpoint.
#[derive(Debug, Deserialize)]
struct SaveCheckpointRequest {
    workflow_name: String,
    current_phase: u32,
    #[serde(default)]
    total_phases: Option<u32>,
    #[serde(default)]
    completed: bool,
    #[serde(default)]
    restart_permitted: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    repos_to_process: Option<Vec<String>>,
    #[serde(default)]
    work_completed: Option<serde_json::Value>,
    #[serde(default)]
    items_needing_user_input: Option<Vec<String>>,
    #[serde(default)]
    error_message: Option<String>,
}

/// Save or update a checkpoint.
async fn save_checkpoint(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SaveCheckpointRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let data = CheckpointData {
        session_id: None,
        workflow_name: Some(req.workflow_name),
        current_phase: req.current_phase,
        total_phases: req.total_phases,
        completed: req.completed,
        restart_permitted: req.restart_permitted,
        status: req.status,
        run_id: req.run_id,
        repos_to_process: req.repos_to_process,
        work_completed: req.work_completed,
        items_needing_user_input: req.items_needing_user_input,
        created_at: None,
        updated_at: None,
        error_message: req.error_message,
        extra: None,
    };

    state
        .app_state
        .checkpoint_db
        .save_checkpoint(&data)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "message": "Checkpoint saved"
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Delete a checkpoint by workflow name.
async fn delete_checkpoint(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .delete_checkpoint(&name)
        .map(|deleted| {
            Json(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Checkpoint deleted" } else { "Checkpoint not found" }
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Query params for checkpoint status.
#[derive(Debug, Deserialize)]
struct CheckpointStatusQuery {
    completion_value: Option<u32>,
}

/// Check checkpoint status for cross-session continuation.
async fn get_checkpoint_status(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<CheckpointStatusQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let completion_value = query.completion_value.unwrap_or(12); // Default for improve-all

    state
        .app_state
        .checkpoint_db
        .check_checkpoint_status(&name, completion_value)
        .map(|result| {
            Json(match result {
                Some((is_complete, current_phase)) => serde_json::json!({
                    "found": true,
                    "is_complete": is_complete,
                    "current_phase": current_phase
                }),
                None => serde_json::json!({
                    "found": false,
                    "is_complete": false,
                    "current_phase": 0
                }),
            })
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Query params for checkpoint history.
#[derive(Debug, Deserialize)]
struct CheckpointHistoryQuery {
    workflow_name: Option<String>,
    limit: Option<u32>,
}

/// Get checkpoint/session history.
async fn get_checkpoint_history(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<CheckpointHistoryQuery>,
) -> Result<Json<Vec<SessionEvent>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);

    state
        .app_state
        .checkpoint_db
        .get_session_history(query.workflow_name.as_deref(), limit)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

// ============================================================================
// End Checkpoint HTTP API Handlers
// ============================================================================

// ============================================================================
// Task Run HTTP API Handlers
// ============================================================================

/// Query params for listing task runs.
#[derive(Debug, Deserialize)]
struct ListTaskRunsQuery {
    /// Maximum number of task runs to return (default: 50)
    limit: Option<u32>,
}

/// List recent task runs.
/// Uses spawn_blocking to avoid blocking the async runtime on database operations.
async fn list_task_runs(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ListTaskRunsQuery>,
) -> Result<Json<Vec<TaskRun>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(50);
    let db = state.app_state.checkpoint_db.clone();

    tokio::task::spawn_blocking(move || db.get_recent_task_runs(limit))
        .await
        .map_err(|e| {
            error!("spawn_blocking error in list_task_runs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// List only running task runs.
/// Uses spawn_blocking to avoid blocking the async runtime on database operations.
async fn list_running_task_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<Vec<TaskRun>>, (StatusCode, String)> {
    let db = state.app_state.checkpoint_db.clone();

    tokio::task::spawn_blocking(move || db.get_running_task_runs())
        .await
        .map_err(|e| {
            error!("spawn_blocking error in list_running_task_runs: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for creating a task run.
#[derive(Debug, Deserialize)]
struct CreateTaskRunRequest {
    /// Name/identifier for this task
    task_name: String,
    /// The prompt to run (optional for pure automation tasks)
    #[serde(default)]
    prompt: Option<String>,
    /// Task type: 'task', 'automation', or 'scheduled' (defaults to 'task')
    #[serde(default)]
    task_type: Option<String>,
    /// Config ID for automation-enabled tasks
    #[serde(default)]
    config_id: Option<String>,
    /// Workflow name being executed
    #[serde(default)]
    workflow_name: Option<String>,
    /// Maximum number of sessions before giving up (optional)
    #[serde(default)]
    max_sessions: Option<u32>,
    /// Per-run auto-continue setting (defaults to true if not specified)
    #[serde(default)]
    auto_continue: Option<bool>,
    /// JSON-encoded execution steps (optional)
    #[serde(default)]
    execution_steps_json: Option<String>,
    /// JSON-encoded log sources (optional)
    #[serde(default)]
    log_sources_json: Option<String>,
}

/// Create a new task run.
async fn create_task_run(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateTaskRunRequest>,
) -> Result<Json<TaskRun>, (StatusCode, String)> {
    let id = uuid::Uuid::new_v4().to_string();
    let task_type = req.task_type.as_deref().unwrap_or("task");

    let mut input = CreateTaskRunInput::new(&id, &req.task_name).with_task_type(task_type);
    if let Some(ref p) = req.prompt {
        input = input.with_prompt(p);
    }
    if let Some(ref cid) = req.config_id {
        input = input.with_config_id(cid);
    }
    if let Some(ref wn) = req.workflow_name {
        input = input.with_workflow_name(wn);
    }
    if let Some(ms) = req.max_sessions {
        input = input.with_max_sessions(ms);
    }
    if let Some(ac) = req.auto_continue {
        input = input.with_auto_continue(ac);
    }
    if let Some(ref esj) = req.execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Some(ref lsj) = req.log_sources_json {
        input = input.with_log_sources_json(lsj);
    }

    state
        .app_state
        .checkpoint_db
        .create_task_run(&input)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Get a task run by ID.
async fn get_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Option<TaskRun>>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Response for workflow state endpoint.
#[derive(Debug, Serialize)]
struct WorkflowStateResponse {
    task_run_id: String,
    workflow_type: String,
    current_state: String,
    phase: Option<String>,
    iteration: Option<u32>,
    max_iterations: Option<u32>,
    is_complete: bool,
    is_stopped: bool,
    is_paused: bool,
    has_verification_plan: bool,
    workflow_start_time: String,
    state_data: Option<serde_json::Value>,
    /// Workflow stage for UI display (setup, verification, agentic, completion)
    workflow_stage: Option<String>,
    /// Human-readable display name for the workflow stage
    workflow_stage_display: Option<String>,
}

/// Map a workflow state name to a stage for UI display.
fn state_name_to_stage(state_name: &str) -> (Option<String>, Option<String>) {
    // State names are like: setup_running, setup_complete, verification_running, etc.
    let (stage, display) = if state_name.starts_with("setup") {
        ("setup", "Setup")
    } else if state_name.starts_with("verification") {
        ("verification", "Verification")
    } else if state_name.starts_with("agentic") {
        ("agentic", "Agentic")
    } else if state_name.starts_with("completion") {
        ("completion", "Completion")
    } else if state_name == "running" {
        // Legacy running state - no explicit stage
        return (None, None);
    } else {
        return (None, None);
    };
    (Some(stage.to_string()), Some(display.to_string()))
}

/// Get workflow state for a task run.
///
/// This endpoint provides explicit workflow state tracking that replaces
/// implicit state inference from task_runs status fields.
///
/// Works for all workflow types: unified, orchestrator, gui_automation.
async fn get_workflow_state(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<WorkflowStateResponse>, (StatusCode, String)> {
    // First get the task run for metadata
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Try to get explicit workflow state
    let explicit_state = state
        .app_state
        .checkpoint_db
        .get_workflow_execution_state(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Try to get max_iterations from the workflow definition
    let max_iterations = if let Some(ref workflow_name) = task_run.workflow_name {
        state
            .app_state
            .checkpoint_db
            .get_unified_workflow_by_name(workflow_name)
            .ok()
            .flatten()
            .map(|w| w.max_iterations)
    } else {
        None
    };

    if let Some(ws) = explicit_state {
        // Return explicit state
        let state_data: Option<serde_json::Value> = ws
            .state_data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let is_terminal = matches!(
            ws.state_name.as_str(),
            "completion_complete" | "failed" | "stopped"
        );
        let is_stopped = ws.state_name == "stopped";

        // Map state name to stage for UI
        let (workflow_stage, workflow_stage_display) = state_name_to_stage(&ws.state_name);

        Ok(Json(WorkflowStateResponse {
            task_run_id: id,
            workflow_type: ws.workflow_type,
            current_state: ws.state_name,
            phase: ws.phase,
            iteration: ws.iteration,
            max_iterations,
            is_complete: is_terminal,
            is_stopped,
            is_paused: false,             // TODO: Track paused state
            has_verification_plan: false, // TODO: Check if verification plan exists
            workflow_start_time: task_run.created_at,
            state_data,
            workflow_stage,
            workflow_stage_display,
        }))
    } else {
        // Fallback: Infer state from task_run for backward compatibility
        let workflow_type = task_run
            .workflow_type
            .clone()
            .unwrap_or_else(|| "legacy_session".to_string());

        let (current_state, is_complete, is_stopped) = match task_run.status.as_str() {
            "running" => ("running".to_string(), false, false),
            "complete" => ("complete".to_string(), true, false),
            "failed" => ("failed".to_string(), true, false),
            "stopped" => ("stopped".to_string(), true, true),
            other => (other.to_string(), false, false),
        };

        Ok(Json(WorkflowStateResponse {
            task_run_id: id,
            workflow_type,
            current_state,
            phase: None,
            iteration: None,
            max_iterations,
            is_complete,
            is_stopped,
            is_paused: false,
            has_verification_plan: false,
            workflow_start_time: task_run.created_at,
            state_data: None,
            workflow_stage: None,
            workflow_stage_display: None,
        }))
    }
}

// =============================================================================
// Full Workflow State (for frontend restart recovery)
// =============================================================================

/// Full workflow state response for restart recovery.
///
/// This endpoint returns authoritative state from the database, including:
/// - Orchestrator state (phase, iteration, state name)
/// - All step checkpoints (which steps completed)
/// - Progress markers for the current step (intra-step progress)
/// - Resume point (where execution will continue)
#[derive(Debug, Serialize)]
struct FullWorkflowStateResponse {
    /// Task run metadata
    task_run: TaskRunSummary,
    /// Orchestrator state from workflow_execution_state table
    orchestrator_state: Option<OrchestratorStateData>,
    /// All step checkpoints for this execution
    checkpoints: Vec<StepCheckpointData>,
    /// Progress marker for the currently running step (if any)
    current_step_progress: Option<StepProgressData>,
    /// Computed resume point
    resume_point: ResumePointData,
}

/// Task run summary for full state response.
#[derive(Debug, Serialize)]
struct TaskRunSummary {
    id: String,
    status: String,
    task_name: String,
    workflow_type: Option<String>,
    workflow_name: Option<String>,
    started_at: String,
    sessions_count: u32,
}

/// Orchestrator state data from workflow_execution_state table.
#[derive(Debug, Serialize)]
struct OrchestratorStateData {
    state_name: String,
    state_data: Option<serde_json::Value>,
    phase: Option<String>,
    iteration: Option<u32>,
    updated_at: String,
    /// Mapped workflow stage for UI display
    workflow_stage: Option<String>,
    workflow_stage_display: Option<String>,
}

/// Step checkpoint data for full state response.
#[derive(Debug, Serialize)]
struct StepCheckpointData {
    id: String,
    execution_id: String,
    phase: String,
    iteration: Option<u32>,
    step_index: usize,
    step_type: String,
    step_name: Option<String>,
    status: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

/// Step progress marker data for full state response.
#[derive(Debug, Serialize)]
struct StepProgressData {
    checkpoint_id: String,
    marker_type: String,
    current_value: u64,
    total_value: Option<u64>,
    description: Option<String>,
    data_json: Option<serde_json::Value>,
    created_at: String,
}

/// Resume point data for full state response.
#[derive(Debug, Serialize)]
struct ResumePointData {
    /// Type of resume point
    #[serde(rename = "type")]
    resume_type: String,
    /// Iteration number (for verification/agentic phases)
    iteration: Option<u32>,
    /// Step index to resume from
    from_step: Option<usize>,
    /// Human-readable description
    description: String,
}

/// Get full workflow state for restart recovery.
///
/// This endpoint returns authoritative state from the database for the frontend
/// to use when recovering from a restart. It consolidates data from:
/// - task_runs table
/// - workflow_execution_state table
/// - workflow_step_checkpoints table
/// - step_progress_markers table
async fn get_full_workflow_state(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<FullWorkflowStateResponse>, (StatusCode, String)> {
    // Get task run
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get orchestrator state
    let explicit_state = state
        .app_state
        .checkpoint_db
        .get_workflow_execution_state(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let orchestrator_state = explicit_state.map(|ws| {
        let state_data: Option<serde_json::Value> = ws
            .state_data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());
        let (workflow_stage, workflow_stage_display) = state_name_to_stage(&ws.state_name);

        OrchestratorStateData {
            state_name: ws.state_name,
            state_data,
            phase: ws.phase,
            iteration: ws.iteration,
            updated_at: ws.updated_at,
            workflow_stage,
            workflow_stage_display,
        }
    });

    // Get all step checkpoints
    let checkpoints_raw = state
        .app_state
        .checkpoint_db
        .get_all_workflow_step_checkpoints(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let checkpoints: Vec<StepCheckpointData> = checkpoints_raw
        .iter()
        .map(|cp| StepCheckpointData {
            id: cp.id.clone(),
            execution_id: cp.execution_id.clone(),
            phase: cp.phase.clone(),
            iteration: cp.iteration,
            step_index: cp.step_index,
            step_type: cp.step_type.clone(),
            step_name: cp.step_name.clone(),
            status: cp.status.to_string(),
            started_at: cp.started_at.clone(),
            completed_at: cp.completed_at.clone(),
            duration_ms: cp.duration_ms,
            error: cp.error.clone(),
        })
        .collect();

    // Get current step progress
    let progress_raw = state
        .app_state
        .checkpoint_db
        .get_current_step_progress(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let current_step_progress = progress_raw.map(|p| StepProgressData {
        checkpoint_id: p.checkpoint_id,
        marker_type: p.marker_type,
        current_value: p.current_value,
        total_value: p.total_value,
        description: p.description,
        data_json: p
            .data_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        created_at: p.created_at,
    });

    // Compute resume point using ResumeManager
    let resume_point = compute_resume_point(
        state.app_state.checkpoint_db.clone(),
        &id,
        &orchestrator_state,
    );

    Ok(Json(FullWorkflowStateResponse {
        task_run: TaskRunSummary {
            id: task_run.id,
            status: task_run.status,
            task_name: task_run.task_name,
            workflow_type: task_run.workflow_type,
            workflow_name: task_run.workflow_name,
            started_at: task_run.created_at,
            sessions_count: task_run.sessions_count,
        },
        orchestrator_state,
        checkpoints,
        current_step_progress,
        resume_point,
    }))
}

/// Compute the resume point for a workflow execution.
fn compute_resume_point(
    db: std::sync::Arc<crate::database::CheckpointDb>,
    execution_id: &str,
    orchestrator_state: &Option<OrchestratorStateData>,
) -> ResumePointData {
    // Try to use the ResumeManager for accurate resume point calculation
    use crate::unified_workflow_executor::ResumeManager;

    let resume_mgr = ResumeManager::new(db);
    match resume_mgr.determine_resume_point(execution_id) {
        Ok(resume_point) => {
            use crate::unified_workflow_executor::ResumePoint;
            match resume_point {
                ResumePoint::FromStart => ResumePointData {
                    resume_type: "from_start".to_string(),
                    iteration: None,
                    from_step: None,
                    description: resume_point.description(),
                },
                ResumePoint::SetupPhase { from_step } => ResumePointData {
                    resume_type: "setup_phase".to_string(),
                    iteration: None,
                    from_step: Some(from_step),
                    description: resume_point.description(),
                },
                ResumePoint::VerificationPhase {
                    iteration,
                    from_step,
                } => ResumePointData {
                    resume_type: "verification_phase".to_string(),
                    iteration: Some(iteration),
                    from_step: Some(from_step),
                    description: resume_point.description(),
                },
                ResumePoint::AgenticPhase { iteration } => ResumePointData {
                    resume_type: "agentic_phase".to_string(),
                    iteration: Some(iteration),
                    from_step: None,
                    description: resume_point.description(),
                },
                ResumePoint::CompletionPhase { from_step } => ResumePointData {
                    resume_type: "completion_phase".to_string(),
                    iteration: None,
                    from_step: Some(from_step),
                    description: resume_point.description(),
                },
            }
        }
        Err(e) => {
            // Fall back to basic inference from orchestrator state
            warn!("Failed to determine resume point: {}, using fallback", e);
            if let Some(orch) = orchestrator_state {
                ResumePointData {
                    resume_type: orch.phase.clone().unwrap_or_else(|| "unknown".to_string()),
                    iteration: orch.iteration,
                    from_step: Some(0),
                    description: format!("Resume from {} (fallback)", orch.state_name),
                }
            } else {
                ResumePointData {
                    resume_type: "from_start".to_string(),
                    iteration: None,
                    from_step: None,
                    description: "No state found, start from beginning".to_string(),
                }
            }
        }
    }
}

/// Query params for getting task output.
#[derive(Debug, Deserialize)]
struct TaskOutputQuery {
    /// Number of characters from end of output to return (optional)
    tail_chars: Option<usize>,
}

/// Get task output (optionally just the tail).
async fn get_task_output(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<TaskOutputQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // First verify task exists
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let output = if let Some(tail_chars) = query.tail_chars {
        state
            .app_state
            .checkpoint_db
            .get_task_output_tail(&id, tail_chars)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        task_run.output_log
    };

    Ok(Json(serde_json::json!({
        "id": id,
        "output": output,
        "status": task_run.status,
        "sessions_count": task_run.sessions_count
    })))
}

/// Stop a running task run.
async fn stop_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("Stopping task run: {}", id);

    // Verify task exists first
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    if task_run.status != "running" {
        return Ok(Json(serde_json::json!({
            "success": false,
            "message": format!("Task is not running (status: {})", task_run.status)
        })));
    }

    // Kill all tracked AI processes (same logic as stop_ai_analysis)
    // This ensures the actual Claude CLI process is terminated, not just marked as stopped
    let pids_to_kill: Vec<u32> = {
        let mut pids = safe_lock_or_recover(&state.current_ai_pids, "current_ai_pids");
        let pids_copy = pids.clone();
        pids.clear();
        pids_copy
    };

    let mut killed_count = 0;
    for pid in &pids_to_kill {
        info!("Killing AI process PID {} for task {}", pid, id);
        let result = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    info!("Successfully killed process tree for PID {}", pid);
                    killed_count += 1;
                } else {
                    // Process may have already exited
                    killed_count += 1;
                }
            }
            Err(e) => {
                error!("Failed to execute taskkill for PID {}: {}", pid, e);
            }
        }
    }

    // Mark as stopped in database
    state
        .app_state
        .checkpoint_db
        .stop_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Emit status to frontend
    emit_ai_output(
        &state.app_handle,
        &format!(
            "🛑 Task {} stopped (killed {} process(es))",
            id, killed_count
        ),
        "status",
        None,
        None,
    );

    info!("Task {} stopped, killed {} process(es)", id, killed_count);

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Task run stopped, killed {} process(es)", killed_count)
    })))
}

/// Delete a task run.
async fn delete_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .delete_task_run(&id)
        .map(|deleted| {
            Json(serde_json::json!({
                "success": deleted,
                "message": if deleted { "Task run deleted" } else { "Task run not found" }
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Generate an AI summary for a completed task run.
/// The summary includes:
/// - A paragraph summary of what was accomplished
/// - Whether the stated goal was achieved
/// - What remaining work exists (if goal not achieved)
async fn generate_task_summary(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    info!("MCP API: Generating summary for task run: {}", id);

    // Run summary generation in a blocking task
    let db = state.app_state.checkpoint_db.clone();
    let task_id = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        summary_generator::generate_task_summary(&db, &task_id)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Task spawn error: {}", e),
        )
    })?;

    match result {
        Ok(summary_result) => Ok(Json(serde_json::json!({
            "success": true,
            "summary": summary_result.summary,
            "goal_achieved": summary_result.goal_achieved,
            "remaining_work": summary_result.remaining_work,
        }))),
        Err(e) => {
            warn!("Failed to generate summary for task {}: {}", id, e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, e))
        }
    }
}

/// Get the auto-continue setting for a specific task run.
async fn get_task_auto_continue(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .get_task_auto_continue(&id)
        .map(|auto_continue| {
            Json(serde_json::json!({
                "id": id,
                "auto_continue": auto_continue
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for setting auto-continue on a task run.
#[derive(Debug, Deserialize)]
struct SetTaskAutoContinueRequest {
    auto_continue: bool,
}

/// Set the auto-continue setting for a specific task run.
async fn set_task_auto_continue(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(req): Json<SetTaskAutoContinueRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    state
        .app_state
        .checkpoint_db
        .set_task_auto_continue(&id, req.auto_continue)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "id": id,
                "auto_continue": req.auto_continue
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Request body for resuming a task run.
#[derive(Debug, Deserialize)]
struct ResumeTaskRunRequest {
    /// Additional sessions to add (if not already reopened). Default: 1
    additional_sessions: Option<u32>,
}

/// Resume a task run that was previously completed or interrupted.
///
/// This endpoint combines reopening the task in the database with actually
/// triggering the workflow execution. It handles:
/// 1. Reopening the task if it's not already running
/// 2. Extracting the workflow ID from the task ID
/// 3. Starting LoopController to execute the workflow
async fn resume_task_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ResumeTaskRunRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use crate::unified_workflow_executor::{
        convert_json_steps_with_phase, extract_prompt_steps_with_phase,
        extract_workflow_id_from_task_id, LoopConfig, LoopController,
    };

    info!("Resume task run request: {}", id);

    // Get the task run
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // If not already running, reopen it
    let task_run = if task_run.status != "running" {
        let additional_sessions = request.additional_sessions.unwrap_or(1);
        state
            .app_state
            .checkpoint_db
            .reopen_task_run(&id, additional_sessions)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        task_run
    };

    // Try to get the workflow definition - first by extracting ID from task ID, then by name
    let (workflow_id, workflow) = if let Some(wf_id) = extract_workflow_id_from_task_id(&id) {
        // Old format: unified-workflow-{uuid}-{timestamp}
        let wf = state
            .app_state
            .checkpoint_db
            .get_unified_workflow(&wf_id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("Workflow definition not found: {}", wf_id),
                )
            })?;
        (wf_id, wf)
    } else if let Some(ref wf_name) = task_run.workflow_name {
        // New format: workflow-sequence-{timestamp}-workflow-{n}
        // Look up workflow by name instead
        let wf = state
            .app_state
            .checkpoint_db
            .get_unified_workflow_by_name(wf_name)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("Workflow definition not found with name: {}", wf_name),
                )
            })?;
        (wf.id.clone(), wf)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Cannot determine workflow for task ID '{}'. Task has no workflow_name and ID format is not recognized.",
                id
            ),
        ));
    };

    info!(
        "Resuming workflow '{}' (task_id: {}, workflow_id: {}, iteration: {})",
        task_run.task_name,
        id,
        workflow_id,
        task_run.sessions_count + 1
    );

    // Convert steps for LoopController with explicit phase assignment
    let setup_automation_steps =
        convert_json_steps_with_phase(&workflow.setup_steps, 0, Some("setup"));
    // Prepend pre-flight check if enabled (default: true)
    let setup_automation_steps = crate::unified_workflows::prepend_preflight_check_step(
        setup_automation_steps,
        workflow.preflight_check_enabled,
    );
    let setup_prompt_steps = extract_prompt_steps_with_phase(&workflow.setup_steps, Some("setup"));
    let verification_steps =
        convert_json_steps_with_phase(&workflow.verification_steps, 0, Some("verification"));
    // Prepend health check steps if enabled and URLs configured
    // Health checks run BEFORE log_watch to catch server down before scanning logs
    let verification_steps = crate::unified_workflows::prepend_health_check_steps(
        verification_steps,
        workflow.health_check_enabled,
        &workflow.health_check_urls,
    );
    // Prepend log_watch step if enabled (default: true)
    let verification_steps = crate::unified_workflows::prepend_log_watch_step(
        verification_steps,
        workflow.log_watch_enabled,
    );
    let agentic_steps = extract_prompt_steps_with_phase(&workflow.agentic_steps, Some("agentic"));
    let completion_automation_steps =
        convert_json_steps_with_phase(&workflow.completion_steps, 0, Some("completion"));
    let completion_prompt_steps =
        extract_prompt_steps_with_phase(&workflow.completion_steps, Some("completion"));

    // Calculate starting iteration for resume (sessions_count is the number of completed iterations)
    let starting_iteration = task_run.sessions_count;

    // For error-fix workflows, run agentic first (only if starting fresh)
    let run_agentic_first = !workflow.targeted_error_ids.is_empty() && starting_iteration == 0;

    let loop_config = LoopConfig {
        max_iterations: workflow.max_iterations,
        timeout_seconds: workflow.timeout_seconds, // Use workflow setting
        base_prompt: String::new(),
        workflow_name: task_run.task_name.clone(),
        workflow_id: workflow_id.clone(),
        execution_id: id.clone(),
        targeted_error_ids: workflow.targeted_error_ids.clone(),
        starting_iteration,
        run_agentic_first,
    };

    // Spawn the workflow execution in background with panic protection
    let app_state = state.app_state.clone();
    let config_storage = state.config_storage.clone();
    let app_handle = state.app_handle.clone();
    let pid_tracker = state.current_ai_pids.clone();
    let checkpoint_db = state.app_state.checkpoint_db.clone();
    let task_name = task_run.task_name.clone();
    let execution_id_for_guard = id.clone();

    // Use panic-safe spawning to ensure task is marked as failed if workflow panics
    crate::unified_workflow_executor::spawn_workflow_with_panic_guard(
        checkpoint_db,
        execution_id_for_guard,
        task_name.clone(),
        async move {
            let mut controller =
                LoopController::new(app_state, config_storage, app_handle, pid_tracker);

            controller
                .run(
                    loop_config,
                    setup_automation_steps,
                    setup_prompt_steps,
                    verification_steps,
                    agentic_steps,
                    completion_automation_steps,
                    completion_prompt_steps,
                )
                .await
        },
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "message": format!("Task run '{}' resumed successfully", task_run.task_name),
        "task_run_id": id,
        "workflow_id": workflow_id,
        "iteration": task_run.sessions_count + 1
    })))
}

/// Query parameters for task run events.
#[derive(Debug, Deserialize)]
struct TaskRunEventsQuery {
    event_type: Option<String>,
    limit: Option<u32>,
}

/// Get events for a task run from SQLite (hybrid logging).
async fn get_task_run_events(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<TaskRunEventsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let events = state
        .app_state
        .checkpoint_db
        .get_task_run_events(&id, query.event_type.as_deref(), query.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "events": events,
        "count": events.len()
    })))
}

/// Query parameters for paginated checkpoints.
#[derive(Debug, Deserialize)]
struct CheckpointsQuery {
    /// Cursor for pagination (step_index to start from, exclusive).
    cursor: Option<i64>,
    /// Maximum number of checkpoints to return (default: 50).
    limit: Option<usize>,
}

/// Get step checkpoints for a task run with cursor-based pagination.
///
/// This endpoint supports efficient pagination for runs with 1000+ steps.
/// Use the `next_cursor` from the response to fetch the next page.
async fn get_task_run_checkpoints(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<CheckpointsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let limit = query.limit.unwrap_or(50).min(100); // Cap at 100 per page

    let (checkpoints, next_cursor) = state
        .app_state
        .checkpoint_db
        .get_workflow_step_checkpoints_paginated(&id, query.cursor, limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "checkpoints": checkpoints,
        "count": checkpoints.len(),
        "cursor": query.cursor,
        "next_cursor": next_cursor,
        "has_more": next_cursor.is_some()
    })))
}

/// Query parameters for verification results.
#[derive(Debug, Deserialize)]
struct VerificationResultsQuery {
    /// Filter by iteration number (optional)
    iteration: Option<u32>,
    /// Only show failed results (optional, default: false)
    #[serde(default)]
    failed_only: bool,
}

/// Get verification results for a task run.
///
/// Returns detailed verification test results from the orchestrator,
/// including issues, observations, suggestions, and raw output.
/// This is useful for AI agents to understand what specifically failed
/// during verification and why.
async fn get_task_run_verification_results(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<VerificationResultsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get verification results
    let results = if let Some(iteration) = query.iteration {
        state
            .app_state
            .checkpoint_db
            .get_iteration_verification_results(&id, iteration)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    } else {
        state
            .app_state
            .checkpoint_db
            .get_latest_verification_results(&id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    };

    // Filter by failed_only if requested
    let results: Vec<_> = if query.failed_only {
        results.into_iter().filter(|r| !r.passed).collect()
    } else {
        results
    };

    // Calculate summary stats
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let critical_failed = results
        .iter()
        .filter(|r| !r.passed && r.is_critical)
        .count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "results": results,
        "summary": {
            "total": total,
            "passed": passed,
            "failed": failed,
            "critical_failed": critical_failed,
            "all_passed": failed == 0
        },
        "query": {
            "iteration": query.iteration,
            "failed_only": query.failed_only
        }
    })))
}

/// Query parameters for MCP calls endpoint.
#[derive(Debug, Deserialize)]
struct McpCallsQuery {
    /// Filter by success status (optional)
    success: Option<bool>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
}

/// Get MCP tool calls for a task run.
///
/// Returns all MCP tool calls made during the task execution,
/// including server info, tool name, arguments, response, and timing.
/// Useful for AI to understand what external tools were used.
async fn get_task_run_mcp_calls(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<McpCallsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get MCP calls
    let result = state
        .app_state
        .checkpoint_db
        .get_task_run_mcp_calls(&id, query.success)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply limit if specified
    let calls = if let Some(limit) = query.limit {
        result
            .calls
            .into_iter()
            .take(limit as usize)
            .collect::<Vec<_>>()
    } else {
        result.calls
    };

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "calls": calls,
        "summary": {
            "total": result.count,
            "success": result.success_count,
            "failed": result.failed_count
        },
        "query": {
            "success_filter": query.success,
            "limit": query.limit
        }
    })))
}

/// Query parameters for API requests endpoint.
#[derive(Debug, Deserialize)]
struct ApiRequestsQuery {
    /// Filter by success status (optional)
    success: Option<bool>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
}

/// Get API requests for a task run.
///
/// Returns all API requests made during the task execution,
/// including method, URL, status, response, and timing.
async fn get_task_run_api_requests(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<ApiRequestsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get API requests
    let requests = state
        .app_state
        .checkpoint_db
        .get_task_run_api_requests(&id, query.success)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply limit if specified
    let requests = if let Some(limit) = query.limit {
        requests
            .into_iter()
            .take(limit as usize)
            .collect::<Vec<_>>()
    } else {
        requests
    };

    // Calculate summary
    let total = requests.len();
    let success = requests.iter().filter(|r| r.success).count();
    let failed = total - success;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "requests": requests,
        "summary": {
            "total": total,
            "success": success,
            "failed": failed
        },
        "query": {
            "success_filter": query.success,
            "limit": query.limit
        }
    })))
}

/// Query parameters for Playwright results endpoint.
#[derive(Debug, Deserialize)]
struct PlaywrightResultsQuery {
    /// Filter by status (optional: "passed", "failed", "skipped")
    status: Option<String>,
    /// Limit number of results (optional, default: all)
    limit: Option<u32>,
}

/// Get Playwright test results for a task run.
///
/// Returns all Playwright test results including status, duration,
/// console output, page snapshots, and failure screenshots.
async fn get_task_run_playwright_results(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<PlaywrightResultsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get Playwright results
    let results = state
        .app_state
        .checkpoint_db
        .get_task_run_playwright_results(&id, query.status.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Apply limit if specified
    let results = if let Some(limit) = query.limit {
        results.into_iter().take(limit as usize).collect::<Vec<_>>()
    } else {
        results
    };

    // Calculate summary
    let total = results.len();
    let passed = results.iter().filter(|r| r.status == "passed").count();
    let failed = results.iter().filter(|r| r.status == "failed").count();
    let skipped = results.iter().filter(|r| r.status == "skipped").count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "results": results,
        "summary": {
            "total": total,
            "passed": passed,
            "failed": failed,
            "skipped": skipped
        },
        "query": {
            "status_filter": query.status,
            "limit": query.limit
        }
    })))
}

/// Get AWAS (Automated Web Agent System) steps for a task run.
///
/// Returns all AWAS operations including discovery, execution, and element extraction.
async fn get_task_run_awas_steps(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get AWAS steps
    let steps = state
        .app_state
        .checkpoint_db
        .get_task_run_awas_steps(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Calculate summary
    let total = steps.len();
    let success = steps.iter().filter(|s| s.success).count();
    let failed = total - success;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "steps": steps,
        "summary": {
            "total": total,
            "success": success,
            "failed": failed
        }
    })))
}

/// Query parameters for knowledge endpoint.
#[derive(Debug, Deserialize)]
struct KnowledgeQuery {
    /// Filter by category (optional: "finding", "observation", "solution", etc.)
    category: Option<String>,
    /// Only show unresolved entries (optional, default: false)
    #[serde(default)]
    unresolved_only: bool,
}

/// Get knowledge entries for a task run.
///
/// Returns accumulated knowledge from the task execution including:
/// - Findings (bugs, root causes identified)
/// - Observations (things noticed during execution)
/// - Solutions (fixes applied)
/// - Verification feedback (test failure context)
///
/// This helps the AI understand what was discovered and attempted.
async fn get_task_run_knowledge(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<KnowledgeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    let task = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get knowledge entries
    let knowledge = state
        .app_state
        .checkpoint_db
        .list_task_knowledge(&id, query.category.as_deref(), query.unresolved_only)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Calculate summary by category
    let total = knowledge.len();
    let findings = knowledge.iter().filter(|k| k.category == "finding").count();
    let observations = knowledge
        .iter()
        .filter(|k| k.category == "observation")
        .count();
    let solutions = knowledge
        .iter()
        .filter(|k| k.category == "solution")
        .count();
    let feedback = knowledge
        .iter()
        .filter(|k| k.category == "verification_feedback")
        .count();
    let unresolved = knowledge.iter().filter(|k| !k.is_resolved).count();

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "task_name": task.task_name,
        "knowledge": knowledge,
        "summary": {
            "total": total,
            "by_category": {
                "finding": findings,
                "observation": observations,
                "solution": solutions,
                "verification_feedback": feedback
            },
            "unresolved": unresolved
        },
        "query": {
            "category_filter": query.category,
            "unresolved_only": query.unresolved_only
        }
    })))
}

/// Path parameters for step progress endpoint.
#[derive(Debug, Deserialize)]
struct StepProgressPath {
    id: String,
    checkpoint_id: String,
}

/// Get progress markers for a specific step checkpoint.
///
/// Progress markers track intra-step progress (e.g., "analyzed 50/100 files").
async fn get_step_progress_markers(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(path): axum::extract::Path<StepProgressPath>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&path.id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Task run not found: {}", path.id),
            )
        })?;

    // Get progress markers for this checkpoint
    let markers = state
        .app_state
        .checkpoint_db
        .get_step_progress_markers(&path.checkpoint_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Also get the latest marker for quick access
    let latest = state
        .app_state
        .checkpoint_db
        .get_latest_step_progress_marker(&path.checkpoint_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "checkpoint_id": path.checkpoint_id,
        "markers": markers,
        "count": markers.len(),
        "latest": latest
    })))
}

/// Query parameters for current execution steps.
#[derive(Debug, Deserialize)]
struct CurrentExecutionStepsQuery {
    /// Filter by step type (shell_command, prompt, verification, etc.)
    step_type: Option<String>,
    /// Maximum number of steps to return
    limit: Option<u32>,
}

/// Step execution data for dashboard widget.
#[derive(Debug, Serialize)]
struct StepExecutionData {
    id: String,
    step_type: String,
    step_name: String,
    status: String, // "pending", "running", "success", "failed"
    /// Workflow phase: "setup", "verification", "agentic", or "completion"
    phase: Option<String>,
    /// Step index within the phase
    step_index: Option<i64>,
    /// Iteration number for verification/agentic phases (1-indexed)
    iteration: Option<i64>,
    start_time: Option<i64>,
    end_time: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    output: Option<String>,
    // Shell command specific fields
    command: Option<String>,
    working_directory: Option<String>,
    exit_code: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
    /// Original command template (with {{variable}} placeholders) - only present if variables were used
    template_command: Option<String>,
    /// Variables that were resolved during command execution (name -> resolved value)
    resolved_variables: Option<serde_json::Value>,
}

/// Get step executions for the currently running task.
/// This endpoint combines running task detection with event querying,
/// so the frontend doesn't need to track task IDs.
///
/// Events are aggregated by step_name + step_index so that start and complete
/// events for the same step are merged into a single entry.
async fn get_current_execution_steps(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<CurrentExecutionStepsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use std::collections::HashMap;

    // Get running tasks
    let running_tasks = state
        .app_state
        .checkpoint_db
        .get_running_task_runs()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // If no running task, return empty
    if running_tasks.is_empty() {
        return Ok(Json(serde_json::json!({
            "success": true,
            "task_run_id": null,
            "executions": [],
            "count": 0,
            "message": "No running task"
        })));
    }

    // Use the first running task (typically there's only one)
    let task = &running_tasks[0];

    // Get all events for this task (don't filter by event_type, we'll filter in code)
    let events = state
        .app_state
        .checkpoint_db
        .get_task_run_events(&task.id, None, query.limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Aggregate events by action_id (preferred) or step_name + step_index to merge start/complete events
    // Using action_id is more reliable because it's generated from metadata and consistent across events
    // Key: String (action_id or synthesized from step_name + step_index)
    let mut step_map: HashMap<String, StepExecutionData> = HashMap::new();

    for event in events {
        // Only process step-related events
        let event_type = event.event_type.as_str();
        if event_type != "step_execution" && event_type != "shell_command" {
            continue;
        }
        // Parse the event data JSON to extract step information
        let data: Option<serde_json::Value> = event
            .data
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok());

        let event_subtype = event.event_subtype.as_deref().unwrap_or("");
        let message = event.message.as_str();

        // Extract step identification
        let step_name = data
            .as_ref()
            .and_then(|d| d.get("step_name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| message.to_string());

        let step_index = data
            .as_ref()
            .and_then(|d| d.get("step_index"))
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);

        let step_type_str = data
            .as_ref()
            .and_then(|d| d.get("step_type"))
            .and_then(|v| v.as_str())
            .unwrap_or(event_type)
            .to_string();

        // Filter by step type if specified
        if let Some(ref filter_type) = query.step_type {
            if !step_type_str
                .to_lowercase()
                .contains(&filter_type.to_lowercase())
            {
                continue;
            }
        }

        // Extract iteration early for use in fallback key
        let iteration_for_key = data
            .as_ref()
            .and_then(|d| d.get("iteration"))
            .and_then(|v| v.as_i64());

        // Use action_id as the primary key for aggregation (most reliable)
        // Fall back to synthesized key from step_name + step_index + iteration
        // Including iteration in fallback prevents merging steps from different iterations
        let key = event.action_id.clone().unwrap_or_else(|| {
            if let Some(iter) = iteration_for_key {
                format!("{}:{}:{}", step_name, step_index, iter)
            } else {
                format!("{}:{}", step_name, step_index)
            }
        });

        // Get the timestamp for this event
        let event_timestamp = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
            .ok()
            .map(|dt| dt.timestamp_millis());

        // Determine status from event_subtype
        let status = match event_subtype {
            "start" => "running",
            "complete" | "success" => "success",
            "error" | "failed" => "failed",
            _ => "pending",
        }
        .to_string();

        // Check if we already have an entry for this step
        if let Some(existing) = step_map.get_mut(&key) {
            // Merge: prefer completion data over start data
            // Status priority: failed > success > running > pending
            // Once a step is marked as failed, it stays failed (even if we see a "complete" event)
            // This handles duplicate events where one might be error and one complete
            let should_update_status = match (existing.status.as_str(), status.as_str()) {
                // Never downgrade from failed (failed is highest priority)
                ("failed", _) => false,
                // Upgrade running/pending to anything terminal
                ("running", "success") | ("running", "failed") => true,
                ("pending", "success") | ("pending", "failed") => true,
                // Upgrade success only to failed (if somehow we get conflicting events)
                ("success", "failed") => true,
                // Don't change success to success (no-op) or downgrade success to running
                ("success", "success") | ("success", "running") => false,
                // Other cases: update
                _ => status != "running",
            };
            if should_update_status {
                existing.status = status;
            }

            // Update fields that are typically only in complete events
            if let Some(d) = &data {
                // Update phase if not already set
                if existing.phase.is_none() {
                    if let Some(v) = d.get("phase").and_then(|v| v.as_str()) {
                        existing.phase = Some(v.to_string());
                    }
                }
                // Update iteration if not already set
                if existing.iteration.is_none() {
                    if let Some(v) = d.get("iteration").and_then(|v| v.as_i64()) {
                        existing.iteration = Some(v);
                    }
                }
                // Try JSON data first, then fall back to event's top-level duration_ms
                if let Some(v) = d.get("duration_ms").and_then(|v| v.as_i64()) {
                    existing.duration_ms = Some(v);
                } else if existing.duration_ms.is_none() {
                    // Fall back to event's top-level duration_ms field
                    existing.duration_ms = event.duration_ms;
                }
                if let Some(v) = d.get("end_time").and_then(|v| v.as_i64()) {
                    existing.end_time = Some(v);
                }
                if let Some(v) = d.get("exit_code").and_then(|v| v.as_i64()) {
                    existing.exit_code = Some(v as i32);
                }
                if let Some(v) = d.get("stdout").and_then(|v| v.as_str()) {
                    existing.stdout = Some(v.to_string());
                }
                if let Some(v) = d.get("stderr").and_then(|v| v.as_str()) {
                    existing.stderr = Some(v.to_string());
                }
                if let Some(v) = d.get("error").and_then(|v| v.as_str()) {
                    existing.error = Some(v.to_string());
                }
                if let Some(v) = d.get("output").and_then(|v| v.as_str()) {
                    existing.output = Some(v.to_string());
                }
            }
        } else {
            // Extract phase from event data
            let phase = data
                .as_ref()
                .and_then(|d| d.get("phase"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Extract iteration from event data (for verification/agentic phases)
            let iteration = data
                .as_ref()
                .and_then(|d| d.get("iteration"))
                .and_then(|v| v.as_i64());

            // Create new entry
            let step_data = StepExecutionData {
                id: event.id.to_string(),
                step_type: step_type_str,
                step_name,
                status,
                phase,
                step_index: if step_index >= 0 {
                    Some(step_index)
                } else {
                    None
                },
                iteration,
                start_time: event_timestamp,
                end_time: data
                    .as_ref()
                    .and_then(|d| d.get("end_time"))
                    .and_then(|v| v.as_i64()),
                // Try JSON data first, then fall back to event's top-level duration_ms
                duration_ms: data
                    .as_ref()
                    .and_then(|d| d.get("duration_ms"))
                    .and_then(|v| v.as_i64())
                    .or(event.duration_ms),
                error: data
                    .as_ref()
                    .and_then(|d| d.get("error"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                output: data
                    .as_ref()
                    .and_then(|d| d.get("output"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                command: data
                    .as_ref()
                    .and_then(|d| d.get("command"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                working_directory: data
                    .as_ref()
                    .and_then(|d| d.get("working_directory"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                exit_code: data
                    .as_ref()
                    .and_then(|d| d.get("exit_code"))
                    .and_then(|v| v.as_i64())
                    .map(|i| i as i32),
                stdout: data
                    .as_ref()
                    .and_then(|d| d.get("stdout"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                stderr: data
                    .as_ref()
                    .and_then(|d| d.get("stderr"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                template_command: data
                    .as_ref()
                    .and_then(|d| d.get("template_command"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                resolved_variables: data
                    .as_ref()
                    .and_then(|d| d.get("resolved_variables"))
                    .cloned(),
            };

            step_map.insert(key, step_data);
        }
    }

    // Get completed iterations from verification_phase_results
    // Steps from completed iterations should not show as "running"
    let completed_iterations: std::collections::HashSet<i64> = state
        .app_state
        .checkpoint_db
        .get_all_verification_phase_results(&task.id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.get("iteration").and_then(|i| i.as_i64()))
        .collect();

    // Find the maximum iteration number across all steps
    // This helps detect stale "running" steps in older iterations
    let max_iteration: i64 = step_map
        .values()
        .filter_map(|s| s.iteration)
        .max()
        .unwrap_or(1);

    // Get current time for timeout detection
    let now_ms = chrono::Utc::now().timestamp_millis();
    // Steps running for more than 30 minutes are considered stuck
    const STEP_TIMEOUT_MS: i64 = 30 * 60 * 1000;

    // Determine which phases have completed steps (for stale detection of non-iterated phases)
    let has_completed_verification_step = step_map.values().any(|s| {
        s.phase.as_deref() == Some("verification")
            && (s.status == "success" || s.status == "failed")
    });
    let has_completed_agentic_step = step_map.values().any(|s| {
        s.phase.as_deref() == Some("agentic") && (s.status == "success" || s.status == "failed")
    });
    let has_completed_completion_step = step_map.values().any(|s| {
        s.phase.as_deref() == Some("completion") && (s.status == "success" || s.status == "failed")
    });

    // Fix stale "running" status for steps
    // A step is stale if:
    // 1. Its iteration has a verification_phase_result (iteration completed), OR
    // 2. Its iteration is less than the current max iteration (we've moved on), OR
    // 3. It has been running for more than 30 minutes (timeout), OR
    // 4. For setup steps without iteration: the loop has started (verification/agentic has run), OR
    // 5. For any step without iteration: a later phase has completed
    for step_data in step_map.values_mut() {
        if step_data.status == "running" {
            // Check for timeout
            let is_timed_out = step_data
                .start_time
                .map(|start| (now_ms - start) > STEP_TIMEOUT_MS)
                .unwrap_or(false);

            // Check for stale based on iteration or phase progression
            let is_stale = if let Some(iter) = step_data.iteration {
                // For steps with iteration: check iteration-based staleness
                completed_iterations.contains(&iter) || iter < max_iteration
            } else {
                // For steps without iteration: check phase-based staleness
                // Setup steps are stale if verification/agentic/completion has started
                // Verification/agentic steps without iteration are stale if completion has started
                match step_data.phase.as_deref() {
                    Some("setup") => {
                        // Setup is stale if loop has started or later phases have completed
                        max_iteration > 1
                            || has_completed_verification_step
                            || has_completed_agentic_step
                            || has_completed_completion_step
                    }
                    Some("verification") | Some("agentic") => {
                        // These phases normally have iterations, but if not, check if completion ran
                        has_completed_completion_step
                    }
                    Some("completion") => {
                        // Completion rarely gets stale, but timeout will catch it
                        false
                    }
                    _ => false,
                }
            };

            if is_stale || is_timed_out {
                // This iteration completed, we've moved past it, or it timed out.
                // Mark it as "failed" since something went wrong (no completion event).
                step_data.status = "failed".to_string();
                if step_data.error.is_none() {
                    if is_timed_out {
                        step_data.error =
                            Some("Step timed out (running for more than 30 minutes)".to_string());
                    } else {
                        step_data.error =
                            Some("Step did not complete properly (missing end event)".to_string());
                    }
                }
            }
        }
    }

    // Convert map to vector, sorted by start_time
    let mut executions: Vec<StepExecutionData> = step_map.into_values().collect();
    executions.sort_by(|a, b| {
        // Sort by start_time to maintain execution order
        a.start_time.cmp(&b.start_time)
    });

    // Determine current workflow stage from the most recent step event's phase
    // Priority: Find a step that is currently running, or fall back to the most recent step
    let current_stage: Option<String> = executions
        .iter()
        .filter(|e| e.status == "running")
        .filter_map(|e| e.phase.clone())
        .next_back()
        .or_else(|| {
            // Fall back to the most recent step's phase (by start_time, already sorted)
            executions
                .iter()
                .rev()
                .filter_map(|e| e.phase.clone())
                .next()
        });

    Ok(Json(serde_json::json!({
        "success": true,
        "task_run_id": task.id,
        "workflow_name": task.workflow_name,
        "workflow_type": task.workflow_type,
        "workflow_start_time": task.created_at,
        "current_stage": current_stage,
        "executions": executions,
        "count": executions.len()
    })))
}

/// Get screenshots for a task run from SQLite.
async fn get_task_run_screenshots(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify task exists
    state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    let screenshots = state
        .app_state
        .checkpoint_db
        .get_task_run_screenshots(&id, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "task_run_id": id,
        "screenshots": screenshots,
        "count": screenshots.len()
    })))
}

/// Query parameters for execution spans.
#[derive(Debug, Deserialize)]
struct ExecutionSpansQuery {
    /// Filter by execution/task ID
    execution_id: Option<String>,
    /// Filter span names using SQL LIKE pattern (e.g., "workflow.%")
    name_pattern: Option<String>,
    /// Filter spans with duration >= this value
    min_duration_ms: Option<i64>,
    /// Maximum number of spans to return (default: 100)
    limit: Option<u32>,
}

/// Get execution spans from SQLite (tracing data).
///
/// Supports filtering by execution_id, name pattern, and minimum duration.
async fn get_execution_spans(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ExecutionSpansQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let spans = state
        .app_state
        .checkpoint_db
        .get_execution_spans(
            query.execution_id.as_deref(),
            query.name_pattern.as_deref(),
            query.min_duration_ms,
            query.limit,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "spans": spans,
        "count": spans.len(),
        "filters": {
            "execution_id": query.execution_id,
            "name_pattern": query.name_pattern,
            "min_duration_ms": query.min_duration_ms,
            "limit": query.limit.unwrap_or(100)
        }
    })))
}

/// Migrate JSONL logs to SQLite for a task run.
async fn migrate_task_run_logs(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use std::path::PathBuf;
    use tracing::info;

    info!("Migrating JSONL logs to SQLite for task run: {}", id);

    // Verify task exists
    let task_run = state
        .app_state
        .checkpoint_db
        .get_task_run(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Task run not found: {}", id)))?;

    // Get the dev-logs directory path
    let dev_logs_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join(".dev-logs"))
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to resolve .dev-logs path".to_string(),
            )
        })?;

    // Run migration
    let db = state.app_state.checkpoint_db.clone();
    let workflow_name = task_run.workflow_name.clone();
    let task_id = id.clone();

    let result = tokio::task::spawn_blocking(move || {
        crate::log_migration::migrate_logs_to_sqlite(
            &db,
            &task_id,
            &dev_logs_dir,
            workflow_name.as_deref(),
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Task spawn error: {}", e),
        )
    })?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "task_run_id": id,
        "migrated": {
            "general_events": result.general_events,
            "action_events": result.action_events,
            "image_recognition_events": result.image_recognition_events,
            "screenshots": result.screenshots,
            "playwright_results": result.playwright_results
        },
        "errors": result.errors
    })))
}

// ============================================================================
// End Task Run HTTP API Handlers
// ============================================================================

// ============================================================================
// Automation Run HTTP API Handlers (for MCP/AI access)
// ============================================================================

/// Query params for listing automation runs.
#[derive(Debug, Deserialize)]
struct ListAutomationRunsQuery {
    /// Config ID to filter by (optional)
    config_id: Option<String>,
    /// Maximum number of runs to return (default: 20)
    limit: Option<u32>,
}

/// List recent automation runs.
async fn list_automation_runs(
    State(state): State<Arc<ApiState>>,
    axum::extract::Query(query): axum::extract::Query<ListAutomationRunsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let conn = state
        .app_state
        .checkpoint_db
        .connection()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let limit = query.limit.unwrap_or(20);

    // If config_id is provided, filter by it. Otherwise get all recent runs.
    let runs = if let Some(config_id) = &query.config_id {
        tiered_info::get_all_recent_runs(&conn, config_id, limit)
    } else {
        // Get runs across all configs - use a broader query
        get_all_recent_runs(&conn, limit)
    };

    match runs {
        Ok(runs) => Ok(Json(serde_json::json!({
            "success": true,
            "data": runs
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "success": false,
            "error": e
        }))),
    }
}

/// Helper to get recent runs across all configs from task_run_automation.
fn get_all_recent_runs(conn: &rusqlite::Connection, limit: u32) -> Result<Vec<RunDetails>, String> {
    use rusqlite::params;

    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                tra.id, tr.config_id, tra.workflow_name, tra.started_at, tra.ended_at, tra.duration_ms,
                tra.automation_status, tra.success, tra.error_type, tra.error_message,
                tra.actions_summary, tra.states_visited, tra.transitions_executed,
                tra.template_matches, tra.anomalies
            FROM task_run_automation tra
            INNER JOIN task_runs tr ON tra.task_run_id = tr.id
            ORDER BY tra.started_at DESC
            LIMIT ?1
            "#,
        )
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let runs = stmt
        .query_map(params![limit], row_to_run_details_from_automation)
        .map_err(|e| format!("Failed to query runs: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(runs)
}

/// Convert a row from task_run_automation to RunDetails.
fn row_to_run_details_from_automation(row: &rusqlite::Row) -> rusqlite::Result<RunDetails> {
    let status_str: String = row.get(6)?;
    let actions_json: Option<String> = row.get(10)?;
    let states_json: Option<String> = row.get(11)?;
    let transitions_json: Option<String> = row.get(12)?;
    let templates_json: Option<String> = row.get(13)?;
    let anomalies_json: Option<String> = row.get(14)?;

    Ok(RunDetails {
        id: row.get(0)?,
        config_id: row.get(1)?,
        workflow_name: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        duration_ms: row.get(5)?,
        status: tiered_info::RunStatus::from_str(&status_str)
            .unwrap_or(tiered_info::RunStatus::Running),
        success: row.get(7)?,
        error_type: row.get(8)?,
        error_message: row.get(9)?,
        actions_summary: actions_json.and_then(|j| serde_json::from_str(&j).ok()),
        states_visited: states_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        transitions_executed: transitions_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        template_matches: templates_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        anomalies: anomalies_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        screenshots: Vec::new(),
    })
}

/// Get a specific automation run by ID from task_run_automation.
async fn get_automation_run(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    use rusqlite::{params, OptionalExtension};

    let conn = state
        .app_state
        .checkpoint_db
        .connection()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let result: Result<Option<RunDetails>, String> = conn
        .query_row(
            r#"
            SELECT
                tra.id, tr.config_id, tra.workflow_name, tra.started_at, tra.ended_at, tra.duration_ms,
                tra.automation_status, tra.success, tra.error_type, tra.error_message,
                tra.actions_summary, tra.states_visited, tra.transitions_executed,
                tra.template_matches, tra.anomalies
            FROM task_run_automation tra
            INNER JOIN task_runs tr ON tra.task_run_id = tr.id
            WHERE tra.id = ?1
            "#,
            params![id],
            row_to_run_details_from_automation,
        )
        .optional()
        .map_err(|e| format!("Failed to get automation run: {}", e));

    match result {
        Ok(Some(run)) => Ok(Json(serde_json::json!({
            "success": true,
            "data": run
        }))),
        Ok(None) => Ok(Json(serde_json::json!({
            "success": false,
            "error": format!("Run not found: {}", id)
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "success": false,
            "error": e
        }))),
    }
}

// ============================================================================
// End Automation Run HTTP API Handlers
// ============================================================================

// ============================================================================
// Config Storage HTTP API Handlers
// ============================================================================

/// List all stored configurations (metadata only)
async fn list_configs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<ConfigMetadata>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    let configs = storage.list();
    Ok(Json(ApiResponse::success(configs)))
}

/// Request to parse a config file
#[derive(Debug, Deserialize)]
struct ParseConfigRequest {
    /// Path to the config file to parse
    path: String,
}

/// State info for the parse config response
#[derive(Debug, Serialize)]
struct ConfigStateInfo {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    is_initial: bool,
    is_final: bool,
}

/// Transition info for the parse config response
#[derive(Debug, Serialize)]
struct ConfigTransitionInfo {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to_state: Option<String>,
}

/// Response for parsing a config file
#[derive(Debug, Serialize)]
struct ParseConfigResponse {
    states: Vec<ConfigStateInfo>,
    transitions: Vec<ConfigTransitionInfo>,
}

/// Parse a configuration file and return states/transitions without importing
async fn parse_config_file(
    Json(request): Json<ParseConfigRequest>,
) -> Result<Json<ApiResponse<ParseConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ConfigLoader::load_from_file(&request.path) {
        Ok(config) => {
            let states: Vec<ConfigStateInfo> = config
                .states
                .iter()
                .filter_map(|s| {
                    let id = s.get("id")?.as_str()?.to_string();
                    let name = s.get("name")?.as_str()?.to_string();
                    let description = s
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let is_initial = s
                        .get("isInitial")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let is_final = s.get("isFinal").and_then(|v| v.as_bool()).unwrap_or(false);
                    Some(ConfigStateInfo {
                        id,
                        name,
                        description,
                        is_initial,
                        is_final,
                    })
                })
                .collect();

            let transitions: Vec<ConfigTransitionInfo> = config
                .transitions
                .iter()
                .filter_map(|t| {
                    let id = t.get("id")?.as_str()?.to_string();
                    let from_state = t
                        .get("fromState")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let to_state = t
                        .get("toState")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let name = t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            // Generate name from from_state -> to_state if no name
                            match (&from_state, &to_state) {
                                (Some(from), Some(to)) => format!("{} → {}", from, to),
                                (Some(from), None) => format!("{} → ?", from),
                                (None, Some(to)) => format!("? → {}", to),
                                (None, None) => id.clone(),
                            }
                        });
                    Some(ConfigTransitionInfo {
                        id,
                        name,
                        from_state,
                        to_state,
                    })
                })
                .collect();

            Ok(Json(ApiResponse::success(ParseConfigResponse {
                states,
                transitions,
            })))
        }
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Failed to parse config: {}", e))),
        )),
    }
}

/// Request body for importing a config
#[derive(Debug, Deserialize)]
struct ImportConfigRequest {
    /// Path to the config file to import
    path: String,
    /// Name to give the imported config
    name: String,
}

/// Response for import config
#[derive(Debug, Serialize)]
struct ImportConfigResponse {
    id: String,
}

/// Import a configuration from a file
async fn import_config(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportConfigRequest>,
) -> Result<Json<ApiResponse<ImportConfigResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut storage = state.config_storage.lock().await;
    match storage.import_from_file(&request.path, &request.name) {
        Ok(id) => Ok(Json(ApiResponse::success(ImportConfigResponse { id }))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Get a stored configuration by ID
async fn get_stored_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<StoredConfig>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    match storage.get(&id) {
        Ok(config) => Ok(Json(ApiResponse::success(config))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Request body for updating config metadata
#[derive(Debug, Deserialize)]
struct UpdateConfigRequest {
    /// New name (optional)
    name: Option<String>,
    /// New description (optional)
    description: Option<String>,
}

/// Update config metadata
async fn update_stored_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateConfigRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut storage = state.config_storage.lock().await;
    match storage.update_metadata(&id, request.name, request.description) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Delete a stored configuration
async fn delete_stored_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut storage = state.config_storage.lock().await;
    match storage.delete(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

/// Request body for exporting a config
#[derive(Debug, Deserialize)]
struct ExportConfigRequest {
    /// Path to export the config to
    path: String,
}

/// Export a configuration to a file
async fn export_config(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<ExportConfigRequest>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = state.config_storage.lock().await;
    match storage.export_to_file(&id, &request.path) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(crate::config_storage::ConfigStorageError::NotFound(_)) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Config not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(e.to_string())),
        )),
    }
}

// ============================================================================
// End Config Storage HTTP API Handlers
// ============================================================================

// ============================================================================
// State Explorer HTTP API Handlers
// ============================================================================

use crate::state_explorer::{
    ExplorationConfig, ExplorationStrategy, ExplorationTask, StateExplorer, StateMachineGraph,
};

/// Request body for starting state exploration
#[derive(Debug, Deserialize)]
struct StartExplorationRequest {
    /// Path to the qontinui config file
    config_path: String,
    /// Exploration strategy: "exhaustive", "smoke_test", "regression", "random_walk", "targeted"
    #[serde(default = "default_exploration_strategy")]
    strategy: String,
    /// Maximum number of states to visit (0 = unlimited)
    #[serde(default)]
    max_states: u32,
    /// Maximum time in seconds (0 = unlimited)
    #[serde(default)]
    max_duration_seconds: u64,
    /// Target state IDs for targeted strategy
    #[serde(default)]
    target_state_ids: Vec<String>,
    /// Target transition IDs for targeted strategy
    #[serde(default)]
    target_transition_ids: Vec<String>,
    /// Monitor index
    #[serde(default)]
    monitor_index: Option<i32>,
    /// Whether to capture screenshots
    #[serde(default = "default_capture_screenshots")]
    capture_screenshots: bool,
    /// Whether to stop on first failure
    #[serde(default)]
    stop_on_first_failure: bool,
}

fn default_exploration_strategy() -> String {
    "exhaustive".to_string()
}

fn default_capture_screenshots() -> bool {
    true
}

/// Start a state exploration task
async fn start_exploration(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExplorationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting exploration for {} with strategy {}",
        request.config_path, request.strategy
    );

    let config = ExplorationConfig {
        config_path: request.config_path,
        depth: None,
        strategy: request.strategy,
        max_states: request.max_states,
        max_duration_seconds: request.max_duration_seconds,
        target_state_ids: request.target_state_ids,
        target_transition_ids: request.target_transition_ids,
        monitor_index: request.monitor_index,
        capture_screenshots: request.capture_screenshots,
        capture_transition_screenshots: false,
        state_delay_ms: 500,
        output_directory: None,
        stop_on_first_failure: request.stop_on_first_failure,
        random_seed: None,
        checkpoint_batch_size: 10,
        checkpoint_issue_threshold: 5,
        checkpoint_on_critical: true,
        interleave_with_agentic: false,
    };

    let task = ExplorationTask::new(config, state.app_state.clone());

    // Run the task
    match task.execute().await {
        Ok(result) => {
            let result_json = serde_json::to_value(&result).unwrap_or_default();
            Ok(Json(ApiResponse::success(result_json)))
        }
        Err(e) => {
            error!("Exploration failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get available exploration strategies
async fn get_exploration_strategies() -> Json<ApiResponse<serde_json::Value>> {
    let strategies = serde_json::json!({
        "strategies": [
            {
                "id": "exhaustive",
                "name": "Exhaustive",
                "description": "Visit every state and transition - complete but slow",
                "recommended_for": "Full exploration runs, nightly builds"
            },
            {
                "id": "smoke_test",
                "name": "Smoke Test",
                "description": "Quick path through critical states with descriptions",
                "recommended_for": "Quick checks, CI/CD pipelines"
            },
            {
                "id": "regression",
                "name": "Regression",
                "description": "Focus on previously-failed areas",
                "recommended_for": "After fixes, before releases"
            },
            {
                "id": "random_walk",
                "name": "Random Walk",
                "description": "Random exploration to discover unexpected behaviors",
                "recommended_for": "Exploratory testing, chaos engineering"
            },
            {
                "id": "targeted",
                "name": "Targeted",
                "description": "Explore only specific states/transitions",
                "recommended_for": "Specific feature exploration"
            }
        ]
    });

    Json(ApiResponse::success(strategies))
}

/// Preview exploration plan without executing
async fn preview_exploration(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExplorationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load the current config
    let config_lock = safe_lock_or_recover(&state.app_state.current_config, "current_config");
    let qontinui_config = match config_lock.as_ref() {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error("No configuration loaded".to_string())),
            ));
        }
    };

    // Convert to JSON value for graph building
    let config_value = match serde_json::to_value(qontinui_config) {
        Ok(v) => v,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to serialize config: {}", e))),
            ));
        }
    };

    drop(config_lock);

    // Build graph and generate path
    let graph = StateMachineGraph::from_config(&config_value);

    let strategy = ExplorationStrategy::from_str(&request.strategy);
    let mut explorer = StateExplorer::new(graph.clone(), strategy);

    if request.max_states > 0 {
        explorer = explorer.with_max_states(request.max_states);
    }

    if !request.target_state_ids.is_empty() || !request.target_transition_ids.is_empty() {
        explorer = explorer.with_targets(
            request.target_state_ids.clone(),
            request.target_transition_ids.clone(),
        );
    }

    let path = explorer.generate_path();

    let plan = serde_json::json!({
        "strategy": format!("{:?}", strategy),
        "total_states_in_config": graph.states.len(),
        "total_transitions_in_config": graph.transitions.len(),
        "states_to_visit": path.states.len(),
        "transitions_to_explore": path.transitions.len(),
        "estimated_cost": path.estimated_cost,
        "states": path.states,
        "transitions": path.transitions,
    });

    Ok(Json(ApiResponse::success(plan)))
}

/// Get exploration history
async fn get_exploration_history(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let reports_dir = crate::paths::get_state_explorer_dir();

    if !reports_dir.exists() {
        return Json(ApiResponse::success(serde_json::json!({ "runs": [] })));
    }

    let mut runs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&reports_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with("exploration-report-"))
                    .unwrap_or(false)
            {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(report) = serde_json::from_str::<serde_json::Value>(&content) {
                        runs.push(serde_json::json!({
                            "run_id": report.get("run_id"),
                            "config_name": report.get("config_name"),
                            "strategy": report.get("strategy"),
                            "started_at": report.get("started_at"),
                            "completed_at": report.get("completed_at"),
                            "summary": report.get("summary"),
                            "report_path": path.to_string_lossy(),
                        }));
                    }
                }
            }
        }
    }

    // Sort by started_at descending and limit
    runs.sort_by(|a, b| {
        let a_time = a.get("started_at").and_then(|t| t.as_str()).unwrap_or("");
        let b_time = b.get("started_at").and_then(|t| t.as_str()).unwrap_or("");
        b_time.cmp(a_time)
    });

    runs.truncate(limit);

    Json(ApiResponse::success(serde_json::json!({ "runs": runs })))
}

/// Get a specific exploration report
async fn get_exploration_report(
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let reports_dir = crate::paths::get_state_explorer_dir();
    let report_path = reports_dir.join(format!("exploration-report-{}.json", run_id));

    if !report_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Report not found for run ID: {}",
                run_id
            ))),
        ));
    }

    let content = std::fs::read_to_string(&report_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to read report: {}", e))),
        )
    })?;

    let report: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to parse report: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(report)))
}

/// Get AI analysis prompt for an exploration report
async fn get_exploration_prompt(
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let reports_dir = crate::paths::get_state_explorer_dir();
    let report_path = reports_dir.join(format!("exploration-report-{}.json", run_id));

    if !report_path.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!(
                "Report not found for run ID: {}",
                run_id
            ))),
        ));
    }

    let content = std::fs::read_to_string(&report_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to read report: {}", e))),
        )
    })?;

    let report: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to parse report: {}", e))),
        )
    })?;

    let ai_prompt = report
        .get("ai_analysis_prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("No analysis prompt available")
        .to_string();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "run_id": run_id,
        "prompt": ai_prompt,
    }))))
}

// ============================================================================
// End State Explorer HTTP API Handlers
// ============================================================================

// ============================================================================
// Verification Test HTTP API Handlers
// ============================================================================

use crate::commands::testing::{
    execute_verification_test, execute_verification_test_suite, ExecuteTestResponse,
    ExecuteTestSuiteRequest, ExecuteTestSuiteResponse,
};
use crate::database::{CreateVerificationTestInput, TestResultStatus};
use crate::test_executor::{RepoTestConfig, TestCategory, TestDefinition, TestType, VisionConfig};

/// Request for listing tests with filters
#[derive(Debug, Deserialize)]
struct ListTestsQuery {
    enabled_only: Option<bool>,
    test_type: Option<String>,
    category: Option<String>,
}

/// Request for listing test results with filters
#[derive(Debug, Deserialize)]
struct ListTestResultsQuery {
    test_id: Option<String>,
    task_run_id: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
}

/// Request for executing a test by ID
#[derive(Debug, Deserialize)]
struct ExecuteTestByIdRequest {
    task_run_id: Option<String>,
}

/// List all verification tests
async fn list_tests(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTestsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::database::VerificationTest>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let db = &state.app_state.checkpoint_db;

    let test_type_enum: Option<crate::database::TestType> =
        query.test_type.as_ref().and_then(|t| t.parse().ok());

    match db.list_verification_tests(
        query.enabled_only.unwrap_or(false),
        test_type_enum.as_ref(),
        query.category.as_deref(),
    ) {
        Ok(tests) => Ok(Json(ApiResponse::success(tests))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list tests: {}", e))),
        )),
    }
}

/// Get a verification test by ID
async fn get_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    match db.get_verification_test(&id) {
        Ok(Some(test)) => Ok(Json(ApiResponse::success(test))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Test not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get test: {}", e))),
        )),
    }
}

/// Create a new verification test
async fn create_test(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateVerificationTestInput>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    info!(
        "Creating verification test: {} (type: {:?})",
        input.name, input.test_type
    );

    match db.create_verification_test(&input) {
        Ok(test) => Ok(Json(ApiResponse::success(test))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create test: {}", e))),
        )),
    }
}

/// Update a verification test
async fn update_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<CreateVerificationTestInput>,
) -> Result<Json<ApiResponse<crate::database::VerificationTest>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    info!("Updating verification test: {} ({})", input.name, id);

    match db.update_verification_test(&id, &input) {
        Ok(test) => Ok(Json(ApiResponse::success(test))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to update test: {}", e))),
        )),
    }
}

/// Delete a verification test
async fn delete_test(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    info!("Deleting verification test: {}", id);

    match db.delete_verification_test(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(()))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Test not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to delete test: {}", e))),
        )),
    }
}

/// Execute a verification test by ID
async fn execute_test_by_id(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ExecuteTestByIdRequest>,
) -> Result<Json<ApiResponse<ExecuteTestResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    // Get the test from database
    let test = match db.get_verification_test(&id) {
        Ok(Some(test)) => test,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Test not found: {}", id))),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get test: {}", e))),
            ))
        }
    };

    // Convert to TestDefinition
    let test_def = db_test_to_definition(&test);

    info!(
        "Executing verification test: {} (type: {:?})",
        test.name, test.test_type
    );

    // Execute the test
    let response = execute_verification_test(test_def);

    // Store the result if task_run_id is provided
    if let Some(task_run_id) = request.task_run_id {
        let result_input = crate::database::CreateTestResultInput {
            test_id: id.clone(),
            task_run_id: Some(task_run_id),
        };

        match db.create_test_result(&result_input) {
            Ok(test_result) => {
                // Convert status
                let db_status = match response.result.status {
                    crate::test_executor::TestStatus::Passed => TestResultStatus::Passed,
                    crate::test_executor::TestStatus::Failed => TestResultStatus::Failed,
                    crate::test_executor::TestStatus::Error => TestResultStatus::Error,
                    crate::test_executor::TestStatus::Timeout => TestResultStatus::Timeout,
                    crate::test_executor::TestStatus::Skipped => TestResultStatus::Skipped,
                    _ => TestResultStatus::Error,
                };

                // Update with execution results
                if let Err(e) = db.update_test_result(
                    &test_result.id,
                    &db_status,
                    Some(&response.result.output),
                    response.result.error.as_deref(),
                    response.result.structured_output.as_ref(),
                    response.result.assertions_passed,
                    response.result.assertions_failed,
                    &response.result.screenshots,
                ) {
                    warn!("Failed to update test result: {}", e);
                }
            }
            Err(e) => {
                warn!("Failed to create test result: {}", e);
            }
        }
    }

    Ok(Json(ApiResponse::success(response)))
}

/// Execute multiple tests as a suite
async fn execute_test_suite_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteTestSuiteRequest>,
) -> Result<Json<ApiResponse<ExecuteTestSuiteResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Executing test suite: {} tests (parallel: {})",
        request.tests.len(),
        request.parallel
    );

    let response = execute_verification_test_suite(request);
    Ok(Json(ApiResponse::success(response)))
}

/// List test results with optional filtering
async fn list_test_results(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTestResultsQuery>,
) -> Result<Json<ApiResponse<Vec<crate::database::TestResult>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let db = &state.app_state.checkpoint_db;

    // Parse status filter
    let status_enum: Option<TestResultStatus> =
        query
            .status
            .as_ref()
            .and_then(|s| match s.to_lowercase().as_str() {
                "pending" => Some(TestResultStatus::Pending),
                "running" => Some(TestResultStatus::Running),
                "passed" => Some(TestResultStatus::Passed),
                "failed" => Some(TestResultStatus::Failed),
                "skipped" => Some(TestResultStatus::Skipped),
                "error" => Some(TestResultStatus::Error),
                "timeout" => Some(TestResultStatus::Timeout),
                _ => None,
            });

    let limit = query.limit.unwrap_or(100);

    // Query based on provided filters
    let results = if let Some(test_id) = &query.test_id {
        db.get_results_for_test(test_id, Some(limit))
    } else if let Some(task_run_id) = &query.task_run_id {
        db.get_results_for_task_run(task_run_id)
    } else {
        db.list_test_results(status_enum.as_ref(), limit)
    };

    match results {
        Ok(results) => Ok(Json(ApiResponse::success(results))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list test results: {}", e))),
        )),
    }
}

/// Get a specific test result by ID
async fn get_test_result(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<crate::database::TestResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;

    match db.get_test_result(&id) {
        Ok(Some(result)) => Ok(Json(ApiResponse::success(result))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Test result not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get test result: {}", e))),
        )),
    }
}

/// Get test history summary (aggregated stats)
async fn get_test_history(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListTestResultsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let db = &state.app_state.checkpoint_db;
    let limit = query.limit.unwrap_or(1000);

    // Get all results (or filtered by test_id if provided)
    let results = if let Some(test_id) = &query.test_id {
        db.get_results_for_test(test_id, Some(limit))
    } else {
        db.list_test_results(None, limit)
    };

    match results {
        Ok(results) => {
            // Aggregate stats
            let mut total = 0;
            let mut passed = 0;
            let mut failed = 0;
            let mut error = 0;
            let mut timeout = 0;
            let mut skipped = 0;
            let mut total_duration_ms: i64 = 0;

            for result in &results {
                total += 1;
                total_duration_ms += result.duration_ms.unwrap_or(0);
                match result.status {
                    TestResultStatus::Passed => passed += 1,
                    TestResultStatus::Failed => failed += 1,
                    TestResultStatus::Error => error += 1,
                    TestResultStatus::Timeout => timeout += 1,
                    TestResultStatus::Skipped => skipped += 1,
                    _ => {}
                }
            }

            let pass_rate = if total > 0 {
                (passed as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            let summary = serde_json::json!({
                "total_runs": total,
                "passed": passed,
                "failed": failed,
                "error": error,
                "timeout": timeout,
                "skipped": skipped,
                "pass_rate": format!("{:.1}%", pass_rate),
                "total_duration_ms": total_duration_ms,
                "average_duration_ms": if total > 0 { total_duration_ms / total as i64 } else { 0 },
                "recent_results": results.iter().take(20).collect::<Vec<_>>(),
            });

            Ok(Json(ApiResponse::success(summary)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get test history: {}", e))),
        )),
    }
}

/// Convert database VerificationTest to executor TestDefinition
fn db_test_to_definition(test: &crate::database::VerificationTest) -> TestDefinition {
    // Parse category from string
    let category = test
        .category
        .as_ref()
        .map(|c| match c.as_str() {
            "visual" => TestCategory::Visual,
            "dom" => TestCategory::Dom,
            "network" => TestCategory::Network,
            "data" => TestCategory::Data,
            "log" => TestCategory::Log,
            "layout" => TestCategory::Layout,
            "unit" => TestCategory::Unit,
            "integration" => TestCategory::Integration,
            _ => TestCategory::Custom,
        })
        .unwrap_or(TestCategory::Custom);

    // Parse vision config if present
    let vision_config = test
        .vision_config
        .as_ref()
        .and_then(|v| serde_json::from_value::<VisionConfig>(v.clone()).ok());

    // Parse repo test config if present
    let repo_test_config = test
        .repo_test_config
        .as_ref()
        .and_then(|v| serde_json::from_value::<RepoTestConfig>(v.clone()).ok());

    // Convert test type
    let test_type = match test.test_type {
        crate::database::TestType::PlaywrightCdp => TestType::PlaywrightCdp,
        crate::database::TestType::QontinuiVision => TestType::QontinuiVision,
        crate::database::TestType::PythonScript => TestType::PythonScript,
        crate::database::TestType::RepositoryTest => TestType::RepositoryTest,
    };

    TestDefinition {
        id: test.id.clone(),
        name: test.name.clone(),
        test_type,
        category,
        playwright_code: test.playwright_code.clone(),
        vision_config,
        python_code: test.python_code.clone(),
        repo_test_config,
        timeout_seconds: test.timeout_seconds.unwrap_or(60),
        is_critical: test.is_critical,
        config: test.config.clone(),
    }
}

// ============================================================================
// End Verification Test HTTP API Handlers
// ============================================================================

// ============================================================================
// AI Context HTTP API Handlers
// ============================================================================

/// Request body for creating a context
#[derive(Debug, Deserialize)]
struct CreateContextRequest {
    name: String,
    content: String,
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(rename = "autoInclude")]
    auto_include: Option<context::ContextAutoInclude>,
}

/// Request body for updating a context
#[derive(Debug, Deserialize)]
struct UpdateContextRequest {
    name: Option<String>,
    content: Option<String>,
    category: Option<Option<String>>,
    tags: Option<Vec<String>>,
    #[serde(rename = "autoInclude")]
    auto_include: Option<Option<context::ContextAutoInclude>>,
}

/// Request body for duplicating a context
#[derive(Debug, Deserialize)]
struct DuplicateContextRequest {
    #[serde(rename = "targetScope")]
    target_scope: String,
}

/// Convert Context to ContextWithMetadata
fn context_to_with_metadata(
    ctx: context::Context,
    scope: context::ContextScope,
    library: &context::UserContextLibrary,
) -> context::ContextWithMetadata {
    let metadata = library.metadata.iter().find(|m| m.context_id == ctx.id);

    context::ContextWithMetadata {
        context: ctx,
        scope,
        enabled: metadata.map(|m| m.enabled).unwrap_or(true),
        use_count: metadata.map(|m| m.use_count).unwrap_or(0),
        last_used_at: metadata.and_then(|m| m.last_used_at.clone()),
        web_sync_status: metadata.and_then(|m| m.web_sync_status.clone()),
    }
}

/// GET /contexts - List all contexts from all scopes
async fn list_all_contexts(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<context::ContextWithMetadata>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let library = context::load_user_context_library();
    let mut all_contexts: Vec<context::ContextWithMetadata> = Vec::new();

    // Add project contexts from loaded config (if any)
    if let Ok(config_lock) = state.app_state.current_config.lock() {
        if let Some(ref config) = *config_lock {
            for ctx in context::get_project_contexts_from_config(&config.contexts) {
                all_contexts.push(context_to_with_metadata(
                    ctx,
                    context::ContextScope::Project,
                    &library,
                ));
            }
        }
    }

    // Add user contexts
    for ctx in context::get_all_user_contexts() {
        all_contexts.push(context_to_with_metadata(
            ctx,
            context::ContextScope::User,
            &library,
        ));
    }

    // Add builtin contexts
    for ctx in context::get_builtin_contexts() {
        all_contexts.push(context_to_with_metadata(
            ctx,
            context::ContextScope::Builtin,
            &library,
        ));
    }

    Ok(Json(ApiResponse::success(all_contexts)))
}

/// GET /contexts/categories - List all unique categories
async fn list_context_categories(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut categories = context::get_user_context_categories();

    // Add categories from project contexts
    if let Ok(config_lock) = state.app_state.current_config.lock() {
        if let Some(ref config) = *config_lock {
            for ctx in context::get_project_contexts_from_config(&config.contexts) {
                if let Some(cat) = ctx.category {
                    if !categories.contains(&cat) {
                        categories.push(cat);
                    }
                }
            }
        }
    }

    // Add categories from builtin contexts
    for ctx in context::get_builtin_contexts() {
        if let Some(cat) = ctx.category {
            if !categories.contains(&cat) {
                categories.push(cat);
            }
        }
    }

    categories.sort();
    categories.dedup();

    Ok(Json(ApiResponse::success(categories)))
}

/// GET /contexts/tags - List all unique tags
async fn list_context_tags(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let library = context::load_user_context_library();
    let mut tags: Vec<String> = Vec::new();

    // Collect tags from user contexts
    for ctx in &library.contexts {
        for tag in &ctx.tags {
            if !tags.contains(tag) {
                tags.push(tag.clone());
            }
        }
    }

    // Collect tags from project contexts
    if let Ok(config_lock) = state.app_state.current_config.lock() {
        if let Some(ref config) = *config_lock {
            for ctx in context::get_project_contexts_from_config(&config.contexts) {
                for tag in ctx.tags {
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
            }
        }
    }

    // Collect tags from builtin contexts
    for ctx in context::get_builtin_contexts() {
        for tag in ctx.tags {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    tags.sort();

    Ok(Json(ApiResponse::success(tags)))
}

/// POST /contexts/{scope} - Create a new context
async fn create_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(scope): axum::extract::Path<String>,
    Json(req): Json<CreateContextRequest>,
) -> Result<Json<ApiResponse<context::ContextWithMetadata>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scope.as_str() {
        "project" => {
            // Project contexts are stored in the loaded config
            let ctx = {
                let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to lock config: {}", e))),
                    )
                })?;

                let config = config_lock.as_mut().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(api_error(
                            "No project loaded. Please load a project configuration first.",
                        )),
                    )
                })?;

                // Create the context
                let ctx = context::create_project_context(
                    req.name,
                    req.content,
                    req.category,
                    req.tags,
                    req.auto_include,
                );

                // Add to config
                context::add_project_context_to_config(&mut config.contexts, ctx.clone()).map_err(
                    |e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(api_error(format!("Failed to add context to config: {}", e))),
                        )
                    },
                )?;

                ctx
            }; // config_lock dropped here

            // Save the config to the file
            if let Err(e) = save_current_config_to_file(&state.app_state) {
                warn!(
                    "Failed to save config after creating project context: {}",
                    e
                );
            }

            // Mark as pending sync to qontinui-web
            if let Err(e) =
                context::set_web_sync_status(&ctx.id, Some(context::WebSyncStatus::Pending))
            {
                warn!("Failed to set pending sync status for context: {}", e);
            }

            let library = context::load_user_context_library();
            Ok(Json(ApiResponse::success(context_to_with_metadata(
                ctx,
                context::ContextScope::Project,
                &library,
            ))))
        }
        "user" => {
            // User contexts are stored in the user library
            match context::create_user_context(
                req.name,
                req.content,
                req.category,
                req.tags,
                req.auto_include,
            ) {
                Ok(ctx) => {
                    let library = context::load_user_context_library();
                    Ok(Json(ApiResponse::success(context_to_with_metadata(
                        ctx,
                        context::ContextScope::User,
                        &library,
                    ))))
                }
                Err(e) => Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to create context: {}", e))),
                )),
            }
        }
        "builtin" => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Cannot create builtin contexts")),
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid scope: {}", scope))),
        )),
    }
}

/// PUT /contexts/{scope}/{id} - Update a context
async fn update_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((scope, id)): axum::extract::Path<(String, String)>,
    Json(req): Json<UpdateContextRequest>,
) -> Result<Json<ApiResponse<context::ContextWithMetadata>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scope.as_str() {
        "project" => {
            // Project contexts are stored in the loaded config
            let updated_ctx = {
                let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to lock config: {}", e))),
                    )
                })?;

                let config = config_lock.as_mut().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(api_error(
                            "No project loaded. Please load a project configuration first.",
                        )),
                    )
                })?;

                // Get the existing context
                let existing = context::get_project_context_from_config(&config.contexts, &id)
                    .ok_or_else(|| {
                        (
                            StatusCode::NOT_FOUND,
                            Json(api_error(format!("Context not found: {}", id))),
                        )
                    })?;

                // Create updated context
                let updated_ctx = context::Context {
                    id: existing.id,
                    name: req.name.unwrap_or(existing.name),
                    content: req.content.unwrap_or(existing.content),
                    category: req.category.unwrap_or(existing.category),
                    tags: req.tags.unwrap_or(existing.tags),
                    auto_include: req.auto_include.unwrap_or(existing.auto_include),
                    created_at: existing.created_at,
                    modified_at: chrono::Utc::now().to_rfc3339(),
                };

                // Update in config
                context::update_project_context_in_config(
                    &mut config.contexts,
                    updated_ctx.clone(),
                )
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to update context: {}", e))),
                    )
                })?;

                updated_ctx
            }; // config_lock dropped here

            // Save the config to the file
            if let Err(e) = save_current_config_to_file(&state.app_state) {
                warn!(
                    "Failed to save config after updating project context: {}",
                    e
                );
            }

            let library = context::load_user_context_library();
            Ok(Json(ApiResponse::success(context_to_with_metadata(
                updated_ctx,
                context::ContextScope::Project,
                &library,
            ))))
        }
        "user" => {
            // User contexts are stored in the user library
            match context::update_user_context(
                &id,
                req.name,
                req.content,
                req.category,
                req.tags,
                req.auto_include,
            ) {
                Ok(ctx) => {
                    let library = context::load_user_context_library();
                    Ok(Json(ApiResponse::success(context_to_with_metadata(
                        ctx,
                        context::ContextScope::User,
                        &library,
                    ))))
                }
                Err(e) => Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!("Context not found: {}", e))),
                )),
            }
        }
        "builtin" => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Cannot update builtin contexts")),
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid scope: {}", scope))),
        )),
    }
}

/// DELETE /contexts/{scope}/{id} - Delete a context
async fn delete_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((scope, id)): axum::extract::Path<(String, String)>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match scope.as_str() {
        "project" => {
            // Project contexts are stored in the loaded config
            {
                let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to lock config: {}", e))),
                    )
                })?;

                let config = config_lock.as_mut().ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(api_error(
                            "No project loaded. Please load a project configuration first.",
                        )),
                    )
                })?;

                context::delete_project_context_from_config(&mut config.contexts, &id).map_err(
                    |e| {
                        (
                            StatusCode::NOT_FOUND,
                            Json(api_error(format!("Context not found: {}", e))),
                        )
                    },
                )?;
            } // config_lock dropped here

            // Save the config to the file
            if let Err(e) = save_current_config_to_file(&state.app_state) {
                warn!(
                    "Failed to save config after deleting project context: {}",
                    e
                );
            }

            Ok(Json(ApiResponse::success(())))
        }
        "user" => match context::delete_user_context(&id) {
            Ok(()) => Ok(Json(ApiResponse::success(()))),
            Err(e) => Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Context not found: {}", e))),
            )),
        },
        "builtin" => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Cannot delete builtin contexts")),
        )),
        _ => Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid scope: {}", scope))),
        )),
    }
}

/// POST /contexts/{scope}/{id}/duplicate - Duplicate a context
async fn duplicate_context_handler(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((scope, id)): axum::extract::Path<(String, String)>,
    Json(req): Json<DuplicateContextRequest>,
) -> Result<Json<ApiResponse<context::ContextWithMetadata>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Find the source context from the appropriate scope
    let source_ctx = match scope.as_str() {
        "builtin" => context::get_builtin_contexts()
            .into_iter()
            .find(|c| c.id == id),
        "project" => {
            // Try to find in project contexts
            let config_lock = state.app_state.current_config.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to lock config: {}", e))),
                )
            })?;

            if let Some(ref config) = *config_lock {
                context::get_project_context_from_config(&config.contexts, &id)
            } else {
                None
            }
        }
        _ => context::get_user_context(&id),
    };

    let Some(source) = source_ctx else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Context not found: {}", id))),
        ));
    };

    // Create copy based on target scope
    let library = context::load_user_context_library();

    if req.target_scope == "project" {
        // Create copy in project config
        let ctx = {
            let mut config_lock = state.app_state.current_config.lock().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to lock config: {}", e))),
                )
            })?;

            let config = config_lock.as_mut().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(api_error(
                        "No project loaded. Please load a project configuration first.",
                    )),
                )
            })?;

            let ctx = context::create_project_context(
                format!("{} (Copy)", source.name),
                source.content.clone(),
                source.category.clone(),
                source.tags.clone(),
                source.auto_include.clone(),
            );

            context::add_project_context_to_config(&mut config.contexts, ctx.clone()).map_err(
                |e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!("Failed to add context to config: {}", e))),
                    )
                },
            )?;

            ctx
        }; // config_lock dropped here

        // Save the config to the file
        if let Err(e) = save_current_config_to_file(&state.app_state) {
            warn!(
                "Failed to save config after duplicating to project context: {}",
                e
            );
        }

        Ok(Json(ApiResponse::success(context_to_with_metadata(
            ctx,
            context::ContextScope::Project,
            &library,
        ))))
    } else {
        // Create copy in user library
        match context::create_user_context(
            format!("{} (Copy)", source.name),
            source.content.clone(),
            source.category.clone(),
            source.tags.clone(),
            source.auto_include.clone(),
        ) {
            Ok(ctx) => Ok(Json(ApiResponse::success(context_to_with_metadata(
                ctx,
                context::ContextScope::User,
                &library,
            )))),
            Err(e) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to duplicate context: {}", e))),
            )),
        }
    }
}

/// POST /contexts/metadata/{id}/enable - Enable a context
async fn enable_context_handler(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match context::set_context_enabled(&id, true) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to enable context: {}", e))),
        )),
    }
}

/// POST /contexts/metadata/{id}/disable - Disable a context
async fn disable_context_handler(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match context::set_context_enabled(&id, false) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to disable context: {}", e))),
        )),
    }
}

/// POST /contexts/:id/approve-sync - Approve syncing a project context to qontinui-web
async fn approve_context_sync(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Approving context sync for: {}", id);

    // Get the context from the loaded config
    let ctx = {
        let config_lock = state.app_state.current_config.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to lock config: {}", e))),
            )
        })?;

        let config = config_lock.as_ref().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "No project loaded. Please load a project configuration first.",
                )),
            )
        })?;

        context::get_project_context_from_config(&config.contexts, &id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Context not found: {}", id))),
            )
        })?
    };

    // Get the project ID from the loaded config
    let project_id = {
        let config_lock = state.app_state.current_config.lock().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to lock config: {}", e))),
            )
        })?;

        let config = config_lock.as_ref().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error("No project loaded")),
            )
        })?;

        config.metadata.project_id.clone().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "No project ID found in configuration. Cannot sync to qontinui-web.",
                )),
            )
        })?
    };

    // Sync to qontinui-web
    match sync_context_to_web(&project_id, &ctx).await {
        Ok(_) => {
            // Mark as synced
            if let Err(e) = context::set_web_sync_status(&id, Some(context::WebSyncStatus::Synced))
            {
                warn!("Failed to update sync status: {}", e);
            }

            info!("Successfully synced context {} to qontinui-web", id);
            Ok(Json(ApiResponse::success(serde_json::json!({
                "synced": true,
                "contextId": id,
                "projectId": project_id
            }))))
        }
        Err(e) => {
            error!("Failed to sync context to qontinui-web: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to sync to qontinui-web: {}", e))),
            ))
        }
    }
}

/// POST /contexts/:id/dismiss-sync - Dismiss syncing a project context
async fn dismiss_context_sync(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Dismissing context sync for: {}", id);

    match context::set_web_sync_status(&id, Some(context::WebSyncStatus::Dismissed)) {
        Ok(()) => {
            info!("Dismissed sync for context: {}", id);
            Ok(Json(ApiResponse::success(())))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to dismiss sync: {}", e))),
        )),
    }
}

/// Sync a context to qontinui-web by updating the project configuration
async fn sync_context_to_web(project_id: &str, ctx: &context::Context) -> Result<(), String> {
    use crate::auth::AuthManager;

    let auth_manager = AuthManager::new();

    // Check if authenticated
    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in to qontinui-web first.".to_string());
    }

    let access_token = auth_manager
        .get_access_token()
        .map_err(|e| format!("Failed to get access token: {}", e))?;

    // Get the API base URL
    let api_url = std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://localhost:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    });

    let client = reqwest::Client::new();

    // First, get the current project configuration
    let get_response = client
        .get(format!("{}/api/v1/projects/{}", api_url, project_id))
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| format!("Network error fetching project: {}", e))?;

    if !get_response.status().is_success() {
        let status = get_response.status();
        let error_text = get_response.text().await.unwrap_or_default();
        return Err(format!(
            "Failed to fetch project ({}): {}",
            status, error_text
        ));
    }

    let project: serde_json::Value = get_response
        .json()
        .await
        .map_err(|e| format!("Failed to parse project response: {}", e))?;

    // Get the current configuration and contexts
    let mut configuration = project
        .get("configuration")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let mut contexts: Vec<serde_json::Value> = configuration
        .get("contexts")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    // Check if context already exists (by ID)
    let existing_index = contexts
        .iter()
        .position(|c| c.get("id").and_then(|id| id.as_str()) == Some(&ctx.id));

    // Convert our context to JSON
    let ctx_json =
        serde_json::to_value(ctx).map_err(|e| format!("Failed to serialize context: {}", e))?;

    if let Some(idx) = existing_index {
        // Update existing context
        contexts[idx] = ctx_json;
        info!(
            "Updated existing context {} in qontinui-web project",
            ctx.id
        );
    } else {
        // Add new context
        contexts.push(ctx_json);
        info!("Added new context {} to qontinui-web project", ctx.id);
    }

    // Update the configuration
    configuration["contexts"] = serde_json::Value::Array(contexts);

    // PUT the updated project
    let update_body = serde_json::json!({
        "configuration": configuration
    });

    let put_response = client
        .put(format!("{}/api/v1/projects/{}", api_url, project_id))
        .bearer_auth(&access_token)
        .json(&update_body)
        .send()
        .await
        .map_err(|e| format!("Network error updating project: {}", e))?;

    if !put_response.status().is_success() {
        let status = put_response.status();
        let error_text = put_response.text().await.unwrap_or_default();
        return Err(format!(
            "Failed to update project ({}): {}",
            status, error_text
        ));
    }

    Ok(())
}

// ============================================================================
// End AI Context HTTP API Handlers
// ============================================================================

// ============================================================================
// DOM Capture HTTP API Handlers
// ============================================================================

/// List all DOM captures
async fn list_dom_captures(
) -> Result<Json<ApiResponse<Vec<DomCapture>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let captures = DomCaptureLogger::list_captures();
    Ok(Json(ApiResponse::success(captures)))
}

/// Get a specific DOM capture by ID
async fn get_dom_capture(
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<DomCapture>>, (StatusCode, Json<ApiResponse<()>>)> {
    match DomCaptureLogger::get_capture(&id) {
        Some(capture) => Ok(Json(ApiResponse::success(capture))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("DOM capture not found: {}", id))),
        )),
    }
}

/// Get the HTML content of a DOM capture
async fn get_dom_capture_html(
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    match DomCaptureLogger::get_capture_html(&id) {
        Ok(html) => Ok((
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Receive DOM capture from browser extension
async fn receive_dom_from_extension(
    Json(request): Json<ReceiveExtensionDomRequest>,
) -> Result<Json<ApiResponse<DomCapture>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Received DOM capture from extension: {} ({} bytes)",
        request.url,
        request.html.len()
    );

    // Use auto-link to find and link recent screenshots
    match DomCaptureLogger::log_capture_with_auto_link(
        &request.url,
        &request.page_title,
        &request.html,
        request.selector.as_deref(),
        DomCaptureSource::Extension,
        DomCaptureTrigger::OnDemand,
        request.task_run_id.as_deref(),
        None, // Will auto-find recent screenshot
    ) {
        Ok(capture) => {
            info!("Stored DOM capture: {} from {}", capture.id, capture.url);
            Ok(Json(ApiResponse::success(capture)))
        }
        Err(e) => {
            error!("Failed to store DOM capture: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to store DOM capture: {}", e))),
            ))
        }
    }
}

// ============================================================================
// End DOM Capture HTTP API Handlers
// ============================================================================

// ============================================================================
// API Request HTTP API Handlers
// ============================================================================

/// Request body for importing a cURL command
#[derive(Debug, Deserialize)]
struct ImportCurlRequest {
    curl_command: String,
}

/// Request body for testing an API request
#[derive(Debug, Deserialize)]
struct TestApiRequestBody {
    method: String,
    url: String,
    headers: Option<std::collections::HashMap<String, String>>,
    body: Option<String>,
    content_type: Option<String>,
    timeout_ms: Option<u64>,
    follow_redirects: Option<bool>,
    variables: Option<std::collections::HashMap<String, String>>,
}

/// Import a cURL command and return the parsed configuration
async fn import_curl_command(
    Json(request): Json<ImportCurlRequest>,
) -> Result<Json<ApiResponse<crate::api_request::ParsedCurl>>, (StatusCode, Json<ApiResponse<()>>)>
{
    info!(
        "Importing cURL command: {} bytes",
        request.curl_command.len()
    );

    match crate::api_request::parse_curl(&request.curl_command) {
        Ok(parsed) => {
            info!("Parsed cURL: {} {}", parsed.method, parsed.url);
            Ok(Json(ApiResponse::success(parsed)))
        }
        Err(e) => {
            error!("Failed to parse cURL command: {}", e);
            Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("Failed to parse cURL command: {}", e))),
            ))
        }
    }
}

/// Test an API request immediately (for debugging/testing in the editor)
async fn test_api_request(
    Json(request): Json<TestApiRequestBody>,
) -> Result<
    Json<ApiResponse<crate::api_request::ApiRequestResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Testing API request: {} {}", request.method, request.url);

    // Parse method
    let method = match request.method.to_uppercase().as_str() {
        "GET" => crate::api_request::HttpMethod::Get,
        "POST" => crate::api_request::HttpMethod::Post,
        "PUT" => crate::api_request::HttpMethod::Put,
        "PATCH" => crate::api_request::HttpMethod::Patch,
        "DELETE" => crate::api_request::HttpMethod::Delete,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!(
                    "Invalid HTTP method: {}",
                    request.method
                ))),
            ))
        }
    };

    // Build config
    let config = crate::api_request::ApiRequestConfig {
        step_id: None,
        step_name: None,
        method,
        url: request.url.clone(),
        resolved_url: None,
        headers: request.headers,
        body: request.body,
        content_type: request.content_type,
        timeout_ms: request.timeout_ms.or(Some(30000)),
        follow_redirects: request.follow_redirects.or(Some(true)),
        credential_id: None,
        extractions: None,
        assertions: None,
    };

    // Create executor with provided variables
    let executor = crate::api_request::ApiRequestExecutor::new();
    if let Some(vars) = request.variables {
        for (key, value) in vars {
            executor.resolver().set(&key, &value);
        }
    }

    // Execute the request (no credentials for test endpoint)
    match executor.execute(&config, None).await {
        Ok(result) => {
            info!(
                "API request completed: {} {} - {} in {}ms",
                request.method, request.url, result.status_code, result.response_time_ms
            );
            Ok(Json(ApiResponse::success(result)))
        }
        Err(e) => {
            error!("API request failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("API request failed: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Import cURL to API Request Library
// ============================================================================

/// Request body for importing cURL to library
#[derive(Debug, Deserialize)]
struct ImportCurlToLibraryRequest {
    curl_command: String,
    /// Custom name for the saved request (optional, defaults to URL-based name)
    name: Option<String>,
    /// Category for organization (optional, defaults to "imported")
    category: Option<String>,
}

/// Import a cURL command and save it to the API Request Library
async fn import_curl_to_library(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ImportCurlToLibraryRequest>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Importing cURL to library: {} bytes",
        request.curl_command.len()
    );

    // Parse the cURL command
    let parsed = match crate::api_request::parse_curl(&request.curl_command) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to parse cURL command: {}", e);
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("Failed to parse cURL command: {}", e))),
            ));
        }
    };

    // Generate a name from the URL if not provided
    let name = request.name.unwrap_or_else(|| {
        // Extract path from URL for a meaningful name
        if let Ok(url) = tauri::Url::parse(&parsed.url) {
            let path = url.path();
            if path.len() > 1 {
                // Remove leading slash and take first segment
                let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                if let Some(first) = segments.first() {
                    return format!("{} {}", parsed.method, first);
                }
            }
        }
        format!("{} Request", parsed.method)
    });

    // Create the saved API request
    let create_request = crate::saved_api_requests::CreateSavedApiRequestRequest {
        name,
        description: String::new(),
        category: request.category.unwrap_or_else(|| "imported".to_string()),
        tags: vec![],
        method: parsed.method,
        url: parsed.url.clone(),
        headers: parsed.headers,
        body: parsed.body,
        body_content_type: parsed.content_type,
        timeout_ms: 30000,
        follow_redirects: true,
        variable_extractions: vec![],
        assertions: vec![],
        credential_id: None,
    };

    match state
        .app_state
        .checkpoint_db
        .create_saved_api_request(&create_request)
    {
        Ok(saved) => {
            info!(
                "Saved API request to library: {} ({}) - {} {}",
                saved.name, saved.id, saved.method, parsed.url
            );
            Ok(Json(ApiResponse::success(saved)))
        }
        Err(e) => {
            error!("Failed to save API request to library: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to save API request: {}", e))),
            ))
        }
    }
}

// ============================================================================
// Saved API Requests Library HTTP API Handlers
// ============================================================================

/// List all saved API requests
async fn list_saved_api_requests(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<crate::saved_api_requests::SavedApiRequest>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.checkpoint_db.list_saved_api_requests() {
        Ok(requests) => Ok(Json(ApiResponse::success(requests))),
        Err(e) => {
            error!("Failed to list saved API requests: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to list saved API requests: {}",
                    e
                ))),
            ))
        }
    }
}

/// Get a single saved API request by ID
async fn get_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.checkpoint_db.get_saved_api_request(&id) {
        Ok(Some(request)) => Ok(Json(ApiResponse::success(request))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get saved API request: {}", e))),
            ))
        }
    }
}

/// Create a new saved API request
async fn create_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<crate::saved_api_requests::CreateSavedApiRequestRequest>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Creating saved API request: {} {}",
        request.method, request.url
    );
    match state
        .app_state
        .checkpoint_db
        .create_saved_api_request(&request)
    {
        Ok(created) => {
            info!(
                "Created saved API request: {} ({})",
                created.name, created.id
            );
            Ok(Json(ApiResponse::success(created)))
        }
        Err(e) => {
            error!("Failed to create saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Update a saved API request
async fn update_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<crate::saved_api_requests::UpdateSavedApiRequestRequest>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Updating saved API request: {}", id);
    match state
        .app_state
        .checkpoint_db
        .update_saved_api_request(&id, &request)
    {
        Ok(updated) => {
            info!(
                "Updated saved API request: {} ({})",
                updated.name, updated.id
            );
            Ok(Json(ApiResponse::success(updated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Delete a saved API request
async fn delete_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting saved API request: {}", id);
    match state.app_state.checkpoint_db.delete_saved_api_request(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(serde_json::json!({
            "deleted": true,
            "id": id
        })))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to delete saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

/// Search saved API requests
async fn search_saved_api_requests(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<crate::saved_api_requests::SearchSavedApiRequestsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::saved_api_requests::SavedApiRequest>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .checkpoint_db
        .search_saved_api_requests(&query)
    {
        Ok(requests) => Ok(Json(ApiResponse::success(requests))),
        Err(e) => {
            error!("Failed to search saved API requests: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to search saved API requests: {}",
                    e
                ))),
            ))
        }
    }
}

/// Get all categories from saved API requests
async fn get_saved_api_request_categories(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state
        .app_state
        .checkpoint_db
        .get_saved_api_request_categories()
    {
        Ok(categories) => Ok(Json(ApiResponse::success(categories))),
        Err(e) => {
            error!("Failed to get saved API request categories: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get categories: {}", e))),
            ))
        }
    }
}

/// Get all tags from saved API requests
async fn get_saved_api_request_tags(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.app_state.checkpoint_db.get_saved_api_request_tags() {
        Ok(tags) => Ok(Json(ApiResponse::success(tags))),
        Err(e) => {
            error!("Failed to get saved API request tags: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get tags: {}", e))),
            ))
        }
    }
}

/// Duplicate a saved API request
async fn duplicate_saved_api_request(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::saved_api_requests::SavedApiRequest>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Duplicating saved API request: {}", id);
    match state
        .app_state
        .checkpoint_db
        .duplicate_saved_api_request(&id)
    {
        Ok(duplicated) => {
            info!("Duplicated saved API request: {} -> {}", id, duplicated.id);
            Ok(Json(ApiResponse::success(duplicated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Saved API request not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to duplicate saved API request: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to duplicate saved API request: {}",
                    e
                ))),
            ))
        }
    }
}

// ============================================================================
// End Saved API Requests Library HTTP API Handlers
// ============================================================================

// ============================================================================
// Unified Workflows HTTP API Handlers
// ============================================================================

/// List all unified workflows
async fn list_unified_workflows(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<crate::unified_workflows::UnifiedWorkflow>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.checkpoint_db.list_unified_workflows() {
        Ok(workflows) => Ok(Json(ApiResponse::success(workflows))),
        Err(e) => {
            error!("Failed to list unified workflows: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to list unified workflows: {}",
                    e
                ))),
            ))
        }
    }
}

/// Get a single unified workflow by ID
async fn get_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(workflow)) => Ok(Json(ApiResponse::success(workflow))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to get unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get unified workflow: {}", e))),
            ))
        }
    }
}

/// Create a new unified workflow
async fn create_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<crate::unified_workflows::CreateUnifiedWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Creating unified workflow: {}", request.name);
    match state
        .app_state
        .checkpoint_db
        .create_unified_workflow(&request)
    {
        Ok(created) => {
            info!(
                "Created unified workflow: {} ({})",
                created.name, created.id
            );
            Ok(Json(ApiResponse::success(created)))
        }
        Err(e) => {
            error!("Failed to create unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to create unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Update a unified workflow
async fn update_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<crate::unified_workflows::UpdateUnifiedWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Updating unified workflow: {}", id);
    match state
        .app_state
        .checkpoint_db
        .update_unified_workflow(&id, &request)
    {
        Ok(updated) => {
            info!(
                "Updated unified workflow: {} ({})",
                updated.name, updated.id
            );
            Ok(Json(ApiResponse::success(updated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to update unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to update unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Delete a unified workflow
async fn delete_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Deleting unified workflow: {}", id);
    match state.app_state.checkpoint_db.delete_unified_workflow(&id) {
        Ok(true) => Ok(Json(ApiResponse::success(serde_json::json!({
            "deleted": true,
            "id": id
        })))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to delete unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to delete unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Search unified workflows
async fn search_unified_workflows(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<crate::unified_workflows::SearchUnifiedWorkflowsQuery>,
) -> Result<
    Json<ApiResponse<Vec<crate::unified_workflows::UnifiedWorkflow>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    match state
        .app_state
        .checkpoint_db
        .search_unified_workflows(&query)
    {
        Ok(workflows) => Ok(Json(ApiResponse::success(workflows))),
        Err(e) => {
            error!("Failed to search unified workflows: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to search unified workflows: {}",
                    e
                ))),
            ))
        }
    }
}

/// Duplicate a unified workflow
async fn duplicate_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::UnifiedWorkflow>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Duplicating unified workflow: {}", id);
    match state
        .app_state
        .checkpoint_db
        .duplicate_unified_workflow(&id)
    {
        Ok(duplicated) => {
            info!("Duplicated unified workflow: {} -> {}", id, duplicated.id);
            Ok(Json(ApiResponse::success(duplicated)))
        }
        Err(e) if e.contains("not found") => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to duplicate unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to duplicate unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Export a single unified workflow as a standalone JSON file
async fn export_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::WorkflowExport>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Exporting unified workflow: {}", id);

    match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(workflow)) => {
            let export = crate::unified_workflows::WorkflowExport {
                manifest: crate::unified_workflows::WorkflowExportManifest {
                    version: "1.0.0".to_string(),
                    exported_at: chrono::Utc::now().to_rfc3339(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                    content_type: "unified_workflow".to_string(),
                },
                workflow,
            };
            info!("Exported unified workflow: {}", id);
            Ok(Json(ApiResponse::success(export)))
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Unified workflow not found: {}", id))),
        )),
        Err(e) => {
            error!("Failed to export unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to export unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Import a unified workflow from an export file
async fn import_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<crate::unified_workflows::ImportWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::unified_workflows::ImportWorkflowResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Importing unified workflow: {} (strategy: {})",
        request.workflow.name, request.conflict_strategy
    );

    let mut workflow = request.workflow;
    let original_id = workflow.id.clone();
    let mut overwritten = false;

    // Check if workflow with this ID already exists
    let existing = state
        .app_state
        .checkpoint_db
        .get_unified_workflow(&workflow.id)
        .ok()
        .flatten();

    match request.conflict_strategy.as_str() {
        "keep" => {
            // Try to use the original ID, fail if it exists
            if existing.is_some() {
                return Err((
                    StatusCode::CONFLICT,
                    Json(api_error(format!(
                        "Workflow with ID '{}' already exists. Use 'generate' or 'overwrite' strategy.",
                        workflow.id
                    ))),
                ));
            }
        }
        "overwrite" => {
            // If exists, delete it first
            if existing.is_some() {
                if let Err(e) = state
                    .app_state
                    .checkpoint_db
                    .delete_unified_workflow(&workflow.id)
                {
                    error!("Failed to delete existing workflow for overwrite: {}", e);
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(api_error(format!(
                            "Failed to delete existing workflow: {}",
                            e
                        ))),
                    ));
                }
                overwritten = true;
            }
        }
        _ => {
            // Always generate a new ID
            workflow.id = uuid::Uuid::new_v4().to_string();
        }
    }

    // Update timestamps
    let now = chrono::Utc::now().to_rfc3339();
    workflow.updated_at = now.clone();
    if request.conflict_strategy != "overwrite" || !overwritten {
        workflow.created_at = now;
    }

    // Create the workflow using the existing create function logic
    let create_request = crate::unified_workflows::CreateUnifiedWorkflowRequest {
        name: workflow.name.clone(),
        description: workflow.description.clone(),
        category: workflow.category.clone(),
        tags: workflow.tags.clone(),
        setup_steps: workflow.setup_steps.clone(),
        verification_steps: workflow.verification_steps.clone(),
        agentic_steps: workflow.agentic_steps.clone(),
        completion_steps: workflow.completion_steps.clone(),
        max_iterations: workflow.max_iterations,
        timeout_seconds: workflow.timeout_seconds,
        provider: workflow.provider.clone(),
        model: workflow.model.clone(),
        skip_ai_summary: workflow.skip_ai_summary,
        log_source_selection: workflow.log_source_selection.clone(),
        context_ids: workflow.context_ids.clone(),
        disabled_context_ids: workflow.disabled_context_ids.clone(),
        auto_include_contexts: workflow.auto_include_contexts,
        prompt_template: workflow.prompt_template.clone(),
        log_watch_enabled: workflow.log_watch_enabled,
        health_check_enabled: workflow.health_check_enabled,
        health_check_urls: workflow.health_check_urls.clone(),
        preflight_check_enabled: workflow.preflight_check_enabled,
    };

    // Use the database's create function but with our custom ID
    match state
        .app_state
        .checkpoint_db
        .create_unified_workflow_with_id(&workflow.id, &create_request)
    {
        Ok(created) => {
            info!(
                "Imported unified workflow: {} ({}) [overwritten: {}]",
                created.name, created.id, overwritten
            );
            Ok(Json(ApiResponse::success(
                crate::unified_workflows::ImportWorkflowResult {
                    workflow: created,
                    overwritten,
                    original_id: if workflow.id != original_id {
                        Some(original_id)
                    } else {
                        None
                    },
                },
            )))
        }
        Err(e) => {
            error!("Failed to import unified workflow: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to import unified workflow: {}",
                    e
                ))),
            ))
        }
    }
}

/// Generate a unified workflow from natural language description using AI
async fn generate_unified_workflow_handler(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<workflow_generation::GenerateWorkflowRequest>,
) -> Result<
    Json<ApiResponse<workflow_generation::GenerateWorkflowResponse>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!(
        "Generating unified workflow from description: {}...",
        &request.description[..request.description.len().min(50)]
    );

    // Run the generation in a blocking task since it uses sync AI provider
    let result =
        tokio::task::spawn_blocking(move || workflow_generation::generate_workflow(request))
            .await
            .map_err(|e| {
                error!(
                    "Failed to spawn blocking task for workflow generation: {}",
                    e
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to generate workflow: {}", e))),
                )
            })?;

    if result.success {
        info!(
            "Successfully generated workflow: {}",
            result
                .workflow
                .as_ref()
                .map(|w| w.name.as_str())
                .unwrap_or("unknown")
        );
        Ok(Json(ApiResponse::success(result)))
    } else {
        warn!(
            "Workflow generation failed: {}",
            result.error.as_deref().unwrap_or("unknown error")
        );
        // Still return success HTTP status with the error in the response body
        // This allows the client to show the error message to the user
        Ok(Json(ApiResponse::success(result)))
    }
}

// =============================================================================
// NOTE: run_unified_workflow_with_verification_loop was removed and replaced
// with the modular unified_workflow_executor module.
// See: src/unified_workflow_executor/
// =============================================================================

/// Request body for running a unified workflow
#[derive(Debug, Deserialize)]
struct RunUnifiedWorkflowRequest {
    /// Monitor index to use (defaults to 0)
    #[serde(default)]
    monitor_index: Option<i32>,
    /// Timeout in seconds (defaults to 300)
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Optional task_run_id for resuming an existing execution.
    /// If provided, the workflow will resume from where it left off.
    /// If not provided, the system will check for incomplete task_runs and auto-resume,
    /// or create a new execution if none exist.
    #[serde(default)]
    task_run_id: Option<String>,
    /// Force a fresh start even if there's an incomplete task_run.
    /// When true, creates a new execution_id instead of resuming.
    #[serde(default)]
    force_fresh_start: bool,
}

/// Request body for executing an inline workflow (without saving to database)
/// Used by Quick Fix to run a workflow directly without cluttering the library
#[derive(Debug, Deserialize, Serialize)]
struct ExecuteInlineWorkflowRequest {
    /// Workflow name
    name: String,
    /// Description
    #[serde(default)]
    description: String,
    /// Setup phase steps
    #[serde(default)]
    setup_steps: Vec<serde_json::Value>,
    /// Verification phase steps
    #[serde(default)]
    verification_steps: Vec<serde_json::Value>,
    /// Agentic phase steps
    #[serde(default)]
    agentic_steps: Vec<serde_json::Value>,
    /// Completion phase steps
    #[serde(default)]
    completion_steps: Vec<serde_json::Value>,
    /// Maximum iterations for agentic phase
    #[serde(default = "default_max_iterations")]
    max_iterations: u32,
    /// Timeout in seconds
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Monitor index to use (defaults to 0)
    #[serde(default)]
    monitor_index: Option<i32>,
    /// Error IDs targeted by this workflow
    #[serde(default)]
    targeted_error_ids: Vec<i64>,
    /// Workflow settings (optional, extracted from generated workflow)
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

fn default_max_iterations() -> u32 {
    10
}

/// Run a unified workflow by ID
///
/// This endpoint executes a unified workflow by:
/// 1. Fetching the workflow from the database
/// 2. Converting phase steps to executable steps
/// 3. Running setup -> verification -> agentic -> completion phases
async fn run_unified_workflow(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<RunUnifiedWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::step_executor::ExecutionResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Running unified workflow: {}", id);

    // Fetch the workflow
    let workflow = match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Unified workflow not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get unified workflow: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get unified workflow: {}", e))),
            ));
        }
    };

    info!(
        "Executing unified workflow '{}' with {} setup, {} verification, {} agentic, {} completion steps",
        workflow.name,
        workflow.setup_steps.len(),
        workflow.verification_steps.len(),
        workflow.agentic_steps.len(),
        workflow.completion_steps.len()
    );

    // Save unified workflow config to .dev-logs for Claude Code debugging access
    // This is separate from GUI automation configs to avoid confusion
    if let Ok(workflow_json) = serde_json::to_value(&workflow) {
        crate::executor::file_logger::save_unified_workflow_config(&workflow_json, &workflow.name);
    }

    let monitor_index = request.monitor_index.unwrap_or(0);
    let _timeout_seconds = request.timeout_seconds.unwrap_or(300);

    // Convert JSON steps to ExecutionStepConfig
    // For now, we run phases sequentially: setup -> (verification + agentic) -> completion
    let mut all_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();

    // Helper to convert Value steps to ExecutionStepConfig
    let convert_step = |step: &serde_json::Value,
                        monitor: i32|
     -> Option<crate::step_executor::ExecutionStepConfig> {
        // Try to deserialize the step directly
        if let Ok(mut config) =
            serde_json::from_value::<crate::step_executor::ExecutionStepConfig>(step.clone())
        {
            // Set monitor index if not specified
            if config.monitor_index.is_none() {
                config.monitor_index = Some(monitor);
            }

            // Fix ambiguous "command" field mapping based on step_type
            // Both shell_command and check_command have alias "command", so serde picks one arbitrarily.
            // We need to ensure the command goes to the right field based on step_type.
            if let Some(command) = step.get("command").and_then(|v| v.as_str()) {
                let cmd = command.to_string();
                match config.step_type.as_str() {
                    "shell_command" => {
                        if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                    }
                    "check" => {
                        if config.check_command.is_none() {
                            config.check_command = Some(cmd);
                        }
                    }
                    "test" => {
                        // Test steps may also use command field
                        if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                    }
                    _ => {}
                }
            }

            // Fix prompt content mapping - "content" field maps to prompt_content for prompt steps
            if config.step_type == "prompt" && config.prompt_content.is_none() {
                if let Some(content) = step.get("content").and_then(|v| v.as_str()) {
                    config.prompt_content = Some(content.to_string());
                }
            }

            return Some(config);
        }

        // Fall back to extracting type and creating manually
        let step_type = step.get("type").and_then(|t| t.as_str())?;
        let name = step
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());

        Some(crate::step_executor::ExecutionStepConfig {
            step_type: step_type.to_string(),
            name,
            action_type: step
                .get("actionType")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            target_image_id: step
                .get("targetImageId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            target_image_name: step
                .get("targetImageName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            monitor_index: Some(monitor),
            take_screenshot: step
                .get("takeScreenshot")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            screenshot_delay: step
                .get("screenshotDelay")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(0),
            screenshot_monitor: step.get("screenshotMonitor").cloned(),
            playwright_script_id: step
                .get("playwrightScriptId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            playwright_script_content: step
                .get("playwrightScript")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            playwright_target_url: None,
            prompt_content: step
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            timeout_seconds: step.get("timeoutSeconds").and_then(|v| v.as_u64()),
            initial_state_ids: None,
            is_setup: step.get("isSetup").and_then(|v| v.as_bool()),
            phase: step
                .get("phase")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            run_on_subsequent_iterations: None,
            test_id: step
                .get("test_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            test_type: step
                .get("test_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            test_is_critical: step.get("is_critical").and_then(|v| v.as_bool()),
            sub_step_id: step
                .get("subStepId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            awas_url: step
                .get("awasUrl")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            awas_action_id: step
                .get("awasActionName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            awas_params: step.get("awasParameters").cloned(),
            awas_html: None,
            awas_base_url: step
                .get("awasBaseUrl")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            mcp_server_id: step
                .get("mcpServerId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            mcp_server_name: step
                .get("mcpServerName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            mcp_tool_name: step
                .get("mcpToolName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            mcp_arguments: step.get("mcpArguments").cloned(),
            mcp_fail_on_error: step.get("mcpFailOnError").and_then(|v| v.as_bool()),
            // Shell command fields
            shell_command: step
                .get("command")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            shell_command_id: step
                .get("shell_command_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            shell_command_working_directory: step
                .get("working_directory")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            shell_command_fail_on_error: step.get("fail_on_error").and_then(|v| v.as_bool()),
            // API request fields
            api_method: step
                .get("method")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_url: step
                .get("url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_headers: step.get("headers").cloned(),
            api_body: step
                .get("body")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_content_type: step
                .get("content_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_output_variable: step
                .get("output_variable")
                .or_else(|| step.get("apiOutputVariable"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_extractions: step
                .get("extractions")
                .or_else(|| step.get("apiExtractions"))
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            api_timeout_ms: step
                .get("timeout_ms")
                .or_else(|| step.get("apiTimeoutMs"))
                .and_then(|v| v.as_u64()),
            // Check fields - support both snake_case and camelCase
            check_type: step
                .get("check_type")
                .or_else(|| step.get("checkType"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            check_command: step
                .get("command")
                .or_else(|| step.get("check_command"))
                .or_else(|| step.get("checkCommand"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            check_working_directory: step
                .get("working_directory")
                .or_else(|| step.get("workingDirectory"))
                .or_else(|| step.get("check_working_directory"))
                .or_else(|| step.get("checkWorkingDirectory"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            check_auto_fix: step
                .get("auto_fix")
                .or_else(|| step.get("autoFix"))
                .or_else(|| step.get("check_auto_fix"))
                .or_else(|| step.get("checkAutoFix"))
                .and_then(|v| v.as_bool()),
            check_url: step
                .get("check_url")
                .or_else(|| step.get("checkUrl"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expected_status: step
                .get("expected_status")
                .or_else(|| step.get("expectedStatus"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u16),
            // Macro fields
            macro_id: step
                .get("macro_id")
                .or_else(|| step.get("macroId"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // Check group fields
            check_group_id: step
                .get("check_group_id")
                .or_else(|| step.get("checkGroupId"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // Log watch fields
            log_sources: step
                .get("logSources")
                .or_else(|| step.get("log_sources"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                }),
            time_window_seconds: step
                .get("timeWindowSeconds")
                .or_else(|| step.get("time_window_seconds"))
                .and_then(|v| v.as_u64()),
            error_patterns: step
                .get("errorPatterns")
                .or_else(|| step.get("error_patterns"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                }),
            // Spec fields
            spec_group_json: step.get("spec_group").cloned(),
            spec_element_source: step
                .get("element_source")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            spec_stop_on_failure: step.get("stop_on_failure").and_then(|v| v.as_bool()),
            spec_prefetched_elements: None,
            // Error resolved fields
            error_id: None,
            error_pattern: None,
            error_source: None,
            // Gate fields
            gate_required_steps: step
                .get("required_steps")
                .or_else(|| step.get("gateRequiredSteps"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                }),
            gate_stop_on_failure: step
                .get("stop_on_failure")
                .or_else(|| step.get("gateStopOnFailure"))
                .and_then(|v| v.as_bool()),
        })
    };

    // Add setup steps (mark as setup phase)
    for step in &workflow.setup_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.is_setup = Some(true);
            config.phase = Some("setup".to_string());
            all_steps.push(config);
        }
    }

    // Add verification steps
    for step in &workflow.verification_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("verification".to_string());
            all_steps.push(config);
        }
    }

    // Add agentic steps
    for step in &workflow.agentic_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("agentic".to_string());
            all_steps.push(config);
        }
    }

    // Add completion steps (mark as completion phase)
    for step in &workflow.completion_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("completion".to_string());
            all_steps.push(config);
        }
    }

    if all_steps.is_empty() {
        return Ok(Json(ApiResponse::success(
            crate::step_executor::ExecutionResult {
                success: true,
                total_steps: 0,
                successful_steps: 0,
                failed_steps: 0,
                total_duration_ms: 0,
                steps: vec![],
                captured_logs: None,
                captured_runner_logs: None,
                verification_passed: None,
                loop_result: None,
                task_summary: None,
            },
        )));
    }

    // Pre-fetch external elements for spec steps to avoid HTTP self-call deadlock
    // (same pattern as execute_inline_workflow)
    let needs_external_elements = all_steps
        .iter()
        .any(|s| s.step_type == "spec" && s.spec_element_source.as_deref() == Some("external"));

    let prefetched_elements = if needs_external_elements {
        info!("Pre-fetching external elements for spec steps (run endpoint)");
        match send_extension_command(
            state.clone(),
            "getElements",
            serde_json::json!({"includeNonInteractive": true}),
            10,
        )
        .await
        {
            Ok(response) => {
                let elements = response
                    .get("elements")
                    .cloned()
                    .unwrap_or(serde_json::json!([]));
                info!(
                    "Pre-fetched {} external elements",
                    elements.as_array().map(|a| a.len()).unwrap_or(0)
                );
                Some(elements)
            }
            Err(e) => {
                warn!("Failed to pre-fetch external elements: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Inject prefetched elements into spec steps.
    // When prefetch fails (None) but external specs exist, inject an empty array
    // to prevent the SpecHandler from making HTTP self-calls that can deadlock.
    // The React handler will return a clear "No external elements" error.
    if needs_external_elements {
        let elements_to_inject = prefetched_elements.unwrap_or_else(|| serde_json::json!([]));
        for step in &mut all_steps {
            if step.step_type == "spec" && step.spec_element_source.as_deref() == Some("external") {
                step.spec_prefetched_elements = Some(elements_to_inject.clone());
            }
        }
    }

    // Separate automation steps from AI steps using the robust categorize_steps helper.
    // This replaces the fragile string-based partition logic.
    let (automation_steps, prompt_steps) = categorize_steps(all_steps, |s| &s.step_type);
    let has_prompt_steps = !prompt_steps.is_empty();

    // Determine execution_id for resume support:
    // 1. If task_run_id is explicitly provided, use it (explicit resume)
    // 2. If force_fresh_start is false, check for incomplete task_run (auto-resume)
    // 3. Otherwise, generate a new execution_id
    let execution_id = if let Some(ref provided_id) = request.task_run_id {
        info!(
            "Using provided task_run_id for explicit resume: {}",
            provided_id
        );
        provided_id.clone()
    } else if !request.force_fresh_start {
        // Check for incomplete task_run to auto-resume
        match state
            .app_state
            .checkpoint_db
            .get_incomplete_task_run_for_workflow(&id)
        {
            Ok(Some(existing_id)) => {
                info!(
                    "Found incomplete task_run {} for workflow {} - auto-resuming",
                    existing_id, id
                );
                existing_id
            }
            Ok(None) => {
                // No incomplete run found, generate new execution_id
                let new_id = format!(
                    "unified-workflow-{}-{}",
                    id,
                    chrono::Utc::now().timestamp_millis()
                );
                info!("No incomplete task_run found, starting fresh: {}", new_id);
                new_id
            }
            Err(e) => {
                warn!(
                    "Failed to check for incomplete task_run: {} - starting fresh",
                    e
                );
                format!(
                    "unified-workflow-{}-{}",
                    id,
                    chrono::Utc::now().timestamp_millis()
                )
            }
        }
    } else {
        // force_fresh_start = true
        let new_id = format!(
            "unified-workflow-{}-{}",
            id,
            chrono::Utc::now().timestamp_millis()
        );
        info!("Force fresh start requested, new execution_id: {}", new_id);
        new_id
    };

    // If workflow has prompt steps, use AI-based execution
    if has_prompt_steps {
        info!(
            "Workflow '{}' has {} prompt steps - using AI-based execution",
            workflow.name,
            prompt_steps.len()
        );

        // Combine all prompt contents into a single prompt
        let combined_prompt = prompt_steps
            .iter()
            .filter_map(|s| s.prompt_content.as_ref())
            .map(|content| content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        if combined_prompt.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "Workflow has prompt steps but no prompt content".to_string(),
                )),
            ));
        }

        // =====================================================================
        // UNIFIED VERIFICATION-AGENTIC LOOP (required for all AI workflows)
        // =====================================================================
        // All AI workflows must have verification steps. The loop:
        // 1. Runs verification FIRST to tell the agentic phase what to work on
        // 2. Builds failure context from verification results
        // 3. Loops: verification -> agentic until pass or max_iterations
        // 4. Cannot be bypassed by AI claiming [TASK_COMPLETE]
        //
        // Verification steps can be:
        // - Automated tests (Playwright, shell commands)
        // - AI-based verification (prompt steps that check work quality)
        //
        // If you want a simple AI task, add a verification step like:
        // "Review the changes and verify the task was completed correctly"
        if workflow.verification_steps.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "AI workflows require at least one verification step. \
                     Verification can be automated (tests) or AI-based (a prompt that checks work quality). \
                     This ensures the AI's work is verified before marking the task complete.".to_string(),
                )),
            ));
        }

        info!(
            "Unified workflow '{}' has {} verification steps - using verification-agentic loop",
            workflow.name,
            workflow.verification_steps.len()
        );

        // Separate steps by phase for the new loop function
        // Setup automation steps (shell commands, workflows, etc.)
        let setup_automation_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        // Prepend pre-flight check if enabled (default: true)
        let setup_automation_steps = crate::unified_workflows::prepend_preflight_check_step(
            setup_automation_steps,
            workflow.preflight_check_enabled,
        );
        // Setup prompt steps (AI tasks during setup)
        let setup_prompt_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        let verification_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("verification"))
            .cloned()
            .collect();

        // Prepend health check steps if enabled and URLs configured
        // Health checks run BEFORE log_watch to catch server down before scanning logs
        let verification_steps = crate::unified_workflows::prepend_health_check_steps(
            verification_steps,
            workflow.health_check_enabled,
            &workflow.health_check_urls,
        );

        // Prepend log_watch step if enabled (default: true)
        let verification_steps = crate::unified_workflows::prepend_log_watch_step(
            verification_steps,
            workflow.log_watch_enabled,
        );

        // Warn if verification_steps is empty but workflow had verification steps
        // This can happen if all verification steps were prompt-type (which shouldn't happen)
        if verification_steps.is_empty() && !workflow.verification_steps.is_empty() {
            warn!(
                    "WARNING: workflow.verification_steps has {} items but extracted verification_steps is empty! \
                     This means all verification steps have step_type='prompt' which is not supported. \
                     Verification will auto-pass, causing completion to run immediately.",
                    workflow.verification_steps.len()
                );
        }

        // Filter prompt_steps by phase - agentic prompts only
        let agentic_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("agentic"))
            .cloned()
            .collect();
        // Completion steps: combine non-prompt completion steps with prompt completion steps
        let mut completion_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("completion"))
            .cloned()
            .collect();
        // Add completion prompts (e.g., AI summary) to completion_steps
        completion_steps.extend(
            prompt_steps
                .iter()
                .filter(|s| s.phase.as_deref() == Some("completion"))
                .cloned(),
        );

        // Create task_run for tracking with workflow_type="unified"
        // This prevents TaskMonitor and legacy session code from modifying status
        let execution_steps_json = serde_json::to_string(&automation_steps).ok();
        let mut input = CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&combined_prompt)
            .with_task_type("ai")
            .with_workflow_name(&workflow.name)
            .with_max_sessions(workflow.max_iterations)
            .with_auto_continue(true)
            .with_workflow_type("unified"); // LoopController is sole authority on status
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
            warn!(
                "Failed to create task_run for unified workflow {}: {}",
                execution_id, e
            );
        }

        // Separate completion steps into automation and prompt steps
        let (completion_automation_steps, completion_prompt_steps) =
            categorize_steps(completion_steps, |s| &s.step_type);

        // Run the verification-agentic loop using the new modular architecture
        // Timeout priority: request override > workflow setting > None (no timeout)
        let timeout_seconds = request.timeout_seconds.or(workflow.timeout_seconds);

        // For error-fix workflows, run agentic first
        let run_agentic_first = !workflow.targeted_error_ids.is_empty();

        let loop_config = crate::unified_workflow_executor::LoopConfig {
            max_iterations: workflow.max_iterations,
            timeout_seconds, // None = no timeout (default)
            base_prompt: combined_prompt,
            workflow_name: workflow.name.clone(),
            workflow_id: workflow.id.clone(),
            execution_id: execution_id.clone(),
            targeted_error_ids: workflow.targeted_error_ids.clone(),
            starting_iteration: 0, // Fresh start
            run_agentic_first,
        };

        let mut controller = crate::unified_workflow_executor::LoopController::new(
            state.app_state.clone(),
            state.config_storage.clone(),
            state.app_handle.clone(),
            state.current_ai_pids.clone(),
        );

        let result = controller
            .run(
                loop_config,
                setup_automation_steps,
                setup_prompt_steps,
                verification_steps,
                agentic_steps,
                completion_automation_steps,
                completion_prompt_steps,
            )
            .await;

        return Ok(Json(ApiResponse::success(result.to_execution_result())));
    }

    // No prompt steps - use step_executor for automation-only workflow
    // Create a task_run record so the workflow shows in the Active page
    // Serialize full step configuration so re-execution on resume has all fields
    let execution_steps_json = serde_json::to_string(&automation_steps).ok();

    // Create task_run to track this execution (enables Active page monitoring)
    let mut input = CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_task_type("automation") // identifies as automation task
        .with_workflow_name(&workflow.name); // helps identify this in the dashboard
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!(
            "Failed to create task_run for unified workflow {}: {}",
            execution_id, e
        );
    }

    // Create step executor
    let executor = crate::step_executor::StepExecutor::with_app_handle(
        state.app_state.clone(),
        state.config_storage.clone(),
        state.app_handle.clone(),
    );

    // Execute automation steps only (no prompt steps)
    let result = executor
        .execute_steps_with_log_sources(&automation_steps, &execution_id, &[])
        .await;

    info!(
        "Unified workflow '{}' completed: {} of {} steps succeeded",
        workflow.name, result.successful_steps, result.total_steps
    );

    // Update task_run status based on result
    if result.success {
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .complete_task_run(&execution_id)
        {
            warn!(
                "Failed to mark task_run {} as completed: {}",
                execution_id, e
            );
        }
    } else {
        let error_msg = result
            .steps
            .iter()
            .find(|s| !s.success)
            .and_then(|s| s.error.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("Unknown error");
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .fail_task_run(&execution_id, error_msg)
        {
            warn!("Failed to mark task_run {} as failed: {}", execution_id, e);
        }
    }

    Ok(Json(ApiResponse::success(result)))
}

/// Stores the last inline workflow request for re-execution via "Run Last Workflow".
/// Inline workflows aren't saved to the database, so this provides a way to re-run them.
static LAST_INLINE_WORKFLOW: std::sync::Mutex<Option<serde_json::Value>> =
    std::sync::Mutex::new(None);

/// Get the last inline workflow definition for re-execution
async fn get_last_inline_workflow(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let guard = LAST_INLINE_WORKFLOW
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(workflow) => Ok(Json(ApiResponse::success(workflow.clone()))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(
                "No inline workflow has been executed yet".to_string(),
            )),
        )),
    }
}

/// Execute an inline workflow without saving to the database
///
/// This endpoint is used by Quick Fix to run a generated workflow directly
/// without cluttering the workflow library. The workflow is executed with
/// a temporary ID and is not persisted.
async fn execute_inline_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteInlineWorkflowRequest>,
) -> Result<
    Json<ApiResponse<crate::step_executor::ExecutionResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    info!("Executing inline workflow: {}", request.name);

    // Check for duplicate running error-fix workflows
    // This prevents multiple Quick Fix workflows from targeting the same errors
    if request.name.contains("Fix") && request.name.contains("Error") {
        if let Ok(Some(existing_id)) = state
            .app_state
            .checkpoint_db
            .has_running_error_fix_workflow()
        {
            warn!(
                "Duplicate error-fix workflow prevented - already running: {}",
                existing_id
            );
            return Err((
                StatusCode::CONFLICT,
                Json(api_error(format!(
                    "An error-fix workflow is already running (task_id: {}). \
                     Please wait for it to complete or stop it before starting a new one.",
                    existing_id
                ))),
            ));
        }
    }

    // Store the request for re-execution via "Run Last Workflow" button
    if let Ok(request_json) = serde_json::to_value(&request) {
        let mut guard = LAST_INLINE_WORKFLOW
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(request_json);
    }

    // Create a temporary workflow object (not saved to DB)
    let execution_id = uuid::Uuid::new_v4().to_string();
    let workflow = crate::unified_workflows::UnifiedWorkflow {
        id: format!("inline-{}", execution_id),
        name: request.name.clone(),
        description: request.description,
        category: "error-fix".to_string(),
        tags: vec!["inline".to_string(), "quick-fix".to_string()],
        setup_steps: request.setup_steps,
        verification_steps: request.verification_steps,
        agentic_steps: request.agentic_steps,
        completion_steps: request.completion_steps,
        max_iterations: request.max_iterations,
        timeout_seconds: request.timeout_seconds,
        provider: None,
        model: None,
        skip_ai_summary: false,
        targeted_error_ids: request.targeted_error_ids,
        log_source_selection: Default::default(),
        context_ids: vec![],
        disabled_context_ids: vec![],
        auto_include_contexts: true,
        prompt_template: None,
        log_watch_enabled: true,
        health_check_enabled: false,
        health_check_urls: vec![],
        preflight_check_enabled: true,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    info!(
        "Inline workflow '{}' has {} setup, {} verification, {} agentic, {} completion steps",
        workflow.name,
        workflow.setup_steps.len(),
        workflow.verification_steps.len(),
        workflow.agentic_steps.len(),
        workflow.completion_steps.len()
    );

    // Save unified workflow config to .dev-logs for debugging (uses inline- prefix)
    if let Ok(workflow_json) = serde_json::to_value(&workflow) {
        crate::executor::file_logger::save_unified_workflow_config(&workflow_json, &workflow.name);
    }

    let monitor_index = request.monitor_index.unwrap_or(0);

    // Convert JSON steps to ExecutionStepConfig
    let mut all_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();

    // Helper to convert Value steps to ExecutionStepConfig
    let convert_step = |step: &serde_json::Value,
                        monitor: i32|
     -> Option<crate::step_executor::ExecutionStepConfig> {
        if let Ok(mut config) =
            serde_json::from_value::<crate::step_executor::ExecutionStepConfig>(step.clone())
        {
            if config.monitor_index.is_none() {
                config.monitor_index = Some(monitor);
            }
            if let Some(command) = step.get("command").and_then(|v| v.as_str()) {
                let cmd = command.to_string();
                match config.step_type.as_str() {
                    "shell_command" => {
                        if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                    }
                    "check" => {
                        if config.check_command.is_none() {
                            config.check_command = Some(cmd);
                        }
                    }
                    "test" => {
                        if config.shell_command.is_none() {
                            config.shell_command = Some(cmd);
                        }
                    }
                    _ => {}
                }
            }
            if config.step_type == "prompt" && config.prompt_content.is_none() {
                if let Some(content) = step.get("content").and_then(|v| v.as_str()) {
                    config.prompt_content = Some(content.to_string());
                }
            }
            return Some(config);
        }
        let step_type = step.get("type").and_then(|t| t.as_str())?;
        let name = step
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        Some(crate::step_executor::ExecutionStepConfig {
            step_type: step_type.to_string(),
            name,
            monitor_index: Some(monitor),
            ..Default::default()
        })
    };

    // Convert all phase steps with phase markers
    for step in &workflow.setup_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("setup".to_string());
            all_steps.push(config);
        }
    }
    for step in &workflow.verification_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("verification".to_string());
            all_steps.push(config);
        }
    }
    for step in &workflow.agentic_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("agentic".to_string());
            all_steps.push(config);
        }
    }
    for step in &workflow.completion_steps {
        if let Some(mut config) = convert_step(step, monitor_index) {
            config.phase = Some("completion".to_string());
            all_steps.push(config);
        }
    }

    info!(
        "Converted {} total steps for inline workflow",
        all_steps.len()
    );

    // Pre-fetch external elements for spec steps to avoid HTTP self-call deadlock
    // Check if any spec steps need external elements
    let needs_external_elements = all_steps
        .iter()
        .any(|s| s.step_type == "spec" && s.spec_element_source.as_deref() == Some("external"));

    let prefetched_elements = if needs_external_elements {
        info!("Pre-fetching external elements for spec steps");
        match send_extension_command(
            state.clone(),
            "getElements",
            serde_json::json!({"includeNonInteractive": true}),
            10,
        )
        .await
        {
            Ok(response) => {
                let elements = response
                    .get("elements")
                    .cloned()
                    .unwrap_or(serde_json::json!([]));
                info!(
                    "Pre-fetched {} external elements",
                    elements.as_array().map(|a| a.len()).unwrap_or(0)
                );
                Some(elements)
            }
            Err(e) => {
                warn!("Failed to pre-fetch external elements: {}", e);
                None
            }
        }
    } else {
        None
    };

    // Inject prefetched elements into spec steps
    if let Some(ref elements) = prefetched_elements {
        for step in &mut all_steps {
            if step.step_type == "spec" && step.spec_element_source.as_deref() == Some("external") {
                step.spec_prefetched_elements = Some(elements.clone());
            }
        }
    }

    // Separate steps by type
    let (automation_steps, prompt_steps) = categorize_steps(all_steps, |s| &s.step_type);

    // If there are prompt steps, use the verification-agentic loop
    if !prompt_steps.is_empty() {
        let combined_prompt = prompt_steps
            .iter()
            .filter_map(|s| s.prompt_content.as_ref())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        if combined_prompt.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(api_error(
                    "Workflow has prompt steps but no prompt content".to_string(),
                )),
            ));
        }

        // Separate steps by phase
        let setup_automation_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        // Prepend pre-flight check if enabled (default: true)
        let setup_automation_steps = crate::unified_workflows::prepend_preflight_check_step(
            setup_automation_steps,
            workflow.preflight_check_enabled,
        );
        let setup_prompt_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("setup"))
            .cloned()
            .collect();
        let verification_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("verification"))
            .cloned()
            .collect();

        // Prepend log_watch step
        let verification_steps = crate::unified_workflows::prepend_log_watch_step(
            verification_steps,
            workflow.log_watch_enabled,
        );

        let agentic_steps: Vec<_> = prompt_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("agentic"))
            .cloned()
            .collect();
        let mut completion_steps: Vec<_> = automation_steps
            .iter()
            .filter(|s| s.phase.as_deref() == Some("completion"))
            .cloned()
            .collect();
        completion_steps.extend(
            prompt_steps
                .iter()
                .filter(|s| s.phase.as_deref() == Some("completion"))
                .cloned(),
        );

        // Create task_run for tracking (marked as inline/temporary)
        let execution_steps_json = serde_json::to_string(&automation_steps).ok();
        let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
            .with_prompt(&combined_prompt)
            .with_task_type("ai")
            .with_workflow_name(format!("[Inline] {}", workflow.name))
            .with_max_sessions(workflow.max_iterations)
            .with_auto_continue(true)
            .with_workflow_type("unified");
        if let Some(esj) = execution_steps_json {
            input = input.with_execution_steps_json(esj);
        }
        if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
            warn!(
                "Failed to create task_run for inline workflow {}: {}",
                execution_id, e
            );
        }

        let (completion_automation_steps, completion_prompt_steps) =
            categorize_steps(completion_steps, |s| &s.step_type);

        // For error-fix workflows (with targeted_error_ids), run agentic first.
        // This ensures the AI attempts to fix errors before verification runs,
        // since log_watch verification may pass immediately if logs are currently clean.
        let run_agentic_first = !workflow.targeted_error_ids.is_empty();

        let loop_config = crate::unified_workflow_executor::LoopConfig {
            max_iterations: workflow.max_iterations,
            timeout_seconds: request.timeout_seconds,
            base_prompt: combined_prompt,
            workflow_name: workflow.name.clone(),
            workflow_id: workflow.id.clone(),
            execution_id: execution_id.clone(),
            targeted_error_ids: workflow.targeted_error_ids.clone(),
            starting_iteration: 0,
            run_agentic_first,
        };

        let mut controller = crate::unified_workflow_executor::LoopController::new(
            state.app_state.clone(),
            state.config_storage.clone(),
            state.app_handle.clone(),
            state.current_ai_pids.clone(),
        );

        let result = controller
            .run(
                loop_config,
                setup_automation_steps,
                setup_prompt_steps,
                verification_steps,
                agentic_steps,
                completion_automation_steps,
                completion_prompt_steps,
            )
            .await;

        return Ok(Json(ApiResponse::success(result.to_execution_result())));
    }

    // No prompt steps - use step_executor for automation-only workflow
    let execution_steps_json = serde_json::to_string(&automation_steps).ok();
    let mut input = crate::database::CreateTaskRunInput::new(&execution_id, &workflow.name)
        .with_task_type("automation")
        .with_workflow_name(format!("[Inline] {}", workflow.name));
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!(
            "Failed to create task_run for inline workflow {}: {}",
            execution_id, e
        );
    }

    let executor = crate::step_executor::StepExecutor::with_app_handle(
        state.app_state.clone(),
        state.config_storage.clone(),
        state.app_handle.clone(),
    );

    let result = executor
        .execute_steps_with_log_sources(&automation_steps, &execution_id, &[])
        .await;

    info!(
        "Inline workflow '{}' completed: {} of {} steps succeeded",
        workflow.name, result.successful_steps, result.total_steps
    );

    // Update task_run status
    if result.success {
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .complete_task_run(&execution_id)
        {
            warn!(
                "Failed to mark task_run {} as completed: {}",
                execution_id, e
            );
        }
    } else {
        let error_msg = result
            .steps
            .iter()
            .find(|s| !s.success)
            .and_then(|s| s.error.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("Unknown error");
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .fail_task_run(&execution_id, error_msg)
        {
            warn!("Failed to mark task_run {} as failed: {}", execution_id, e);
        }
    }

    Ok(Json(ApiResponse::success(result)))
}

/// Workflow execution statistics
#[derive(Serialize)]
struct WorkflowStats {
    #[serde(rename = "totalRuns")]
    total_runs: u32,
    #[serde(rename = "successCount")]
    success_count: u32,
    #[serde(rename = "failureCount")]
    failure_count: u32,
    #[serde(rename = "lastRunAt")]
    last_run_at: Option<String>,
    #[serde(rename = "lastRunStatus")]
    last_run_status: Option<String>,
    #[serde(rename = "avgDurationMs")]
    avg_duration_ms: Option<i64>,
}

/// Get execution statistics for a unified workflow
async fn get_unified_workflow_stats(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<WorkflowStats>>, (StatusCode, Json<ApiResponse<()>>)> {
    // First verify the workflow exists
    let workflow = match state.app_state.checkpoint_db.get_unified_workflow(&id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Unified workflow not found: {}", id))),
            ));
        }
        Err(e) => {
            error!("Failed to get unified workflow: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get unified workflow: {}", e))),
            ));
        }
    };

    // Query stats from task_runs table by workflow_name
    let conn = match state.app_state.checkpoint_db.connection() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to get database connection: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!(
                    "Failed to get database connection: {}",
                    e
                ))),
            ));
        }
    };

    let stats_result: Result<WorkflowStats, rusqlite::Error> = conn.query_row(
        r#"
        SELECT
            COUNT(*) as total_runs,
            SUM(CASE WHEN status = 'complete' THEN 1 ELSE 0 END) as success_count,
            SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failure_count,
            MAX(created_at) as last_run_at,
            (SELECT status FROM task_runs WHERE workflow_name = ?1
             ORDER BY created_at DESC LIMIT 1) as last_run_status,
            AVG(CASE WHEN completed_at IS NOT NULL
                THEN (julianday(completed_at) - julianday(created_at)) * 86400000
                END) as avg_duration_ms
        FROM task_runs
        WHERE workflow_name = ?1
        "#,
        [&workflow.name],
        |row| {
            Ok(WorkflowStats {
                total_runs: row.get::<_, i64>(0)? as u32,
                success_count: row.get::<_, i64>(1)? as u32,
                failure_count: row.get::<_, i64>(2)? as u32,
                last_run_at: row.get(3)?,
                last_run_status: row.get(4)?,
                // AVG() returns a float, so we need to convert it to i64
                avg_duration_ms: row.get::<_, Option<f64>>(5)?.map(|f| f as i64),
            })
        },
    );

    match stats_result {
        Ok(stats) => Ok(Json(ApiResponse::success(stats))),
        Err(e) => {
            // If no rows found, return empty stats
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                Ok(Json(ApiResponse::success(WorkflowStats {
                    total_runs: 0,
                    success_count: 0,
                    failure_count: 0,
                    last_run_at: None,
                    last_run_status: None,
                    avg_duration_ms: None,
                })))
            } else {
                error!("Failed to query workflow stats: {}", e);
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to query workflow stats: {}", e))),
                ))
            }
        }
    }
}

/// Request body for running a sequence of workflows
#[derive(Deserialize)]
struct RunWorkflowSequenceRequest {
    workflow_ids: Vec<String>,
    #[serde(default = "default_stop_on_failure")]
    stop_on_failure: bool,
}

fn default_stop_on_failure() -> bool {
    true
}

/// Response for workflow sequence execution
#[derive(Serialize)]
struct WorkflowSequenceResponse {
    task_run_id: String,
    workflow_count: usize,
    workflow_names: Vec<String>,
}

/// Run a sequence of unified workflows
async fn run_workflow_sequence(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowSequenceRequest>,
) -> Result<Json<ApiResponse<WorkflowSequenceResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    if request.workflow_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("workflow_ids cannot be empty".to_string())),
        ));
    }

    info!(
        "Running workflow sequence: {} workflows, stop_on_failure={}",
        request.workflow_ids.len(),
        request.stop_on_failure
    );

    // Fetch all workflows and validate they exist
    let mut workflows: Vec<crate::unified_workflows::UnifiedWorkflow> = Vec::new();
    for id in &request.workflow_ids {
        match state.app_state.checkpoint_db.get_unified_workflow(id) {
            Ok(Some(w)) => workflows.push(w),
            Ok(None) => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(api_error(format!("Workflow not found: {}", id))),
                ));
            }
            Err(e) => {
                error!("Failed to get workflow {}: {}", id, e);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(api_error(format!("Failed to get workflow: {}", e))),
                ));
            }
        }
    }

    let workflow_names: Vec<String> = workflows.iter().map(|w| w.name.clone()).collect();
    let sequence_name = if workflows.len() == 1 {
        workflows[0].name.clone()
    } else {
        format!(
            "Sequence: {} + {} more",
            workflows[0].name,
            workflows.len() - 1
        )
    };

    // Create a combined prompt describing the sequence
    let sequence_description = workflows
        .iter()
        .enumerate()
        .map(|(i, w)| format!("{}. {} - {}", i + 1, w.name, w.description))
        .collect::<Vec<_>>()
        .join("\n");

    let combined_prompt = format!(
        "Execute the following workflow sequence{}:\n\n{}\n\nTotal workflows: {}",
        if request.stop_on_failure {
            " (stopping on first failure)"
        } else {
            ""
        },
        sequence_description,
        workflows.len()
    );

    // Create a single task run for the sequence
    let execution_id = format!(
        "workflow-sequence-{}",
        chrono::Utc::now().timestamp_millis()
    );

    // Build combined steps from all workflows
    let mut all_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();
    let monitor_index = 0i32;

    for (workflow_idx, workflow) in workflows.iter().enumerate() {
        info!(
            "Adding workflow {}/{}: {} ({} setup, {} verification, {} agentic, {} completion steps)",
            workflow_idx + 1,
            workflows.len(),
            workflow.name,
            workflow.setup_steps.len(),
            workflow.verification_steps.len(),
            workflow.agentic_steps.len(),
            workflow.completion_steps.len()
        );

        // Helper to convert Value steps to ExecutionStepConfig (same as run_unified_workflow)
        let convert_step = |step: &serde_json::Value,
                            monitor: i32|
         -> Option<crate::step_executor::ExecutionStepConfig> {
            if let Ok(mut config) =
                serde_json::from_value::<crate::step_executor::ExecutionStepConfig>(step.clone())
            {
                if config.monitor_index.is_none() {
                    config.monitor_index = Some(monitor);
                }
                return Some(config);
            }

            let step_type = step.get("type").and_then(|t| t.as_str())?;
            let name = step
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());

            Some(crate::step_executor::ExecutionStepConfig {
                step_type: step_type.to_string(),
                name,
                monitor_index: Some(monitor),
                ..Default::default()
            })
        };

        // Add steps from each phase
        for step in &workflow.setup_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("setup".to_string());
                all_steps.push(config);
            }
        }

        for step in &workflow.verification_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("verification".to_string());
                all_steps.push(config);
            }
        }

        for step in &workflow.agentic_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("agentic".to_string());
                all_steps.push(config);
            }
        }

        for step in &workflow.completion_steps {
            if let Some(mut config) = convert_step(step, monitor_index) {
                config.phase = Some("completion".to_string());
                all_steps.push(config);
            }
        }
    }

    // Create task run for tracking
    let execution_steps_json = serde_json::to_string(&all_steps).ok();
    let mut input = CreateTaskRunInput::new(&execution_id, &sequence_name)
        .with_prompt(&combined_prompt)
        .with_task_type("ai")
        .with_workflow_name(workflow_names.join(", "))
        .with_auto_continue(true)
        .with_workflow_type("unified");
    if let Some(esj) = execution_steps_json {
        input = input.with_execution_steps_json(esj);
    }
    if let Err(e) = state.app_state.checkpoint_db.create_task_run(&input) {
        warn!("Failed to create task_run for sequence: {}", e);
    }

    // Capture values for response before moving workflows into the async block
    let workflow_count = workflows.len();
    let workflow_names_response = workflow_names.clone();

    // Spawn background task to execute workflow sequence with panic protection
    let state_clone = state.clone();
    let execution_id_clone = execution_id.clone();
    let stop_on_failure = request.stop_on_failure;
    let checkpoint_db_for_guard = state.app_state.checkpoint_db.clone();
    let sequence_name_for_guard = format!("Workflow Sequence ({} workflows)", workflow_count);
    let execution_id_for_guard = execution_id.clone();

    // Use panic-safe spawning to ensure task is marked as failed if sequence panics
    crate::unified_workflow_executor::spawn_sequence_with_panic_guard(
        checkpoint_db_for_guard,
        execution_id_for_guard,
        sequence_name_for_guard,
        async move {
            info!(
                "Starting workflow sequence execution: {} workflows",
                workflow_count
            );

            let mut controller = crate::unified_workflow_executor::LoopController::new(
                state_clone.app_state.clone(),
                state_clone.config_storage.clone(),
                state_clone.app_handle.clone(),
                state_clone.current_ai_pids.clone(),
            );

            let mut all_results: Vec<crate::step_executor::StepExecutionResult> = Vec::new();
            let mut sequence_success = true;
            let mut failed_workflow: Option<String> = None;

            for (idx, workflow) in workflows.iter().enumerate() {
                info!(
                    "=== Executing workflow {}/{}: {} ===",
                    idx + 1,
                    workflow_count,
                    workflow.name
                );

                // Convert workflow steps to ExecutionStepConfig
                let monitor_index = 0i32;
                let convert_step =
                    |step: &serde_json::Value,
                     monitor: i32|
                     -> Option<crate::step_executor::ExecutionStepConfig> {
                        if let Ok(mut config) = serde_json::from_value::<
                            crate::step_executor::ExecutionStepConfig,
                        >(step.clone())
                        {
                            if config.monitor_index.is_none() {
                                config.monitor_index = Some(monitor);
                            }
                            return Some(config);
                        }

                        let step_type = step.get("type").and_then(|t| t.as_str())?;
                        let name = step
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string());

                        Some(crate::step_executor::ExecutionStepConfig {
                            step_type: step_type.to_string(),
                            name,
                            monitor_index: Some(monitor),
                            ..Default::default()
                        })
                    };

                // Collect all steps with phases
                let mut workflow_steps: Vec<crate::step_executor::ExecutionStepConfig> = Vec::new();

                for step in &workflow.setup_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("setup".to_string());
                        workflow_steps.push(config);
                    }
                }
                for step in &workflow.verification_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("verification".to_string());
                        workflow_steps.push(config);
                    }
                }
                for step in &workflow.agentic_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("agentic".to_string());
                        workflow_steps.push(config);
                    }
                }
                for step in &workflow.completion_steps {
                    if let Some(mut config) = convert_step(step, monitor_index) {
                        config.phase = Some("completion".to_string());
                        workflow_steps.push(config);
                    }
                }

                // Separate automation from prompt steps
                let (automation_steps, prompt_steps) =
                    categorize_steps(workflow_steps, |s| &s.step_type);

                // Check if this is an AI workflow (has prompt steps)
                let has_prompt_steps = !prompt_steps.is_empty();

                if has_prompt_steps {
                    // Separate by phase
                    let setup_automation: Vec<_> = automation_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("setup"))
                        .cloned()
                        .collect();
                    // Prepend pre-flight check if enabled (default: true)
                    let setup_automation = crate::unified_workflows::prepend_preflight_check_step(
                        setup_automation,
                        workflow.preflight_check_enabled,
                    );
                    let setup_prompts: Vec<_> = prompt_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("setup"))
                        .cloned()
                        .collect();
                    let verification: Vec<_> = automation_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("verification"))
                        .cloned()
                        .collect();
                    let agentic: Vec<_> = prompt_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("agentic"))
                        .cloned()
                        .collect();
                    let completion_automation: Vec<_> = automation_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("completion"))
                        .cloned()
                        .collect();
                    let completion_prompts: Vec<_> = prompt_steps
                        .iter()
                        .filter(|s| s.phase.as_deref() == Some("completion"))
                        .cloned()
                        .collect();

                    // Build prompt content
                    let prompt_content = prompt_steps
                        .iter()
                        .filter_map(|s| s.prompt_content.as_ref())
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n---\n\n");

                    // Create workflow-specific execution ID for internal tracking
                    // Note: We don't create a separate task_run row for each workflow in a sequence
                    // The parent sequence task_run is sufficient for tracking. Creating child task_runs
                    // caused duplicate entries to appear in the running tasks list.
                    let workflow_exec_id = format!("{}-workflow-{}", execution_id_clone, idx + 1);

                    // For error-fix workflows, run agentic first
                    let run_agentic_first = !workflow.targeted_error_ids.is_empty();

                    let loop_config = crate::unified_workflow_executor::LoopConfig {
                        max_iterations: workflow.max_iterations,
                        timeout_seconds: workflow.timeout_seconds, // Use workflow setting
                        base_prompt: prompt_content,
                        workflow_name: workflow.name.clone(),
                        workflow_id: workflow.id.clone(),
                        execution_id: workflow_exec_id,
                        targeted_error_ids: workflow.targeted_error_ids.clone(),
                        starting_iteration: 0, // Fresh start
                        run_agentic_first,
                    };

                    let result = controller
                        .run(
                            loop_config,
                            setup_automation,
                            setup_prompts,
                            verification,
                            agentic,
                            completion_automation,
                            completion_prompts,
                        )
                        .await;

                    all_results.extend(result.step_results);

                    if !result.success {
                        sequence_success = false;
                        failed_workflow = Some(workflow.name.clone());
                        error!("Workflow '{}' failed in sequence", workflow.name);

                        if stop_on_failure {
                            info!(
                                "Stopping sequence due to workflow failure (stop_on_failure=true)"
                            );
                            break;
                        }
                    } else {
                        info!("Workflow '{}' completed successfully", workflow.name);
                    }
                } else {
                    // Automation-only workflow - use StepExecutor
                    let executor = crate::step_executor::StepExecutor::with_app_handle(
                        state_clone.app_state.clone(),
                        state_clone.config_storage.clone(),
                        state_clone.app_handle.clone(),
                    );

                    let workflow_exec_id = format!("{}-workflow-{}", execution_id_clone, idx + 1);

                    let result = executor
                        .execute_steps_with_log_sources(&automation_steps, &workflow_exec_id, &[])
                        .await;

                    all_results.extend(result.steps);

                    if !result.success {
                        sequence_success = false;
                        failed_workflow = Some(workflow.name.clone());
                        error!("Automation workflow '{}' failed in sequence", workflow.name);

                        if stop_on_failure {
                            info!(
                                "Stopping sequence due to workflow failure (stop_on_failure=true)"
                            );
                            break;
                        }
                    } else {
                        info!(
                            "Automation workflow '{}' completed successfully",
                            workflow.name
                        );
                    }
                }
            }

            // Update task_run status
            if sequence_success {
                info!("Workflow sequence completed successfully");
                let _ = state_clone
                    .app_state
                    .checkpoint_db
                    .complete_task_run(&execution_id_clone);
            } else {
                let error_msg = match failed_workflow {
                    Some(name) => format!("Workflow '{}' failed", name),
                    None => "Sequence failed".to_string(),
                };
                error!("Workflow sequence failed: {}", error_msg);
                let _ = state_clone
                    .app_state
                    .checkpoint_db
                    .fail_task_run(&execution_id_clone, &error_msg);
            }
        },
    );

    Ok(Json(ApiResponse::success(WorkflowSequenceResponse {
        task_run_id: execution_id,
        workflow_count,
        workflow_names: workflow_names_response,
    })))
}

// ============================================================================
// End Unified Workflows HTTP API Handlers
// ============================================================================

// ============================================================================
// Check Generation HTTP API Handlers
// ============================================================================

/// Scan a workspace for projects and their configuration
async fn scan_workspace_handler(
    Json(request): Json<crate::check_generation::ScanWorkspaceRequest>,
) -> Result<
    Json<ApiResponse<crate::check_generation::WorkspaceScanResult>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    use crate::check_generation::scan_workspace;

    info!(
        "Scanning workspace: {} (max_depth: {})",
        request.base_directory, request.max_depth
    );

    // Run scan in blocking task since it involves file I/O
    let result = tokio::task::spawn_blocking(move || scan_workspace(&request))
        .await
        .map_err(|e| {
            error!("Workspace scan task failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Scan task failed: {}", e))),
            )
        })?;

    match result {
        Ok(scan_result) => {
            info!(
                "Workspace scan complete: {} projects found in {} directories",
                scan_result.projects.len(),
                scan_result.total_directories_scanned
            );
            Ok(Json(ApiResponse::success(scan_result)))
        }
        Err(e) => {
            error!("Workspace scan failed: {}", e);
            Err((StatusCode::BAD_REQUEST, Json(api_error(e))))
        }
    }
}

/// Generate check suggestions using AI based on workspace scan results
async fn generate_checks_handler(
    Json(request): Json<crate::check_generation::GenerateChecksRequest>,
) -> Result<
    Json<ApiResponse<crate::check_generation::GenerateChecksResponse>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    use crate::check_generation::generate_checks;

    info!(
        "Generating checks for {} projects",
        request.workspace_scan.projects.len()
    );

    // Run generation in blocking task since it may call AI
    let result = tokio::task::spawn_blocking(move || generate_checks(&request))
        .await
        .map_err(|e| {
            error!("Check generation task failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Generation task failed: {}", e))),
            )
        })?;

    info!(
        "Check generation complete: {} suggestions, success={}",
        result.suggested_checks.len(),
        result.success
    );

    Ok(Json(ApiResponse::success(result)))
}

/// Repair check-group associations based on naming convention
///
/// Checks are named with format "{group_name} - {tool_name}" (e.g., "multistate - Ruff Linting").
/// This endpoint finds checks that match groups by this pattern and ensures they are linked.
async fn repair_check_associations_handler(
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("Repairing check-group associations via MCP API");

    let db = CheckpointDb::new().map_err(|e| {
        error!("Failed to open database: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to open database: {}", e))),
        )
    })?;

    match db.repair_check_group_associations() {
        Ok(count) => {
            let message = if count > 0 {
                format!("Repaired {} check-group associations", count)
            } else {
                "All check-group associations are already correct".to_string()
            };
            info!("{}", message);
            Ok(Json(ApiResponse::success(serde_json::json!({
                "message": message,
                "associations_created": count
            }))))
        }
        Err(e) => {
            error!("Failed to repair associations: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to repair associations: {}", e))),
            ))
        }
    }
}

// ============================================================================
// End Check Generation HTTP API Handlers
// ============================================================================

// ============================================================================
// Recording & Playback HTTP API Handlers
// ============================================================================

use crate::recording::{
    AddActionInput, CreateRecordingInput, ExportFormat, ExportOptions, RecordedAction, Recording,
    RecordingStatus, RecordingStorage, ScriptGenerator,
};

/// List all recordings
async fn list_recordings_handler(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<Recording>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    let status_filter = params
        .get("status")
        .and_then(|s| s.parse::<RecordingStatus>().ok());
    let limit = params.get("limit").and_then(|l| l.parse::<i32>().ok());

    match storage.list_recordings(status_filter, limit) {
        Ok(recordings) => Ok(Json(ApiResponse::success(recordings))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to list recordings: {}", e))),
        )),
    }
}

/// Create a new recording
async fn create_recording_handler(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<CreateRecordingInput>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.create_recording(input) {
        Ok(recording) => {
            info!("Created recording: {} ({})", recording.name, recording.id);
            Ok(Json(ApiResponse::success(recording)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create recording: {}", e))),
        )),
    }
}

/// Get a specific recording
async fn get_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.get_recording(&id) {
        Ok(Some(recording)) => Ok(Json(ApiResponse::success(recording))),
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Recording not found: {}", id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get recording: {}", e))),
        )),
    }
}

/// Delete a recording
async fn delete_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<()>> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.delete_recording(&id) {
        Ok(()) => {
            info!("Deleted recording: {}", id);
            Json(ApiResponse::success(()))
        }
        Err(e) => Json(api_error(format!("Failed to delete recording: {}", e))),
    }
}

/// Get actions for a recording
async fn get_recording_actions_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<Vec<RecordedAction>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.get_recording_actions(&id) {
        Ok(actions) => Ok(Json(ApiResponse::success(actions))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get recording actions: {}", e))),
        )),
    }
}

/// Add an action to a recording
async fn add_recording_action_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<AddActionInput>,
) -> Result<Json<ApiResponse<RecordedAction>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    // Verify recording exists and is in recording status
    match storage.get_recording(&id) {
        Ok(Some(recording)) => {
            if recording.status != RecordingStatus::Recording {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(api_error(format!(
                        "Cannot add actions to recording with status: {}",
                        recording.status
                    ))),
                ));
            }
        }
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Recording not found: {}", id))),
            ));
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get recording: {}", e))),
            ));
        }
    }

    match storage.add_action(&id, input) {
        Ok(action) => {
            debug!("Added action to recording {}: {:?}", id, action.action_type);
            Ok(Json(ApiResponse::success(action)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to add action: {}", e))),
        )),
    }
}

/// Update recording status
#[derive(Debug, Deserialize)]
struct UpdateRecordingStatusInput {
    status: RecordingStatus,
}

async fn update_recording_status_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateRecordingStatusInput>,
) -> Result<Json<ApiResponse<Recording>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.update_recording_status(&id, input.status) {
        Ok(recording) => {
            info!("Updated recording {} status to: {}", id, input.status);
            Ok(Json(ApiResponse::success(recording)))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to update recording status: {}",
                e
            ))),
        )),
    }
}

/// Export a recording to script
async fn export_recording_handler(
    State(state): State<Arc<ApiState>>,
    Path((id, format)): Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    // Parse export format
    let export_format: ExportFormat = format.parse().map_err(|e: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid export format: {}", e))),
        )
    })?;

    // Get recording
    let recording = storage
        .get_recording(&id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to get recording: {}", e))),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Recording not found: {}", id))),
            )
        })?;

    // Get actions
    let actions = storage.get_recording_actions(&id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get actions: {}", e))),
        )
    })?;

    // Build export options from query params
    let options = ExportOptions {
        wait_strategy: params
            .get("wait_strategy")
            .cloned()
            .unwrap_or_else(|| "networkidle".to_string()),
        fixed_wait_ms: params
            .get("fixed_wait_ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000),
        selector_priority: params
            .get("selector_priority")
            .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
            .unwrap_or_else(|| vec!["ui_id".to_string(), "css".to_string(), "xpath".to_string()]),
        include_visibility_assertions: params
            .get("include_visibility_assertions")
            .map(|s| s == "true")
            .unwrap_or(false),
        include_timing_assertions: params
            .get("include_timing_assertions")
            .map(|s| s == "true")
            .unwrap_or(false),
        test_name: params.get("test_name").cloned(),
        test_description: params.get("test_description").cloned(),
    };

    // Generate script
    let script_content = ScriptGenerator::generate(&recording, &actions, export_format, &options)
        .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to generate script: {}", e))),
        )
    })?;

    let file_name = ScriptGenerator::default_file_name(&recording, export_format);

    // Save export record
    let export = storage
        .save_export(
            &id,
            export_format,
            &script_content,
            &file_name,
            Some(&options),
        )
        .map_err(|e| {
            warn!("Failed to save export record: {}", e);
            // Continue even if save fails
        })
        .ok();

    info!("Exported recording {} to {} format", id, export_format);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "recording_id": id,
        "format": export_format.to_string(),
        "file_name": file_name,
        "script_content": script_content,
        "export_id": export.map(|e| e.id),
    }))))
}

/// Get exports for a recording
async fn get_recording_exports_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<
    Json<ApiResponse<Vec<crate::recording::RecordingExport>>>,
    (StatusCode, Json<ApiResponse<()>>),
> {
    let storage = RecordingStorage::new(state.app_state.checkpoint_db.clone());

    match storage.get_recording_exports(&id) {
        Ok(exports) => Ok(Json(ApiResponse::success(exports))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get recording exports: {}", e))),
        )),
    }
}

// ============================================================================
// End Recording & Playback HTTP API Handlers
// ============================================================================

/// Create the API router
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
) -> Router {
    // Get dev_logs path for session manager
    let dev_logs_path = get_workspace_paths_internal()
        .map(|(_, dev_logs, _)| dev_logs)
        .unwrap_or_else(|_| std::path::PathBuf::from(".dev-logs"));

    // Ensure dev_logs directory exists
    let _ = std::fs::create_dir_all(&dev_logs_path);

    // Initialize config storage (graceful degradation if directory creation fails)
    let config_storage = match ConfigStorage::new() {
        Ok(storage) => {
            info!("Config storage initialized successfully");
            Arc::new(tokio::sync::Mutex::new(storage))
        }
        Err(e) => {
            warn!(
                "Config storage initialization failed (non-fatal): {}. Using degraded mode.",
                e
            );
            Arc::new(tokio::sync::Mutex::new(ConfigStorage::new_degraded()))
        }
    };

    // Create UnifiedActionService for deterministic execution
    let action_service = Arc::new(UnifiedActionService::new(
        app_state.clone(),
        config_storage.clone(),
    ));

    let api_state = Arc::new(ApiState {
        app_state,
        rag_state,
        app_handle: app_handle.clone(),
        current_config_id: std::sync::Mutex::new(None),
        config_storage,
        action_service,
        current_ai_pids: Arc::new(std::sync::Mutex::new(Vec::new())),
        orchestrator_states: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        extraction_state: Arc::new(ExtractionState::new()),
        resuming_task_ids: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        ui_bridge_pending: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        extension_ws_sender: Arc::new(tokio::sync::Mutex::new(None)),
        extension_pending_requests: Arc::new(tokio::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        extension_connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        extension_last_pong: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        extension_connected_since: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        extension_reconnect_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    });

    // Set up UI Bridge response listener
    // This listens for "ui-bridge-response" events from the React frontend
    // and delivers responses to waiting HTTP handlers
    {
        let pending = api_state.ui_bridge_pending.clone();
        let handle = app_handle.clone();

        // We need to use tauri's listen which returns a sync result
        // The listener callback will be called on the main thread

        use tauri::Listener;

        let pending_for_listener = pending.clone();
        let _listener_id = handle.listen("ui-bridge-response", move |event| {
            let pending = pending_for_listener.clone();

            // Parse the response payload
            if let Ok(response) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                // Spawn a task to handle the response since we need async
                let runtime = tokio::runtime::Handle::try_current();
                if let Ok(rt) = runtime {
                    rt.spawn(async move {
                        handle_ui_bridge_response(pending, response).await;
                    });
                } else {
                    warn!("UI Bridge: No tokio runtime available for response handling");
                }
            } else {
                warn!(
                    "UI Bridge: Failed to parse response payload: {}",
                    event.payload()
                );
            }
        });
        info!("UI Bridge: Response listener set up");
    }

    // Set up Spec Execution response listener
    // This listens for "spec-execute-response" events from the React frontend
    // and delivers responses to waiting spec step handlers
    {
        let handle = app_handle.clone();

        use tauri::Listener;

        let _listener_id = handle.listen("spec-execute-response", move |event| {
            // Parse the response payload
            if let Ok(response) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                // Spawn a task to handle the response since we need async
                let runtime = tokio::runtime::Handle::try_current();
                if let Ok(rt) = runtime {
                    rt.spawn(async move {
                        crate::step_executor::handlers::spec::handle_spec_execute_response(
                            response,
                        )
                        .await;
                    });
                } else {
                    warn!("Spec Execute: No tokio runtime available for response handling");
                }
            } else {
                warn!(
                    "Spec Execute: Failed to parse response payload: {}",
                    event.payload()
                );
            }
        });
        info!("Spec Execute: Response listener set up");
    }

    // Resume interrupted unified workflows on startup
    let state_for_resume = api_state.clone();
    let resume_config_storage = api_state.config_storage.clone();
    let resume_pid_tracker = api_state.current_ai_pids.clone();
    tokio::spawn(async move {
        // Small delay to let the server fully start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Note: We no longer use global_auto_continue here.
        // Each workflow's per-task auto_continue setting determines whether it gets resumed.
        // The global setting is now only used for the UI toggle, not startup resume logic.

        // Log to debug file
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crate::paths::get_workflow_debug_log_path())
        {
            use std::io::Write;
            let _ = writeln!(
                f,
                "[{}] STARTUP_RESUME_CHECK: Processing interrupted workflows (per-task auto_continue)",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
            );
        }

        // Process interrupted workflows - each workflow's per-task auto_continue setting
        // determines whether it gets resumed or marked as failed
        let resume_config = crate::unified_workflow_executor::ResumeConfig {
            resume_enabled: true, // Let the function check per-task auto_continue
        };

        let count = crate::unified_workflow_executor::resume_interrupted_workflows(
            state_for_resume.app_state.checkpoint_db.clone(),
            state_for_resume.app_state.clone(),
            resume_config_storage,
            state_for_resume.app_handle.clone(),
            resume_pid_tracker,
            resume_config,
        )
        .await;

        if count > 0 {
            info!(
                "Processed {} interrupted unified workflow(s) on startup",
                count
            );
        }
    });

    // Configure CORS to allow requests from WSL
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // WebSocket endpoint for live execution event streaming
        .route("/ws/events", get(ws_events_handler))
        // WebSocket endpoint for Chrome extension connection (UI Bridge exploration)
        .route("/ws/extension", get(ws_extension_handler))
        // SSE endpoint for MCP notification streaming (alternative to WebSocket)
        .route("/sse/events", get(sse_events_handler))
        // Chrome extension status and command endpoints (for Python bridge)
        .route("/extension/status", get(get_extension_status))
        .route("/extension/command", post(send_extension_command_handler))
        .route("/health", get(health))
        // Bridge management endpoints (multi-bridge support)
        .route("/bridges", get(list_bridges).post(create_bridge))
        .route("/bridges/:bridge_id", get(get_bridge).delete(remove_bridge))
        .route("/bridges/:bridge_id/workflow", post(run_bridge_workflow))
        .route("/gui-lock", get(get_gui_lock))
        // Headless-only mode configuration (for server deployments)
        .route(
            "/config/headless-only",
            get(get_headless_only).post(set_headless_only),
        )
        // Debug endpoints for AI sessions
        .route("/debug/app/errors", get(get_debug_errors))
        .route("/findings/summary", get(get_findings_summary))
        .route("/launch-debug-chrome", post(launch_debug_chrome))
        .route("/status", get(get_status))
        .route("/tool-version", get(get_tool_version))
        .route("/monitors", get(get_monitors))
        .route("/load-config", post(load_config))
        .route("/load-last-config", post(load_last_config))
        .route("/run-workflow", post(run_workflow))
        .route("/execute-steps", post(execute_steps))
        .route("/stop-execution", post(stop_execution))
        .route("/execute", post(execute_python_command))
        .route("/execute-action", post(execute_action))
        .route("/capture-screenshot", post(capture_screenshot_step))
        .route("/screenshots/list", get(list_screenshots_endpoint))
        .route("/action-log/view", get(get_action_log_view_endpoint))
        // State navigation route
        .route("/go-to-state", post(go_to_state))
        // Web extraction routes
        .route("/extraction/start", post(start_web_extraction))
        .route("/extraction/vision", post(start_vision_extraction))
        .route("/extraction/stop", post(stop_web_extraction))
        .route("/extraction/status", get(get_extraction_status))
        .route("/extraction/stats", post(update_extraction_stats))
        .route("/extraction/complete", post(complete_extraction))
        .route(
            "/extraction/:extraction_id/screenshot/:screenshot_id",
            get(get_extraction_screenshot),
        )
        // UI-TARS extraction routes
        .route("/uitars-extraction/start", post(start_uitars_extraction))
        .route("/uitars-extraction/stop", post(stop_uitars_extraction))
        .route(
            "/uitars-extraction/status",
            get(get_uitars_extraction_status),
        )
        .route(
            "/uitars-extraction/results",
            get(get_uitars_extraction_results),
        )
        // Vision extraction route (runs Edge Detection, SAM3, OCR on desktop)
        .route("/vision-extraction/extract", post(run_vision_extraction))
        // Pattern matching routes
        .route("/pattern/find", post(pattern_find))
        .route("/pattern/find-all", post(pattern_find_all))
        // Model management routes
        .route("/models", get(list_models))
        .route("/models/download", post(download_model))
        .route("/models/delete", post(delete_model))
        .route("/models/disk-usage", get(get_models_disk_usage))
        .route("/models/:model_id", get(get_model_status))
        // Integration testing routes
        .route("/testing/start", post(start_integration_test))
        .route("/testing/status/:id", get(get_test_run_status))
        .route("/testing/results/:id", get(get_integration_test_results))
        .route("/testing/runs", get(list_integration_test_runs))
        .route("/testing/mock-action", post(mock_gui_action))
        .route("/testing/states", get(get_testing_states))
        .route("/testing/transitions", get(get_testing_transitions))
        .route("/testing/find-path", post(find_testing_path))
        .route("/testing/traverse", post(traverse_to_state))
        .route("/testing/active-states", get(get_testing_active_states))
        .route("/testing/mock-mode", post(set_testing_mock_mode))
        .route("/testing/mocked-actions", get(get_mocked_actions))
        .route("/testing/clear-mocked-actions", post(clear_mocked_actions))
        .route("/testing/assertion", post(run_testing_assertion))
        .route("/testing/end/:id", post(end_integration_test))
        // Playwright State Collector routes
        .route(
            "/playwright-collection/start",
            post(start_playwright_collection),
        )
        .route(
            "/playwright-collection/status",
            get(get_playwright_collection_status),
        )
        .route(
            "/playwright-collection/results",
            get(get_playwright_collection_results),
        )
        .route(
            "/playwright-collection/stop",
            post(stop_playwright_collection),
        )
        // RAG routes
        .route("/rag/import", post(import_rag))
        .route("/rag/list", get(list_rag_configs))
        .route("/rag/availability", get(get_rag_availability))
        .route("/rag/segment", post(segment_screenshot))
        .route("/rag/:project_id/status", get(get_rag_status))
        .route("/rag/:project_id/load", post(load_rag_project))
        .route("/rag/:project_id", delete(delete_rag_config))
        // AI Analysis routes
        .route("/stop-ai-analysis", post(stop_ai_analysis))
        // Runner restart route (for AI self-healing)
        .route("/restart-runner", post(restart_runner))
        // REMOVED: Old AI Developer routes - use /sessions API instead
        // Prompt Library routes
        .route("/prompts", get(list_prompts))
        .route("/prompts", post(create_prompt))
        .route("/prompts/run", post(run_prompt))
        .route("/prompts/search", get(search_prompts))
        .route("/prompts/categories", get(get_prompt_categories))
        .route("/prompts/tags", get(get_prompt_tags))
        .route("/prompts/import", post(import_prompts))
        .route("/prompts/export", get(export_prompts))
        .route("/prompts/:id", get(get_prompt))
        .route("/prompts/:id", put(update_prompt))
        .route("/prompts/:id", delete(delete_prompt))
        .route("/prompts/:id/duplicate", post(duplicate_prompt))
        // Macro Library routes
        .route("/macros", get(list_macros))
        .route("/macros", post(create_macro))
        .route("/macros/search", get(search_macros))
        .route("/macros/categories", get(get_macro_categories))
        .route("/macros/tags", get(get_macro_tags))
        .route(
            "/macros/:id",
            get(get_macro).put(update_macro).delete(delete_macro),
        )
        .route("/macros/:id/run", post(run_macro))
        // Playwright Script Library routes
        .route("/playwright/scripts", get(list_playwright_scripts))
        .route("/playwright/scripts", post(create_playwright_script))
        .route("/playwright/scripts/search", get(search_playwright_scripts))
        .route(
            "/playwright/scripts/categories",
            get(get_playwright_categories),
        )
        .route("/playwright/scripts/tags", get(get_playwright_tags))
        .route(
            "/playwright/scripts/import",
            post(import_playwright_scripts),
        )
        .route("/playwright/scripts/export", get(export_playwright_scripts))
        .route("/playwright/scripts/:id", get(get_playwright_script))
        .route("/playwright/scripts/:id", put(update_playwright_script))
        .route("/playwright/scripts/:id", delete(delete_playwright_script))
        .route("/playwright/scripts/:id/run", post(run_playwright_script))
        .route(
            "/playwright/scripts/:id/duplicate",
            post(duplicate_playwright_script),
        )
        // Scriptlet routes
        .route("/scriptlets", get(list_scriptlets))
        .route("/scriptlets", post(create_scriptlet))
        .route("/scriptlets/search", get(search_scriptlets))
        .route("/scriptlets/categories", get(get_scriptlet_categories))
        .route("/scriptlets/:id", get(get_scriptlet))
        .route("/scriptlets/:id", put(update_scriptlet))
        .route("/scriptlets/:id", delete(delete_scriptlet))
        // Inline Python execution
        .route("/execute-python", post(execute_inline_python))
        // Auto-continue setting routes (global) - combined GET and POST on same route
        .route(
            "/workflow/auto-continue",
            get(get_auto_continue_setting).post(set_auto_continue_setting),
        )
        // Per-workflow auto-continue setting routes - combined GET and POST on same route
        .route(
            "/workflow/active/auto-continue",
            get(get_workflow_auto_continue).post(set_workflow_auto_continue),
        )
        // Backup and Restore routes
        .route("/backup", get(create_backup_handler))
        .route("/backup/info", post(get_backup_info_handler))
        .route("/restore", post(restore_backup_handler))
        // Checkpoint/Database routes (SQLite)
        .route("/checkpoints", get(list_checkpoints).post(save_checkpoint))
        .route(
            "/checkpoints/:name",
            get(get_checkpoint).delete(delete_checkpoint),
        )
        .route("/checkpoints/:name/status", get(get_checkpoint_status))
        .route("/checkpoints/history", get(get_checkpoint_history))
        // Task Run routes (simplified task execution model)
        .route("/task-runs", get(list_task_runs).post(create_task_run))
        .route("/task-runs/running", get(list_running_task_runs))
        .route("/task-runs/:id", get(get_task_run).delete(delete_task_run))
        .route("/task-runs/:id/output", get(get_task_output))
        .route("/task-runs/:id/workflow-state", get(get_workflow_state))
        .route("/task-runs/:id/orchestrator-state", get(get_workflow_state)) // Alias for backward compatibility
        .route("/task-runs/:id/full-state", get(get_full_workflow_state)) // Full state for restart recovery
        .route("/task-runs/:id/stop", post(stop_task_run))
        .route(
            "/task-runs/:id/auto-continue",
            get(get_task_auto_continue).put(set_task_auto_continue),
        )
        .route("/task-runs/:id/resume", post(resume_task_run))
        .route(
            "/task-runs/:id/generate-summary",
            post(generate_task_summary),
        )
        // Hybrid logging routes (SQLite event storage)
        .route("/task-runs/:id/events", get(get_task_run_events))
        .route("/task-runs/:id/screenshots", get(get_task_run_screenshots))
        .route(
            "/task-runs/:id/playwright-results",
            get(get_task_run_playwright_results),
        )
        // Execution spans (tracing data for performance analysis)
        .route("/execution-spans", get(get_execution_spans))
        .route("/task-runs/:id/migrate-logs", post(migrate_task_run_logs))
        // Step checkpoint routes (for dashboard progress display)
        .route("/task-runs/:id/checkpoints", get(get_task_run_checkpoints))
        // Verification results (for AI agents to access detailed test results)
        .route(
            "/task-runs/:id/verification-results",
            get(get_task_run_verification_results),
        )
        // Additional data endpoints for AI agents
        .route("/task-runs/:id/mcp-calls", get(get_task_run_mcp_calls))
        .route(
            "/task-runs/:id/api-requests",
            get(get_task_run_api_requests),
        )
        .route("/task-runs/:id/awas-steps", get(get_task_run_awas_steps))
        .route("/task-runs/:id/knowledge", get(get_task_run_knowledge))
        .route(
            "/task-runs/:id/steps/:checkpoint_id/progress",
            get(get_step_progress_markers),
        )
        // Current execution steps (for dashboard widgets - no task ID needed)
        .route("/current-execution/steps", get(get_current_execution_steps))
        // Automation Run routes (for MCP/AI access to task_run_automation)
        .route("/runs", get(list_automation_runs))
        .route("/runs/:id", get(get_automation_run))
        // Config Storage routes
        .route("/configs", get(list_configs).post(import_config))
        .route("/configs/parse", post(parse_config_file))
        .route(
            "/configs/:id",
            get(get_stored_config)
                .put(update_stored_config)
                .delete(delete_stored_config),
        )
        .route("/configs/:id/export", post(export_config))
        // State Explorer routes
        .route("/state-explorer/start", post(start_exploration))
        .route(
            "/state-explorer/strategies",
            get(get_exploration_strategies),
        )
        .route("/state-explorer/preview", post(preview_exploration))
        .route("/state-explorer/history", get(get_exploration_history))
        .route("/state-explorer/:run_id", get(get_exploration_report))
        .route(
            "/state-explorer/:run_id/prompt",
            get(get_exploration_prompt),
        )
        // Verification Test routes (test CRUD and execution)
        .route("/tests", get(list_tests).post(create_test))
        .route("/tests/execute-suite", post(execute_test_suite_handler))
        .route("/tests/history", get(get_test_history))
        .route(
            "/tests/:id",
            get(get_test).put(update_test).delete(delete_test),
        )
        .route("/tests/:id/execute", post(execute_test_by_id))
        .route("/test-results", get(list_test_results))
        .route("/test-results/:id", get(get_test_result))
        // AI Context routes
        .route("/contexts", get(list_all_contexts))
        .route("/contexts/categories", get(list_context_categories))
        .route("/contexts/tags", get(list_context_tags))
        .route("/contexts/:scope", post(create_context_handler))
        .route(
            "/contexts/:scope/:id",
            put(update_context_handler).delete(delete_context_handler),
        )
        .route(
            "/contexts/:scope/:id/duplicate",
            post(duplicate_context_handler),
        )
        .route(
            "/contexts/metadata/:id/enable",
            post(enable_context_handler),
        )
        .route(
            "/contexts/metadata/:id/disable",
            post(disable_context_handler),
        )
        // Context sync approval routes (for syncing project contexts to qontinui-web)
        .route("/contexts/:id/approve-sync", post(approve_context_sync))
        .route("/contexts/:id/dismiss-sync", post(dismiss_context_sync))
        // DOM Capture routes
        .route("/dom/captures", get(list_dom_captures))
        .route("/dom/captures/:id", get(get_dom_capture))
        .route("/dom/captures/:id/html", get(get_dom_capture_html))
        .route("/dom/receive", post(receive_dom_from_extension))
        // API Request routes
        .route("/api-request/import-curl", post(import_curl_command))
        .route(
            "/api-request/import-to-library",
            post(import_curl_to_library),
        )
        .route("/api-request/test", post(test_api_request))
        // Saved API Requests Library routes
        .route(
            "/saved-api-requests",
            get(list_saved_api_requests).post(create_saved_api_request),
        )
        .route("/saved-api-requests/search", get(search_saved_api_requests))
        .route(
            "/saved-api-requests/categories",
            get(get_saved_api_request_categories),
        )
        .route("/saved-api-requests/tags", get(get_saved_api_request_tags))
        .route(
            "/saved-api-requests/:id",
            get(get_saved_api_request)
                .put(update_saved_api_request)
                .delete(delete_saved_api_request),
        )
        .route(
            "/saved-api-requests/:id/duplicate",
            post(duplicate_saved_api_request),
        )
        // Unified Workflows routes
        .route(
            "/unified-workflows",
            get(list_unified_workflows).post(create_unified_workflow),
        )
        .route("/unified-workflows/search", get(search_unified_workflows))
        .route(
            "/unified-workflows/:id",
            get(get_unified_workflow)
                .put(update_unified_workflow)
                .delete(delete_unified_workflow),
        )
        .route(
            "/unified-workflows/:id/duplicate",
            post(duplicate_unified_workflow),
        )
        .route(
            "/unified-workflows/:id/export",
            get(export_unified_workflow),
        )
        .route("/unified-workflows/import", post(import_unified_workflow))
        .route(
            "/unified-workflows/generate",
            post(generate_unified_workflow_handler),
        )
        .route("/unified-workflows/:id/run", post(run_unified_workflow))
        .route(
            "/unified-workflows/execute-inline",
            post(execute_inline_workflow),
        )
        .route(
            "/unified-workflows/last-inline",
            get(get_last_inline_workflow),
        )
        .route(
            "/unified-workflows/:id/stats",
            get(get_unified_workflow_stats),
        )
        .route(
            "/unified-workflows/run-sequence",
            post(run_workflow_sequence),
        )
        // AWAS (Application Web Automation Specification) routes
        .route("/awas/discover", post(awas_discover))
        .route("/awas/execute", post(awas_execute))
        .route("/awas/check-support", post(awas_check_support))
        .route("/awas/actions", get(awas_list_actions))
        .route("/awas/extract-elements", post(awas_extract_elements))
        // Check generation routes (AI-assisted check configuration)
        .route("/checks/scan-workspace", post(scan_workspace_handler))
        .route("/checks/generate", post(generate_checks_handler))
        // Check database integrity/repair routes
        .route(
            "/checks/repair-associations",
            post(repair_check_associations_handler),
        )
        // Render logging routes (for UI testing)
        .route(
            "/render-log",
            get(get_render_log).delete(clear_render_log_handler),
        )
        .route("/render-log/path", get(get_render_log_path))
        // Navigation routes (for UI testing)
        .route("/navigate", post(navigate_to_page))
        // UI Bridge routes (AI-driven UI automation via React UI Bridge)
        .route(
            "/ui-bridge/control/elements",
            get(ui_bridge_get_elements_handler),
        )
        .route(
            "/ui-bridge/control/element/:id",
            get(ui_bridge_get_element_handler),
        )
        .route(
            "/ui-bridge/control/element/:id/action",
            post(ui_bridge_execute_action_handler),
        )
        .route(
            "/ui-bridge/control/components",
            get(ui_bridge_get_components_handler),
        )
        .route(
            "/ui-bridge/control/component/:id",
            get(ui_bridge_get_component_handler),
        )
        .route(
            "/ui-bridge/control/component/:id/action/:action_id",
            post(ui_bridge_execute_component_action_handler),
        )
        .route(
            "/ui-bridge/control/discover",
            post(ui_bridge_discover_handler),
        )
        .route(
            "/ui-bridge/control/snapshot",
            get(ui_bridge_get_snapshot_handler),
        )
        .route("/ui-bridge/control/specs", get(ui_bridge_get_specs_handler))
        .route(
            "/ui-bridge/control/spec/:id",
            get(ui_bridge_get_spec_handler),
        )
        // UI Bridge Exploration (uses qontinui library via Python bridge)
        .route("/ui-bridge/explore", post(start_ui_bridge_exploration))
        .route(
            "/ui-bridge/explore/status",
            get(get_ui_bridge_exploration_status),
        )
        .route(
            "/ui-bridge/explore/results",
            get(get_ui_bridge_exploration_results),
        )
        .route("/ui-bridge/explore/stop", post(stop_ui_bridge_exploration))
        // UI Bridge state discovery from render logs (separate from exploration)
        .route(
            "/ui-bridge/discover-states",
            post(discover_states_from_renders),
        )
        // Recording & Playback routes (browser interaction recording for script generation)
        .route(
            "/recordings",
            get(list_recordings_handler).post(create_recording_handler),
        )
        .route(
            "/recordings/:id",
            get(get_recording_handler).delete(delete_recording_handler),
        )
        .route(
            "/recordings/:id/actions",
            get(get_recording_actions_handler).post(add_recording_action_handler),
        )
        .route(
            "/recordings/:id/status",
            put(update_recording_status_handler),
        )
        .route(
            "/recordings/:id/export/:format",
            get(export_recording_handler),
        )
        .route(
            "/recordings/:id/exports",
            get(get_recording_exports_handler),
        )
        // Error Monitor endpoints (application log error detection for debug agent)
        .route("/error-monitor/errors", get(get_error_monitor_errors))
        .route("/error-monitor/summary", get(get_error_monitor_summary))
        .route("/error-monitor/debug-context", get(get_error_debug_context))
        .route(
            "/error-monitor/errors/:id/resolve",
            post(resolve_error_monitor_error),
        )
        .route(
            "/error-monitor/errors/:id/acknowledge",
            post(acknowledge_error_monitor_error),
        )
        .route("/error-monitor/fix-workflow", post(generate_fix_workflow))
        .layer(cors)
        // Allow up to 100MB request bodies for configs with embedded images
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
        .with_state(api_state)
}

/// Try to bind to a port with SO_REUSEADDR
fn try_bind_port(port: u16) -> Result<std::net::TcpListener, std::io::Error> {
    // Create socket with SO_REUSEADDR to allow binding even if there are zombie connections
    // This is necessary on Windows where TIME_WAIT/CLOSE_WAIT sockets can block port binding
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    socket.listen(1024)?;
    Ok(socket.into())
}

/// Start the MCP API server
pub async fn start_server(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = create_router(app_state, rag_state, app_handle);

    // Try the requested port first, then fallback ports if zombie connections are blocking
    // This can happen on Windows when previous process crashes leave orphaned sockets
    let ports_to_try = [port, port + 1, port + 2];
    let mut last_error = None;

    for try_port in ports_to_try {
        match try_bind_port(try_port) {
            Ok(std_listener) => {
                let listener = tokio::net::TcpListener::from_std(std_listener)?;
                if try_port != port {
                    warn!(
                        "Primary port {} was blocked, using fallback port {}. \
                         Restart the app after zombie connections clear.",
                        port, try_port
                    );
                }
                info!("MCP API server listening on port {}", try_port);
                axum::serve(listener, router).await?;
                return Ok(());
            }
            Err(e) => {
                warn!("Failed to bind to port {}: {}", try_port, e);
                last_error = Some(e);
            }
        }
    }

    Err(Box::new(last_error.unwrap_or_else(|| {
        std::io::Error::other("All ports failed")
    })))
}
