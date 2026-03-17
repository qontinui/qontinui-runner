//! Heartbeat service for fleet registration.
//!
//! Periodically sends heartbeat to the web backend so the dev dashboard
//! can track all runner instances across machines.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{debug, info};

use serde::Serialize;

use crate::commands::AppState;

#[derive(Debug, Serialize)]
struct HeartbeatPayload {
    hostname: String,
    ip: String,
    port: u16,
    instance_name: Option<String>,
    os: String,
    os_version: Option<String>,
    running_task_count: u32,
    running_task_ids: Vec<String>,
}

/// Start the heartbeat background task.
///
/// Spawns a tokio task that sends a heartbeat POST to the web backend
/// every 30 seconds. The backend URL is configured via `QONTINUI_WEB_BACKEND_URL`
/// environment variable (default: `http://localhost:8000`).
pub fn start_heartbeat(app_state: Arc<AppState>) {
    let backend_url = std::env::var("QONTINUI_WEB_BACKEND_URL")
        .unwrap_or_else(|_| "http://localhost:8000".to_string());

    let heartbeat_url = format!("{}/api/v1/dev-dashboard/heartbeat", backend_url);

    info!("Starting heartbeat service -> {}", heartbeat_url);

    tauri::async_runtime::spawn(async move {
        // Wait 5 seconds for the API to be ready before the first heartbeat
        tokio::time::sleep(Duration::from_secs(5)).await;

        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                debug!("Failed to create heartbeat HTTP client: {}", e);
                return;
            }
        };

        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let instance_name = std::env::var("QONTINUI_INSTANCE_NAME").ok();

        let os = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
        .to_string();

        let mut ticker = interval(Duration::from_secs(30));

        loop {
            ticker.tick().await;

            let port = app_state.api_port.load(Ordering::Relaxed);
            if port == 0 {
                debug!("API port not yet assigned, skipping heartbeat");
                continue;
            }

            // Get local IP address
            let ip = get_local_ip().unwrap_or_else(|| "127.0.0.1".to_string());

            // Get running task count from the database
            let (task_count, task_ids) = get_running_tasks(&app_state);

            let payload = HeartbeatPayload {
                hostname: hostname.clone(),
                ip,
                port,
                instance_name: instance_name.clone(),
                os: os.clone(),
                os_version: None,
                running_task_count: task_count,
                running_task_ids: task_ids,
            };

            match client.post(&heartbeat_url).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!("Heartbeat sent successfully");
                }
                Ok(resp) => {
                    debug!("Heartbeat response: {}", resp.status());
                }
                Err(e) => {
                    debug!("Heartbeat failed (backend may not be running): {}", e);
                }
            }
        }
    });
}

/// Get the local IP address (non-loopback).
fn get_local_ip() -> Option<String> {
    // Use a UDP socket trick to find the local IP without actually sending data
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// Get count and IDs of currently running tasks from the database.
fn get_running_tasks(app_state: &AppState) -> (u32, Vec<String>) {
    match app_state.checkpoint_db.get_running_task_runs(None) {
        Ok(runs) => {
            let count = runs.len() as u32;
            let ids = runs.into_iter().map(|r| r.id).collect();
            (count, ids)
        }
        Err(_) => (0, vec![]),
    }
}
