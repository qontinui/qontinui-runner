//! Clipboard sync and file sharing command handlers for Tauri.
//!
//! Provides commands to push clipboard content and files to the qontinui-web
//! backend so they can be relayed to mobile devices.

use crate::auth::AuthManager;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{error, info};

/// Response from POST /api/v1/clipboard
#[derive(Debug, Serialize, Deserialize)]
pub struct ClipboardEntryResponse {
    pub id: String,
    pub user_id: String,
    pub source_device_id: String,
    pub source_device_name: String,
    pub content_type: String,
    pub text_content: Option<String>,
    pub image_url: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

/// Request body for POST /api/v1/clipboard
#[derive(Debug, Serialize)]
struct ClipboardPushRequest {
    source_device_id: String,
    source_device_name: String,
    content_type: String,
    text_content: Option<String>,
}

use crate::api_config::get_api_base_url;

/// Get device name (hostname + platform)
fn get_device_name() -> String {
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Unknown".to_string());

    let platform = match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows PC",
        "linux" => "Linux PC",
        other => other,
    };

    format!("{} ({})", hostname, platform)
}

/// Push text content to the clipboard relay backend for mobile access.
///
/// # Arguments
/// * `text` - The text content to share
///
/// # Returns
/// The created clipboard entry from the backend
#[tauri::command]
pub async fn share_to_mobile(text: String) -> Result<ClipboardEntryResponse, String> {
    info!("Sharing text to mobile ({} chars)", text.len());

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let device_id = auth_manager.get_device_id().map_err(|e| {
        error!("Failed to get device ID: {}", e);
        format!("Failed to get device ID: {}", e)
    })?;

    let request_body = ClipboardPushRequest {
        source_device_id: device_id,
        source_device_name: get_device_name(),
        content_type: "text".to_string(),
        text_content: Some(text),
    };

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/v1/clipboard", get_api_base_url()))
        .bearer_auth(&access_token)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| {
            error!("Clipboard push failed: {:?}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!(
            "Clipboard push failed with status {}: {}",
            status, error_text
        );
        return Err(format!("Failed to share: {}", error_text));
    }

    let entry: ClipboardEntryResponse = response.json().await.map_err(|e| {
        error!("Failed to parse clipboard response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!("Shared to mobile: entry_id={}", entry.id);
    Ok(entry)
}

/// Response from POST /api/v1/files/upload
#[derive(Debug, Serialize, Deserialize)]
pub struct SharedFileResponse {
    pub id: String,
    pub user_id: String,
    pub source_device_id: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub created_at: String,
    pub expires_at: String,
}

/// Upload a file to the backend for mobile access.
///
/// # Arguments
/// * `file_path` - Absolute path to the file on disk
///
/// # Returns
/// The created shared file entry from the backend
#[tauri::command]
pub async fn share_file_to_mobile(file_path: String) -> Result<SharedFileResponse, String> {
    let path = PathBuf::from(&file_path);

    if !path.exists() {
        return Err(format!("File not found: {}", file_path));
    }

    // Check file size before reading into memory (50 MB limit, matches backend)
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;
    const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
    if metadata.len() > MAX_FILE_SIZE {
        return Err(format!(
            "File too large ({:.1} MB). Maximum size is {} MB.",
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_FILE_SIZE / (1024 * 1024)
        ));
    }

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let file_bytes = tokio::fs::read(&path).await.map_err(|e| {
        error!("Failed to read file {}: {}", file_path, e);
        format!("Failed to read file: {}", e)
    })?;

    let size = file_bytes.len();
    info!("Sharing file to mobile: {} ({} bytes)", filename, size);

    let auth_manager = AuthManager::new();

    if !auth_manager.has_tokens() {
        return Err("Not authenticated. Please log in first.".to_string());
    }

    let access_token = auth_manager.get_access_token().map_err(|e| {
        error!("Failed to get access token: {}", e);
        format!("Failed to get access token: {}", e)
    })?;

    let device_id = auth_manager.get_device_id().map_err(|e| {
        error!("Failed to get device ID: {}", e);
        format!("Failed to get device ID: {}", e)
    })?;

    // Guess MIME type from extension
    let mime_type = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    let file_part = multipart::Part::bytes(file_bytes)
        .file_name(filename.clone())
        .mime_str(&mime_type)
        .map_err(|e| format!("Invalid MIME type: {}", e))?;

    let form = multipart::Form::new()
        .text("source_device_id", device_id)
        .part("file", file_part);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/api/v1/files/upload", get_api_base_url()))
        .bearer_auth(&access_token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            error!("File upload failed: {:?}", e);
            format!("Network error: {}", e)
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("File upload failed with status {}: {}", status, error_text);
        return Err(format!("Upload failed: {}", error_text));
    }

    let entry: SharedFileResponse = response.json().await.map_err(|e| {
        error!("Failed to parse file upload response: {}", e);
        format!("Invalid response from server: {}", e)
    })?;

    info!(
        "File shared to mobile: id={}, filename={}",
        entry.id, entry.filename
    );
    Ok(entry)
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_clipboard")
        .invoke_handler(tauri::generate_handler![
            share_to_mobile,
            share_file_to_mobile,
        ])
        .build()
}
