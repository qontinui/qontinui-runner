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
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Json},
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
use crate::commands::project_logs;
use crate::commands::rag::{send_embeddings_to_web, RAGState};
use crate::commands::AppState;
use crate::config::ConfigLoader;
use crate::config_storage::{ConfigMetadata, ConfigStorage, StoredConfig};
use crate::context;
use crate::findings::storage as finding_storage;
use crate::findings::{Finding, FindingParser, ParsedFinding};
use crate::mcp::types::{GoToStateRequest, GoToStateResult};
use crate::rag::{ImportResult, QontinuiConfig, RAGConfigSummary};
use crate::scriptlets;
use crate::session::SessionManager;
use crate::settings;
use crate::task_monitor::TaskMonitor;
use crate::task_recorder::{TaskConfig, TaskRecorder};
use crate::tiered_info::{self, RunDetails};
// WorkflowManager import removed - using unified SessionManager instead
use axum::routing::{delete, put};
use tauri::{Emitter, Manager};

// Windows-specific imports for process creation flags
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

// Windows constants for process creation
#[cfg(target_os = "windows")]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

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
const FINDING_INSTRUCTIONS: &str = r#"
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
/// If `pid_tracker` is provided, the child process PID will be stored there so it can be
/// killed by the stop_ai_analysis endpoint.
fn run_claude_session_inline(
    working_dir: &str,
    prompt: &str,
    session_id: &str,
    app_handle: &tauri::AppHandle,
    timeout_seconds: u64,
    session_ctx: Option<AiOutputSessionContext>,
    finding_ctx: Option<FindingContext>,
    pid_tracker: Option<Arc<std::sync::Mutex<Vec<u32>>>>,
) -> Result<(bool, String), String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant};

    info!(
        "Running Claude session inline: {} (timeout: {}s)",
        session_id, timeout_seconds
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

    // Stdout reader thread
    let stdout = child.stdout.take();
    let app_handle_stdout = app_handle.clone();
    let has_output_stdout = has_output.clone();
    let session_ctx_stdout = session_ctx.clone();
    let finding_ctx_for_stdout = finding_ctx.clone();

    let stdout_handle = thread::spawn(move || {
        let mut all_text = String::new();
        // Create finding parser if we have a finding context
        let mut finding_parser = if finding_ctx_for_stdout.is_some() {
            Some(FindingParser::new())
        } else {
            None
        };

        // Buffer to accumulate text until we have complete lines for finding parsing.
        // Stream-json sends partial text chunks (content_block_delta), so markers like
        // [FINDING:code_bug:high] can be split across multiple events. We need to buffer
        // and only parse complete lines (ending with \n) for findings.
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

                        // Parse complete lines for findings
                        // We buffer text until we encounter newlines, then process
                        // complete lines through the finding parser
                        if let Some(ref mut parser) = finding_parser {
                            line_buffer.push_str(&text);

                            // Process complete lines from the buffer
                            while let Some(newline_pos) = line_buffer.find('\n') {
                                let complete_line = line_buffer[..newline_pos].to_string();
                                line_buffer = line_buffer[newline_pos + 1..].to_string();

                                if let Some(parsed_finding) = parser.process_line(&complete_line) {
                                    // Send the parsed finding to the processor thread
                                    let _ = finding_tx.send(parsed_finding);
                                }
                            }
                        }

                        all_text.push_str(&text);
                    }
                }
            }
        }

        // Process any remaining text in the buffer (final line without trailing newline)
        if let Some(ref mut parser) = finding_parser {
            if !line_buffer.is_empty() {
                if let Some(parsed_finding) = parser.process_line(&line_buffer) {
                    let _ = finding_tx.send(parsed_finding);
                }
            }
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

    // Stderr reader thread
    let stderr = child.stderr.take();
    let stderr_handle = thread::spawn(move || {
        let mut output = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut output);
        }
        output
    });

    // Wait for process with inactivity timeout
    let status = loop {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last_activity_secs = last_activity.load(Ordering::Relaxed);
        let inactive_secs = now_secs.saturating_sub(last_activity_secs);

        if inactive_secs > timeout_seconds {
            warn!(
                "Session {} timed out after {}s of inactivity",
                session_id, inactive_secs
            );
            let _ = child.kill();
            thread::sleep(Duration::from_millis(500));
            let _ = child.try_wait();
            let _ = stop_tx.send(());
            let _ = heartbeat_handle.join();
            let _ = std::fs::remove_file(&prompt_file);
            return Err(format!(
                "Session timed out after {}s of inactivity",
                inactive_secs
            ));
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
    let all_output = stdout_handle.join().unwrap_or_default();
    let stderr_output = stderr_handle.join().unwrap_or_default();
    // Wait for the finding processor thread to complete
    // (it will exit when the stdout thread closes the channel sender)
    let detected_findings = finding_processor_handle.join().unwrap_or_default();
    let _ = std::fs::remove_file(&prompt_file);

    // Log summary of detected findings
    if !detected_findings.is_empty() {
        info!(
            "Session {} detected {} findings",
            session_id,
            detected_findings.len()
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
        "Session {} completed: success={}, output_len={}, findings={}",
        session_id,
        success,
        all_output.len(),
        detected_findings.len()
    );

    // Remove PID from tracker now that session is complete
    remove_pid(&pid_tracker);

    Ok((success, all_output))
}

/// Default port for the MCP API server
pub const MCP_API_PORT: u16 = 9876;

/// Shared state for the API server
pub struct ApiState {
    pub app_state: Arc<AppState>,
    pub rag_state: Arc<RAGState>,
    pub app_handle: tauri::AppHandle,
    /// Unified session manager for all AI sessions (Prompt Library + AI Builder)
    pub session: Arc<SessionManager>,
    /// Task monitor for watching Claude session output
    pub task_monitor: Arc<TaskMonitor>,
    /// Currently loaded config ID (for tracking which config is active)
    pub current_config_id: std::sync::Mutex<Option<String>>,
    /// Persistent storage for configurations
    pub config_storage: Arc<tokio::sync::Mutex<ConfigStorage>>,
    /// Unified action service for deterministic execution
    pub action_service: Arc<UnifiedActionService>,
    /// Currently running AI process PIDs (for stopping)
    pub current_ai_pids: Arc<std::sync::Mutex<Vec<u32>>>,
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
}

/// Create an error response
fn api_error(message: impl Into<String>) -> ApiResponse<()> {
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
    /// Timeout in seconds for execution completion (default: 300)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    300 // 5 minutes default timeout
}

fn default_action_timeout() -> u64 {
    30 // 30 seconds default timeout for single action
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
    /// Timeout in seconds for action completion (default: 30)
    #[serde(default = "default_action_timeout")]
    pub timeout_seconds: u64,
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

/// Health check endpoint
async fn health() -> Json<ApiResponse<String>> {
    Json(ApiResponse::success("ok".to_string()))
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

    let dev_logs_path =
        std::path::PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs");

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

    // Log files to scan with their service names
    let log_files = [
        ("backend.log", "backend"),
        ("frontend.log", "frontend"),
        ("qontinui-api.log", "api"),
        ("runner-tauri.log", "runner"),
        ("runner-actions.jsonl", "runner-actions"),
    ];

    // Regex patterns for error detection
    let error_patterns = [
        // Python/FastAPI errors
        (r"(?i)(error|exception|traceback|failed)", "error"),
        // TypeScript/Next.js errors
        (r"(?i)(ERROR|error:|\[error\])", "error"),
        // Warnings
        (r"(?i)(warning|warn|\[warn\])", "warning"),
    ];

    for (filename, service) in &log_files {
        // Apply service filter if specified
        if let Some(ref svc_filter) = query.service {
            if !service.eq_ignore_ascii_case(svc_filter) {
                continue;
            }
        }

        let file_path = dev_logs_path.join(filename);
        if !file_path.exists() {
            continue;
        }

        if let Ok(file) = std::fs::File::open(&file_path) {
            let reader = BufReader::new(file);
            let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

            // Process from end (most recent) to beginning
            let mut i = lines.len();
            while i > 0 {
                i -= 1;
                let line = &lines[i];

                // Determine log level
                let mut level = None;
                for (pattern, lvl) in &error_patterns {
                    if let Ok(re) = Regex::new(pattern) {
                        if re.is_match(line) {
                            level = Some(*lvl);
                            break;
                        }
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
                    let timestamp = if let Ok(ts_re) =
                        Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})")
                    {
                        ts_re
                            .captures(line)
                            .and_then(|c| c.get(1))
                            .map(|m| m.as_str().to_string())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };

                    // Collect context (surrounding lines for stack traces)
                    let mut context_lines = Vec::new();
                    let mut j = i + 1;
                    while j < lines.len() && j < i + 10 {
                        let ctx_line = &lines[j];
                        // Stop at next log entry (has timestamp or is empty)
                        if ctx_line.is_empty()
                            || Regex::new(r"^\d{4}-\d{2}-\d{2}")
                                .map(|re| re.is_match(ctx_line))
                                .unwrap_or(false)
                        {
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
/// Note: Findings are stored per-task-run. This endpoint returns findings
/// from the most recent task runs.
async fn get_findings_summary() -> Json<ApiResponse<serde_json::Value>> {
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
    let db_path = app_data_dir.join("qontinui-runner.db");

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

    // Get recent task run IDs
    let task_run_ids: Vec<String> =
        match db.prepare("SELECT id FROM task_runs ORDER BY created_at DESC LIMIT 5") {
            Ok(mut stmt) => stmt
                .query_map([], |row| row.get(0))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            Err(_) => vec![],
        };

    let mut all_findings = Vec::new();
    for task_run_id in &task_run_ids {
        if let Ok(findings) = finding_storage::get_findings_for_task(&db, task_run_id) {
            all_findings.extend(findings);
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
        // Use unwrap_or_else to recover from poisoned mutex (after a panic)
        let bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        let (executor_running, executor_state) = if let Some(ref bridge) = *bridge_lock {
            (bridge.is_running(), bridge.get_state().name().to_string())
        } else {
            (false, "not_started".to_string())
        };
        drop(bridge_lock);

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

    // Step 2: Store the configuration in app state
    *app_state.current_config.lock().unwrap_or_else(|poisoned| {
        warn!("load_config_internal: current_config mutex was poisoned, recovering");
        poisoned.into_inner()
    }) = Some(config);
    info!("load_config_internal: Configuration stored in app state");

    // Step 3: Send debug settings and configuration to Python bridge
    let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
        warn!("load_config_internal: python_bridge mutex was poisoned, recovering");
        poisoned.into_inner()
    });

    if let Some(ref mut bridge) = *bridge_lock {
        if !bridge.is_running() {
            warn!("load_config_internal: Python executor not running, config stored but not sent to executor");
            return Ok(summary);
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
        bridge.load_configuration(config_path).map_err(|e| {
            error!(
                "load_config_internal: Failed to send configuration to Python: {}",
                e
            );
            format!("Failed to send configuration to Python: {}", e)
        })?;

        info!("load_config_internal: Configuration sent to Python executor");
    } else {
        warn!("load_config_internal: Python executor not initialized, config stored but not sent");
    }

    Ok(summary)
}

/// Load a configuration file
///
/// This mirrors the behavior from commands/config.rs:
/// 1. Loads and validates the configuration file
/// 2. Stores it in the app state (current_config)
/// 3. Saves the path for auto-load functionality
/// 4. Sends debug settings to the Python executor
/// 5. Sends the configuration to the Python executor
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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
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
            Ok((summary, config_data))
        } else {
            Err("Python executor not initialized".to_string())
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
async fn run_workflow(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RunWorkflowRequest>,
) -> Result<Json<ApiResponse<WorkflowExecutionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Running workflow: {} (timeout: {}s)",
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

    // Create step executor
    let executor = crate::step_executor::StepExecutor::new(
        state.app_state.clone(),
        state.config_storage.clone(),
    );

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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
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
            Ok((summary, config_data))
        } else {
            Err("Python executor not initialized".to_string())
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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            match bridge.stop_execution() {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("Failed to stop execution: {}", e)),
            }
        } else {
            Err("Python executor not initialized".to_string())
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

/// Execute a single action (e.g., click on an image, type text, press hotkey)
///
/// This endpoint allows executing individual GUI actions without running a full workflow.
/// Uses the UnifiedActionService for deterministic execution, ensuring both
/// manual API calls and AI task execution use the same code path.
async fn execute_action(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ExecuteActionRequest>,
) -> Result<Json<ApiResponse<ExecuteActionResult>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Executing action: {} (image: {}, text: {:?}, hotkey: {:?}, timeout: {}s)",
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
            request.timeout_seconds,
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

    // Get qontinui-api URL
    let api_url = std::env::var("QONTINUI_API_URL_VISION")
        .unwrap_or_else(|_| "http://localhost:8001".to_string());

    // Build URL with query parameters
    let mut url = format!("{}/api/capture/screenshot/current", api_url);
    let mut params = Vec::new();
    if let Some(mon) = request.monitor {
        params.push(format!("monitor={}", mon));
    }
    // Always request high quality for AI analysis
    params.push("quality=95".to_string());
    if !params.is_empty() {
        url = format!("{}?{}", url, params.join("&"));
    }

    // Capture screenshot via qontinui-api
    let client = reqwest::Client::new();
    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            error!("MCP API: Failed to capture screenshot: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Network error: {}", e)),
            })));
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "MCP API: Screenshot capture failed with status {}: {}",
            status, error_text
        );
        return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
            success: false,
            screenshot_path: None,
            absolute_path: None,
            width: None,
            height: None,
            monitor: request.monitor,
            error: Some(format!("Capture failed: {}", error_text)),
        })));
    }

    // Parse response
    let capture_response: serde_json::Value = match response.json().await {
        Ok(j) => j,
        Err(e) => {
            error!("MCP API: Failed to parse screenshot response: {}", e);
            return Ok(Json(ApiResponse::success(CaptureScreenshotResponse {
                success: false,
                screenshot_path: None,
                absolute_path: None,
                width: None,
                height: None,
                monitor: request.monitor,
                error: Some(format!("Invalid API response: {}", e)),
            })));
        }
    };

    let screenshot_base64 = match capture_response
        .get("screenshot_base64")
        .and_then(|s| s.as_str())
    {
        Some(s) => s.to_string(),
        None => {
            error!("MCP API: No screenshot_base64 in response");
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
    let dev_logs_path =
        std::path::PathBuf::from(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs");
    let screenshots_dir = dev_logs_path.join("screenshots");
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
        session_id: None,
        session_name: None,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct ExtractionStats {
    pub states_found: u32,
    pub transitions_found: u32,
    pub warnings: u32,
    pub errors: u32,
}

/// Start web extraction
async fn start_web_extraction(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartExtractionRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting web extraction for {} URLs",
        request.urls.len()
    );

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

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            bridge.send_command("start_web_extraction", Some(params))
        } else {
            Err("Python executor not initialized".to_string())
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
        Ok(_) => {
            info!("MCP API: Web extraction started");
            Ok(Json(ApiResponse::success(serde_json::json!({
                "started": true,
                "message": "Web extraction started"
            }))))
        }
        Err(e) => {
            error!("MCP API: Failed to start web extraction: {}", e);
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

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            bridge.send_command("stop_web_extraction", None)
        } else {
            Err("Python executor not initialized".to_string())
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
        Ok(_) => {
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
    let app_state = state.app_state.clone();

    let result = tokio::task::spawn_blocking(move || {
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            bridge.send_command("get_extraction_status", None)
        } else {
            Err("Python executor not initialized".to_string())
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
        Ok(_) => {
            // Note: send_command doesn't return data, so we return a default status
            // TODO: Implement proper extraction status tracking in app state
            Ok(Json(ApiResponse::success(ExtractionStatusResponse {
                is_running: false,
                extraction_id: None,
                stats: None,
            })))
        }
        Err(e) => {
            error!("MCP API: Failed to get extraction status: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
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
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("MCP API: Checking RAG availability");

    // TODO: Actually check ML model availability
    // For now, return a placeholder response
    Ok(Json(ApiResponse::success(serde_json::json!({
        "available": true,
        "models": {
            "clip": true,
            "text": true,
            "ocr": true,
            "sam": false
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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (2 minute timeout for SAM3 processing)
            let timeout_duration = std::time::Duration::from_secs(120);
            bridge.send_command_and_wait("segment_screenshot", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
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
    #[serde(default = "default_ocr_confidence_threshold")]
    pub ocr_confidence_threshold: f64,
    /// Fusion: IoU threshold for merging results
    #[serde(default = "default_iou_threshold")]
    pub iou_threshold: f64,
}

fn default_vision_techniques() -> Vec<String> {
    vec!["edge".to_string(), "sam3".to_string(), "ocr".to_string()]
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

fn default_ocr_confidence_threshold() -> f64 {
    0.5
}

fn default_iou_threshold() -> f64 {
    0.5
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
        let mut bridge_lock = app_state.python_bridge.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("MCP API: python_bridge mutex was poisoned, recovering");
            poisoned.into_inner()
        });

        if let Some(ref mut bridge) = *bridge_lock {
            if !bridge.is_running() {
                return Err("Python executor not running".to_string());
            }

            // Send command and wait for response (3 minute timeout for vision processing)
            let timeout_duration = std::time::Duration::from_secs(180);
            bridge.send_command_and_wait("run_vision_extraction", Some(params), timeout_duration)
        } else {
            Err("Python executor not initialized".to_string())
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
// AI Workflow Request/Response Types
// ============================================================================

use crate::ai_workflows;
use crate::gui_workflows;

/// Request to create a new AI workflow
#[derive(Debug, Deserialize)]
pub struct CreateAiWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<ai_workflows::ExecutionStep>,
    #[serde(default)]
    pub goal: String,
    #[serde(default = "default_ai_workflow_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub capture_input_validation: bool,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub context_ids: Vec<String>,
    #[serde(default)]
    pub disabled_context_ids: Vec<String>,
    #[serde(default = "default_auto_include_contexts")]
    pub auto_include_contexts: bool,
}

fn default_auto_include_contexts() -> bool {
    true
}

fn default_ai_workflow_max_iterations() -> u32 {
    10
}

/// Request to update an existing AI workflow
#[derive(Debug, Deserialize)]
pub struct UpdateAiWorkflowRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<ai_workflows::ExecutionStep>>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub capture_input_validation: Option<bool>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub context_ids: Option<Vec<String>>,
    #[serde(default)]
    pub disabled_context_ids: Option<Vec<String>>,
    #[serde(default)]
    pub auto_include_contexts: Option<bool>,
}

// ============================================================================
// GUI Workflow Request/Response Types
// ============================================================================

/// Request to create a new GUI workflow
#[derive(Debug, Deserialize)]
pub struct CreateGuiWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub steps: Vec<gui_workflows::GuiWorkflowStep>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Request to update an existing GUI workflow
#[derive(Debug, Deserialize)]
pub struct UpdateGuiWorkflowRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub steps: Option<Vec<gui_workflows::GuiWorkflowStep>>,
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
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>, // Session ID for grouping output across continuations
    #[serde(rename = "sessionName")]
    pub session_name: Option<String>, // Human-readable session name
}

/// Session context for AI output events
#[derive(Debug, Clone, Default)]
pub struct AiOutputSessionContext {
    pub session_id: Option<String>,
    pub session_name: Option<String>,
}

/// Context for finding detection during AI sessions.
/// Contains the information needed to store findings in the database.
#[derive(Debug, Clone)]
pub struct FindingContext {
    /// The task_run_id for storing findings (same as session_id in most cases)
    pub task_run_id: String,
    /// The current session/phase number within the task run
    pub session_num: u32,
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

/// Emit AI output event to frontend
pub fn emit_ai_output(
    app_handle: &tauri::AppHandle,
    line: &str,
    source: &str,
    action_id: Option<&str>,
    session_ctx: Option<&AiOutputSessionContext>,
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
        session_id: session_ctx.and_then(|ctx| ctx.session_id.clone()),
        session_name: session_ctx.and_then(|ctx| ctx.session_name.clone()),
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
        let mut pids = state.current_ai_pids.lock().unwrap();
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
    let task_monitor = &state.task_monitor;
    for task in &running_tasks {
        // Stop monitoring
        if let Err(e) = task_monitor.stop_monitoring(&task.id).await {
            warn!("MCP API: Failed to stop monitoring for {}: {}", task.id, e);
        }

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

/// Helper function to mark a task run as complete with retry logic.
/// Retries up to 3 times with exponential backoff (100ms, 200ms, 400ms).
/// Returns true if successfully marked complete, false otherwise.
async fn complete_task_run_with_retry(db: Arc<CheckpointDb>, task_id: &str) -> bool {
    let task_id_owned = task_id.to_string();
    let max_retries = 3;

    for retry in 0..max_retries {
        let db_clone = db.clone();
        let id = task_id_owned.clone();

        match tokio::task::spawn_blocking(move || db_clone.complete_task_run(&id)).await {
            Ok(Ok(())) => {
                if retry > 0 {
                    info!(
                        "Task run {} marked complete after {} retries",
                        task_id_owned, retry
                    );
                }
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
    // Determine mode and get prompt name + content
    let (prompt_name, prompt_content, prompt_id, prompt_max_sessions) =
        if let Some(ref id) = request.prompt_id {
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
            )
        } else if let (Some(name), Some(content)) = (&request.name, &request.content) {
            // Mode 2: Ad-hoc prompt
            (name.clone(), content.clone(), None, None)
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
        let config_lock = state.app_state.current_config.lock().unwrap();
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
                        let mut config_lock = state.app_state.current_config.lock().unwrap();
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
        let config_lock = state.app_state.current_config.lock().unwrap();
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
2. You CAN restart backend, frontend, and qontinui-api without issues
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
2. You CAN restart backend, frontend, and qontinui-api without issues
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

    // Inject configured log sources from active profile if available
    // This tells the AI where to find logs for debugging
    if let Ok(configs) = project_logs::list_project_configs_internal() {
        let all_sources: Vec<_> = configs
            .iter()
            .flat_map(|c| {
                // Get sources from active profile (or legacy fallback)
                let profile_name = c
                    .get_active_profile()
                    .map(|p| p.name.as_str())
                    .unwrap_or("Default");
                c.get_active_log_sources()
                    .iter()
                    .filter(|s| s.enabled)
                    .map(|s| format!("- **{}** ({}): `{}`", s.name, profile_name, s.path))
                    .collect::<Vec<_>>()
            })
            .collect();

        if !all_sources.is_empty() {
            let log_sources_section = format!(
                r#"## Configured Log Sources

The following log files have been configured for monitoring. Use these paths to check for errors:

{}

---

"#,
                all_sources.join("\n")
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
        let config_lock = state.app_state.current_config.lock().unwrap();
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
        "MCP API: Running prompt '{}' (session: {}, max_sessions: {:?}, images: {})",
        prompt_name,
        session_id,
        max_sessions,
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

    db.create_task_run_with_config(
        &task_run_id,
        &prompt_name,
        Some(&enhanced_prompt),
        "task", // task_type
        None,   // config_id
        None,   // workflow_name
        max_sessions,
        None, // auto_continue
        None, // execution_steps_json
        None, // log_sources_json
    )
    .map_err(|e| {
        error!("MCP API: Failed to create task run: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to create task run: {}", e))),
        )
    })?;

    info!("MCP API: Created task run with ID: {}", task_run_id);

    // Create session context for AI output events so frontend can display the task name
    let session_ctx = AiOutputSessionContext {
        session_id: Some(task_run_id.clone()),
        session_name: Some(prompt_name.clone()),
    };

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
            "activity_log": []
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
        Ok((response, log_file, dev_logs_path)) => {
            // Start monitoring the task for [TASK_COMPLETE] marker
            let task_monitor = state.task_monitor.clone();
            let task_run_id = response.task_run_id.clone();

            // Spawn monitoring in background
            tokio::spawn(async move {
                if let Err(e) = task_monitor
                    .start_monitoring(&task_run_id, log_file, dev_logs_path)
                    .await
                {
                    error!("Failed to start task monitoring for {}: {}", task_run_id, e);
                } else {
                    info!("Started task monitoring for {}", task_run_id);
                }
            });

            Ok(Json(ApiResponse::success(response)))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
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
// AI Workflow Handlers
// ============================================================================

/// List all AI workflows
async fn list_ai_workflows(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<ai_workflows::AiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let category = params.get("category").map(|s| s.as_str());
    let workflows = ai_workflows::list_workflows(category);
    Ok(Json(ApiResponse::success(workflows)))
}

/// Get a single AI workflow by ID
async fn get_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<ai_workflows::AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::get_workflow(&id) {
        Some(workflow) => Ok(Json(ApiResponse::success(workflow))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("AI workflow not found: {}", id))),
        )),
    }
}

/// Create a new AI workflow
async fn create_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateAiWorkflowRequest>,
) -> Result<Json<ApiResponse<ai_workflows::AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::create_workflow(
        request.name,
        request.description,
        request.steps,
        request.goal,
        request.max_iterations,
        request.capture_input_validation,
        request.category,
        request.tags,
        request.context_ids,
        request.disabled_context_ids,
        request.auto_include_contexts,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing AI workflow
async fn update_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateAiWorkflowRequest>,
) -> Result<Json<ApiResponse<ai_workflows::AiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::update_workflow(
        &id,
        request.name,
        request.description,
        request.steps,
        request.goal,
        request.max_iterations,
        request.capture_input_validation,
        request.category,
        request.tags,
        request.context_ids,
        request.disabled_context_ids,
        request.auto_include_contexts,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete an AI workflow
async fn delete_ai_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match ai_workflows::delete_workflow(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search AI workflows by query
async fn search_ai_workflows(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<ai_workflows::AiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = ai_workflows::search_workflows(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Get all AI workflow categories
async fn get_ai_workflow_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = ai_workflows::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all AI workflow tags
async fn get_ai_workflow_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = ai_workflows::get_tags();
    Ok(Json(ApiResponse::success(tags)))
}

// ============================================================================
// GUI Workflow Handlers
// ============================================================================

/// List all GUI workflows
async fn list_gui_workflows(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<gui_workflows::GuiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let category = params.get("category").map(|s| s.as_str());
    let workflows = gui_workflows::list_workflows(category);
    Ok(Json(ApiResponse::success(workflows)))
}

/// Get a single GUI workflow by ID
async fn get_gui_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<gui_workflows::GuiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match gui_workflows::get_workflow(&id) {
        Some(workflow) => Ok(Json(ApiResponse::success(workflow))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("GUI workflow not found: {}", id))),
        )),
    }
}

/// Create a new GUI workflow
async fn create_gui_workflow(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CreateGuiWorkflowRequest>,
) -> Result<Json<ApiResponse<gui_workflows::GuiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match gui_workflows::create_workflow(
        request.name,
        request.description,
        request.steps,
        request.category,
        request.tags,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e)))),
    }
}

/// Update an existing GUI workflow
async fn update_gui_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(request): Json<UpdateGuiWorkflowRequest>,
) -> Result<Json<ApiResponse<gui_workflows::GuiWorkflow>>, (StatusCode, Json<ApiResponse<()>>)> {
    match gui_workflows::update_workflow(
        &id,
        request.name,
        request.description,
        request.steps,
        request.category,
        request.tags,
    ) {
        Ok(workflow) => Ok(Json(ApiResponse::success(workflow))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Delete a GUI workflow
async fn delete_gui_workflow(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    match gui_workflows::delete_workflow(&id) {
        Ok(()) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Err((StatusCode::NOT_FOUND, Json(api_error(e)))),
    }
}

/// Search GUI workflows by query
async fn search_gui_workflows(
    State(_state): State<Arc<ApiState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<gui_workflows::GuiWorkflow>>>, (StatusCode, Json<ApiResponse<()>>)>
{
    let query = params.get("q").map(|s| s.as_str()).unwrap_or("");
    let results = gui_workflows::search_workflows(query);
    Ok(Json(ApiResponse::success(results)))
}

/// Get all GUI workflow categories
async fn get_gui_workflow_categories(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let categories = gui_workflows::get_categories();
    Ok(Json(ApiResponse::success(categories)))
}

/// Get all GUI workflow tags
async fn get_gui_workflow_tags(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<String>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let tags = gui_workflows::get_tags();
    Ok(Json(ApiResponse::success(tags)))
}

/// Run a GUI workflow by ID
async fn run_gui_workflow(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Get the workflow
    let workflow = match gui_workflows::get_workflow(&id) {
        Some(w) => w,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(api_error(format!("GUI workflow not found: {}", id))),
            ));
        }
    };

    // Increment run count
    if let Err(e) = gui_workflows::increment_run_count(&id) {
        tracing::warn!(
            "Failed to increment run count for GUI workflow {}: {}",
            id,
            e
        );
    }

    let start_time = std::time::Instant::now();
    let mut step_results: Vec<serde_json::Value> = Vec::new();
    let mut successful_steps = 0;
    let mut failed_steps = 0;

    // Execute each step using the action service
    let action_service = state.action_service.clone();

    for (idx, step) in workflow.steps.iter().enumerate() {
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
                        let timeout = step.timeout_seconds.unwrap_or(60);
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
        "workflow_id": id,
        "workflow_name": workflow.name,
        "total_steps": workflow.steps.len(),
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

// ============================================================================
// Unified Session API Handlers
// ============================================================================

use crate::iteration_bundle::{self, IterationBundle, RelevantLogSources};
use crate::log_consolidation;
use crate::session::{Session, SessionConfig, SessionStatus};
use crate::step_executor::{ExecutionResult, ExecutionStepConfig, LogSourceConfig, StepExecutor};

/// Request to start a new unified session
#[derive(Debug, Deserialize)]
struct StartSessionRequest {
    /// Session name (for display)
    name: String,
    /// Initial prompt content
    prompt: String,
    /// Prompt for continuation (if different)
    continuation_prompt: Option<String>,
    /// Total phases/iterations (0 = unlimited)
    #[serde(default)]
    total_phases: u32,
    /// Whether this session uses GUI automation
    #[serde(default)]
    uses_gui: bool,
    /// Timeout per phase in seconds (default 1800 = 30 min)
    #[serde(default = "default_session_timeout")]
    timeout_seconds: u64,

    /// Execution steps to run BEFORE spawning AI (deterministic)
    #[serde(default)]
    execution_steps: Vec<ExecutionStepConfig>,

    /// Log sources to capture during execution steps
    #[serde(default)]
    log_sources: Vec<LogSourceConfig>,

    // Multi-session workflow configuration
    /// Path to external checkpoint file for cross-session workflows
    #[serde(default)]
    checkpoint_path: Option<String>,
    /// JSON field name in checkpoint that tracks current phase (default: "current_phase")
    #[serde(default = "default_phase_field")]
    phase_field: String,
    /// Workflow is complete when phase_field reaches this value
    #[serde(default)]
    completion_value: Option<u32>,

    // Context injection
    /// Context IDs to explicitly include in the prompt
    #[serde(default)]
    context_ids: Vec<String>,
    /// Whether to auto-detect and include relevant contexts
    #[serde(default)]
    auto_include_contexts: bool,

    // AI provider override
    /// AI provider override (e.g., "claude_cli", "gemini_api")
    #[serde(default)]
    provider: Option<String>,
    /// AI model override (e.g., "claude-sonnet-4-20250514", "gemini-2.0-flash")
    #[serde(default)]
    model: Option<String>,

    /// Whether to capture input for validation (comparing reported vs actual positions)
    #[serde(default)]
    capture_input_validation: bool,
}

fn default_phase_field() -> String {
    "current_phase".to_string()
}

fn default_session_timeout() -> u64 {
    1800
}

/// Response from starting a session
#[derive(Debug, Serialize)]
struct StartSessionResponse {
    session: Session,
}

/// List all unified sessions
async fn list_sessions(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<Vec<Session>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let sessions = state.session.list_sessions().await;
    Ok(Json(ApiResponse::success(sessions)))
}

/// Get a specific session
async fn get_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Session>>, (StatusCode, Json<ApiResponse<()>>)> {
    match state.session.get_session(&session_id).await {
        Some(session) => Ok(Json(ApiResponse::success(session))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Session {} not found", session_id))),
        )),
    }
}

/// Start a new unified session
async fn start_session(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartSessionRequest>,
) -> Result<Json<ApiResponse<StartSessionResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Clone values BEFORE moving them into config
    let session_name = request.name.clone();
    let prompt_for_task_run = request.prompt.clone();

    // Serialize execution steps for storage (used for re-execution on resume)
    let execution_steps_json = if !request.execution_steps.is_empty() {
        serde_json::to_string(&request.execution_steps).ok()
    } else {
        None
    };
    let log_sources_json = if !request.log_sources.is_empty() {
        serde_json::to_string(&request.log_sources).ok()
    } else {
        None
    };

    // Multi-session workflow config
    let workflow_checkpoint_path = request.checkpoint_path.clone();
    let workflow_phase_field = request.phase_field.clone();
    let workflow_completion_value = request.completion_value;

    // Debug: Log the workflow config values received
    info!(
        "Session workflow config: checkpoint_path={:?}, phase_field={:?}, completion_value={:?}",
        workflow_checkpoint_path, workflow_phase_field, workflow_completion_value
    );

    // Clear existing checkpoint file for fresh workflow start
    // This ensures new workflow runs don't resume from old checkpoints
    if let Some(ref cp_path) = workflow_checkpoint_path {
        let checkpoint_path = std::path::PathBuf::from(cp_path);
        if checkpoint_path.exists() {
            info!(
                "Clearing existing checkpoint file for fresh workflow start: {:?}",
                checkpoint_path
            );
            if let Err(e) = std::fs::remove_file(&checkpoint_path) {
                warn!("Failed to remove old checkpoint file: {}", e);
            }
        }
    }

    // Execute deterministic steps BEFORE spawning AI session
    // This runs workflows, actions, screenshots, etc. and collects results
    // Uses the unified StepExecutor module (same code path as /execute-steps endpoint)
    let execution_result = if !request.execution_steps.is_empty() {
        info!(
            "Executing {} deterministic steps before AI session",
            request.execution_steps.len()
        );
        // Use a temporary session ID for screenshot naming
        let temp_session_id = format!(
            "pre-{}-{}",
            chrono::Utc::now().format("%Y%m%d%H%M%S"),
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("0000")
        );
        let executor = StepExecutor::new(state.app_state.clone(), state.config_storage.clone());
        executor
            .execute_steps_with_log_sources(
                &request.execution_steps,
                &temp_session_id,
                &request.log_sources,
            )
            .await
    } else {
        ExecutionResult {
            success: true,
            total_steps: 0,
            successful_steps: 0,
            failed_steps: 0,
            total_duration_ms: 0,
            steps: Vec::new(),
            captured_logs: None,
            captured_runner_logs: None,
        }
    };

    // Generate execution summary using iteration bundle
    // This provides structured, filtered data with step-linked screenshots
    let execution_summary = if !execution_result.steps.is_empty() {
        // Determine which logs are relevant based on step types
        let relevant_logs = RelevantLogSources::from_steps(&request.execution_steps);

        // New session starts at iteration 1
        let iteration = 1u32;

        // Use session name as temporary ID (actual ID will be assigned when session starts)
        let temp_task_id = session_name.clone();

        // Create the iteration bundle
        let timestamp_now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut bundle = IterationBundle::new(
            iteration,
            temp_task_id,
            timestamp_now.clone(),
            timestamp_now,
            &execution_result,
            &relevant_logs,
        );

        // Add application logs if captured
        if let Some(ref captured) = execution_result.captured_logs {
            bundle.add_application_logs(&captured.sources, &request.log_sources);
        }

        // Add runner logs if captured (GUI automation events)
        if let Some(ref runner_logs) = execution_result.captured_runner_logs {
            bundle.add_gui_automation_logs(
                runner_logs.actions.clone(),
                runner_logs.image_recognition.clone(),
            );

            // Add input validation data if enabled
            if request.capture_input_validation {
                let input_validation = iteration_bundle::collect_input_validation_from_actions(
                    &runner_logs.actions,
                    &session_name,
                );
                bundle.add_input_validation_logs(input_validation);
            }
        }

        // Render to markdown
        bundle.to_markdown()
    } else {
        String::new()
    };

    // Inject contexts into the prompt if requested
    let final_prompt = if !request.context_ids.is_empty() || request.auto_include_contexts {
        // Extract action types from loaded config for auto-detection
        let action_types: Vec<String> = {
            let config_lock = state.app_state.current_config.lock().unwrap();
            if let Some(ref config) = *config_lock {
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

        let recent_errors: Vec<String> = Vec::new();

        let (enhanced_prompt, used_context_ids) = context::inject_contexts(
            &request.prompt,
            &request.context_ids,
            request.auto_include_contexts,
            &request.prompt, // Use prompt for auto-detection matching
            &action_types,
            &recent_errors,
        );

        if !used_context_ids.is_empty() {
            info!(
                "Session start: Injected {} contexts into prompt: {:?}",
                used_context_ids.len(),
                used_context_ids
            );
        }

        enhanced_prompt
    } else {
        request.prompt.clone()
    };

    // Append execution summary to prompt if steps were executed
    // If no steps were configured, add a warning so the AI knows
    let final_prompt_with_results = if !execution_summary.is_empty() {
        format!("{}\n{}", final_prompt, execution_summary)
    } else if request.execution_steps.is_empty() {
        // No execution steps were configured - this may be intentional or a bug
        format!(
            "{}\n\n\
            ## ⚠️ No Pre-Execution Steps Configured\n\n\
            This task was started WITHOUT execution steps. This means:\n\
            - No GUI automation, screenshots, or tests were run\n\
            - There is no Iteration Bundle with pre-execution results\n\n\
            If this task SHOULD have pre-execution steps (e.g., clicking buttons, \
            capturing screenshots, running Playwright tests), the UI that started \
            this task needs to include the `execution_steps` parameter.\n\n\
            If this is intentional (e.g., a pure code analysis task), ignore this warning.\n",
            final_prompt
        )
    } else {
        // Steps were configured but produced no summary (execution failed?)
        format!(
            "{}\n\n\
            ## ⚠️ Pre-Execution Steps Produced No Results\n\n\
            Execution steps were configured but produced no Iteration Bundle. \
            This may indicate the steps failed or returned empty output.\n\
            Check the runner logs at `.dev-logs/runner-*.jsonl` for details.\n",
            final_prompt
        )
    };

    let config = SessionConfig {
        prompt: final_prompt_with_results,
        continuation_prompt: request.continuation_prompt,
        total_phases: request.total_phases,
        uses_gui: request.uses_gui,
        timeout_seconds: request.timeout_seconds,
        stall_threshold_seconds: 300,
        name: request.name,
        description: String::new(),
        custom_config: serde_json::json!({}),
        provider: request.provider,
        model: request.model,
    };

    match state.session.start_session(config).await {
        Ok(session) => {
            info!("Started unified session: {}", session.id);

            // Create task_run record for auto-continue tracking
            // This is critical - without this, auto-continue won't work
            // Store execution_steps_json and log_sources_json so they can be re-executed on resume
            let task_run_id = session.id.clone();
            if let Err(e) = state.app_state.checkpoint_db.create_task_run_with_config(
                &task_run_id,
                &session_name,
                Some(&prompt_for_task_run),
                "task", // task_type
                None,   // config_id
                None,   // workflow_name
                None,   // max_sessions - None means unlimited
                None,   // auto_continue - defaults to true
                execution_steps_json.clone(),
                log_sources_json.clone(),
            ) {
                warn!(
                    "Failed to create task_run for session {}: {}",
                    task_run_id, e
                );
                // Continue anyway - session will still run, just without auto-continue tracking
            } else {
                info!("Created task_run {} for session tracking", task_run_id);
            }

            // Spawn the execution loop
            let session_id = session.id.clone();
            let state_clone = state.clone();
            let workspace_root = get_workspace_paths_internal()
                .map(|(root, _, _)| root.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());

            tokio::spawn(async move {
                // =====================================================================
                // CROSS-SESSION CONTINUATION LOOP (Deterministic, Runner-Managed)
                // =====================================================================
                //
                // This outer loop handles continuation ACROSS sessions (spawning new
                // sessions when one ends). This is different from the inner phase loop
                // in run_unified_session_loop which handles phases WITHIN a single session.
                //
                // Why this is here (not in the AI):
                // - The AI might crash, timeout, or hit context limits mid-work
                // - The runner ALWAYS runs after the session ends
                // - The runner can reliably check the checkpoint and continue
                // - The AI just needs to save progress; the runner handles continuation
                //
                // Multi-session workflows are configured with:
                // - checkpoint_path: Path to the checkpoint JSON file
                // - phase_field: JSON field name containing current phase (e.g., "current_phase")
                // - completion_value: Workflow is complete when phase_field >= this value
                //
                // Max sessions is a safety limit to prevent infinite loops.
                // =====================================================================

                // Simple session execution - task_runs table handles state tracking
                // The TaskMonitor captures output to task_runs.output_log
                // [TASK_COMPLETE] in output signals completion
                // Startup resume handles continuation after restart

                info!("Starting session '{}' (id: {})", session_name, session_id);

                // Log session start
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                    .open(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\workflow-debug.log") {
                    use std::io::Write;
                    let _ = writeln!(f, "[{}] START_SESSION: name={}, id={}",
                        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                        session_name, session_id);
                }

                // Create session context for grouping output
                let session_ctx = AiOutputSessionContext {
                    session_id: Some(session_id.clone()),
                    session_name: Some(session_name.clone()),
                };

                // Run the session
                run_unified_session_loop(
                    state_clone.clone(),
                    session_id.clone(),
                    workspace_root.clone(),
                    None, // No external checkpoint - database tracks state
                    Some(session_ctx),
                    None, // No task_run_id - use session_id
                )
                .await;

                // Safety net: ensure database status is updated when session loop exits
                // The session loop should handle this, but we add a fallback in case of
                // early returns or errors that don't update the status
                if let Ok(db) = CheckpointDb::new() {
                    if let Ok(Some(task)) = db.get_task_run(&session_id) {
                        if task.status == "running" {
                            info!("Session '{}' exited but status still 'running', marking as complete", session_name);
                            if let Err(e) = db.complete_task_run(&session_id) {
                                warn!("Failed to mark session {} as complete: {}", session_id, e);
                            }
                        }
                    }
                }

                info!("Session '{}' completed", session_name);
            });

            Ok(Json(ApiResponse::success(StartSessionResponse { session })))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to start session: {}", e))),
        )),
    }
}

/// Stop a unified session
async fn stop_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Option<Session>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session = state
        .session
        .stop_session(&session_id, "Stopped by user")
        .await;
    Ok(Json(ApiResponse::success(session)))
}

/// Delete a unified session
async fn delete_session(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<()>>, (StatusCode, Json<ApiResponse<()>>)> {
    state.session.remove_session(&session_id).await;
    Ok(Json(ApiResponse::success(())))
}

/// Check if AI output contains goal completion markers
/// Returns true if any marker indicates the goal has been achieved
fn check_goal_completion_markers(output: &str) -> bool {
    // List of markers that indicate goal completion
    // Claude should output one of these when the goal is achieved
    // NOTE: [TASK_COMPLETE] is the canonical marker used by TaskMonitor and prompts
    let completion_markers = [
        "[TASK_COMPLETE]", // Primary marker - used by TaskMonitor and prompts
        "[GOAL_COMPLETE]",
        "[GOAL_ACHIEVED]",
        "[STOP_SESSION]",
        "[SESSION_COMPLETE]",
    ];

    // Check for explicit markers (case-insensitive)
    let output_upper = output.to_uppercase();
    for marker in &completion_markers {
        if output_upper.contains(marker) {
            info!("Goal completion marker detected: {}", marker);
            return true;
        }
    }

    // Also check for common completion patterns in structured output
    // These patterns appear in Claude's session summaries
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
                info!("Goal completion pattern detected: {}", pattern);
                return true;
            }
        }
    }

    false
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

/// Response for resumable workflow check
#[derive(Debug, Serialize)]
struct ResumableWorkflowInfo {
    /// Whether a resumable workflow exists
    has_resumable: bool,
    /// Whether an AI workflow is currently running (prevents Continue button)
    is_running: bool,
    /// Whether auto-continue on restart is enabled (global setting)
    auto_continue_enabled: bool,
    /// Workflow name (if resumable)
    name: Option<String>,
    /// Current phase/iteration
    current_phase: Option<u32>,
    /// Total phases (0 = unlimited)
    total_phases: Option<u32>,
    /// When the workflow was started
    started_at: Option<String>,
    /// Number of cross-session continuations
    cross_session_count: Option<u32>,
    /// Status from checkpoint
    status: Option<String>,
    /// All currently active sessions (for concurrent session display)
    #[serde(default)]
    active_sessions: Vec<ActiveSessionInfo>,
}

/// Get information about any resumable workflow
/// Uses task_runs table from database - no more external checkpoint files
async fn get_resumable_workflow(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<ResumableWorkflowInfo>> {
    // Check if AI is currently running (use async version to avoid blocking)
    let has_running_tasks = has_running_ai_tasks_async(state.app_state.checkpoint_db.clone()).await;

    // Also check session manager for running sessions
    let sessions = state.session.list_sessions().await;
    let has_running_session = sessions.iter().any(|s| {
        matches!(
            s.status,
            crate::session::SessionStatus::Running
                | crate::session::SessionStatus::WaitingForContinuation
        )
    });

    let is_running = has_running_tasks || has_running_session;

    // Get the global auto-continue setting
    let auto_continue_enabled = settings::get_auto_continue_ai_workflow();

    // Get running tasks from database
    let db = match CheckpointDb::new() {
        Ok(db) => db,
        Err(_) => {
            return Json(ApiResponse::success(ResumableWorkflowInfo {
                has_resumable: false,
                is_running,
                auto_continue_enabled,
                name: None,
                current_phase: None,
                total_phases: None,
                started_at: None,
                cross_session_count: None,
                status: None,
                active_sessions: vec![],
            }));
        }
    };

    let running_tasks = db.get_running_task_runs().unwrap_or_default();

    // Build active_sessions from database running tasks (uses task_run_id which matches AI output)
    let active_sessions: Vec<ActiveSessionInfo> = running_tasks
        .iter()
        .map(|t| ActiveSessionInfo {
            id: t.id.clone(),
            name: t.task_name.clone(),
            status: t.status.clone(),
            started_at: t.created_at.clone(),
            uses_gui: false, // Database doesn't track this, default to false
        })
        .collect();

    if running_tasks.is_empty() {
        return Json(ApiResponse::success(ResumableWorkflowInfo {
            has_resumable: false,
            is_running,
            auto_continue_enabled,
            name: None,
            current_phase: None,
            total_phases: None,
            started_at: None,
            cross_session_count: None,
            status: None,
            active_sessions,
        }));
    }

    // Return info about the most recent running task
    let task = &running_tasks[0];
    Json(ApiResponse::success(ResumableWorkflowInfo {
        has_resumable: true,
        is_running,
        auto_continue_enabled,
        name: Some(task.task_name.clone()),
        current_phase: Some(task.sessions_count),
        total_phases: task.max_sessions,
        started_at: Some(task.created_at.clone()),
        cross_session_count: Some(task.sessions_count),
        status: Some(task.status.clone()),
        active_sessions,
    }))
}

/// Response type for resume workflow
#[derive(Debug, Serialize)]
struct ResumeWorkflowResponse {
    message: String,
    name: String,
}

/// Manually resume running tasks from database
async fn resume_workflow(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<ResumeWorkflowResponse>> {
    // Check if AI analysis is already running (use async version to avoid blocking)
    if has_running_ai_tasks_async(state.app_state.checkpoint_db.clone()).await {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some(
                "AI analysis is already running. Stop it first before resuming.".to_string(),
            ),
        });
    }

    // Get running tasks from database
    let db = match CheckpointDb::new() {
        Ok(db) => db,
        Err(e) => {
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Failed to open database: {}", e)),
            });
        }
    };

    let running_tasks = match db.get_running_task_runs() {
        Ok(tasks) => tasks,
        Err(e) => {
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Failed to get running tasks: {}", e)),
            });
        }
    };

    if running_tasks.is_empty() {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some("No running tasks to resume".to_string()),
        });
    }

    let task_name = running_tasks[0].task_name.clone();
    info!("Manually resuming {} running task(s)", running_tasks.len());

    // Resume all running tasks
    let state_clone = state.clone();
    tokio::spawn(async move {
        resume_all_running_tasks_on_startup(state_clone).await;
    });

    Json(ApiResponse::success(ResumeWorkflowResponse {
        message: format!("Resuming {} task(s)", running_tasks.len()),
        name: task_name,
    }))
}

/// Request body for force continue
#[derive(Debug, Deserialize)]
struct ForceContinueRequest {
    /// Optional task run ID to continue (if not provided, continues most recent running task)
    #[serde(default)]
    task_run_id: Option<String>,
    /// Optional custom continuation prompt (if not provided, uses a default)
    #[serde(default)]
    prompt: Option<String>,
}

/// Response type for force continue
#[derive(Debug, Serialize)]
struct ForceContinueResponse {
    message: String,
    session_id: String,
}

/// Force continue a session that stopped unexpectedly
/// This creates a new session with context from the last AI output
/// If an active workflow exists, it uses the checkpoint system for proper cross-session continuation
async fn force_continue_session(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ForceContinueRequest>,
) -> Json<ApiResponse<ForceContinueResponse>> {
    // Check if AI analysis is already running (use async version to avoid blocking)
    if has_running_ai_tasks_async(state.app_state.checkpoint_db.clone()).await {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some("AI is already running. Stop it first.".to_string()),
        });
    }

    // Check session manager for running sessions
    let sessions = state.session.list_sessions().await;
    let has_running = sessions.iter().any(|s| {
        matches!(
            s.status,
            crate::session::SessionStatus::Running
                | crate::session::SessionStatus::WaitingForContinuation
        )
    });
    if has_running {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some("A session is already running.".to_string()),
        });
    }

    // Check for running tasks in the database - use output_log for context
    let db = match CheckpointDb::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("Could not open database: {}", e);
            // Fall through to simple continuation
            return force_continue_simple(state, request).await;
        }
    };

    // If a specific task_run_id is provided, look up that task
    // Otherwise, fall back to the most recent running task
    let task = if let Some(ref task_run_id) = request.task_run_id {
        match db.get_task_run(task_run_id) {
            Ok(Some(t)) => Some(t),
            Ok(None) => {
                warn!("Task run with id '{}' not found", task_run_id);
                None
            }
            Err(e) => {
                warn!("Failed to get task run '{}': {}", task_run_id, e);
                None
            }
        }
    } else {
        // Fall back to most recent running task
        db.get_running_task_runs()
            .unwrap_or_default()
            .into_iter()
            .next()
    };

    if let Some(task) = task {
        info!(
            "Force continue: Using task '{}' (id: {}), using consolidated AI output for context",
            task.task_name, task.id
        );

        let workspace_root = get_workspace_paths_internal()
            .map(|(root, _, _)| root.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());

        // Get consolidated AI output for the task run
        // This groups output by source (claude, prompt) with timestamps for better readability
        let consolidated_output = log_consolidation::get_consolidated_output_for_task(
            &task.created_at,
            task.completed_at.as_deref(),
        );

        // Get findings context from database for this task run
        let findings_context = state
            .app_state
            .checkpoint_db
            .format_findings_for_continuation_prompt(&task.id)
            .unwrap_or_else(|e| {
                warn!("Failed to get findings context: {}", e);
                String::new()
            });

        // Create continuation prompt using task's prompt, consolidated output, and findings
        let continuation_prompt = request.prompt.unwrap_or_else(|| {
            let mut prompt = format!(
                "{}\n\n## Force Continue (Session #{})\n\n\
                The previous session was interrupted. Here's the output from the previous session:\n\n\
                {}\n\n",
                task.prompt.as_deref().unwrap_or(""),
                task.sessions_count + 1,
                consolidated_output
            );

            // Include findings context if there are any findings
            if !findings_context.is_empty() {
                prompt.push_str(&findings_context);
                prompt.push_str("\n\n");
            }

            prompt.push_str(FINDING_INSTRUCTIONS);
            prompt.push_str("\nContinue the task from where you left off. When complete, print [TASK_COMPLETE].");
            prompt
        });

        // Create session name
        let session_name = format!(
            "{} (Force Continue #{})",
            task.task_name,
            task.sessions_count + 1
        );

        let config = crate::session::SessionConfig {
            prompt: continuation_prompt,
            continuation_prompt: None,
            total_phases: 1,
            uses_gui: false,
            timeout_seconds: 1800,
            stall_threshold_seconds: 300,
            name: session_name.clone(),
            description: format!("Force continued session #{}", task.sessions_count + 1),
            custom_config: serde_json::json!({}),
            provider: None, // Use default provider for continued sessions
            model: None,
        };

        // Get task_id for context grouping
        let task_id = task.id.clone();

        // Note: sessions_count will be incremented when output is appended via append_task_output

        match state.session.start_session(config).await {
            Ok(session) => {
                let session_id = session.id.clone();
                let state_clone = state.clone();
                let sid = session_id.clone();

                // Create session context for grouping output
                let run_ctx = AiOutputSessionContext {
                    session_id: Some(task_id),
                    session_name: Some(session_name),
                };

                // Spawn the execution
                tokio::spawn(async move {
                    run_unified_session_loop(
                        state_clone,
                        sid,
                        workspace_root,
                        None,
                        Some(run_ctx),
                        None,
                    )
                    .await;
                });

                return Json(ApiResponse::success(ForceContinueResponse {
                    message: "Force continue from running task".to_string(),
                    session_id,
                }));
            }
            Err(e) => {
                return Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some(format!("Failed to start session: {}", e)),
                });
            }
        }
    }

    // No running tasks - use simple one-shot continuation
    force_continue_simple(state, request).await
}

/// Simple force continue without task context - reads ai-output.jsonl for context
async fn force_continue_simple(
    state: Arc<ApiState>,
    request: ForceContinueRequest,
) -> Json<ApiResponse<ForceContinueResponse>> {
    // Fallback: No running task - use simple one-shot continuation
    info!("Force continue: No active workflow config found. Using simple one-shot continuation.");

    // Read the last AI output to provide context
    let ai_output_path = std::path::PathBuf::from(
        "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/ai-output.jsonl",
    );
    let last_lines = if ai_output_path.exists() {
        match std::fs::read_to_string(&ai_output_path) {
            Ok(content) => {
                // Get the last 50 lines of AI output for context
                let lines: Vec<&str> = content.lines().collect();
                let start = if lines.len() > 50 {
                    lines.len() - 50
                } else {
                    0
                };
                let recent_output: Vec<String> = lines[start..]
                    .iter()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .filter_map(|v| {
                        v.get("line")
                            .and_then(|l| l.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                recent_output.join("\n")
            }
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    // Create continuation prompt
    let continuation_prompt = request.prompt.unwrap_or_else(|| {
        if last_lines.is_empty() {
            "Continue from where you left off. If you're unsure, check the last few messages in the conversation.".to_string()
        } else {
            format!(
                "The previous session was cut off. Here's the recent context:\n\n---\n{}\n---\n\nPlease continue from where you left off. Complete any unfinished work.",
                if last_lines.len() > 3000 {
                    format!("...{}", &last_lines[last_lines.len() - 3000..])
                } else {
                    last_lines
                }
            )
        }
    });

    info!(
        "Force continuing session with {} chars of context",
        continuation_prompt.len()
    );

    // Create a session to continue
    let config = crate::session::SessionConfig {
        prompt: continuation_prompt,
        continuation_prompt: None,
        total_phases: 1,
        uses_gui: false,
        timeout_seconds: 1800,
        stall_threshold_seconds: 300,
        name: "Force Continue".to_string(),
        description: "Manually continued session".to_string(),
        custom_config: serde_json::json!({}),
        provider: None, // Use default provider for force continue
        model: None,
    };

    match state.session.start_session(config).await {
        Ok(session) => {
            let session_id = session.id.clone();

            // Spawn the execution loop
            let state_clone = state.clone();
            let sid = session_id.clone();
            let workspace_root = get_workspace_paths_internal()
                .map(|(root, _, _)| root.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string());

            tokio::spawn(async move {
                run_unified_session_loop(state_clone, sid, workspace_root, None, None, None).await;
            });

            Json(ApiResponse::success(ForceContinueResponse {
                message: "Force continue session started".to_string(),
                session_id,
            }))
        }
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Failed to start session: {}", e)),
        }),
    }
}

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

/// Resume ALL running tasks from the database on startup.
///
/// This is the single, clean system for auto-continue:
/// 1. Query task_runs WHERE status = 'running'
/// 2. For EACH running task, spawn a continuation session
/// 3. The AI reads output_log to understand context and continue
///
/// Returns the number of tasks resumed.
pub async fn resume_all_running_tasks_on_startup(state: Arc<ApiState>) -> usize {
    // Open the database
    let db = match CheckpointDb::new() {
        Ok(db) => db,
        Err(e) => {
            warn!("Failed to open database for task resume: {}", e);
            return 0;
        }
    };

    // Get all running task runs
    let running_tasks = match db.get_running_task_runs() {
        Ok(tasks) => tasks,
        Err(e) => {
            warn!("Failed to get running task runs: {}", e);
            return 0;
        }
    };

    if running_tasks.is_empty() {
        info!("No running tasks found in database to resume");
        return 0;
    }

    // Log to debug file
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\workflow-debug.log")
    {
        use std::io::Write;
        let _ = writeln!(
            f,
            "[{}] STARTUP_RESUME: Found {} running task(s) in database",
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
            running_tasks.len()
        );
    }

    info!("Found {} running task(s) to resume", running_tasks.len());

    // Load the last configuration before resuming (for visual automation)
    // Must use spawn_blocking because load_config_internal accesses python_bridge
    // which uses block_on internally - cannot call block_on from async context
    if let Some(config_path) = settings::get_last_config_path() {
        info!("Loading last config before resume: {}", config_path);
        let app_state_clone = state.app_state.clone();
        let config_path_clone = config_path.clone();
        let load_result = tokio::task::spawn_blocking(move || {
            load_config_internal(&app_state_clone, &config_path_clone)
        })
        .await;

        match load_result {
            Ok(Ok(_)) => {
                info!("Successfully loaded config for resume");
            }
            Ok(Err(e)) => {
                warn!(
                    "Failed to load last config: {}. Visual automation may fail.",
                    e
                );
            }
            Err(e) => {
                warn!(
                    "spawn_blocking failed for config load: {}. Visual automation may fail.",
                    e
                );
            }
        }
    }

    let mut resumed_count = 0;

    // Resume EACH running task
    for task in &running_tasks {
        info!(
            "Resuming task '{}' (id: {}, session #{})",
            task.task_name,
            task.id,
            task.sessions_count + 1
        );

        // =====================================================================
        // Re-execute deterministic steps BEFORE resuming AI session
        // =====================================================================
        // Parse and re-execute execution_steps_json if present
        // This ensures the AI sees fresh results from workflows/actions/screenshots
        let execution_summary = if let Some(ref steps_json) = task.execution_steps_json {
            match serde_json::from_str::<Vec<ExecutionStepConfig>>(steps_json) {
                Ok(execution_steps) if !execution_steps.is_empty() => {
                    info!(
                        "Re-executing {} deterministic steps for resumed task",
                        execution_steps.len()
                    );

                    // Parse log sources if present
                    let log_sources: Vec<LogSourceConfig> = task
                        .log_sources_json
                        .as_ref()
                        .and_then(|json| serde_json::from_str(json).ok())
                        .unwrap_or_default();

                    // Create a temp session ID for screenshot naming
                    let temp_session_id = format!(
                        "resume-{}-{}",
                        chrono::Utc::now().format("%Y%m%d%H%M%S"),
                        uuid::Uuid::new_v4()
                            .to_string()
                            .split('-')
                            .next()
                            .unwrap_or("0000")
                    );

                    // Execute steps using StepExecutor
                    let executor =
                        StepExecutor::new(state.app_state.clone(), state.config_storage.clone());
                    let execution_result = executor
                        .execute_steps_with_log_sources(
                            &execution_steps,
                            &temp_session_id,
                            &log_sources,
                        )
                        .await;

                    // Generate execution summary using IterationBundle
                    if !execution_result.steps.is_empty() {
                        let relevant_logs = RelevantLogSources::from_steps(&execution_steps);
                        let iteration = task.sessions_count + 1;
                        let timestamp_now =
                            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

                        let mut bundle = IterationBundle::new(
                            iteration,
                            task.id.clone(),
                            timestamp_now.clone(),
                            timestamp_now,
                            &execution_result,
                            &relevant_logs,
                        );

                        // Add application logs if captured
                        if let Some(ref captured) = execution_result.captured_logs {
                            bundle.add_application_logs(&captured.sources, &log_sources);
                        }

                        // Add runner logs if captured
                        if let Some(ref runner_logs) = execution_result.captured_runner_logs {
                            bundle.add_gui_automation_logs(
                                runner_logs.actions.clone(),
                                runner_logs.image_recognition.clone(),
                            );
                        }

                        bundle.to_markdown()
                    } else {
                        String::new()
                    }
                }
                Ok(_) => {
                    // Empty steps array
                    String::new()
                }
                Err(e) => {
                    warn!(
                        "Failed to parse execution_steps_json for task {}: {}",
                        task.id, e
                    );
                    String::new()
                }
            }
        } else {
            String::new()
        };

        // Build continuation prompt with consolidated AI output
        // The AI reads this to understand where to continue from
        // Uses the consolidated format which groups output by source (claude, prompt)
        // with timestamps for better readability
        let consolidated_output = log_consolidation::get_consolidated_output_for_task(
            &task.created_at,
            task.completed_at.as_deref(),
        );

        // Get findings context from database for this task run
        let findings_context = db
            .format_findings_for_continuation_prompt(&task.id)
            .unwrap_or_else(|e| {
                warn!("Failed to get findings context for resume: {}", e);
                String::new()
            });

        // Build continuation prompt including findings context
        // Check if task has execution steps configured (even if summary is empty due to failure)
        let has_execution_steps = task.execution_steps_json.as_ref().is_some_and(|json| {
            serde_json::from_str::<Vec<serde_json::Value>>(json)
                .is_ok_and(|steps| !steps.is_empty())
        });

        // Build appropriate pre-execution message based on:
        // 1. Whether steps are configured (has_execution_steps)
        // 2. Whether execution produced actual results (execution_summary not empty)
        let pre_execution_message = if has_execution_steps && !execution_summary.is_empty() {
            // Steps configured AND produced results - tell AI to look at them
            "\
            **IMPORTANT:** Pre-execution steps (GUI automation, screenshots, tests) were re-run. \
            The Pre-Execution Results section below shows fresh results from this iteration.\n\n\
            Read the previous output below to understand context and continue from where the last session left off.\n\n"
                .to_string()
        } else if has_execution_steps {
            // Steps configured but no results (execution failed or produced empty output)
            "\
            **NOTE:** Pre-execution steps were configured but produced no results. \
            This may indicate execution failed or steps returned empty output. \
            Check the runner logs for details.\n\n\
            Read the previous output below to understand context and continue from where the last session left off.\n\n"
                .to_string()
        } else {
            // No steps configured at all - execution_steps_json is NULL
            // This is a BUG - the task was created without execution steps!
            "\
            **⚠️ WARNING: No execution steps configured (execution_steps_json is NULL)**\n\n\
            This task has NO pre-execution steps. This likely means:\n\
            - The workflow was started from a UI component that doesn't pass execution_steps\n\
            - Or the task was created before the execution_steps fix was applied\n\n\
            **Impact:** No GUI automation, screenshots, or tests were run. You won't have an Iteration Bundle.\n\n\
            **What to do:** Check the task configuration. If this task SHOULD have pre-execution steps, \
            the UI code that started this task needs to include the `execution_steps` parameter.\n\n\
            Read the previous output below to understand context and continue from where the last session left off.\n\n"
                .to_string()
        };

        let mut continuation_prompt = format!(
            "{}\n\n\
            ## Session #{} - Continuation\n\n\
            Pre-execution steps were re-run before this session started.\n\n\
            {}\
            {}\n\n",
            task.prompt.as_deref().unwrap_or(""),
            task.sessions_count + 1,
            pre_execution_message,
            consolidated_output
        );

        // Include findings context if there are any findings
        if !findings_context.is_empty() {
            continuation_prompt.push_str(&findings_context);
            continuation_prompt.push_str("\n\n");
        }

        // Include pre-execution results if steps were re-executed
        if !execution_summary.is_empty() {
            continuation_prompt.push_str(&execution_summary);
            continuation_prompt.push_str("\n\n");
        }

        continuation_prompt.push_str(FINDING_INSTRUCTIONS);
        continuation_prompt
            .push_str("\nContinue the task. When complete, print [TASK_COMPLETE].\n");

        // Create session config
        let session_config = SessionConfig {
            prompt: continuation_prompt,
            continuation_prompt: None,
            total_phases: 1,
            uses_gui: false,
            timeout_seconds: 600,
            stall_threshold_seconds: 300,
            name: format!("{} (resumed)", task.task_name),
            description: format!(
                "Resumed after restart - session #{}",
                task.sessions_count + 1
            ),
            custom_config: serde_json::json!({}),
            provider: None, // Use default provider for resumed sessions
            model: None,
        };

        // Create session context for AI output events so frontend can display the task name
        let session_ctx = AiOutputSessionContext {
            session_id: Some(task.id.clone()),
            session_name: Some(task.task_name.clone()),
        };

        // Start the session
        match state.session.start_session(session_config).await {
            Ok(session) => {
                info!(
                    "Started resume session {} for task '{}'",
                    session.id, task.task_name
                );

                emit_ai_output(
                    &state.app_handle,
                    &format!(
                        "🔄 Resuming '{}' (session #{})",
                        task.task_name,
                        task.sessions_count + 1
                    ),
                    "status",
                    None,
                    Some(&session_ctx),
                );

                // Increment session count
                if let Err(e) = db.append_task_output(&task.id, "", true) {
                    warn!(
                        "Failed to increment session count for task {}: {}",
                        task.id, e
                    );
                }

                // Run the session loop in background
                let session_id = session.id.clone();
                let workspace_root = get_workspace_paths_internal()
                    .map(|(root, _, _)| root.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string());

                let state_clone = state.clone();
                let task_name = task.task_name.clone();
                let task_id = task.id.clone();
                let run_ctx = session_ctx.clone();

                tokio::spawn(async move {
                    run_unified_session_loop(
                        state_clone,
                        session_id.clone(),
                        workspace_root,
                        None,
                        Some(run_ctx),
                        Some(task_id), // Use original task ID for resumed tasks
                    )
                    .await;
                    info!(
                        "Resumed session {} completed for '{}'",
                        session_id, task_name
                    );
                });

                resumed_count += 1;
            }
            Err(e) => {
                error!("Failed to resume task '{}': {}", task.task_name, e);
                emit_ai_output(
                    &state.app_handle,
                    &format!("❌ Failed to resume '{}': {}", task.task_name, e),
                    "error",
                    None,
                    Some(&session_ctx),
                );
            }
        }
    }

    resumed_count
}

/// Run the unified session execution loop
///
/// For multi-session workflows, pass the external checkpoint info so the loop
/// can exit when the external checkpoint advances, allowing cross-session continuation.
///
/// The `task_run_id` parameter is used for database operations (append_task_output,
/// complete_task_run, fail_task_run). When resuming a task, the session_id is a NEW
/// session but we need to update the ORIGINAL task_run record. Pass `Some(task.id)`
/// for resumed tasks, or `None` to use session_id as the task_run_id.
async fn run_unified_session_loop(
    state: Arc<ApiState>,
    session_id: String,
    workspace_root: String,
    external_checkpoint: Option<(std::path::PathBuf, String, u32)>, // (path, phase_field, initial_phase)
    run_ctx: Option<AiOutputSessionContext>, // Context for grouping output into a single Run
    task_run_id: Option<String>, // ID to use for database task_run operations (for resumed tasks)
) {
    // Use task_run_id if provided (for resumed tasks), otherwise use session_id
    let db_task_id = task_run_id.unwrap_or_else(|| session_id.clone());
    let session = match state.session.get_session(&session_id).await {
        Some(s) => s,
        None => {
            error!("Session {} not found for execution", session_id);
            return;
        }
    };

    let config = session.config.clone();
    let timeout = config.timeout_seconds;
    let mut current_prompt = config.prompt.clone();
    let continuation_prompt = config.continuation_prompt.clone();
    let app_handle = state.app_handle.clone();

    // Use provided run context for grouping output, or create a default one for single sessions
    // For multi-session workflows, run_ctx should have the workflow_run_id
    // For single sessions, we use the session_id as the run identifier
    let session_ctx = run_ctx.unwrap_or_else(|| AiOutputSessionContext {
        session_id: Some(session_id.clone()),
        session_name: Some(config.name.clone()),
    });

    info!(
        "Starting unified session loop for {}, run_id: {:?}",
        session_id, session_ctx.session_id
    );

    let mut phase = 0u32;

    loop {
        phase += 1;

        // Check if task was stopped BEFORE starting a new phase
        // This prevents multi-step tasks from restarting after the Stop button is clicked
        if let Ok(Some(task)) = state.app_state.checkpoint_db.get_task_run(&db_task_id) {
            if task.status == "stopped" || !task.auto_continue {
                info!(
                    "Task {} was stopped or auto_continue disabled, exiting session loop (status={}, auto_continue={})",
                    db_task_id, task.status, task.auto_continue
                );
                emit_ai_output(
                    &app_handle,
                    &format!("🛑 Session {} stopped by user", session_id),
                    "status",
                    Some(&session_id),
                    Some(&session_ctx),
                );
                return;
            }
        }

        let phase_session_id = if phase == 1 {
            format!("session-{}", &session_id[..8.min(session_id.len())])
        } else {
            format!(
                "session-{}-phase-{}",
                &session_id[..8.min(session_id.len())],
                phase
            )
        };

        // Update session state to running
        if let Some(mut s) = state.session.get_session(&session_id).await {
            s.status = SessionStatus::Running;
            s.checkpoint.current_phase = phase;
            s.checkpoint.sessions_spawned += 1;
            s.checkpoint.status = "running".to_string();
            s.log_event("phase_started", &format!("Phase {} started", phase));
            let _ = state.session.update_session(s).await;
        }

        // Increment sessions_count in database for this phase
        // This keeps task_run.sessions_count in sync with session.checkpoint.sessions_spawned
        // Use db_task_id (not session_id) for database operations - critical for resumed tasks
        if let Err(e) = state
            .app_state
            .checkpoint_db
            .append_task_output(&db_task_id, "", true)
        {
            warn!(
                "Failed to increment sessions_count for task {}: {}",
                db_task_id, e
            );
        }

        emit_ai_output(
            &app_handle,
            &format!(
                "🚀 Running phase {} (session {})...",
                phase, phase_session_id
            ),
            "status",
            Some(&session_id),
            Some(&session_ctx),
        );

        // Run Claude session
        let workspace = workspace_root.clone();
        let prompt = current_prompt.clone();
        let sid = phase_session_id.clone();
        let handle = app_handle.clone();
        let timeout_secs = timeout;
        let ctx_for_claude = Some(session_ctx.clone());

        // Create finding context for this phase
        // Use db_task_id for finding storage - critical for resumed tasks
        let finding_ctx_for_claude = Some(FindingContext {
            task_run_id: db_task_id.clone(),
            session_num: phase,
        });

        // Clone PID tracker for the blocking task
        let pid_tracker = state.current_ai_pids.clone();

        let result = tokio::task::spawn_blocking(move || {
            run_claude_session_inline(
                &workspace,
                &prompt,
                &sid,
                &handle,
                timeout_secs,
                ctx_for_claude,
                finding_ctx_for_claude,
                Some(pid_tracker),
            )
        })
        .await;

        let session_result = match result {
            Ok(Ok((success, output))) => Ok((success, output)),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Task join error: {}", e)),
        };

        match session_result {
            Ok((success, output)) => {
                if !success {
                    warn!("Phase {} completed with errors, continuing...", phase);
                }

                // Append session output to task_run.output_log in database
                // This preserves output across restarts and enables debugging
                // Limit output to last 50KB to prevent database bloat
                // Use db_task_id (not session_id) - critical for resumed tasks
                let output_to_store = if output.len() > 50_000 {
                    format!(
                        "\n\n=== Phase {} Output (truncated, last 50KB) ===\n{}",
                        phase,
                        &output[output.len() - 50_000..]
                    )
                } else {
                    format!("\n\n=== Phase {} Output ===\n{}", phase, output)
                };
                if let Err(e) = state.app_state.checkpoint_db.append_task_output(
                    &db_task_id,
                    &output_to_store,
                    false,
                ) {
                    warn!("Failed to append output for task {}: {}", db_task_id, e);
                }

                // Note: External checkpoint is now checked by the CROSS-SESSION loop
                // (in start_session's tokio::spawn), not here. This allows:
                // - Better separation of concerns: within-session vs cross-session
                // - The cross-session loop uses configurable completion_value
                // - Sessions end naturally via total_phases or goal markers

                // Check session checkpoint for completion
                if let Some(mut s) = state.session.get_session(&session_id).await {
                    if s.checkpoint.is_complete() {
                        s.status = SessionStatus::Completed;
                        s.checkpoint.mark_completed();
                        let _ = state.session.update_session(s).await;

                        // Update task_run in database to match session status (with retry)
                        // Use db_task_id (not session_id) - critical for resumed tasks
                        if !complete_task_run_with_retry(
                            state.app_state.checkpoint_db.clone(),
                            &db_task_id,
                        )
                        .await
                        {
                            error!(
                                "Failed to mark task_run {} as complete in database - AI status may be stale",
                                db_task_id
                            );
                        }

                        emit_ai_output(
                            &app_handle,
                            &format!("✅ Session {} completed successfully", session_id),
                            "status",
                            Some(&session_id),
                            Some(&session_ctx),
                        );
                        info!("Session {} completed after {} phases", session_id, phase);
                        return;
                    }

                    // Check if AI output indicates goal completion
                    if check_goal_completion_markers(&output) {
                        s.status = SessionStatus::Completed;
                        s.checkpoint.completed = true;
                        s.checkpoint.status = "goal_achieved".to_string();
                        let _ = state.session.update_session(s).await;

                        // Update task_run in database to match session status (with retry)
                        // Use db_task_id (not session_id) - critical for resumed tasks
                        if !complete_task_run_with_retry(
                            state.app_state.checkpoint_db.clone(),
                            &db_task_id,
                        )
                        .await
                        {
                            error!(
                                "Failed to mark task_run {} as complete in database - AI status may be stale",
                                db_task_id
                            );
                        }

                        emit_ai_output(
                            &app_handle,
                            &format!(
                                "🎯 Session {} completed - goal achieved after {} phases",
                                session_id, phase
                            ),
                            "status",
                            Some(&session_id),
                            Some(&session_ctx),
                        );
                        info!(
                            "Session {} completed early - goal achieved after {} phases",
                            session_id, phase
                        );
                        return;
                    }

                    // Check if max phases reached
                    if config.total_phases > 0 && phase >= config.total_phases {
                        s.status = SessionStatus::Completed;
                        s.checkpoint.completed = true;
                        s.checkpoint.status = "completed".to_string();
                        let _ = state.session.update_session(s).await;

                        // Update task_run in database to match session status (with retry)
                        // Use db_task_id (not session_id) - critical for resumed tasks
                        if !complete_task_run_with_retry(
                            state.app_state.checkpoint_db.clone(),
                            &db_task_id,
                        )
                        .await
                        {
                            error!(
                                "Failed to mark task_run {} as complete in database - AI status may be stale",
                                db_task_id
                            );
                        }

                        emit_ai_output(
                            &app_handle,
                            &format!(
                                "✅ Session {} completed (reached {} phases)",
                                session_id, phase
                            ),
                            "status",
                            Some(&session_id),
                            Some(&session_ctx),
                        );
                        return;
                    }

                    // Check if external checkpoint has advanced (for multi-session workflows)
                    // This allows the cross-session continuation loop to take over
                    if let Some((ref ext_path, ref ext_field, initial_phase)) = external_checkpoint
                    {
                        if ext_path.exists() {
                            if let Ok(contents) = std::fs::read_to_string(ext_path) {
                                if let Ok(json) =
                                    serde_json::from_str::<serde_json::Value>(&contents)
                                {
                                    let current_ext_phase =
                                        json.get(ext_field).and_then(|v| v.as_u64()).unwrap_or(0)
                                            as u32;

                                    if current_ext_phase > initial_phase {
                                        info!(
                                            "External checkpoint advanced: {} {} -> {}. Exiting internal loop for cross-session continuation.",
                                            ext_field, initial_phase, current_ext_phase
                                        );
                                        s.status = SessionStatus::Completed;
                                        s.checkpoint.completed = true;
                                        s.checkpoint.status = "phase_advanced".to_string();
                                        let _ = state.session.update_session(s.clone()).await;

                                        emit_ai_output(
                                            &app_handle,
                                            &format!(
                                                "📤 Session {} completed phase {} -> {}. Ready for cross-session continuation.",
                                                session_id, initial_phase, current_ext_phase
                                            ),
                                            "status",
                                            Some(&session_id),
                                            Some(&session_ctx),
                                        );
                                        return;
                                    }
                                }
                            }
                        }
                    }

                    // Prepare continuation prompt
                    if let Some(ref cont_prompt) = continuation_prompt {
                        current_prompt = cont_prompt
                            .replace("{phase}", &phase.to_string())
                            .replace("{output}", &output);
                    }

                    s.status = SessionStatus::WaitingForContinuation;
                    s.log_event(
                        "phase_completed",
                        &format!("Phase {} completed, continuing...", phase),
                    );
                    let _ = state.session.update_session(s).await;
                }
            }
            Err(e) => {
                error!("Phase {} failed: {}", phase, e);

                if let Some(mut s) = state.session.get_session(&session_id).await {
                    s.status = SessionStatus::Failed;
                    s.checkpoint.mark_failed(&e);
                    let _ = state.session.update_session(s).await;
                }

                // Update task_run in database to match session status
                // Use db_task_id (not session_id) - critical for resumed tasks
                if let Err(db_err) = state.app_state.checkpoint_db.fail_task_run(&db_task_id, &e) {
                    warn!(
                        "Failed to mark task_run {} as failed: {}",
                        db_task_id, db_err
                    );
                }

                emit_ai_output(
                    &app_handle,
                    &format!("❌ Session {} failed: {}", session_id, e),
                    "error",
                    Some(&session_id),
                    Some(&session_ctx),
                );
                return;
            }
        }

        // Persist state for restart recovery
        let _ = state.session.persist_state().await;
    }
}

// ============================================================================
// Checkpoint HTTP API Handlers
// ============================================================================

use crate::database::{CheckpointData, CheckpointDb, SessionEvent, TaskRun};

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

    state
        .app_state
        .checkpoint_db
        .create_task_run_with_config(
            &id,
            &req.task_name,
            req.prompt.as_deref(),
            task_type,
            req.config_id.as_deref(),
            req.workflow_name.as_deref(),
            req.max_sessions,
            req.auto_continue,
            req.execution_steps_json,
            req.log_sources_json,
        )
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

    state
        .app_state
        .checkpoint_db
        .stop_task_run(&id)
        .map(|_| {
            Json(serde_json::json!({
                "success": true,
                "message": "Task run stopped"
            }))
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

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
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

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
// AI Verification Agent HTTP API Handlers
// ============================================================================

use crate::verification_agent::{
    ExplorationStrategy, StateExplorer, StateMachineGraph, VerificationTask, VerificationTaskConfig,
};

/// Request body for starting verification
#[derive(Debug, Deserialize)]
struct StartVerificationRequest {
    /// Path to the qontinui config file
    config_path: String,
    /// Exploration strategy: "exhaustive", "smoke_test", "regression", "random_walk", "targeted"
    #[serde(default = "default_verification_strategy")]
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

fn default_verification_strategy() -> String {
    "exhaustive".to_string()
}

fn default_capture_screenshots() -> bool {
    true
}

/// Start a verification task
async fn start_verification(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartVerificationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "MCP API: Starting verification for {} with strategy {}",
        request.config_path, request.strategy
    );

    let config = VerificationTaskConfig {
        config_path: request.config_path,
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
    };

    let task = VerificationTask::new(config, state.app_state.clone());

    // Run the task
    match task.execute().await {
        Ok(result) => {
            let result_json = serde_json::to_value(&result).unwrap_or_default();
            Ok(Json(ApiResponse::success(result_json)))
        }
        Err(e) => {
            error!("Verification failed: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(api_error(e))))
        }
    }
}

/// Get available verification strategies
async fn get_verification_strategies() -> Json<ApiResponse<serde_json::Value>> {
    let strategies = serde_json::json!({
        "strategies": [
            {
                "id": "exhaustive",
                "name": "Exhaustive",
                "description": "Visit every state and transition - complete but slow",
                "recommended_for": "Full verification runs, nightly builds"
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
                "description": "Verify only specific states/transitions",
                "recommended_for": "Specific feature verification"
            }
        ]
    });

    Json(ApiResponse::success(strategies))
}

/// Preview verification plan without executing
async fn preview_verification(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<StartVerificationRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Load the current config
    let config_lock = state.app_state.current_config.lock().unwrap();
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
        "transitions_to_verify": path.transitions.len(),
        "estimated_cost": path.estimated_cost,
        "states": path.states,
        "transitions": path.transitions,
    });

    Ok(Json(ApiResponse::success(plan)))
}

/// Get verification history
async fn get_verification_history(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<ApiResponse<serde_json::Value>> {
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    let reports_dir = std::path::PathBuf::from(
        r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\verification",
    );

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
                    .map(|n| n.to_string_lossy().starts_with("verification-report-"))
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

/// Get a specific verification report
async fn get_verification_report(
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let reports_dir = std::path::PathBuf::from(
        r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\verification",
    );
    let report_path = reports_dir.join(format!("verification-report-{}.json", run_id));

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

/// Get AI analysis prompt for a verification report
async fn get_verification_prompt(
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let reports_dir = std::path::PathBuf::from(
        r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\verification",
    );
    let report_path = reports_dir.join(format!("verification-report-{}.json", run_id));

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
// End AI Verification Agent HTTP API Handlers
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
        timeout_seconds: test.timeout_seconds,
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
        "user" | _ => context::get_user_context(&id),
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

    // Create database and task monitor
    let db = Arc::new(
        CheckpointDb::new().expect("Failed to initialize checkpoint database for task monitoring"),
    );
    let task_monitor = Arc::new(TaskMonitor::new(db));

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
        session: Arc::new(SessionManager::new(dev_logs_path)),
        task_monitor,
        current_config_id: std::sync::Mutex::new(None),
        config_storage,
        action_service,
        current_ai_pids: Arc::new(std::sync::Mutex::new(Vec::new())),
    });

    // Restore persisted session state on startup
    let state_for_restore = api_state.clone();
    tokio::spawn(async move {
        // Small delay to let the server fully start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Restore unified session manager state
        if let Err(e) = state_for_restore.session.restore_state().await {
            warn!("Failed to restore session state: {}", e);
        } else {
            let sessions = state_for_restore.session.list_sessions().await;
            let active_count = sessions
                .iter()
                .filter(|s| {
                    matches!(
                        s.status,
                        crate::session::SessionStatus::Running
                            | crate::session::SessionStatus::WaitingForContinuation
                    )
                })
                .count();
            if active_count > 0 {
                info!(
                    "Restored {} session(s), {} active",
                    sessions.len(),
                    active_count
                );
            }
        }

        // Resume running tasks from database after runner restart
        // Simple, clean system: query task_runs WHERE status = 'running'
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        let global_auto_continue = settings::get_auto_continue_ai_workflow();

        // Log to debug file
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\Users\Joshua\Documents\qontinui_parent_directory\.dev-logs\workflow-debug.log",
        ) {
            use std::io::Write;
            let _ = writeln!(
                f,
                "[{}] STARTUP_RESUME_CHECK: global_auto_continue={}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"),
                global_auto_continue
            );
        }

        if global_auto_continue {
            let resumed = resume_all_running_tasks_on_startup(state_for_restore).await;
            if resumed > 0 {
                info!("Resumed {} running task(s) from database", resumed);
            }
        } else {
            info!("Global auto-continue is disabled, skipping task resume");
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
        .route("/health", get(health))
        // Debug endpoints for AI sessions
        .route("/debug/app/errors", get(get_debug_errors))
        .route("/findings/summary", get(get_findings_summary))
        .route("/launch-debug-chrome", post(launch_debug_chrome))
        .route("/status", get(get_status))
        .route("/monitors", get(get_monitors))
        .route("/load-config", post(load_config))
        .route("/load-last-config", post(load_last_config))
        .route("/run-workflow", post(run_workflow))
        .route("/execute-steps", post(execute_steps))
        .route("/stop-execution", post(stop_execution))
        .route("/execute-action", post(execute_action))
        .route("/capture-screenshot", post(capture_screenshot_step))
        // State navigation route
        .route("/go-to-state", post(go_to_state))
        // Web extraction routes
        .route("/extraction/start", post(start_web_extraction))
        .route("/extraction/stop", post(stop_web_extraction))
        .route("/extraction/status", get(get_extraction_status))
        .route(
            "/extraction/:extraction_id/screenshot/:screenshot_id",
            get(get_extraction_screenshot),
        )
        // Vision extraction route (runs Edge Detection, SAM3, OCR on desktop)
        .route("/vision-extraction/extract", post(run_vision_extraction))
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
        // AI Workflow Library routes
        .route("/ai-workflows", get(list_ai_workflows))
        .route("/ai-workflows", post(create_ai_workflow))
        .route("/ai-workflows/search", get(search_ai_workflows))
        .route("/ai-workflows/categories", get(get_ai_workflow_categories))
        .route("/ai-workflows/tags", get(get_ai_workflow_tags))
        .route(
            "/ai-workflows/:id",
            get(get_ai_workflow)
                .put(update_ai_workflow)
                .delete(delete_ai_workflow),
        )
        // GUI Workflow Library routes
        .route("/gui-workflows", get(list_gui_workflows))
        .route("/gui-workflows", post(create_gui_workflow))
        .route("/gui-workflows/search", get(search_gui_workflows))
        .route(
            "/gui-workflows/categories",
            get(get_gui_workflow_categories),
        )
        .route("/gui-workflows/tags", get(get_gui_workflow_tags))
        .route(
            "/gui-workflows/:id",
            get(get_gui_workflow)
                .put(update_gui_workflow)
                .delete(delete_gui_workflow),
        )
        .route("/gui-workflows/:id/run", post(run_gui_workflow))
        // Unified Session routes (replaces workflows and ai-developer)
        .route("/sessions", get(list_sessions))
        .route("/sessions/start", post(start_session))
        .route("/sessions/:id", get(get_session).delete(delete_session))
        .route("/sessions/:id/stop", post(stop_session))
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
        // Workflow resume routes
        .route("/workflow/resumable", get(get_resumable_workflow))
        .route("/workflow/resume", post(resume_workflow))
        .route("/workflow/force-continue", post(force_continue_session))
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
        .route("/task-runs/:id/stop", post(stop_task_run))
        .route(
            "/task-runs/:id/auto-continue",
            get(get_task_auto_continue).put(set_task_auto_continue),
        )
        // Automation Run routes (for MCP/AI access to task_run_automation)
        .route("/runs", get(list_automation_runs))
        .route("/runs/:id", get(get_automation_run))
        // Config Storage routes
        .route("/configs", get(list_configs).post(import_config))
        .route(
            "/configs/:id",
            get(get_stored_config)
                .put(update_stored_config)
                .delete(delete_stored_config),
        )
        .route("/configs/:id/export", post(export_config))
        // AI Verification Agent routes
        .route("/verification/start", post(start_verification))
        .route("/verification/strategies", get(get_verification_strategies))
        .route("/verification/preview", post(preview_verification))
        .route("/verification/history", get(get_verification_history))
        .route("/verification/:run_id", get(get_verification_report))
        .route("/verification/:run_id/prompt", get(get_verification_prompt))
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
