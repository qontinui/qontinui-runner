//! AI Session Management
//!
//! Handles AI analysis session lifecycle: starting, stopping, and monitoring
//! AI-powered sessions. Includes prompt execution, task completion tracking,
//! log migration, and MCP tool context generation.

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::Manager;
use tracing::{error, info, warn};

use crate::context;
use crate::database::{CheckpointDb, CreateTaskRunInput};
use crate::mcp::shared::{
    emit_ai_output, get_workspace_paths_internal, spawn_python_with_console, FINDING_INSTRUCTIONS,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::prompts;
use crate::safe_lock::safe_lock_or_recover;
use crate::settings;

// Re-export AiSessionContext from the canonical location
pub use crate::execution_context::AiSessionContext;
use crate::runtime_env::{AiSessionContextExt, ExecutionContextExt};

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

/// Request to restart the runner (for AI self-healing workflow)
#[derive(Debug, Deserialize)]
pub struct RestartRunnerRequest {
    /// Reason for restart (logged for debugging)
    pub reason: String,
    /// Delay before restart in seconds (default: 3)
    #[serde(default)]
    pub delay_seconds: Option<u64>,
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

// ============================================================================
// Routes
// ============================================================================

/// Build the router for AI session management endpoints.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/stop-ai-analysis", post(stop_ai_analysis))
        .route("/restart-runner", post(restart_runner))
        .route("/prompts/run", post(run_prompt))
}

// ============================================================================
// Handlers
// ============================================================================

/// Stop the currently running AI analysis
///
/// This endpoint stops all running tasks by:
/// 1. Killing all tracked AI process PIDs (the actual Claude CLI processes)
/// 2. Getting running task runs from the database
/// 3. Stopping monitoring for each task
/// 4. Marking tasks as stopped in the database
pub async fn stop_ai_analysis(
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
        let result = crate::process_helpers::no_window("taskkill")
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

    // Close all interactive Claude sessions via SessionManager
    if let Some(session_manager) = state
        .app_handle
        .try_state::<Arc<crate::claude_session::SessionManager>>()
    {
        session_manager.close_all_sessions();
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

    let running_tasks = match db.get_running_task_runs(None) {
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
pub async fn restart_runner(
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
    match db.get_running_task_runs(None) {
        Ok(tasks) => !tasks.is_empty(),
        Err(e) => {
            warn!("Failed to check running tasks: {}", e);
            false
        }
    }
}

/// Check if any AI analysis tasks are currently running (async version).
/// Uses spawn_blocking to avoid blocking the async runtime.
pub async fn has_running_ai_tasks_async(db: Arc<CheckpointDb>) -> bool {
    match tokio::task::spawn_blocking(move || db.get_running_task_runs(None)).await {
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
pub async fn migrate_logs_for_task(
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
pub async fn complete_task_run_with_retry(db: Arc<CheckpointDb>, task_id: &str) -> bool {
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

// get_workspace_paths_internal is now in crate::mcp::shared
// and re-exported at the top of this file

/// Generate MCP tool context documentation for AI sessions.
///
/// This function creates a markdown documentation string describing the available
/// MCP tools for GUI automation, including the specific workflows, states, and
/// images available in the loaded configuration.
pub fn generate_mcp_tool_context(config: &crate::config::QontinuiConfig) -> String {
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

    // SDK Tools - for interacting with UI Bridge SDK-integrated apps
    context.push_str(
        r#"
## Available SDK Tools (UI Bridge)

The following tools interact with SDK-integrated web apps via the runner's HTTP API.
Use these to inspect, interact with, and test web applications that have the UI Bridge SDK installed.

**Content Discovery:** These tools discover both **interactive elements** (buttons, inputs, links)
and **content elements** (headings, paragraphs, labels, metrics, badges, status indicators).
Content elements have a `contentType` field (e.g., `heading`, `paragraph`, `label`, `metric-value`,
`badge`, `status-message`, `description-text`, `list-item`, `table-cell`, `code-block`, `nav-text`)
and may have a `contentRole` from `data-content-role` attributes (e.g., `heading`, `body-text`,
`label`, `metric`, `badge`, `status`, `description`).

**Content filtering** (supported by element/snapshot tools):
- `includeContent` (bool) — include content elements (default: true)
- `contentOnly` (bool) — return only content elements, excluding interactive ones
- `contentRole` (string) — filter to a specific content role

This lets you read page text, find specific metrics/labels/statuses, and verify content changes without screenshots.

### Connection

#### sdk_connect
Connect to a UI Bridge SDK app for element inspection and interaction.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_connect", "url": "http://localhost:3001"}
```

#### sdk_status
Check SDK app connection status. Returns whether connected and app details.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_status"}
```

### Element Inspection

#### sdk_elements
List all registered UI elements (interactive and content) in the connected SDK app.
Returns element IDs, types, labels, state, and contentType/contentRole for content elements.
Accepts optional `includeContent`, `contentOnly`, and `contentRole` filters.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_elements"}
{"tool": "mcp__qontinui__sdk_elements", "contentOnly": true, "contentRole": "metric"}
```

#### sdk_snapshot
Get a complete UI snapshot with all elements (interactive + content) and their current state.
Includes visibility, bounds, text content, available actions, and contentType/contentRole for content elements.
Accepts optional `includeContent`, `contentOnly`, and `contentRole` filters.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_snapshot"}
{"tool": "mcp__qontinui__sdk_snapshot", "contentOnly": true}
```

### AI-Powered Interaction

#### sdk_ai_search
Search for elements (interactive or content) by natural language description.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_search", "text": "Submit button"}
{"tool": "mcp__qontinui__sdk_ai_search", "text": "total revenue metric"}
```

#### sdk_ai_execute
Execute an action by natural language instruction.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_execute", "instruction": "click the Submit button"}
```

#### sdk_ai_assert
Assert element state using natural language.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_ai_assert", "text": "error message", "state": "hidden"}
```

#### sdk_page_summary
Get an AI-friendly summary of the current page, including layout, navigation, and key elements.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_page_summary"}
```

### Screenshots

#### sdk_screenshot
Capture a screenshot of the monitor where the SDK app is running.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_screenshot"}
```

### Per-App Analysis

These tools analyze the currently connected SDK app's page structure and data.
They work on a single app — use them independently or as building blocks.

#### sdk_analyze_data
Extract labeled data values from the page. Each value is classified by type
(text, number, currency, date, email, url, phone, percentage, boolean) and
normalized for comparison.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_data"}
```

#### sdk_analyze_regions
Segment the page into semantic regions: header, navigation, sidebar,
main-content, footer, form, table, card, modal, toolbar. Each region
includes its bounding box and contained element IDs.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_regions"}
```

#### sdk_analyze_structured_data
Detect and extract tables (with column headers and row data) and lists
(with field schemas and items) from the page based on spatial layout patterns.
**Usage:**
```json
{"tool": "mcp__qontinui__sdk_analyze_structured_data"}
```

### Cross-App Comparison

#### sdk_cross_app_compare
Compare two SDK-integrated apps by connecting to each, capturing semantic
snapshots (including content elements), and running a full analysis. Returns
scores (0-1) for data completeness, format alignment, presentation alignment,
navigation parity, action parity, and an overall score. Also returns a
prioritized issue list. Content elements enable text-level comparison across apps.

Set `include_components` to true to also fetch and compare registered
components between the two apps.

**Usage:**
```json
{"tool": "mcp__qontinui__sdk_cross_app_compare", "source_url": "http://localhost:1420", "target_url": "http://localhost:3001", "include_components": true}
```
"#,
    );

    context
}

// Prompt CRUD handlers (list, get, create, update, delete, categories, tags,
// import, export, duplicate, search) moved to crate::mcp::prompts

/// Run a prompt by spawning a Claude session
///
/// Supports two modes:
/// 1. Lookup prompt from database: provide `prompt_id`
/// 2. Ad-hoc prompt: provide `name` and `content`
///
/// Optional image analysis: provide `image_paths`, `video_paths`, or `trace_path`
/// to enhance the prompt with visual analysis data.
pub async fn run_prompt(
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

    let (all_images, trace_timeline) = super::trace_verification::collect_images_for_analysis(
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
    let supervisor_available = super::auto_continue::check_supervisor_available();
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
- POST /workflow-loop/signal-restart - Signal that runner restart is needed (use during workflow loops)

**IMPORTANT:** If you modified qontinui-runner Rust code, use `"rebuild": true` to recompile before restart.

**Workflow Loop Signal:** If you are running inside a supervisor workflow loop and you modify runner code, call:
```powershell
Invoke-RestMethod -Uri "http://localhost:9875/workflow-loop/signal-restart" -Method Post
```
This tells the supervisor to restart the runner between iterations. If you don't signal, the loop skips the restart (saving time when only non-runner repos were changed).

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

// Remaining prompt CRUD handlers (categories, tags, import, export, duplicate, search)
// moved to crate::mcp::prompts

// Macro handlers moved to crate::mcp::macros
// Playwright script handlers moved to crate::mcp::playwright
// Prompt snippet handlers moved to crate::mcp::prompt_snippets
