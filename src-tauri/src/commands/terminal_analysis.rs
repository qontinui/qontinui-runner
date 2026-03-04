//! Tauri commands for terminal session analysis.
//!
//! Provides 5 analysis commands that accept terminal content as input,
//! call the AI provider, and return canvas panel JSON for rendering in the runner UI.
//!
//! All commands share the same response shape:
//! ```json
//! { "success": true, "panels": [ { "panel_id", "title", "component", "data" } ] }
//! ```
//!
//! Valid component values: Markdown, Table, FileTree, Timeline, Checklist,
//! FindingList, ProgressChart, KeyValue, Alert, ArchitectureGraph.

use crate::commands::{AppState, CommandResponse};
use crate::doctor::DoctorHandle;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{info, warn};

// ============================================================================
// Shared system-prompt builder
// ============================================================================

/// Prefix appended to every analysis prompt so the model knows the output schema.
const CANVAS_SCHEMA_INSTRUCTION: &str = r#"
Return a JSON object with this exact shape — no markdown fences, no prose outside the JSON:
{
  "panels": [
    {
      "panel_id": "<unique string>",
      "title": "<human-readable title>",
      "component": "<ComponentType>",
      "data": { /* component-specific data — see types below */ }
    }
  ]
}

Valid component types and their data schemas:

Markdown   — { "markdown": "<markdown string>" }
Table      — { "headers": ["col1","col2"], "rows": [["v1","v2"]] }
FileTree   — { "files": [{ "path": "src/foo.ts", "status": "modified"|"added"|"deleted"|"unchanged", "description": "..." }] }
Timeline   — { "events": [{ "timestamp": "...", "label": "...", "description": "...", "type": "info"|"success"|"warning"|"error" }] }
Checklist  — { "items": [{ "label": "...", "checked": true|false, "description": "..." }] }
FindingList— { "findings": [{ "severity": "critical"|"high"|"medium"|"low"|"info", "title": "...", "description": "...", "location": "..." }] }
ProgressChart— { "label": "...", "value": 0-100, "items": [{ "label": "...", "value": 0-100, "status": "done"|"in_progress"|"pending"|"failed" }] }
KeyValue   — { "entries": [{ "key": "...", "value": "..." }] }
Alert      — { "level": "info"|"success"|"warning"|"error", "title": "...", "message": "..." }
ArchitectureGraph — { "nodes": [{ "id": "...", "label": "...", "layer": "frontend"|"backend"|"runner"|"python"|"shared", "description": "...", "changed": true|false }], "edges": [{ "source": "id1", "target": "id2", "label": "...", "type": "dependency"|"api" }] }

Generate 2–6 panels. Each panel must have a unique panel_id. Do NOT wrap output in markdown code blocks.
"#;

// ============================================================================
// Helper: call AI and parse panel JSON
// ============================================================================

fn run_analysis(
    system_prompt: &str,
    user_content: &str,
    doctor_handle: Option<&DoctorHandle>,
) -> Result<serde_json::Value, String> {
    let full_prompt = format!(
        "{}\n\n{}\n\n---\nContent to analyze:\n\n{}",
        CANVAS_SCHEMA_INSTRUCTION, system_prompt, user_content
    );

    let response = crate::ai_provider::run_prompt_sync(&full_prompt, doctor_handle);

    if !response.success {
        return Err(response
            .error
            .unwrap_or_else(|| "AI analysis failed".to_string()));
    }

    // Try to parse the response as JSON.  The model sometimes wraps in fences.
    let raw = response.output.trim();
    let stripped = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str::<serde_json::Value>(stripped).map_err(|e| {
        warn!(
            "Failed to parse AI analysis JSON: {}\nRaw output: {}",
            e,
            &stripped[..stripped.len().min(200)]
        );
        format!("Failed to parse AI response as JSON: {}", e)
    })
}

// ============================================================================
// Command 1: Summarize Session
// ============================================================================

/// Analyze a Claude Code terminal session and produce a session summary.
///
/// Input is the raw scrollback text from the active terminal tab.
/// Returns canvas panels: Markdown summary, Timeline of key events, Checklist of completed tasks, FileTree of changed files.
#[tauri::command]
pub async fn analyze_session_summary(
    app_state: tauri::State<'_, Arc<AppState>>,
    input: String,
) -> Result<CommandResponse, String> {
    info!("analyze_session_summary: input_len={}", input.len());

    if input.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No terminal content to analyze. Run some commands first.".to_string()),
            data: None,
        });
    }

    let doctor_handle = app_state.doctor_handle.lock().await.clone();

    let system_prompt = r#"You are analyzing a Claude Code (AI coding assistant) terminal session.
Produce a clear, structured summary with these panels:
1. Markdown panel: High-level session summary (what was worked on, key outcomes, current status)
2. Timeline panel: Chronological sequence of key events (file edits, tool calls, test runs, errors)
3. Checklist panel: Tasks that were completed (checked) and tasks that are still pending (unchecked)
4. FileTree panel: Files that were created, modified, or deleted (infer from the session content)

Focus on what matters for a developer to quickly understand what happened in this session."#;

    let result = tokio::task::spawn_blocking(move || {
        run_analysis(system_prompt, &input, doctor_handle.as_ref())
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    match result {
        Ok(panels_json) => Ok(CommandResponse {
            success: true,
            message: Some("Session summary generated".to_string()),
            data: Some(panels_json),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        }),
    }
}

// ============================================================================
// Command 2: Architecture
// ============================================================================

/// Analyze plan content or terminal selection and produce an architecture diagram.
///
/// Input is plan content or selected text describing the system being built.
/// Returns canvas panels: ArchitectureGraph (React Flow), KeyValue of component descriptions.
#[tauri::command]
pub async fn analyze_architecture(
    app_state: tauri::State<'_, Arc<AppState>>,
    input: String,
) -> Result<CommandResponse, String> {
    info!("analyze_architecture: input_len={}", input.len());

    if input.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some(
                "No content to analyze. Select text in the terminal or have a plan loaded."
                    .to_string(),
            ),
            data: None,
        });
    }

    let doctor_handle = app_state.doctor_handle.lock().await.clone();

    let system_prompt = r#"You are analyzing code, plans, or documentation to extract the software architecture.
Produce these panels:
1. ArchitectureGraph panel: A graph of components/services as nodes, with edges showing dependencies and API calls.
   - Assign each node a layer: "frontend" (React/Next.js/Vite), "backend" (FastAPI/Express/server), "runner" (Tauri/desktop), "python" (Python libs/scripts), "shared" (packages/libraries/types shared across multiple layers).
   - Set changed=true on nodes that appear to be the focus of current work.
   - Include 3–12 nodes. Focus on meaningful architectural components, not individual files.
2. KeyValue panel: List the key architectural components with a one-line description each.

For the Qontinui project specifically: frontend=Next.js or Vite UI, backend=FastAPI, runner=Tauri Rust+TS desktop app, python=qontinui core library, shared=shared npm packages."#;

    let result = tokio::task::spawn_blocking(move || {
        run_analysis(system_prompt, &input, doctor_handle.as_ref())
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    match result {
        Ok(panels_json) => Ok(CommandResponse {
            success: true,
            message: Some("Architecture diagram generated".to_string()),
            data: Some(panels_json),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        }),
    }
}

// ============================================================================
// Command 3: Change Impact
// ============================================================================

/// Analyze git diff or file change output and produce an impact assessment.
///
/// Input should be git diff output or a list of changed files from the terminal.
/// Returns canvas panels: FileTree of changes, FindingList of risks/issues, Table of affected areas.
#[tauri::command]
pub async fn analyze_change_impact(
    app_state: tauri::State<'_, Arc<AppState>>,
    input: String,
) -> Result<CommandResponse, String> {
    info!("analyze_change_impact: input_len={}", input.len());

    if input.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some(
                "No content to analyze. Select git diff output or file changes in the terminal."
                    .to_string(),
            ),
            data: None,
        });
    }

    let doctor_handle = app_state.doctor_handle.lock().await.clone();

    let system_prompt = r#"You are analyzing code changes (git diff or file listings) to assess their impact.
Produce these panels:
1. FileTree panel: All changed files with status (modified/added/deleted) and a brief description of what changed.
2. FindingList panel: Risks, potential bugs, breaking changes, or areas needing attention. Use severity: critical/high/medium/low/info.
3. Table panel: Impact summary — columns: Area, Files Changed, Risk Level, Notes. Show which subsystems are affected.
4. Alert panel (if there are high/critical findings): A warning alert summarizing the most important concern.

Be specific about what the changes do and what could break."#;

    let result = tokio::task::spawn_blocking(move || {
        run_analysis(system_prompt, &input, doctor_handle.as_ref())
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    match result {
        Ok(panels_json) => Ok(CommandResponse {
            success: true,
            message: Some("Change impact analysis generated".to_string()),
            data: Some(panels_json),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        }),
    }
}

// ============================================================================
// Command 4: Plan Progress
// ============================================================================

/// Analyze plan content against terminal scrollback to assess progress.
///
/// Input is the plan content concatenated with recent terminal scrollback.
/// Returns canvas panels: Checklist of plan items, ProgressChart, Markdown status summary.
#[tauri::command]
pub async fn analyze_plan_progress(
    app_state: tauri::State<'_, Arc<AppState>>,
    input: String,
) -> Result<CommandResponse, String> {
    info!("analyze_plan_progress: input_len={}", input.len());

    if input.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No plan content or terminal activity to analyze.".to_string()),
            data: None,
        });
    }

    let doctor_handle = app_state.doctor_handle.lock().await.clone();

    let system_prompt = r#"You are analyzing an implementation plan alongside terminal session output to assess progress.
The input may contain a plan (markdown headers, checklists, numbered items) followed by terminal activity.
Produce these panels:
1. Checklist panel: Every task/item from the plan, checked=true if it appears to be done, false if pending or in-progress.
2. ProgressChart panel: Overall completion percentage (0-100) based on checked items. Include sub-items for each major section of the plan.
3. Markdown panel: A concise status report — what's done, what's in progress, what's next, any blockers.

Be realistic about completion — only mark something done if there's clear evidence in the terminal output."#;

    let result = tokio::task::spawn_blocking(move || {
        run_analysis(system_prompt, &input, doctor_handle.as_ref())
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    match result {
        Ok(panels_json) => Ok(CommandResponse {
            success: true,
            message: Some("Plan progress analysis generated".to_string()),
            data: Some(panels_json),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        }),
    }
}

// ============================================================================
// Command 5: Cross-Tab (All Sessions)
// ============================================================================

/// Analyze scrollback from all terminal tabs to produce a cross-session overview.
///
/// Input is concatenated scrollback from all tabs, with `--- Tab: {name} ---` headers.
/// Returns canvas panels: one Markdown panel per tab, plus an overall Markdown summary.
#[tauri::command]
pub async fn analyze_cross_tab(
    app_state: tauri::State<'_, Arc<AppState>>,
    input: String,
) -> Result<CommandResponse, String> {
    info!("analyze_cross_tab: input_len={}", input.len());

    if input.trim().is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No terminal content found across tabs.".to_string()),
            data: None,
        });
    }

    let doctor_handle = app_state.doctor_handle.lock().await.clone();

    let system_prompt = r#"You are analyzing multiple Claude Code terminal sessions (tabs) simultaneously.
The input has sections separated by "--- Tab: {name} ---" headers.
Produce these panels:
1. One Markdown panel per tab (use panel_id like "tab-1", "tab-2"): Summarize what that session is working on, its current status, and any notable output.
2. A final Markdown panel (panel_id "cross-tab-summary"): An overall summary across all sessions — what work is happening in parallel, how the sessions relate, and what the developer should focus on next.

Use the tab name as the panel title. Keep each per-tab summary concise (3–6 bullets)."#;

    let result = tokio::task::spawn_blocking(move || {
        run_analysis(system_prompt, &input, doctor_handle.as_ref())
    })
    .await
    .map_err(|e| format!("spawn_blocking failed: {}", e))?;

    match result {
        Ok(panels_json) => Ok(CommandResponse {
            success: true,
            message: Some("Cross-tab analysis generated".to_string()),
            data: Some(panels_json),
        }),
        Err(e) => Ok(CommandResponse {
            success: false,
            message: Some(e),
            data: None,
        }),
    }
}

// ============================================================================
// Plan content loader
// ============================================================================

/// Candidate plan file name patterns (checked in order, case-insensitive prefix match).
const PLAN_PATTERNS: &[&str] = &["plan", "todo", "notes", "roadmap"];

/// Returns true if `name` looks like a plan/notes markdown file.
fn is_plan_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    if !lower.ends_with(".md") {
        return false;
    }
    PLAN_PATTERNS.iter().any(|p| lower.starts_with(p))
}

/// Walk a single directory (non-recursive) and collect plan files with modification times.
fn collect_plan_files(dir: &PathBuf, candidates: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_plan_file(name) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                candidates.push((mtime, path));
            }
        }
    }
}

// ============================================================================
// Command 6: Page Architecture (static — no AI)
// ============================================================================

/// Return a hardcoded architecture diagram of the Terminal page's component hierarchy.
///
/// No input required, no AI call — returns static ArchitectureGraph + KeyValue panels.
#[tauri::command]
pub async fn analyze_page_architecture() -> Result<CommandResponse, String> {
    info!("analyze_page_architecture: returning static page map");

    let panels = serde_json::json!({
        "panels": [
            {
                "panel_id": "page-arch-graph",
                "title": "Terminal Page — Component Architecture",
                "component": "ArchitectureGraph",
                "data": {
                    "nodes": [
                        { "id": "terminal-page",    "label": "TerminalPage",              "layer": "frontend", "changed": true,  "description": "Orchestrator — three-panel layout with tabs, sidebar, and right panel" },
                        { "id": "tab-bar",           "label": "TerminalTabBar",            "layer": "frontend", "changed": false, "description": "Tab management — create, close, rename, and switch terminal tabs" },
                        { "id": "action-bar",        "label": "TerminalActionBar",         "layer": "frontend", "changed": true,  "description": "Sidebar toggle, plan indicator, and analysis action buttons" },
                        { "id": "notification",      "label": "TerminalNotification",      "layer": "frontend", "changed": false, "description": "Auto-dismiss notification bar for success/error messages" },
                        { "id": "terminal-inst",     "label": "TerminalInstance",          "layer": "frontend", "changed": false, "description": "xterm.js terminal emulator (one per tab)" },
                        { "id": "session-sidebar",   "label": "TranscriptSessionSidebar",  "layer": "frontend", "changed": true,  "description": "Left panel — Claude Code session list with plan dot indicators" },
                        { "id": "content-panel",     "label": "TranscriptContentPanel",    "layer": "frontend", "changed": true,  "description": "Right panel — message viewer with workflow generation" },
                        { "id": "analysis-panel",    "label": "TerminalAnalysisPanel",     "layer": "frontend", "changed": true,  "description": "Right panel — canvas-based analysis output renderer" },
                        { "id": "workflow-preview",  "label": "WorkflowPreviewPanel",      "layer": "frontend", "changed": false, "description": "Right panel — generated workflow Execute/Edit/Save actions" },
                        { "id": "use-terminal-mgr",  "label": "useTerminalManager",        "layer": "shared",   "changed": false, "description": "Tab state management hook (create, close, rename, reconnect)" },
                        { "id": "use-transcript",    "label": "useTranscriptSessions",     "layer": "shared",   "changed": true,  "description": "Session list + message loading hook" },
                        { "id": "transcript-rs",     "label": "transcript commands",       "layer": "runner",   "changed": true,  "description": "Rust: list_sessions, read_session, generate_workflow" },
                        { "id": "analysis-rs",       "label": "analysis commands",         "layer": "runner",   "changed": true,  "description": "Rust: 5 AI analysis types + page-architecture (static)" },
                        { "id": "ai-provider",       "label": "AI Provider",               "layer": "backend",  "changed": false, "description": "LLM prompt/response backend for analysis commands" }
                    ],
                    "edges": [
                        { "source": "terminal-page",   "target": "tab-bar",          "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "action-bar",       "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "notification",     "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "terminal-inst",    "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "session-sidebar",  "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "content-panel",    "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "analysis-panel",   "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "workflow-preview", "label": "renders",   "type": "dependency" },
                        { "source": "terminal-page",   "target": "use-terminal-mgr", "label": "uses",     "type": "dependency" },
                        { "source": "terminal-page",   "target": "use-transcript",   "label": "uses",     "type": "dependency" },
                        { "source": "content-panel",   "target": "transcript-rs",    "label": "invoke",   "type": "api" },
                        { "source": "use-transcript",  "target": "transcript-rs",    "label": "invoke",   "type": "api" },
                        { "source": "analysis-panel",  "target": "analysis-rs",      "label": "invoke",   "type": "api" },
                        { "source": "analysis-rs",     "target": "ai-provider",      "label": "LLM call", "type": "api" }
                    ]
                }
            },
            {
                "panel_id": "page-arch-legend",
                "title": "Component Responsibilities",
                "component": "KeyValue",
                "data": {
                    "entries": [
                        { "key": "TerminalPage",              "value": "Top-level orchestrator — manages three-panel layout, analysis dispatch, and workflow generation" },
                        { "key": "TerminalTabBar",            "value": "Tab strip with create/close/rename — keyboard shortcuts Ctrl+Shift+T/W and Ctrl+Tab" },
                        { "key": "TerminalActionBar",         "value": "Two-row bar: session browser toggle + plan indicator (row 1), analysis buttons (row 2)" },
                        { "key": "TerminalNotification",      "value": "Auto-dismiss success/error banner below the action bar" },
                        { "key": "TerminalInstance",          "value": "xterm.js terminal — one per tab, provides getScrollback() and getSelection() for analysis" },
                        { "key": "TranscriptSessionSidebar",  "value": "Left panel — lists Claude Code sessions from SQLite, shows plan-dot status per session" },
                        { "key": "TranscriptContentPanel",    "value": "Right panel — displays messages from a selected session, offers 'Generate Workflow' action" },
                        { "key": "TerminalAnalysisPanel",     "value": "Right panel — renders CanvasWidget panels (graphs, tables, checklists) from analysis commands" },
                        { "key": "WorkflowPreviewPanel",      "value": "Right panel — shows generated workflow with Execute, Edit in Builder, and Save actions" },
                        { "key": "useTerminalManager",        "value": "Hook: tab CRUD, PTY session reconnection, and state persistence" },
                        { "key": "useTranscriptSessions",     "value": "Hook: loads session list and individual session messages via Tauri IPC" },
                        { "key": "transcript commands (Rust)", "value": "list_sessions, read_session, get_latest, generate_workflow_standalone" },
                        { "key": "analysis commands (Rust)",   "value": "session_summary, architecture, change_impact, plan_progress, cross_tab, page_architecture" },
                        { "key": "AI Provider",               "value": "Configurable LLM backend — used by 5 analysis commands (page_architecture is static)" }
                    ]
                }
            }
        ]
    });

    Ok(CommandResponse {
        success: true,
        message: Some("Page architecture diagram generated".to_string()),
        data: Some(panels),
    })
}

// ============================================================================
// Plan content loader
// ============================================================================

/// Find and return the content of the most recently modified plan file.
///
/// Search order:
/// 1. Workspace root (e.g. `qontinui_parent/`)
/// 2. Sibling `qontinui-dev-notes/` directory (per project conventions)
/// 3. Parent of workspace root (one level up)
#[tauri::command]
pub fn get_latest_plan_content() -> Result<CommandResponse, String> {
    let workspace_root = crate::mcp::shared::get_workspace_paths_internal()
        .map(|(root, _, _)| root)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mut search_dirs: Vec<PathBuf> = vec![workspace_root.clone()];

    // Sibling qontinui-dev-notes directory
    if let Some(parent) = workspace_root.parent() {
        let dev_notes = parent.join("qontinui-dev-notes");
        if dev_notes.is_dir() {
            search_dirs.push(dev_notes);
        }
        // Also check parent itself
        search_dirs.push(parent.to_path_buf());
    }

    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in &search_dirs {
        collect_plan_files(dir, &mut candidates);
    }

    if candidates.is_empty() {
        return Ok(CommandResponse {
            success: false,
            message: Some("No plan files found".to_string()),
            data: Some(serde_json::json!({ "found": false })),
        });
    }

    // Sort newest first
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let (_, best_path) = &candidates[0];

    let filename = best_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("plan.md")
        .to_string();

    let content = std::fs::read_to_string(best_path)
        .map_err(|e| format!("Failed to read {}: {}", best_path.display(), e))?;

    info!(
        "get_latest_plan_content: loaded '{}' ({} chars)",
        filename,
        content.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Loaded plan: {}", filename)),
        data: Some(serde_json::json!({
            "found": true,
            "filename": filename,
            "path": best_path.to_string_lossy(),
            "content": content,
        })),
    })
}
