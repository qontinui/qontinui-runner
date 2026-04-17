//! HTTP API endpoints for OS-level window enumeration and activation.
//!
//! Exposes the `window_manager` module over REST so the UI Bridge and
//! external tools can list and activate browser windows.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::window_manager::{self, WindowInfo};

/// Query params for `GET /system/windows`.
#[derive(Debug, Deserialize)]
pub struct ListWindowsQuery {
    pub filter: Option<String>,
}

/// Request body for `POST /system/windows/activate`.
#[derive(Debug, Deserialize)]
pub struct ActivateWindowRequest {
    pub pid: Option<u32>,
    pub title: Option<String>,
}

/// List visible top-level OS windows, optionally filtered by title substring.
pub async fn list_system_windows(
    Query(params): Query<ListWindowsQuery>,
) -> Json<ApiResponse<Vec<WindowInfo>>> {
    let windows = window_manager::list_windows(params.filter.as_deref());
    Json(ApiResponse::success(windows))
}

/// Activate (bring to foreground) an OS window by PID or title substring.
pub async fn activate_system_window(
    Json(body): Json<ActivateWindowRequest>,
) -> Result<Json<ApiResponse<bool>>, (StatusCode, Json<ApiResponse<()>>)> {
    let result = if let Some(pid) = body.pid {
        window_manager::activate_window_by_pid(pid)
    } else if let Some(ref title) = body.title {
        window_manager::activate_window_by_title(title)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("Either 'pid' or 'title' must be provided")),
        ));
    };

    Ok(Json(ApiResponse::success(result)))
}

/// Build the axum routes for window management.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/system/windows", get(list_system_windows))
        .route("/system/windows/activate", post(activate_system_window))
}
