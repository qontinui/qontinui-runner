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
/// every 30 seconds. If this is a secondary instance (`QONTINUI_PRIMARY_PORT`
/// is set), also sends a heartbeat to the primary runner every 15 seconds.
pub fn start_heartbeat(app_state: Arc<AppState>) {
    let backend_url = std::env::var("QONTINUI_WEB_BACKEND_URL")
        .unwrap_or_else(|_| "http://localhost:8000".to_string());

    let heartbeat_url = format!("{}/api/v1/dev-dashboard/heartbeat", backend_url);

    // If this is a secondary, also heartbeat to the primary runner
    let primary_port = crate::instance::primary_port();
    let registration_id: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    if let Some(pp) = primary_port {
        info!(
            "Starting heartbeat service -> {} + primary runner on port {}",
            heartbeat_url, pp
        );
    } else {
        info!("Starting heartbeat service -> {}", heartbeat_url);
    }

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

        // Backend heartbeat: every 30s. Primary heartbeat: every 15s.
        // We tick every 15s and send the backend heartbeat every other tick.
        let mut ticker = interval(Duration::from_secs(15));
        let mut tick_count: u64 = 0;

        loop {
            ticker.tick().await;
            tick_count += 1;

            let port = app_state.api_port.load(Ordering::Relaxed);
            if port == 0 {
                debug!("API port not yet assigned, skipping heartbeat");
                continue;
            }

            // Get running task count from PostgreSQL
            let (task_count, task_ids) = match app_state.pg_db.get_running_task_runs(None).await {
                Ok(runs) => {
                    let count = runs.len() as u32;
                    let ids = runs.into_iter().map(|r| r.id).collect();
                    (count, ids)
                }
                Err(_) => get_running_tasks_fallback(),
            };

            // Send to web backend every 30s (every other tick)
            if tick_count.is_multiple_of(2) {
                let ip = get_local_ip().unwrap_or_else(|| "127.0.0.1".to_string());
                let payload = HeartbeatPayload {
                    hostname: hostname.clone(),
                    ip,
                    port,
                    instance_name: instance_name.clone(),
                    os: os.clone(),
                    os_version: None,
                    running_task_count: task_count,
                    running_task_ids: task_ids.clone(),
                };

                match client.post(&heartbeat_url).json(&payload).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("Heartbeat sent to backend");
                    }
                    Ok(resp) => {
                        debug!("Backend heartbeat response: {}", resp.status());
                    }
                    Err(e) => {
                        debug!("Backend heartbeat failed: {}", e);
                    }
                }
            }

            // Send to primary runner every 15s (every tick) if we're a secondary
            if let Some(pp) = primary_port {
                // Resolve our registration ID (cached after first successful registration)
                let reg_id = {
                    let guard = registration_id.lock().await;
                    guard.clone()
                };

                let id = match reg_id {
                    Some(id) => id,
                    None => {
                        // Try to register first
                        if let Some(id) = crate::instance::register_with_primary().await {
                            let mut guard = registration_id.lock().await;
                            *guard = Some(id.clone());
                            id
                        } else {
                            continue;
                        }
                    }
                };

                let url = format!("http://127.0.0.1:{}/instances/{}/heartbeat", pp, id);
                let body = serde_json::json!({
                    "running_task_count": task_count,
                    "running_task_ids": task_ids,
                });

                match client.post(&url).json(&body).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        debug!("Heartbeat sent to primary runner");
                    }
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                        // Primary doesn't know us — clear cached ID so we re-register
                        debug!("Primary returned 404 for heartbeat — will re-register");
                        let mut guard = registration_id.lock().await;
                        *guard = None;
                    }
                    Ok(_) | Err(_) => {
                        debug!("Primary runner heartbeat failed (primary may be down)");
                    }
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

/// Fallback when PG query fails — returns empty.
fn get_running_tasks_fallback() -> (u32, Vec<String>) {
    (0, vec![])
}
