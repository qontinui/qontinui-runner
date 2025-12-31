//! Log API commands for exposing internal logs via HTTP API
//!
//! This module provides Tauri commands for the frontend to sync logs to Rust state,
//! and exports the log storage for use by the HTTP API server.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Log Entry Types (matching TypeScript definitions)
// ============================================================================

/// General log entry (matches TypeScript LogEntry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub timestamp: String,
    pub level: String, // "info" | "warning" | "error" | "debug" | "success"
    pub message: String,
}

/// Image recognition log entry (matches TypeScript ImageRecognitionEntry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRecognitionEntry {
    pub id: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_timestamp: Option<String>,
    pub node: String,
    pub template: String,
    pub confidence: f64,
    pub found: bool,
    pub threshold: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_off: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_match_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_data: Option<String>, // Base64 encoded
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_debug_image: Option<String>, // Base64 encoded
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_data: Option<String>, // Base64 encoded
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_region_image: Option<String>, // Base64 encoded
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_index: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Action log entry (matches TypeScript ActionLogEntry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLogEntry {
    pub action_type: String,
    pub timestamp: f64, // Unix timestamp in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// AI output log entry (matches TypeScript AiOutputEntry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiOutputEntry {
    pub id: String,
    pub timestamp: i64,
    pub line: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// Session/workflow ID for grouping loops by workflow
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Human-readable session/workflow name (the task title)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
}

/// Issue entry (matches TypeScript DetectedIssue)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueEntry {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,
    pub status: String,
    pub detected_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

/// RAG log entry (matches TypeScript RagLogEntry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagLogEntry {
    pub id: String,
    pub timestamp: i64, // Unix timestamp in milliseconds
    #[serde(rename = "type")]
    pub entry_type: String, // "info" | "progress" | "success" | "error" | "warning"
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<RagLogDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagLogDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elements_processed: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_elements: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_image_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_image_name: Option<String>,
}

/// Project log source content (matches TypeScript LogSourceContent)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectLogSource {
    pub id: String,
    pub name: String,
    pub path: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ============================================================================
// Log Store - Thread-safe storage for all log types
// ============================================================================

/// Central storage for all log types, accessible from both Tauri commands and HTTP API
#[derive(Default)]
pub struct LogApiStore {
    pub general_logs: Vec<LogEntry>,
    pub image_logs: Vec<ImageRecognitionEntry>,
    pub action_logs: Vec<ActionLogEntry>,
    pub ai_output_logs: Vec<AiOutputEntry>,
    pub issues: Vec<IssueEntry>,
    pub rag_logs: Vec<RagLogEntry>,
    pub project_logs: Vec<ProjectLogSource>,
}

impl LogApiStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_all(&mut self) {
        self.general_logs.clear();
        self.image_logs.clear();
        self.action_logs.clear();
        self.ai_output_logs.clear();
        self.issues.clear();
        self.rag_logs.clear();
        self.project_logs.clear();
    }
}

/// Global log store wrapped in Arc<RwLock> for thread-safe access
pub type SharedLogStore = Arc<RwLock<LogApiStore>>;

/// Create a new shared log store
pub fn create_log_store() -> SharedLogStore {
    Arc::new(RwLock::new(LogApiStore::new()))
}

// ============================================================================
// Tauri Commands - Frontend syncs logs to Rust state
// ============================================================================

/// Sync general logs from frontend to Rust state
#[tauri::command]
pub async fn sync_general_logs(
    logs: Vec<LogEntry>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.general_logs = logs;
    info!("Synced {} general logs from frontend", store.general_logs.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} general logs", store.general_logs.len())),
        data: None,
    })
}

/// Sync image recognition logs from frontend to Rust state
#[tauri::command]
pub async fn sync_image_logs(
    logs: Vec<ImageRecognitionEntry>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.image_logs = logs;
    info!("Synced {} image logs from frontend", store.image_logs.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} image logs", store.image_logs.len())),
        data: None,
    })
}

/// Sync action logs from frontend to Rust state
#[tauri::command]
pub async fn sync_action_logs(
    logs: Vec<ActionLogEntry>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.action_logs = logs;
    info!("Synced {} action logs from frontend", store.action_logs.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} action logs", store.action_logs.len())),
        data: None,
    })
}

/// Sync AI output logs from frontend to Rust state
#[tauri::command]
pub async fn sync_ai_output_logs(
    logs: Vec<AiOutputEntry>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.ai_output_logs = logs;
    info!("Synced {} AI output logs from frontend", store.ai_output_logs.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} AI output logs", store.ai_output_logs.len())),
        data: None,
    })
}

/// Sync issues from frontend to Rust state
#[tauri::command]
pub async fn sync_issues(
    issues: Vec<IssueEntry>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.issues = issues;
    info!("Synced {} issues from frontend", store.issues.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} issues", store.issues.len())),
        data: None,
    })
}

/// Sync RAG logs from frontend to Rust state
#[tauri::command]
pub async fn sync_rag_logs(
    logs: Vec<RagLogEntry>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.rag_logs = logs;
    info!("Synced {} RAG logs from frontend", store.rag_logs.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} RAG logs", store.rag_logs.len())),
        data: None,
    })
}

/// Sync project logs from frontend to Rust state
#[tauri::command]
pub async fn sync_project_logs(
    logs: Vec<ProjectLogSource>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.project_logs = logs;
    info!("Synced {} project log sources from frontend", store.project_logs.len());
    Ok(CommandResponse {
        success: true,
        message: Some(format!("Synced {} project log sources", store.project_logs.len())),
        data: None,
    })
}

/// Sync all logs at once (more efficient for initial sync)
#[tauri::command]
pub async fn sync_all_logs(
    general: Vec<LogEntry>,
    image: Vec<ImageRecognitionEntry>,
    actions: Vec<ActionLogEntry>,
    ai_output: Vec<AiOutputEntry>,
    issues: Vec<IssueEntry>,
    rag: Vec<RagLogEntry>,
    project: Vec<ProjectLogSource>,
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.general_logs = general;
    store.image_logs = image;
    store.action_logs = actions;
    store.ai_output_logs = ai_output;
    store.issues = issues;
    store.rag_logs = rag;
    store.project_logs = project;

    info!(
        "Synced all logs: general={}, image={}, actions={}, ai={}, issues={}, rag={}, project={}",
        store.general_logs.len(),
        store.image_logs.len(),
        store.action_logs.len(),
        store.ai_output_logs.len(),
        store.issues.len(),
        store.rag_logs.len(),
        store.project_logs.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some("Synced all logs".to_string()),
        data: Some(serde_json::json!({
            "general": store.general_logs.len(),
            "image": store.image_logs.len(),
            "actions": store.action_logs.len(),
            "ai_output": store.ai_output_logs.len(),
            "issues": store.issues.len(),
            "rag": store.rag_logs.len(),
            "project": store.project_logs.len()
        })),
    })
}

/// Clear all logs in Rust state
#[tauri::command]
pub async fn clear_log_api_store(
    state: tauri::State<'_, SharedLogStore>,
) -> Result<CommandResponse, String> {
    let mut store = state.write().await;
    store.clear_all();
    info!("Cleared all logs in API store");
    Ok(CommandResponse {
        success: true,
        message: Some("Cleared all logs".to_string()),
        data: None,
    })
}
