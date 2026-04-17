//! Tauri commands for OS-level window enumeration and activation.
//!
//! These commands expose `window_manager` functionality to the Tauri frontend,
//! enabling the UI Bridge to bring browser windows to the foreground when
//! `window.focus()` is blocked by the browser.

use crate::window_manager::{self, WindowInfo};

/// List visible top-level OS windows, optionally filtered by title substring.
#[tauri::command]
pub fn list_system_windows(filter: Option<String>) -> Vec<WindowInfo> {
    window_manager::list_windows(filter.as_deref())
}

/// Activate (bring to foreground) an OS window by PID or title substring.
///
/// If `pid` is provided, activates the window belonging to that process.
/// If only `title` is provided, activates the first window whose title
/// contains the given substring.
#[tauri::command]
pub fn activate_system_window(pid: Option<u32>, title: Option<String>) -> bool {
    if let Some(pid) = pid {
        window_manager::activate_window_by_pid(pid)
    } else if let Some(title) = title {
        window_manager::activate_window_by_title(&title)
    } else {
        false
    }
}
