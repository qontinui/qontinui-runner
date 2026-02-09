//! Interaction recording HTTP endpoints for MCP API
//!
//! Provides HTTP handlers for starting, stopping, and querying
//! interaction recording sessions (video + input capture).
//! These endpoints call the same underlying logic as the Tauri commands
//! in `commands/interaction.rs`.

use axum::{extract::State, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tracing::{error, info};

use crate::executor::{is_default_bridge_running, with_default_bridge};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Request body for starting interaction recording
#[derive(Debug, Deserialize)]
pub struct StartRecordingRequest {
    /// Frames per second for video recording (default: 30)
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Optional output directory (defaults to .dev-logs/interactions)
    pub output_dir: Option<String>,
}

fn default_fps() -> u32 {
    30
}

/// Response for start recording
#[derive(Debug, Serialize)]
pub struct StartRecordingResponse {
    pub session_id: String,
    pub status: String,
}

/// Response for stop recording
#[derive(Debug, Serialize)]
pub struct StopRecordingResponse {
    pub status: String,
}

/// Response for recording status
#[derive(Debug, Serialize)]
pub struct RecordingStatusResponse {
    pub is_recording: bool,
    pub session_id: Option<String>,
    pub duration: Option<f64>,
    pub events_count: Option<u64>,
    pub fps: Option<u32>,
}

/// POST /interaction-recording/start
///
/// Start interaction recording (video + input capture).
/// Generates a UUID for the session and delegates to the Python bridge.
pub async fn start_interaction_recording_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<StartRecordingRequest>,
) -> Result<Json<ApiResponse<StartRecordingResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let session_id = uuid::Uuid::new_v4().to_string();

    info!(
        "HTTP: Starting interaction recording: session_id={}, fps={}",
        session_id, req.fps
    );

    if !is_default_bridge_running(&state.app_state) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(
                "Python executor is not running. Please start the executor first.",
            )),
        ));
    }

    let fps = req.fps;
    let output_dir = req.output_dir.clone();
    let sid = session_id.clone();

    let params = json!({
        "session_id": sid,
        "fps": fps,
        "output_dir": output_dir,
    });

    let result = with_default_bridge(&state.app_state, |bridge| {
        bridge
            .send_command("start_interaction_recording", Some(params))
            .map_err(|e| {
                error!("Failed to start interaction recording: {}", e);
                format!("Failed to start interaction recording: {}", e)
            })
    });

    match result {
        Ok(Ok(())) => Ok(Json(ApiResponse::success(StartRecordingResponse {
            session_id,
            status: "recording".to_string(),
        }))),
        Ok(Err(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to start interaction recording: {}",
                e
            ))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to access Python bridge: {}", e))),
        )),
    }
}

/// POST /interaction-recording/stop
///
/// Stop interaction recording and save the captured data.
pub async fn stop_interaction_recording_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<StopRecordingResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!("HTTP: Stopping interaction recording");

    if !is_default_bridge_running(&state.app_state) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(
                "Python executor is not running. Please start the executor first.",
            )),
        ));
    }

    let result = with_default_bridge(&state.app_state, |bridge| {
        bridge
            .send_command("stop_interaction_recording", None)
            .map_err(|e| {
                error!("Failed to stop interaction recording: {}", e);
                format!("Failed to stop interaction recording: {}", e)
            })
    });

    match result {
        Ok(Ok(())) => Ok(Json(ApiResponse::success(StopRecordingResponse {
            status: "stopped".to_string(),
        }))),
        Ok(Err(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to stop interaction recording: {}",
                e
            ))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to access Python bridge: {}", e))),
        )),
    }
}

/// GET /interaction-recording/status
///
/// Get the current interaction recording status.
/// Note: The Python bridge's send_command is fire-and-forget, so this
/// endpoint sends the status query command and returns a default response.
/// The actual status is managed on the Python side.
pub async fn get_interaction_recording_status_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<RecordingStatusResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    // Return not recording if executor is not running
    if !is_default_bridge_running(&state.app_state) {
        return Ok(Json(ApiResponse::success(RecordingStatusResponse {
            is_recording: false,
            session_id: None,
            duration: None,
            events_count: None,
            fps: None,
        })));
    }

    let result = with_default_bridge(&state.app_state, |bridge| {
        bridge
            .send_command("get_interaction_recording_status", None)
            .map_err(|e| {
                error!("Failed to get interaction recording status: {}", e);
                format!("Failed to get interaction recording status: {}", e)
            })
    });

    match result {
        Ok(Ok(())) => {
            // send_command is fire-and-forget; return default status.
            // The actual recording status is managed by the Python executor.
            Ok(Json(ApiResponse::success(RecordingStatusResponse {
                is_recording: false,
                session_id: None,
                duration: None,
                events_count: None,
                fps: None,
            })))
        }
        Ok(Err(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "Failed to get interaction recording status: {}",
                e
            ))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to access Python bridge: {}", e))),
        )),
    }
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/interaction-recording/start",
            post(start_interaction_recording_handler),
        )
        .route(
            "/interaction-recording/stop",
            post(stop_interaction_recording_handler),
        )
        .route(
            "/interaction-recording/status",
            get(get_interaction_recording_status_handler),
        )
}
