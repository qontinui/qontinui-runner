//! Tauri command surface for the Doctor health monitoring service.
//!
//! Lives here rather than in `crate::doctor` because it resolves
//! `Arc<AppState>` out of Tauri's managed state, and Tauri resolves
//! managed state by concrete type — so the handler has to name
//! `AppState`, which this module already owns. `crate::doctor` stays a
//! monitoring service that names nothing in `crate::commands`.

use std::sync::Arc;

use crate::commands::AppState;
use crate::doctor::service::ProcessStatus;

/// Query current Doctor health status of all monitored processes.
#[tauri::command]
pub async fn doctor_get_status(
    app_state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<ProcessStatus>, String> {
    let handle_lock = app_state.doctor_handle.lock().await;
    match handle_lock.as_ref() {
        Some(handle) => handle.query_status().await,
        None => Ok(vec![]), // Doctor not started yet
    }
}
