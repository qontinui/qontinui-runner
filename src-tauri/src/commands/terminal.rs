//! Tauri commands for embedded terminal management.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Manager;
use tracing::{info, warn};

use crate::claude_session::SessionManager;
use crate::commands::CommandResponse;
use crate::error::AppError;
use crate::session::{Intent, SessionKind, SessionRegistry};
use crate::terminal::{strip_ansi, TerminalManager};

/// Create a new terminal session.
#[tauri::command]
pub fn terminal_create(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_registry: tauri::State<'_, Arc<SessionRegistry>>,
    app_handle: tauri::AppHandle,
    title: Option<String>,
    working_dir: Option<String>,
    page_id: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<CommandResponse, String> {
    let info = terminal_manager.create(
        title.clone(),
        working_dir.clone(),
        page_id,
        cols,
        rows,
        app_handle,
    )?;

    // Unconditional coord registration — every terminal session is
    // mirrored into the coordinator's session plane so the dashboard
    // renders it from `coord.sessions`. This is NOT gated by dual-write;
    // registration is always-on. Errors are logged and swallowed so a
    // coord hiccup never blocks the operator's terminal.
    let purpose = title
        .filter(|t| t.trim().len() >= 3)
        .unwrap_or_else(|| "Terminal shell session".to_string());
    let intent = Intent {
        kind: SessionKind::TerminalShell,
        purpose,
        repo: None,
        branch: None,
        declared_paths: working_dir
            .map(std::path::PathBuf::from)
            .into_iter()
            .collect(),
        share_output: true,
        redact_secrets: None,
    };
    match session_registry.inner().register_external(intent) {
        Ok(coord_id) => {
            // Store the coord session id on the terminal so close can clean up.
            if let Some(session) = terminal_manager.get(&info.id) {
                session.set_coord_session_id(coord_id);
                // Attach the output pipe so PTY output streams to coord.
                let rx = session.subscribe_output();
                session_registry
                    .inner()
                    .attach_output_pipe(coord_id, rx, true);
            }
            info!(
                terminal_id = %info.id,
                coord_session = %coord_id,
                "terminal_create: registered coord session"
            );
        }
        Err(e) => {
            warn!(
                terminal_id = %info.id,
                error = %e,
                "terminal_create: coord session registration failed — terminal unaffected"
            );
        }
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!(info)),
    })
}

/// Write data (keystrokes) to a terminal's PTY stdin.
///
/// Data is a base64-encoded byte array from the frontend.
#[tauri::command]
pub fn terminal_write(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    data: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let bytes = STANDARD.decode(&data).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Invalid base64 data: {}",
            e
        )))
    })?;

    session.write(&bytes)?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Update a terminal session's display title.
///
/// Phase 2 of the bi-directional title sync (plan
/// `2026-05-11-runner-dispatch-and-terminal-ux-fixes-plan.md`): the
/// frontend's xterm.js `onTitleChange` handler (see
/// `components/terminal/ZoneGrid.tsx`) invokes this whenever it
/// observes a new OSC 0/2 title in a `terminal-output` paint. The
/// runner mirrors the new title onto `TerminalSession.title` and emits
/// a `terminal-title-changed` event so other webview windows / WS
/// subscribers stay consistent.
///
/// Worker pin (plan `2026-05-12-claude-auto-title-suppression-worker-tabs-plan.md`):
/// goes through [`TerminalManager::set_title_unless_worker`] so OSC 0
/// titles emitted by Claude inside a worker pty are silently dropped —
/// `Worker N` stays pinned for the lifetime of the tab. Non-worker
/// terminals (manual `claude` from the Terminal tab, PowerShell, etc.)
/// keep the existing follow-the-child behaviour.
#[tauri::command]
pub fn terminal_set_title(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    app_handle: tauri::AppHandle,
    terminal_id: String,
    title: String,
) -> Result<CommandResponse, String> {
    terminal_manager.set_title_unless_worker(
        session_manager.inner().as_ref(),
        &terminal_id,
        title,
        &app_handle,
    )?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Resize a terminal's PTY dimensions.
#[tauri::command]
pub fn terminal_resize(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    session.resize(cols, rows)?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Close a terminal session and kill its process.
/// Uses spawn_blocking to avoid blocking the IPC thread during thread joins.
#[tauri::command]
pub async fn terminal_close(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    session_registry: tauri::State<'_, Arc<SessionRegistry>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    // Capture the coord session id BEFORE close destroys the terminal.
    let coord_id = terminal_manager
        .get(&terminal_id)
        .and_then(|s| s.coord_session_id());

    let manager = terminal_manager.inner().clone();
    let id = terminal_id.clone();
    tokio::task::spawn_blocking(move || manager.close(&id))
        .await
        .map_err(|e| String::from(AppError::ProcessError(format!("Join error: {}", e))))??;

    // Close the coord mirror — fire-and-forget on error.
    if let Some(coord_id) = coord_id {
        if let Err(e) = session_registry.inner().close_by_id(coord_id) {
            warn!(
                terminal_id = %terminal_id,
                coord_session = %coord_id,
                error = %e,
                "terminal_close: coord session close failed"
            );
        }
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// List all terminal sessions.
#[tauri::command]
pub fn terminal_list(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<CommandResponse, String> {
    let terminals = terminal_manager.list();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "terminals": terminals })),
    })
}

/// Get the server-side cell grid snapshot for a terminal session.
///
/// Returns the parsed `GridSnapshot` rather than raw scrollback bytes, so the
/// frontend can paint the final visible state in one synchronous write.
/// See `plans/terminal-grid-snapshot.md`.
#[tauri::command]
pub fn terminal_get_grid(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let grid_handle = session.grid();
    let snapshot = {
        let g = grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        g.snapshot()
    };
    let value = serde_json::to_value(&snapshot).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize grid snapshot: {}",
            e
        )))
    })?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(value),
    })
}

/// Compact text-only snapshot for verifiers and external tools.
///
/// Returns the rendered grid as `lines: Vec<String>` plus a single
/// `text` field with the lines joined by `\n` — the shape the
/// verification module expects to feed into a prompt or diff.
#[tauri::command]
pub fn terminal_grid_text(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;
    let grid_handle = session.grid();
    let snapshot = {
        let g = grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        g.text_snapshot()
    };
    let value = serde_json::to_value(&snapshot).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize text snapshot: {}",
            e
        )))
    })?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(value),
    })
}

/// Search the rendered grid of one terminal for a substring or regex.
///
/// `regex=true` compiles the needle as a regex; `regex=false` is a
/// case-sensitive substring match. Returns at most one hit per row.
#[tauri::command]
pub fn terminal_grid_search(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    needle: String,
    regex: Option<bool>,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;
    let grid_handle = session.grid();
    let hits = {
        let g = grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        g.search(&needle, regex.unwrap_or(false))
            .map_err(|e| e.to_string())?
    };
    let value = serde_json::to_value(&hits).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize search hits: {}",
            e
        )))
    })?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "terminalId": terminal_id,
            "hits": value,
        })),
    })
}

/// Row-by-row diff of two terminal grids.
#[tauri::command]
pub fn terminal_grid_diff(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    a_terminal_id: String,
    b_terminal_id: String,
) -> Result<CommandResponse, String> {
    let a = terminal_manager
        .get(&a_terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", a_terminal_id))?;
    let b = terminal_manager
        .get(&b_terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", b_terminal_id))?;
    let a_grid_handle = a.grid();
    let b_grid_handle = b.grid();
    // Self-diff would deadlock on a single-mutex re-lock — detect via
    // Arc::ptr_eq and take one lock instead.
    let diff = if Arc::ptr_eq(&a_grid_handle, &b_grid_handle) {
        let grid = a_grid_handle
            .lock()
            .map_err(|e| format!("Grid lock poisoned: {}", e))?;
        grid.diff_lines(&grid)
    } else {
        let ag = a_grid_handle
            .lock()
            .map_err(|e| format!("Grid A lock poisoned: {}", e))?;
        let bg = b_grid_handle
            .lock()
            .map_err(|e| format!("Grid B lock poisoned: {}", e))?;
        ag.diff_lines(&bg)
    };
    let value = serde_json::to_value(&diff).map_err(|e| {
        String::from(AppError::EncodingError(format!(
            "Failed to serialize grid diff: {}",
            e
        )))
    })?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(value),
    })
}

/// Acknowledge bytes received by the frontend (flow control).
#[tauri::command]
pub fn terminal_ack(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    terminal_id: String,
    bytes_acked: u64,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    session.ack(bytes_acked);

    Ok(CommandResponse {
        success: true,
        message: None,
        data: None,
    })
}

/// Save the scrollback buffer for a terminal session to disk.
///
/// Persists the current scrollback buffer to `{app_data}/terminal-scrollback/{terminal_id}.bin`
/// so it can be restored after an app restart. Returns the file path on success.
#[tauri::command]
pub fn terminal_save_scrollback(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let session = terminal_manager
        .get(&terminal_id)
        .ok_or_else(|| format!("Terminal not found: {}", terminal_id))?;

    let (data, _start_offset) = session.get_scrollback_buffer();
    if data.is_empty() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No scrollback data to save".to_string()),
            data: Some(serde_json::json!({ "path": serde_json::Value::Null })),
        });
    }

    let scrollback_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| {
            String::from(AppError::TauriError(format!(
                "Failed to get app data dir: {}",
                e
            )))
        })?
        .join("terminal-scrollback");

    std::fs::create_dir_all(&scrollback_dir).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to create scrollback directory: {}", e),
        )))
    })?;

    let file_path = scrollback_dir.join(format!("{}.bin", terminal_id));
    std::fs::write(&file_path, &data).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to write scrollback file: {}", e),
        )))
    })?;

    let path_str = file_path.to_string_lossy().to_string();
    info!(
        terminal_id = %terminal_id,
        bytes = data.len(),
        path = %path_str,
        "Saved terminal scrollback to disk"
    );

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "path": path_str })),
    })
}

/// Read a previously saved scrollback buffer from disk.
///
/// Returns the base64-encoded scrollback content. Returns an empty string
/// if the file does not exist (best-effort restore).
#[tauri::command]
pub fn terminal_get_saved_scrollback(file_path: String) -> Result<CommandResponse, String> {
    let path = std::path::Path::new(&file_path);
    if !path.exists() {
        return Ok(CommandResponse {
            success: true,
            message: None,
            data: Some(serde_json::json!({ "data": "" })),
        });
    }

    let data = std::fs::read(path).map_err(|e| {
        String::from(AppError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to read scrollback file: {}", e),
        )))
    })?;

    let encoded = STANDARD.encode(&data);
    info!(
        path = %file_path,
        bytes = data.len(),
        "Read saved scrollback from disk"
    );

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "data": encoded })),
    })
}

/// Delete all saved scrollback files from disk.
///
/// Called after successful session restore to clean up stale scrollback data.
#[tauri::command]
pub fn terminal_cleanup_scrollback(
    app_handle: tauri::AppHandle,
) -> Result<CommandResponse, String> {
    let scrollback_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| {
            String::from(AppError::TauriError(format!(
                "Failed to get app data dir: {}",
                e
            )))
        })?
        .join("terminal-scrollback");

    if !scrollback_dir.exists() {
        return Ok(CommandResponse {
            success: true,
            message: Some("No scrollback directory to clean up".to_string()),
            data: None,
        });
    }

    let mut deleted = 0u32;
    match std::fs::read_dir(&scrollback_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    warn!(
                        path = %entry.path().display(),
                        error = %e,
                        "Failed to delete scrollback file"
                    );
                } else {
                    deleted += 1;
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to read scrollback directory");
        }
    }

    info!(deleted = deleted, "Cleaned up terminal scrollback files");

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({ "deleted": deleted })),
    })
}

/// Collect session metadata across terminal pages for AI analysis.
///
/// Returns structured data per session including title, working directory,
/// scrollback preview (last lines), and page association. Used by the
/// session reorganization feature.
#[tauri::command]
pub fn terminal_collect_session_metadata(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    page_ids: Vec<String>,
    max_scrollback_lines: Option<usize>,
) -> Result<CommandResponse, String> {
    let max_lines = max_scrollback_lines.unwrap_or(20);
    let terminals = terminal_manager.list();

    let mut sessions = Vec::new();

    for term in &terminals {
        let page = &term.page_id;
        if !page_ids.contains(page) {
            continue;
        }

        // Get scrollback preview
        let scrollback_preview = if let Some(session) = terminal_manager.get(&term.id) {
            let (data, _offset) = session.get_scrollback_buffer();
            // Decode and extract last N lines
            let text = String::from_utf8_lossy(&data);
            // Strip ANSI escape codes for readability
            let clean: String = strip_ansi(&text);
            let lines: Vec<&str> = clean.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = lines.len().saturating_sub(max_lines);
            lines[start..].join("\n")
        } else {
            String::new()
        };

        sessions.push(serde_json::json!({
            "id": term.id,
            "title": term.title,
            "page_id": page,
            "working_dir": term.working_dir,
            "is_alive": term.is_alive,
            "pid": term.pid,
            "created_at": term.created_at,
            "total_bytes_produced": term.total_bytes_produced,
            "scrollback_preview": scrollback_preview,
        }));
    }

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "sessions": sessions,
            "page_ids": page_ids,
        })),
    })
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// Non-generic because handlers accept concrete `tauri::AppHandle`.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_terminal")
        .invoke_handler(tauri::generate_handler![
            terminal_create,
            terminal_write,
            terminal_resize,
            terminal_set_title,
            terminal_close,
            terminal_list,
            terminal_ack,
            terminal_save_scrollback,
            terminal_get_saved_scrollback,
            terminal_cleanup_scrollback,
            terminal_collect_session_metadata,
            terminal_get_grid,
            terminal_grid_text,
            terminal_grid_search,
            terminal_grid_diff,
        ])
        .build()
}
