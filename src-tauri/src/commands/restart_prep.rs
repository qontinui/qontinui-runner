//! Tauri reads of the operator restart prepare (plan
//! `2026-10-21-operator-prepare-for-restart-and-progressive-restore`, Phase 1).
//!
//! A thin twin of `GET /control/restart-prep`: both call
//! [`crate::restart_prep::status_from_app_blocking`], so the webview and an
//! HTTP reader cannot disagree. Read-only — nothing here writes the record.

/// `restart_prep_status` — the persisted prepare record (if any) and the live
/// cohort a prepare taken now would pin.
#[tauri::command]
pub async fn restart_prep_status(
    app: tauri::AppHandle,
) -> Result<crate::restart_prep::RestartPrepStatus, String> {
    crate::restart_prep::status_from_app_blocking(app).await
}

crate::ipc_group!(restart_prep_status);
