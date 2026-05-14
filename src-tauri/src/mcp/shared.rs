//! Cross-cutting utilities shared across MCP API modules
//!
//! Contains functions and types used by multiple handler modules:
//! - AI output emission to frontend
//! - Finding/progress context types
//! - Workspace path helpers
//! - Finding instruction constants

use serde::Serialize;
use tauri::Emitter;
use tracing::{info, warn};

use crate::settings;

// Re-export AiSessionContext from the canonical location
pub use crate::execution_context::AiSessionContext;

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

/// Context for reflection fix detection during AI sessions.
/// Contains the information needed to store reflection fixes in the database.
#[derive(Debug, Clone)]
pub struct ReflectionFixContext {
    /// The task run ID being analyzed (source)
    pub source_task_run_id: String,
    /// The reflection workflow's own task run ID
    pub reflection_task_run_id: String,
    /// Project/workspace path (set by project reflection trigger)
    pub project_path: Option<String>,
}

/// Emit AI output event to frontend and persist to file.
///
/// Previously, persistence depended on a fragile round-trip:
///   Rust emit → Tauri event → frontend JS → invoke Tauri command → file write
/// Now we also write directly to ai-output.jsonl from Rust for reliability.
pub fn emit_ai_output(
    app_handle: &tauri::AppHandle,
    line: &str,
    source: &str,
    action_id: Option<&str>,
    session_ctx: Option<&AiSessionContext>,
) {
    let now = chrono::Utc::now().timestamp_millis();
    let event = AiOutputEvent {
        id: format!("ai-{}-{}", now, rand::random::<u32>()),
        timestamp: now,
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

    // Also broadcast to WebSocket clients
    if let Ok(json) = serde_json::to_value(&event) {
        crate::event_system::broadcast_ws_notification(app_handle, "ai-output", &json);
    }

    // Emit the structured ai-output-chunk event for the streaming widget.
    // This provides a task_run_id-keyed stream that the frontend can filter by run.
    if let Some(ctx) = session_ctx {
        if source == "claude" {
            let chunk_event = crate::event_system::AppEvent::ai_output_chunk(
                ctx.task_run_id(),
                line,
                line.len(), // approximate; the frontend accumulates its own total
            );
            let chunk_event_name = chunk_event.event_name();
            if let Err(e) = app_handle.emit(chunk_event_name, &chunk_event) {
                warn!("Failed to emit ai-output-chunk event: {}", e);
            }
            if let Ok(json) = serde_json::to_value(&chunk_event) {
                crate::event_system::broadcast_ws_notification(app_handle, chunk_event_name, &json);
            }
        }
    }

    // Persist directly to ai-output.jsonl (don't rely on frontend round-trip)
    let entry = crate::commands::logging::AiOutputEntry {
        id: event.id,
        timestamp: event.timestamp,
        line: event.line,
        source: event.source,
        action_id: event.action_id,
        task_run_id: event.task_run_id,
        session_id: event.session_id,
        session_name: event.session_name,
        phase: event.phase,
        phase_iteration: event.phase_iteration,
        screenshot_path: None,
        screenshot_width: None,
        screenshot_height: None,
    };
    let _ = crate::commands::logging::append_ai_output_log(entry);
}

/// Write AI debug log to file
#[allow(dead_code)]
pub fn write_ai_debug_log(message: &str) {
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

/// Get the current workspace root directory as a String, if available.
pub fn current_project_path() -> Option<String> {
    get_workspace_paths_internal()
        .ok()
        .map(|(root, _, _)| root.to_string_lossy().to_string())
}

/// Get the runner's own checkout directory (the qontinui-runner repo root,
/// parent of `src-tauri/`). Unlike [`current_project_path`] which returns
/// the umbrella workspace_root containing all sibling repos, this returns a
/// directory that is itself a git repo and that holds the runner's
/// `specs/` tree — the right default for self-supervision triggers.
///
/// Returns `None` only if the runner's exe path doesn't sit under a
/// recognizable runner checkout (effectively never in dev/build/bundled
/// layouts).
pub fn current_runner_path() -> Option<String> {
    let exe_path = std::env::current_exe().ok()?;
    let mut current = exe_path.as_path();
    loop {
        let parent = current.parent()?;
        if parent.join("src-tauri").exists()
            || parent.file_name().is_some_and(|n| n == "qontinui-runner")
        {
            return Some(parent.to_string_lossy().to_string());
        }
        current = parent;
    }
}

/// Get the monorepo root directory (parent of qontinui-runner).
/// This is the directory containing all sibling repos.
pub fn get_monorepo_root() -> Option<String> {
    current_project_path().and_then(|workspace_root| {
        // current_project_path() already returns the workspace root (parent of runner).
        // Verify it exists and contains git repos before returning.
        let root = std::path::Path::new(&workspace_root);
        if root.is_dir() {
            Some(workspace_root)
        } else {
            None
        }
    })
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

// Windows-specific imports for process creation flags
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

// Windows constants for process creation
#[cfg(target_os = "windows")]
const CREATE_NEW_CONSOLE: u32 = 0x00000010;

/// Spawn Python script with proper console on Windows.
/// Claude CLI requires a console window to function properly.
pub(crate) fn spawn_python_with_console(
    python_path: &str,
    args: &[&std::ffi::OsStr],
    working_dir: &std::path::Path,
) -> std::io::Result<std::process::Child> {
    let mut cmd = crate::process_helpers::no_window(python_path);
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

/// Save the current config back to the original file.
/// This is used when project contexts are modified.
pub(crate) fn save_current_config_to_file(
    app_state: &std::sync::Arc<crate::AppState>,
) -> Result<(), String> {
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
