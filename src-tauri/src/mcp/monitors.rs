//! Monitor handlers for MCP API
//!
//! Provides HTTP handlers for monitor information:
//! list monitors with position, size, and spatial layout.

use axum::{extract::State, http::StatusCode, response::Json};
use std::sync::Arc;
use tauri::Manager;

use crate::mcp::types::{api_error, ApiResponse, ApiState, MonitorInfoResponse, MonitorsResponse};

// ============================================================================
// Handlers
// ============================================================================

/// Get available monitors with position information
pub async fn get_monitors(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<MonitorsResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let app_handle = state.app_handle.clone();

    let window = app_handle.get_webview_window("main").ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error("Failed to get main window")),
        )
    })?;

    let monitors = window.available_monitors().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get monitors: {}", e))),
        )
    })?;

    let primary_monitor = window.current_monitor().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to get current monitor: {}", e))),
        )
    })?;

    // Collect x positions for determining spatial layout
    let x_positions: Vec<i32> = monitors.iter().map(|m| m.position().x).collect();
    let min_x = x_positions.iter().min().copied().unwrap_or(0);
    let max_x = x_positions.iter().max().copied().unwrap_or(0);

    // Build monitor info with positions matching qontinui-schemas/geometry
    let monitor_infos: Vec<MonitorInfoResponse> = monitors
        .iter()
        .enumerate()
        .map(|(idx, monitor)| {
            let mon_position = monitor.position();
            let mon_size = monitor.size();
            let scale_factor = monitor.scale_factor();
            let name = monitor.name().map(|n| n.to_string());

            let is_primary = match &primary_monitor {
                Some(current) => {
                    let current_pos = current.position();
                    let current_size = current.size();
                    mon_position.x == current_pos.x
                        && mon_position.y == current_pos.y
                        && mon_size.width == current_size.width
                        && mon_size.height == current_size.height
                }
                None => idx == 0,
            };

            // Determine position based on x coordinate (matches schema: "left", "center", "right")
            let position = if monitors.len() == 1 {
                "center".to_string()
            } else if mon_position.x == min_x {
                "left".to_string()
            } else if mon_position.x == max_x {
                "right".to_string()
            } else {
                "center".to_string()
            };

            // Build description
            let mut desc_parts = vec![format!("Monitor {}", idx)];
            if is_primary {
                desc_parts.push("primary".to_string());
            }
            desc_parts.push(position.clone());
            desc_parts.push(format!("{}x{}", mon_size.width, mon_size.height));
            let description = format!("{} ({})", desc_parts[0], desc_parts[1..].join(", "));

            MonitorInfoResponse {
                index: idx,
                x: mon_position.x,
                y: mon_position.y,
                width: mon_size.width,
                height: mon_size.height,
                position,
                is_primary: Some(is_primary),
                scale_factor: Some(scale_factor),
                name,
                description: Some(description),
            }
        })
        .collect();

    // Build available descriptors
    let mut descriptors = vec!["primary".to_string()];
    for m in &monitor_infos {
        if !descriptors.contains(&m.position) {
            descriptors.push(m.position.clone());
        }
    }
    for m in &monitor_infos {
        descriptors.push(m.index.to_string());
    }

    Ok(Json(ApiResponse::success(MonitorsResponse {
        count: monitor_infos.len(),
        monitors: monitor_infos,
        available_descriptors: descriptors,
    })))
}

/// Create routes for this module.
pub fn routes() -> axum::Router<std::sync::Arc<crate::mcp::types::ApiState>> {
    use axum::routing::get;
    axum::Router::new().route("/monitors", get(get_monitors))
}
