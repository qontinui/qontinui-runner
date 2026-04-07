//! Tauri commands for the Discovery Push mechanism.
//!
//! These commands expose discovery functionality to the frontend and MCP API.

use crate::commands::AppState;
use crate::discoveries::{
    self, sync_discoveries_batch, DiscoveryPayload, DiscoveryToSync, PendingDiscovery, SyncStatus,
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
    match state.pg_db.get_pending_discoveries().await {
        Ok(vals) => {
            let discoveries: Vec<PendingDiscovery> = vals
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            Ok(DiscoveryResponse::ok(discoveries))
        }
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}

/// Get a summary of pending discoveries for the UI dashboard.
#[tauri::command]
pub async fn get_discovery_summary(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<DiscoverySummary>, String> {
    match state.pg_db.get_discovery_summary().await {
        Ok(val) => {
            let pending_count = val["pending_count"].as_u64().unwrap_or(0) as u32;
            let ready = val["ready_for_sync"].as_u64().unwrap_or(0) as u32;
            let can_sync = val["can_sync"].as_bool().unwrap_or(false);
            let recent: Vec<DiscoveryPreview> = Vec::new(); // PG returns raw JSON, skip preview deserialization
            let summary = DiscoverySummary {
                pending_count,
                ready_for_sync: ready,
                can_sync,
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
#[tauri::command]
pub async fn sync_discoveries(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<SyncResultResponse>, String> {
    info!("Manual sync of pending discoveries triggered");

    let to_sync_pairs = state
        .pg_db
        .extract_discoveries_for_sync()
        .await
        .map_err(|e| format!("Failed to extract discoveries: {}", e))?;
    // Convert to DiscoveryToSync format for sync_discoveries_batch
    let to_sync: Vec<DiscoveryToSync> = to_sync_pairs
        .into_iter()
        .filter_map(|(id, payload_str)| {
            serde_json::from_str::<DiscoveryPayload>(&payload_str)
                .ok()
                .map(|payload| DiscoveryToSync { id, payload })
        })
        .collect();

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

    // Phase 3: Apply sync results via PG
    let mut sent = 0u32;
    let mut failed = 0u32;
    let mut errors = Vec::new();
    for sr in &sync_results {
        if sr.success {
            let _ = state.pg_db.mark_discovery_synced(&sr.id).await;
            sent += 1;
        } else {
            let err_msg = sr.error.as_deref().unwrap_or("unknown");
            let _ = state.pg_db.mark_discovery_failed(&sr.id, err_msg).await;
            failed += 1;
            errors.push(format!("{}: {}", sr.id, err_msg));
        }
    }
    let remaining_status = state.pg_db.get_sync_status().await.unwrap_or_default();
    let remaining = remaining_status["pending_count"].as_u64().unwrap_or(0) as u32;

    let response = SyncResultResponse {
        sent,
        failed,
        errors,
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

    match state.pg_db.delete_discovery(&id).await {
        Ok(deleted) => Ok(DiscoveryResponse::ok(deleted)),
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}

/// Clear all failed discoveries (exceeded retry limit).
#[tauri::command]
pub async fn clear_failed_discoveries(
    state: State<'_, Arc<AppState>>,
) -> Result<DiscoveryResponse<u32>, String> {
    info!("Clearing failed discoveries");

    match state.pg_db.cleanup_failed_discoveries().await {
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
    match state.pg_db.get_sync_status().await {
        Ok(val) => {
            let status = SyncStatus {
                pending_count: val["pending_count"].as_u64().unwrap_or(0) as u32,
                ready_for_retry: val["ready_for_retry"].as_u64().unwrap_or(0) as u32,
                authenticated: val["authenticated"].as_bool().unwrap_or(false),
            };
            Ok(DiscoveryResponse::ok(status))
        }
        Err(e) => Ok(DiscoveryResponse::err(e)),
    }
}
