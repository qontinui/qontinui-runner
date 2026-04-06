//! Issue synchronization commands for syncing detected issues to qontinui-web backend.
//!
//! This module provides the Tauri command for syncing issues detected during
//! AI-assisted automation sessions to the web backend for persistence.

use crate::auth::AuthManager;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::api_config::get_api_base_url;

/// Issue source information - where the AI found/detected the error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(i32, i32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Issue data from the frontend IssueTracker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSyncItem {
    pub id: String,
    pub session_id: String,
    #[serde(rename = "type")]
    pub issue_type: String,
    pub severity: String,
    pub title: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i32>,
    pub source: IssueSource,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Unix timestamp in milliseconds
    pub detected_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
}

/// Request payload for syncing issues to backend
#[derive(Debug, Serialize)]
struct SyncIssuesRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    issues: Vec<BackendIssueSyncItem>,
}

/// Issue item formatted for backend API
#[derive(Debug, Serialize)]
struct BackendIssueSyncItem {
    id: String,
    session_id: String,
    #[serde(rename = "type")]
    issue_type: String,
    severity: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<i32>,
    source: BackendIssueSource,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<String>,
    detected_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_at: Option<String>,
}

/// Issue source formatted for backend API
#[derive(Debug, Serialize)]
struct BackendIssueSource {
    #[serde(rename = "type")]
    source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_range: Option<(i32, i32)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Response from the sync endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncIssuesResponse {
    pub synced: i32,
    pub updated: i32,
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Convert Unix timestamp (milliseconds) to ISO 8601 datetime string
fn timestamp_to_iso(timestamp_ms: i64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

/// Sync detected issues to the qontinui-web backend.
///
/// This command:
/// 1. Gets the access token from the keychain
/// 2. Converts issues to the backend format
/// 3. POSTs to /api/v1/issues/sync
/// 4. Returns the sync result
///
/// # Arguments
///
/// * `issues` - List of issues detected during the session
/// * `project_id` - Optional project ID to associate issues with
///
/// # Errors
///
/// Returns an error string if:
/// - Not authenticated
/// - Network request fails
/// - Backend returns an error
#[tauri::command]
pub async fn sync_issues_to_backend(
    issues: Vec<IssueSyncItem>,
    project_id: Option<String>,
) -> Result<SyncIssuesResponse, String> {
    if issues.is_empty() {
        info!("No issues to sync");
        return Ok(SyncIssuesResponse {
            synced: 0,
            updated: 0,
            errors: vec![],
        });
    }

    info!(
        "Syncing {} issues to backend (project_id: {:?})",
        issues.len(),
        project_id
    );

    let auth_manager = AuthManager::new();

    // Check authentication
    if !auth_manager.has_tokens() {
        warn!("Cannot sync issues: not authenticated");
        return Err("Not authenticated. Please log in to sync issues.".to_string());
    }

    // Get access token
    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    // Convert issues to backend format
    let backend_issues: Vec<BackendIssueSyncItem> = issues
        .into_iter()
        .map(|issue| BackendIssueSyncItem {
            id: issue.id,
            session_id: issue.session_id,
            issue_type: issue.issue_type,
            severity: issue.severity,
            title: issue.title,
            description: if issue.description.is_empty() {
                None
            } else {
                Some(issue.description)
            },
            file: issue.file,
            line: issue.line,
            source: BackendIssueSource {
                source_type: issue.source.source_type,
                path: issue.source.path,
                line_range: issue.source.line_range,
                description: issue.source.description,
            },
            status: issue.status,
            resolution: issue.resolution,
            detected_at: timestamp_to_iso(issue.detected_at),
            resolved_at: issue.resolved_at.map(timestamp_to_iso),
        })
        .collect();

    let request = SyncIssuesRequest {
        project_id,
        issues: backend_issues,
    };

    // Send to backend
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/v1/issues/sync", get_api_base_url()))
        .bearer_auth(&access_token)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to sync issues: {}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("Issue sync failed with status {}: {}", status, error_text);
        return Err(format!("Failed to sync issues: {}", error_text));
    }

    let sync_response: SyncIssuesResponse = response.json().await.map_err(|e| {
        error!("Failed to parse sync response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!(
        "Issues synced successfully: {} new, {} updated",
        sync_response.synced, sync_response.updated
    );

    if !sync_response.errors.is_empty() {
        warn!(
            "Sync had {} errors: {:?}",
            sync_response.errors.len(),
            sync_response.errors
        );
    }

    Ok(sync_response)
}
