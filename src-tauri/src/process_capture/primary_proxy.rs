//! Proxy for secondary runners to forward process capture requests to the primary runner.
//!
//! When a runner is a secondary instance (QONTINUI_INSTANCE_NAME is set), it doesn't
//! start managed processes (web frontend, backend, etc.) — only the primary does.
//! This module allows secondary runners to transparently access the primary's process
//! data by proxying HTTP requests to the primary runner's API.

use once_cell::sync::Lazy;
use serde::Deserialize;

use super::types::{OutputLine, ProcessConfig, ProcessStatus};

/// Get the primary runner's port from the environment, if this is a secondary instance.
/// Returns None if this is the primary runner.
pub fn primary_port() -> Option<u16> {
    std::env::var("QONTINUI_PRIMARY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
}

/// Check if this runner is a secondary instance that should proxy process requests.
pub fn is_secondary() -> bool {
    std::env::var("QONTINUI_INSTANCE_NAME").is_ok() && primary_port().is_some()
}

/// Response wrapper matching the primary runner's ApiResponse<T> format.
#[derive(Deserialize)]
struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

/// Shared HTTP client for all proxy requests (reuses connections).
static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .pool_max_idle_per_host(4)
        .build()
        .expect("Failed to create HTTP client")
});

fn client() -> &'static reqwest::Client {
    &CLIENT
}

fn base_url() -> String {
    let port = primary_port().unwrap_or(9876);
    format!("http://127.0.0.1:{}", port)
}

/// Proxy: GET /processes/status
pub async fn get_all_status() -> Result<Vec<ProcessStatus>, String> {
    let url = format!("{}/processes/status", base_url());
    let resp = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach primary runner: {}", e))?;

    let api: ApiResponse<Vec<ProcessStatus>> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse primary response: {}", e))?;

    if api.success {
        Ok(api.data.unwrap_or_default())
    } else {
        Err(api.error.unwrap_or_else(|| "Unknown error".to_string()))
    }
}

/// Proxy: GET /processes/{id}/output?tail=N
pub async fn get_output(id: &str, tail: usize) -> Result<Vec<OutputLine>, String> {
    let url = format!("{}/processes/{}/output?tail={}", base_url(), id, tail);
    let resp = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach primary runner: {}", e))?;

    let api: ApiResponse<Vec<OutputLine>> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse primary response: {}", e))?;

    if api.success {
        Ok(api.data.unwrap_or_default())
    } else {
        Err(api.error.unwrap_or_else(|| "Unknown error".to_string()))
    }
}

/// Proxy: GET /processes (list with configs)
pub async fn get_configs() -> Result<Vec<ProcessConfig>, String> {
    // The primary doesn't have a separate /processes/configs endpoint,
    // but configs are loaded from shared settings, so the secondary
    // can read them locally. This is a fallback that returns empty.
    Ok(Vec::new())
}

/// Proxy: POST /processes/{id}/start
pub async fn start_process(id: &str) -> Result<(), String> {
    let url = format!("{}/processes/{}/start", base_url(), id);
    let resp = client()
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach primary runner: {}", e))?;

    let api: ApiResponse<String> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse primary response: {}", e))?;

    if api.success {
        Ok(())
    } else {
        Err(api.error.unwrap_or_else(|| "Unknown error".to_string()))
    }
}

/// Proxy: POST /processes/{id}/stop
pub async fn stop_process(id: &str) -> Result<(), String> {
    let url = format!("{}/processes/{}/stop", base_url(), id);
    let resp = client()
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach primary runner: {}", e))?;

    let api: ApiResponse<String> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse primary response: {}", e))?;

    if api.success {
        Ok(())
    } else {
        Err(api.error.unwrap_or_else(|| "Unknown error".to_string()))
    }
}

/// Proxy: POST /processes/{id}/restart
pub async fn restart_process(id: &str) -> Result<(), String> {
    let url = format!("{}/processes/{}/restart", base_url(), id);
    let resp = client()
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach primary runner: {}", e))?;

    let api: ApiResponse<String> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse primary response: {}", e))?;

    if api.success {
        Ok(())
    } else {
        Err(api.error.unwrap_or_else(|| "Unknown error".to_string()))
    }
}

/// Proxy: POST /processes/start-all (via individual starts)
pub async fn start_all() -> Result<(), String> {
    use super::types::ProcessState;
    let statuses = get_all_status().await?;
    for status in statuses {
        if !matches!(
            status.state,
            ProcessState::Running | ProcessState::Healthy | ProcessState::Starting
        ) {
            let _ = start_process(&status.id).await;
        }
    }
    Ok(())
}

/// Proxy: stop all via individual stops
pub async fn stop_all() -> Result<(), String> {
    use super::types::ProcessState;
    let statuses = get_all_status().await?;
    for status in statuses {
        if matches!(
            status.state,
            ProcessState::Running | ProcessState::Healthy | ProcessState::Starting
        ) {
            let _ = stop_process(&status.id).await;
        }
    }
    Ok(())
}
