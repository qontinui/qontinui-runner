//! Helper Task Queue commands (plan `2026-06-29-helper-task-queue`, Phase 1.3).
//!
//! Frontend surface for the Helper Tasks page:
//! - `get_helper_tasks_settings` / `save_helper_tasks_settings` — the owner
//!   emit toggle + allowed kinds (Config tab).
//! - `get_helper_answers` — the answers collected by the background poll
//!   (`helper_tasks::poller`), for the Review tab.

use tracing::info;

use crate::settings::{self, HelperTasksSettings};

use super::CommandResponse;

/// Get the current Helper Task Queue settings.
#[tauri::command]
pub fn get_helper_tasks_settings() -> Result<CommandResponse, String> {
    let helper_settings = settings::get_helper_tasks_settings();
    Ok(CommandResponse {
        success: true,
        message: Some("Helper Task settings retrieved".to_string()),
        data: Some(
            serde_json::to_value(&helper_settings)
                .map_err(|e| format!("serialize helper task settings: {e}"))?,
        ),
    })
}

/// Save Helper Task Queue settings (owner emit toggle + allowed kinds).
#[tauri::command]
pub fn save_helper_tasks_settings(
    helper_tasks_settings: HelperTasksSettings,
) -> Result<CommandResponse, String> {
    info!(
        "Saving Helper Task settings (emit_enabled={}, kinds={:?})",
        helper_tasks_settings.emit_enabled, helper_tasks_settings.emit_kinds
    );
    settings::save_helper_tasks_settings(helper_tasks_settings)?;
    Ok(CommandResponse {
        success: true,
        message: Some("Helper Task settings saved".to_string()),
        data: None,
    })
}

/// Get the helper answers collected by the background poll (oldest first).
#[tauri::command]
pub fn get_helper_answers() -> Result<CommandResponse, String> {
    let answers = crate::helper_tasks::recent_answers();
    Ok(CommandResponse {
        success: true,
        message: Some(format!("{} helper answer(s)", answers.len())),
        data: Some(
            serde_json::to_value(&answers).map_err(|e| format!("serialize helper answers: {e}"))?,
        ),
    })
}
