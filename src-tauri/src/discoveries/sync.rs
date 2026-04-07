//! Sync service for pushing discoveries to qontinui-web.
//!
//! Handles the HTTP communication with the backend API for submitting
//! discoveries. The persistent queue lives in PostgreSQL
//! (`database::pg::tiered_info`); this module contains only the network push
//! logic plus the `SyncStatus`/`DiscoveryToSync` types shared with the
//! Tauri command layer.

use crate::auth::AuthManager;
use tracing::{debug, error, info};

use super::types::{DiscoveryPayload, DiscoveryResponse};

use crate::api_config::get_api_base_url;

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

#[cfg(test)]
mod tests {
    use super::*;

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
