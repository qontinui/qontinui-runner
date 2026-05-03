//! Tauri commands for embedded terminal management.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Manager;
use tracing::{info, warn};

use crate::commands::CommandResponse;
use crate::error::AppError;
use crate::terminal::TerminalManager;

/// Create a new terminal session.
#[tauri::command]
pub fn terminal_create(
    terminal_manager: tauri::State<'_, Arc<TerminalManager>>,
    app_handle: tauri::AppHandle,
    title: Option<String>,
    working_dir: Option<String>,
    page_id: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<CommandResponse, String> {
    let info = terminal_manager.create(title, working_dir, page_id, cols, rows, app_handle)?;

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
    terminal_id: String,
) -> Result<CommandResponse, String> {
    let manager = terminal_manager.inner().clone();
    let id = terminal_id.clone();
    tokio::task::spawn_blocking(move || manager.close(&id))
        .await
        .map_err(|e| String::from(AppError::ProcessError(format!("Join error: {}", e))))??;

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

/// Strip ANSI escape sequences from text for readable scrollback previews.
fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip CSI sequences: ESC [ ... letter
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            // Skip OSC sequences: ESC ] ... BEL
            } else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c == '\x07' {
                        break;
                    }
                }
            } else {
                // Skip next char (two-char escape)
                chars.next();
            }
        } else if ch >= ' ' || ch == '\n' || ch == '\r' || ch == '\t' {
            result.push(ch);
        }
    }
    result
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
            terminal_close,
            terminal_list,
            terminal_ack,
            terminal_save_scrollback,
            terminal_get_saved_scrollback,
            terminal_cleanup_scrollback,
            terminal_collect_session_metadata,
        ])
        .build()
}
