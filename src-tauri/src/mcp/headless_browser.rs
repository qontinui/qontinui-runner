//! Headless browser session management for CLI-based UI Bridge testing.
//!
//! Launches Playwright Chromium instances that navigate to target URLs,
//! allowing the target app's UI Bridge SDK to connect automatically.
//! This enables UI Bridge commands to work from CLI contexts without
//! a manually-opened browser.
//!
//! Phase 3 (Flywheel v1) addition: `spawn_headless` waits for SDK connection
//! to register after launching, enabling unattended gate 4 validation.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::types::{ApiResponse, ApiState};

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequest {
    /// Target URL to navigate to (e.g., "http://localhost:3001/event-history")
    pub url: String,
    /// Whether to run headless (default: true)
    pub headless: Option<bool>,
    /// Navigation timeout in ms (default: 30000)
    pub timeout_ms: Option<u64>,
}

/// Phase 3: Request to spawn a headless browser with SDK connection waiting.
/// Used by the flywheel validator's gate 4 to establish unattended UI Bridge
/// snapshots. Waits up to 10 seconds for the SDK to register before returning.
#[cfg(feature = "spec-authoring")]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnHeadlessRequest {
    /// Target URL to navigate to (e.g., "http://localhost:3001/spec-check")
    pub url: String,
    /// App ID this headless session targets (must be registered in app_registry)
    pub app_id: String,
    /// Whether to run headless (default: true)
    pub headless: Option<bool>,
    /// Navigation timeout in ms (default: 30000)
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadlessSession {
    pub session_id: String,
    pub url: String,
    pub pid: u32,
    pub launched_at: String,
    pub status: String,
    /// Phase 3: App ID this session is connected to (None for legacy launch_headless)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseRequest {
    pub session_id: String,
}

struct SessionEntry {
    session: HeadlessSession,
    child: tokio::process::Child,
}

/// Manages active headless browser sessions.
pub struct HeadlessSessionManager {
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

impl HeadlessSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

// Global singleton
static SESSION_MANAGER: once_cell::sync::Lazy<HeadlessSessionManager> =
    once_cell::sync::Lazy::new(HeadlessSessionManager::new);

// =============================================================================
// Handlers
// =============================================================================

/// Launch a headless browser and navigate to a URL.
pub async fn launch_headless(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<LaunchRequest>,
) -> Result<Json<ApiResponse<HeadlessSession>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let headless = request.headless.unwrap_or(true);
    let timeout = request.timeout_ms.unwrap_or(30000);

    info!(
        "Launching headless browser: url={}, headless={}, timeout={}ms",
        request.url, headless, timeout
    );

    // Find the launcher script
    let script_paths = [
        std::path::PathBuf::from("scripts/headless-launcher.js"),
        std::path::PathBuf::from("../scripts/headless-launcher.js"),
    ];

    let script = script_paths
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::error(
                    "headless-launcher.js not found. Install Playwright: npx playwright install chromium".to_string(),
                )),
            )
        })?;

    // Spawn the Node.js process
    let mut cmd = tokio::process::Command::new("node");
    cmd.arg(script)
        .arg(format!("--url={}", request.url))
        .arg(format!("--timeout={}", timeout));

    if headless {
        cmd.arg("--headless");
    } else {
        cmd.arg("--headless=false");
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Prevent child from blocking on stdin
    cmd.stdin(std::process::Stdio::null());

    let child = cmd.spawn().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::error(format!(
                "Failed to spawn headless browser: {}. Ensure Node.js and Playwright are installed.",
                e
            ))),
        )
    })?;

    let pid = child.id().unwrap_or(0);

    let session = HeadlessSession {
        session_id: session_id.clone(),
        url: request.url.clone(),
        pid,
        launched_at: chrono::Utc::now().to_rfc3339(),
        status: "launched".to_string(),
        app_id: None, // Legacy launch_headless doesn't track app connection
    };

    // Wait briefly for the "ready" signal
    // In a real implementation we'd read stdout lines, but for simplicity
    // we just wait a few seconds for the browser to navigate
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    // Store the session
    {
        let mut sessions = SESSION_MANAGER.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            SessionEntry {
                session: session.clone(),
                child,
            },
        );
    }

    info!(
        "Headless browser launched: session={}, pid={}, url={}",
        session_id, pid, request.url
    );

    Ok(Json(ApiResponse::success(session)))
}

/// List all active headless browser sessions.
pub async fn list_sessions(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<HeadlessSession>>> {
    let sessions = SESSION_MANAGER.sessions.lock().await;
    let list: Vec<HeadlessSession> = sessions.values().map(|e| e.session.clone()).collect();
    Json(ApiResponse::success(list))
}

/// Close a headless browser session.
pub async fn close_session(
    State(_state): State<Arc<ApiState>>,
    Json(request): Json<CloseRequest>,
) -> Result<Json<ApiResponse<String>>, (StatusCode, Json<ApiResponse<()>>)> {
    let mut sessions = SESSION_MANAGER.sessions.lock().await;

    if let Some(mut entry) = sessions.remove(&request.session_id) {
        info!(
            "Closing headless browser: session={}, pid={}",
            request.session_id, entry.session.pid
        );
        // Kill the child process
        let _ = entry.child.kill().await;
        Ok(Json(ApiResponse::success(format!(
            "Session {} closed",
            request.session_id
        ))))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse::error(format!(
                "Session '{}' not found",
                request.session_id
            ))),
        ))
    }
}

// =============================================================================
// Phase 3: Spawn-headless with SDK connection waiting
// =============================================================================

/// Phase 3: Spawn a headless browser and wait for SDK connection to register.
/// This is used by the flywheel validator's gate 4 to establish unattended
/// UI Bridge snapshots for spec-check evaluation.
///
/// Waits up to 10 seconds for the app's SDK to connect before returning.
/// If no connection appears within the timeout, returns 504 Gateway Timeout.
#[cfg(feature = "spec-authoring")]
pub async fn spawn_headless(
    State(state): State<Arc<ApiState>>,
    axum::Json(request): axum::Json<SpawnHeadlessRequest>,
) -> Result<axum::Json<ApiResponse<HeadlessSession>>, (StatusCode, axum::Json<ApiResponse<()>>)> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let headless = request.headless.unwrap_or(true);
    let timeout = request.timeout_ms.unwrap_or(30000);

    info!(
        "spawn_headless: url={}, app_id={}, headless={}, timeout={}ms",
        request.url, request.app_id, headless, timeout
    );

    // Find the launcher script
    let script_paths = [
        std::path::PathBuf::from("scripts/headless-launcher.js"),
        std::path::PathBuf::from("../scripts/headless-launcher.js"),
    ];

    let script = script_paths
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(ApiResponse::error(
                    "headless-launcher.js not found. Install Playwright: npx playwright install chromium".to_string(),
                )),
            )
        })?;

    // Spawn the Node.js process
    let mut cmd = tokio::process::Command::new("node");
    cmd.arg(script)
        .arg(format!("--url={}", request.url))
        .arg(format!("--timeout={}", timeout));

    if headless {
        cmd.arg("--headless");
    } else {
        cmd.arg("--headless=false");
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Prevent child from blocking on stdin
    cmd.stdin(std::process::Stdio::null());

    let child = cmd.spawn().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(ApiResponse::error(format!(
                "Failed to spawn headless browser: {}. Ensure Node.js and Playwright are installed.",
                e
            ))),
        )
    })?;

    let pid = child.id().unwrap_or(0);

    // Wait for the browser to navigate and SDK to connect (up to 10 seconds).
    // The initial 3s sleep matches launch_headless. Then poll the SDK connection
    // manager for the specified app_id with 100ms backoff.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let connection_app_id = {
        let start = tokio::time::Instant::now();
        let max_wait = Duration::from_secs(10);
        let mut found_app_id: Option<String> = None;

        while start.elapsed() < max_wait {
            let guard = state.sdk_connection.lock().await;
            if let Some(conn) = guard.active_connection() {
                let connected_app_id = conn.app_info.app_id.clone();
                // Verify it matches the requested app
                if connected_app_id == request.app_id {
                    found_app_id = Some(connected_app_id);
                    debug!(
                        "spawn_headless: SDK connection established for app_id={}",
                        request.app_id
                    );
                    break;
                } else {
                    debug!(
                        "spawn_headless: SDK connected to wrong app (expected {}, got {})",
                        request.app_id, connected_app_id
                    );
                }
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        found_app_id
    };

    if connection_app_id.is_none() {
        warn!(
            "spawn_headless: timeout waiting for SDK connection (app_id={}, session={})",
            request.app_id, session_id
        );
        // Kill the child process since it failed to connect
        let _ = child.id().and_then(|_| {
            Some({
                let _ = std::process::Command::new("taskkill")
                    .args(&["/PID", &pid.to_string(), "/F"])
                    .output();
            })
        });

        return Err((
            StatusCode::GATEWAY_TIMEOUT,
            axum::Json(ApiResponse::error(format!(
                "SDK connection not established within 10 seconds for app_id={}",
                request.app_id
            ))),
        ));
    }

    let session = HeadlessSession {
        session_id: session_id.clone(),
        url: request.url.clone(),
        pid,
        launched_at: chrono::Utc::now().to_rfc3339(),
        status: "connected".to_string(),
        app_id: connection_app_id.clone(),
    };

    // Store the session
    {
        let mut sessions = SESSION_MANAGER.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            SessionEntry {
                session: session.clone(),
                child,
            },
        );
    }

    info!(
        "spawn_headless: established session={}, pid={}, app_id={}, url={}",
        session_id, pid, request.app_id, request.url
    );

    Ok(axum::Json(ApiResponse::success(session)))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};

    let mut r = axum::Router::new()
        .route("/ui-bridge/headless/launch", post(launch_headless))
        .route("/ui-bridge/headless/sessions", get(list_sessions))
        .route("/ui-bridge/headless/close", post(close_session));

    // Phase 3: spawn-headless route for flywheel validator gate 4
    #[cfg(feature = "spec-authoring")]
    {
        r = r.route(
            "/ui-bridge/control/sdk/spawn-headless",
            post(spawn_headless),
        );
    }

    r
}
