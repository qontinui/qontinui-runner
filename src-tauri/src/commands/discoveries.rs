//! Tauri commands for the Discovery Push mechanism.
//!
//! These commands expose discovery functionality to the frontend and MCP API.

use crate::commands::AppState;
use crate::discoveries::{
    self, apply_sync_results, extract_discoveries_for_sync, get_pending_count,
    get_pending_discoveries, get_sync_status, sync_discoveries_batch, PendingDiscovery, SyncStatus,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;
use tracing::info;

/// Response type for discovery commands.
#[derive(Debug, Serialize)]
pub struct DiscoveryResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T> DiscoveryResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}

/// Summary of pending discoveries for the UI.
#[derive(Debug, Serialize)]
pub struct DiscoverySummary {
    /// Total pending discoveries
    pub pending_count: u32,
    /// Ready for immediate sync
    pub ready_for_sync: u32,
    /// Whether we can sync (authenticated)
    pub can_sync: bool,
    /// Recent discoveries (limited for display)
    pub recent: Vec<DiscoveryPreview>,
}

/// Preview of a discovery for list display.
#[derive(Debug, Serialize)]
pub struct DiscoveryPreview {
    pub id: String,
    pub discovery_type: String,
    pub title: String,
    pub confidence: f64,
    pub runs_observed: u32,
    pub created_at: String,
    pub attempt_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl From<&PendingDiscovery> for DiscoveryPreview {
    fn from(d: &PendingDiscovery) -> Self {
        Self {
            id: d.id.clone(),
            discovery_type: d.payload.discovery_type.as_str().to_string(),
            title: d.payload.title.clone(),
            confidence: d.payload.confidence,
            runs_observed: d.payload.runs_observed,
            created_at: d.created_at.clone(),
            attempt_count: d.attempt_count,
            last_error: d.error.clone(),
        }
    }
}

/// Get all pending discoveries awaiting sync.
#[tauri::command]
pub async fn get_pending_discoveries_cmd(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<Vec<PendingDiscovery>>, String> {
    let db = state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| get_pending_discoveries(conn))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match result {
        Ok(discoveries) => Ok(DiscoveryResponse::ok(discoveries)),
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}

/// Get a summary of pending discoveries for the UI dashboard.
#[tauri::command]
pub async fn get_discovery_summary(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<DiscoverySummary>, String> {
    let db = state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| {
            let status = get_sync_status(conn)?;
            let all = get_pending_discoveries(conn)?;
            let recent: Vec<DiscoveryPreview> = all.iter().take(10).map(DiscoveryPreview::from).collect();
            Ok::<_, String>((status, recent))
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match result {
        Ok((status, recent)) => {
            let summary = DiscoverySummary {
                pending_count: status.pending_count,
                ready_for_sync: status.ready_for_retry,
                can_sync: status.authenticated,
                recent,
            };
            Ok(DiscoveryResponse::ok(summary))
        }
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}

/// Sync status response.
#[derive(Debug, Serialize)]
pub struct SyncResultResponse {
    pub sent: u32,
    pub failed: u32,
    pub errors: Vec<String>,
    pub remaining: u32,
}

/// Trigger sync of pending discoveries to qontinui-web.
///
/// Uses a three-phase approach to avoid Send issues with rusqlite Connection:
/// 1. Extract discoveries (sync, with connection)
/// 2. Push to backend (async, no connection needed)
/// 3. Apply results (sync, with new connection)
#[tauri::command]
pub async fn sync_discoveries(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<SyncResultResponse>, String> {
    info!("Manual sync of pending discoveries triggered");

    let db = &state.checkpoint_db;

    // Phase 1: Extract discoveries (sync operation)
    let db1 = db.clone();
    let to_sync = tokio::task::spawn_blocking(move || {
        db1.with_conn(|conn| extract_discoveries_for_sync(conn))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    if to_sync.is_empty() {
        return Ok(DiscoveryResponse::ok(SyncResultResponse {
            sent: 0,
            failed: 0,
            errors: vec![],
            remaining: 0,
        }));
    }

    info!("Syncing {} pending discoveries", to_sync.len());

    // Phase 2: Push to backend (async operation, no connection)
    let sync_results = sync_discoveries_batch(to_sync).await;

    // Phase 3: Apply results to database (sync operation, fresh connection)
    let db2 = db.clone();
    let (result, remaining) = tokio::task::spawn_blocking(move || {
        db2.with_conn(|conn| {
            let result = apply_sync_results(conn, sync_results)?;
            let remaining = get_pending_count(conn).unwrap_or(0);
            Ok::<_, String>((result, remaining))
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

    let response = SyncResultResponse {
        sent: result.sent,
        failed: result.failed,
        errors: result.errors,
        remaining,
    };

    info!(
        "Sync complete: {} sent, {} failed, {} remaining",
        response.sent, response.failed, response.remaining
    );

    Ok(DiscoveryResponse::ok(response))
}

/// Clear a specific discovery from the pending queue.
#[tauri::command]
pub async fn clear_discovery(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<DiscoveryResponse<bool>, String> {
    info!("Clearing discovery {} from queue", id);

    let db = state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| discoveries::delete_discovery(conn, &id))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match result {
        Ok(deleted) => {
            Ok(DiscoveryResponse::ok(deleted))
        }
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}

/// Clear all failed discoveries (exceeded retry limit).
#[tauri::command]
pub async fn clear_failed_discoveries(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<u32>, String> {
    info!("Clearing failed discoveries");

    let db = state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| discoveries::cleanup_failed_discoveries(conn))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match result {
        Ok(count) => {
            info!("Cleared {} failed discoveries", count);
            Ok(DiscoveryResponse::ok(count))
        }
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}

/// Get the current sync status.
#[tauri::command]
pub async fn get_discovery_sync_status(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<SyncStatus>, String> {
    let db = state.checkpoint_db.clone();

    let result = tokio::task::spawn_blocking(move || {
        db.with_conn(|conn| get_sync_status(conn))
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match result {
        Ok(status) => Ok(DiscoveryResponse::ok(status)),
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}
