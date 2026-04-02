//! Sync service for pushing discoveries to qontinui-web.
//!
//! Handles the HTTP communication with the backend API for submitting
//! discoveries. Supports offline operation via the queue in storage.rs.

use crate::database::Connection;
use crate::auth::AuthManager;
use tracing::{debug, error, info, warn};

use super::storage::{
    get_discoveries_for_retry, get_pending_discoveries, mark_discovery_sent, record_sync_failure,
};
use super::types::{DiscoveryPayload, DiscoveryResponse};

/// Get API base URL for qontinui-web backend.
/// Uses environment variable QONTINUI_API_URL or defaults based on build mode.
fn get_api_base_url() -> String {
    std::env::var("QONTINUI_API_URL").unwrap_or_else(|_| {
        if cfg!(debug_assertions) {
            "http://127.0.0.1:8000".to_string()
        } else {
            "https://qontinui-prod-py.eba-km2u4s23.eu-central-1.elasticbeanstalk.com".to_string()
        }
    })
}

/// Result of a sync operation.
#[derive(Debug)]
pub struct SyncResult {
    /// Number of discoveries successfully sent
    pub sent: u32,
    /// Number of discoveries that failed to send
    pub failed: u32,
    /// Error messages for failed syncs
    pub errors: Vec<String>,
}

impl SyncResult {
    pub fn new() -> Self {
        Self {
            sent: 0,
            failed: 0,
            errors: Vec::new(),
        }
    }

    pub fn record_success(&mut self) {
        self.sent += 1;
    }

    pub fn record_failure(&mut self, error: String) {
        self.failed += 1;
        self.errors.push(error);
    }
}

impl Default for SyncResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Push a single discovery to qontinui-web.
///
/// Returns the response from the backend or an error message.
pub async fn push_discovery(payload: &DiscoveryPayload) -> Result<DiscoveryResponse, String> {
    let auth_manager = AuthManager::new();

    // Check if authenticated
    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Cannot push discoveries.".to_string());
    }

    // Get access token
    let access_token = auth_manager
        .get_access_token()
        .map_err(|e| format!("Failed to get access token: {}", e))?;

    // Build request
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let url = format!(
        "{}/api/v1/projects/{}/discoveries",
        get_api_base_url(),
        payload.project_id
    );

    debug!("Pushing discovery to: {}", url);

    let response = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(payload)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "Request timed out".to_string()
            } else if e.is_connect() {
                "Failed to connect to server".to_string()
            } else {
                format!("Network error: {}", e)
            }
        })?;

    let status = response.status();

    if status.is_success() {
        let result: DiscoveryResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        info!(
            "Discovery pushed successfully: id={}, status={}",
            result.id, result.status
        );
        Ok(result)
    } else {
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());

        let error_msg = match status.as_u16() {
            401 => "Authentication expired. Please log in again.".to_string(),
            403 => "Access denied. Check project permissions.".to_string(),
            404 => "Project not found or discoveries API not available.".to_string(),
            422 => format!("Invalid discovery data: {}", error_text),
            500..=599 => "Server error. Please try again later.".to_string(),
            _ => format!("Request failed ({}): {}", status, error_text),
        };

        error!("Failed to push discovery: {}", error_msg);
        Err(error_msg)
    }
}

/// Collected discovery data for syncing (extracted before async operations).
#[derive(Clone)]
pub struct DiscoveryToSync {
    pub id: String,
    pub payload: DiscoveryPayload,
}

/// Result of syncing a single discovery.
#[derive(Clone)]
pub struct SingleSyncResult {
    pub id: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Extract discoveries ready for sync (sync operation, no connection held).
///
/// Returns the discoveries that should be synced. Call this first,
/// then call `sync_discoveries_batch` with the data, then call
/// `apply_sync_results` to update the database.
pub fn extract_discoveries_for_sync(conn: &Connection) -> Result<Vec<DiscoveryToSync>, String> {
    Err("SQLite removed".to_string())
}

/// Sync a batch of discoveries (async operation, no connection needed).
///
/// This is the async part that does network operations.
pub async fn sync_discoveries_batch(discoveries: Vec<DiscoveryToSync>) -> Vec<SingleSyncResult> {
    let mut results = Vec::new();

    for discovery in discoveries {
        let sync_result = match push_discovery(&discovery.payload).await {
            Ok(_) => SingleSyncResult {
                id: discovery.id,
                success: true,
                error: None,
            },
            Err(error) => SingleSyncResult {
                id: discovery.id,
                success: false,
                error: Some(error),
            },
        };
        results.push(sync_result);
    }

    results
}

/// Apply sync results to the database (sync operation).
///
/// Updates the database based on what succeeded and failed.
pub fn apply_sync_results(
    conn: &Connection,
    results: Vec<SingleSyncResult>,
) -> Result<SyncResult, String> {
    Err("SQLite removed".to_string())
}

/// Sync all pending discoveries to qontinui-web.
///
/// This is a convenience function that combines the three phases:
/// 1. Extract discoveries from database (sync)
/// 2. Push to backend (async)
/// 3. Update database with results (sync)
///
/// NOTE: This function cannot be used in Tauri commands due to Send requirements.
/// Use the phased approach instead (extract -> batch -> apply).
#[allow(dead_code)]
pub async fn sync_pending_discoveries(conn: &Connection) -> Result<SyncResult, String> {
    Err("SQLite removed".to_string())
}

/// Get the sync status summary.
#[derive(Debug, serde::Serialize)]
pub struct SyncStatus {
    /// Total discoveries in queue
    pub pending_count: u32,
    /// Discoveries ready for immediate retry
    pub ready_for_retry: u32,
    /// Whether we're authenticated for sync
    pub authenticated: bool,
}

/// Get current sync status.
pub fn get_sync_status(conn: &Connection) -> Result<SyncStatus, String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
use crate::database::Connection;

    #[test]
    fn test_sync_result() {
        let mut result = SyncResult::new();
        result.record_success();
        result.record_success();
        result.record_failure("Error 1".to_string());

        assert_eq!(result.sent, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.errors.len(), 1);
    }
}
