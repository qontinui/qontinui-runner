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
use crate::ui_error::UiError;

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
    /// Phase 3J.2 — `"healthy"` if no UI error is tracked, `"errored"`
    /// when the React error boundary has reported an unhandled error that
    /// has not yet been cleared OR a fresh Rust crash dump is present
    /// (post-3J follow-up).
    derived_status: String,
    /// Phase 3J.2 — latest UI error snapshot. Serialized as `null` when no
    /// error is tracked (always present as a key so consumers can assume
    /// the shape).
    ui_error: Option<UiError>,
    /// Post-3J follow-up — most recent Rust crash dump surfaced by the
    /// startup scanner. `null` when no fresh dump is present. Independent
    /// of `ui_error`: non-unwinding panics abort the process before the
    /// React boundary can report them.
    recent_crash: Option<HeartbeatRecentCrash>,
}

/// Snake-case projection of [`crate::crash_dumps::RecentCrash`] for the
/// heartbeat wire format. The `/health` endpoint emits the source struct as
/// camelCase to stay in sync with that endpoint's casing; qontinui-web's
/// heartbeat schema uses snake_case (matching `UiErrorPayload`) so we emit
/// a dedicated shape here.
#[derive(Debug, Clone, Serialize)]
struct HeartbeatRecentCrash {
    file_path: String,
    reported_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panic_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    panic_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<String>,
}

impl From<crate::crash_dumps::RecentCrash> for HeartbeatRecentCrash {
    fn from(c: crate::crash_dumps::RecentCrash) -> Self {
        Self {
            file_path: c.file_path,
            reported_at: c.reported_at,
            panic_location: c.panic_location,
            panic_message: c.panic_message,
            thread: c.thread,
        }
    }
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

            // Phase 3J.2 — snapshot the latest UI error (if any) once per
            // tick; used by both the backend and primary-runner heartbeats.
            // Post-3J follow-up also surfaces the most recent Rust crash
            // dump so a runner that just restarted after a non-unwinding
            // panic still reports errored until the dump is dismissed.
            let ui_error_snapshot = app_state.ui_error.get().await;
            let recent_crash_snapshot = app_state
                .crash_dumps
                .get()
                .await
                .map(HeartbeatRecentCrash::from);
            // Read the /health-driven embedding probe cache. `None` (probe
            // hasn't run yet) collapses to "healthy" so a pre-probe heartbeat
            // doesn't flap to "degraded" on boot.
            let derived_status = crate::ui_error::compute_derived_status(
                ui_error_snapshot.is_some(),
                recent_crash_snapshot.is_some(),
                crate::mcp_api::embedding_reachable_cached(),
            )
            .to_string();

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
                    derived_status: derived_status.clone(),
                    ui_error: ui_error_snapshot.clone(),
                    recent_crash: recent_crash_snapshot.clone(),
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
                    // Phase 3J.2 — include UI error status for supervisor
                    // aggregation. `ui_error` is null when healthy.
                    // Post-3J follow-up also includes `recent_crash` so a
                    // runner restarted after a Rust panic is visible to the
                    // primary's aggregation view.
                    "derived_status": derived_status,
                    "ui_error": ui_error_snapshot,
                    "recent_crash": recent_crash_snapshot,
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
