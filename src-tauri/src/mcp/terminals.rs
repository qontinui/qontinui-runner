//! Terminal session HTTP + WebSocket handlers for MCP API
//!
//! Provides HTTP handlers for terminal CRUD and a WebSocket endpoint
//! for bidirectional terminal I/O (output streaming + input/resize).

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{IntoResponse, Json},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use tauri::Manager;
use tracing::{debug, error, info, warn};

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use crate::terminal::TerminalManager;

// ============================================================================
// Request Types
// ============================================================================

/// Request body for creating a new terminal session.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
    /// Which terminal page this session belongs to.
    #[serde(default)]
    pub page_id: Option<String>,
    /// Optional command to execute immediately after shell initialization.
    #[serde(default)]
    pub initial_command: Option<String>,
}

/// Request body for writing data to a terminal.
#[derive(Debug, Deserialize)]
pub struct WriteTerminalRequest {
    /// Base64-encoded bytes to write to terminal stdin.
    pub data: String,
}

/// Request body for resizing a terminal.
#[derive(Debug, Deserialize)]
pub struct ResizeTerminalRequest {
    pub cols: u16,
    pub rows: u16,
}

// ============================================================================
// Helpers
// ============================================================================

/// Get the TerminalManager from Tauri managed state.
fn get_terminal_manager(state: &ApiState) -> Arc<TerminalManager> {
    state
        .app_handle
        .state::<Arc<TerminalManager>>()
        .inner()
        .clone()
}

// ============================================================================
// Handlers
// ============================================================================

/// List all terminal sessions.
pub async fn list_terminals_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let terminals = terminal_manager.list();

    Ok(Json(ApiResponse::success(serde_json::json!({
        "terminals": terminals
    }))))
}

/// Create a new terminal session.
pub async fn create_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let app_handle = state.app_handle.clone();

    info!(
        "HTTP: Creating terminal session (title={:?}, working_dir={:?})",
        request.title, request.working_dir
    );

    match terminal_manager.create(
        request.title,
        request.working_dir,
        request.page_id,
        request.cols,
        request.rows,
        app_handle,
    ) {
        Ok(info) => {
            info!("HTTP: Created terminal session: {}", info.id);

            // Send initial command after a short delay for shell initialization
            if let Some(cmd) = request.initial_command {
                let mgr = terminal_manager.clone();
                let tid = info.id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    if let Some(session) = mgr.get(&tid) {
                        let cmd = format!("{}\r\n", cmd);
                        let _ = session.write(cmd.as_bytes());
                    }
                });
            }

            Ok(Json(ApiResponse::success(serde_json::json!(info))))
        }
        Err(e) => {
            error!("HTTP: Failed to create terminal session: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Failed to create terminal: {}", e))),
            ))
        }
    }
}

/// Write data to a terminal's PTY stdin.
pub async fn write_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<WriteTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    let bytes = STANDARD.decode(&request.data).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(api_error(format!("Invalid base64 data: {}", e))),
        )
    })?;

    session.write(&bytes).map_err(|e| {
        error!("HTTP: Failed to write to terminal {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to write to terminal: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(
        serde_json::json!({ "written": bytes.len() }),
    )))
}

/// Get the scrollback buffer for a terminal session.
pub async fn get_buffer_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    let (data, start_offset) = session.get_scrollback_buffer();
    let total_bytes_produced = session.info().total_bytes_produced;
    let encoded = STANDARD.encode(&data);

    Ok(Json(ApiResponse::success(serde_json::json!({
        "data": encoded,
        "start_offset": start_offset,
        "total_bytes_produced": total_bytes_produced,
    }))))
}

/// Resize a terminal's PTY dimensions.
pub async fn resize_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<ResizeTerminalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    session.resize(request.cols, request.rows).map_err(|e| {
        error!("HTTP: Failed to resize terminal {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("Failed to resize terminal: {}", e))),
        )
    })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "cols": request.cols,
        "rows": request.rows,
    }))))
}

/// Close a terminal session.
pub async fn close_terminal_handler(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);
    let manager = terminal_manager.clone();
    let terminal_id = id.clone();

    info!("HTTP: Closing terminal session: {}", id);

    tokio::task::spawn_blocking(move || manager.close(&terminal_id))
        .await
        .map_err(|e| {
            error!("HTTP: spawn_blocking error for close terminal: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("Internal error: {}", e))),
            )
        })?
        .map_err(|e| {
            error!("HTTP: Failed to close terminal {}: {}", id, e);
            (
                StatusCode::NOT_FOUND,
                Json(api_error(format!("Terminal not found: {}", id))),
            )
        })?;

    Ok(Json(ApiResponse::success(serde_json::json!({
        "closed": true,
        "id": id
    }))))
}

/// WebSocket endpoint for bidirectional terminal I/O.
///
/// Upgrades to WebSocket, then:
/// - Sends scrollback buffer as initial `buffer` frame
/// - Streams live output as `output` frames
/// - Accepts `input` frames (keystrokes) and `resize` frames from the client
/// - Sends `exit` frame when the terminal process dies
pub async fn ws_terminal_handler(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(state): State<Arc<ApiState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiResponse<()>>)> {
    let terminal_manager = get_terminal_manager(&state);

    // Validate terminal exists before upgrading the connection
    let session = terminal_manager.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(api_error(format!("Terminal not found: {}", id))),
        )
    })?;

    // Subscribe before reading scrollback to avoid missing data in between
    let output_rx = session.subscribe_output();
    let (scrollback_data, _) = session.get_scrollback_buffer();
    let is_alive = session.is_alive();

    info!("WebSocket client connecting for terminal {}", id);

    Ok(ws.on_upgrade(move |socket| {
        handle_ws_terminal(socket, id, session, output_rx, scrollback_data, is_alive)
    }))
}

/// Handle a WebSocket connection for a terminal session.
async fn handle_ws_terminal(
    socket: WebSocket,
    terminal_id: String,
    session: Arc<crate::terminal::session::TerminalSession>,
    mut output_rx: tokio::sync::broadcast::Receiver<String>,
    scrollback_data: Vec<u8>,
    is_alive: bool,
) {
    let (mut sender, mut receiver) = socket.split();

    info!("WebSocket connected for terminal {}", terminal_id);

    // Send scrollback buffer as initial frame
    if !scrollback_data.is_empty() {
        let encoded = STANDARD.encode(&scrollback_data);
        let msg = serde_json::json!({"type": "buffer", "data": encoded}).to_string();
        if sender.send(Message::Text(msg.into())).await.is_err() {
            debug!("WebSocket disconnected during scrollback send for terminal {}", terminal_id);
            return;
        }
    }

    // If terminal is already dead, send exit and close
    if !is_alive {
        let exit_code = session.info().exit_code;
        let msg = serde_json::json!({"type": "exit", "exitCode": exit_code}).to_string();
        let _ = sender.send(Message::Text(msg.into())).await;
        return;
    }

    // Spawn output forwarding task: broadcast → WebSocket sender
    let tid_out = terminal_id.clone();
    let session_out = session.clone();
    let output_task = tokio::spawn(async move {
        loop {
            match output_rx.recv().await {
                Ok(data) => {
                    let msg = serde_json::json!({"type": "output", "data": data}).to_string();
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        debug!("WebSocket sender closed for terminal {}", tid_out);
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WebSocket client lagged for terminal {}, skipped {} chunks", tid_out, n);
                    let msg = serde_json::json!({
                        "type": "warning",
                        "message": format!("Skipped {} output chunks due to lag", n)
                    })
                    .to_string();
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    // Broadcast channel closed — terminal exited
                    let exit_code = session_out.info().exit_code;
                    let msg =
                        serde_json::json!({"type": "exit", "exitCode": exit_code}).to_string();
                    let _ = sender.send(Message::Text(msg.into())).await;
                    break;
                }
            }
        }
    });

    // Inbound loop: receive client frames → dispatch to terminal session
    while let Some(result) = receiver.next().await {
        match result {
            Ok(Message::Text(text)) => {
                if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&text) {
                    match msg.get("type").and_then(|t| t.as_str()) {
                        Some("input") => {
                            if let Some(data) = msg.get("data").and_then(|d| d.as_str()) {
                                if let Ok(bytes) = STANDARD.decode(data) {
                                    if let Err(e) = session.write(&bytes) {
                                        warn!("Failed to write to terminal {}: {}", terminal_id, e);
                                    }
                                }
                            }
                        }
                        Some("resize") => {
                            if let (Some(cols), Some(rows)) = (
                                msg.get("cols").and_then(|c| c.as_u64()),
                                msg.get("rows").and_then(|r| r.as_u64()),
                            ) {
                                if let Err(e) = session.resize(cols as u16, rows as u16) {
                                    warn!("Failed to resize terminal {}: {}", terminal_id, e);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {} // Ping/Pong handled automatically by axum
            Err(e) => {
                debug!("WebSocket receive error for terminal {}: {}", terminal_id, e);
                break;
            }
        }
    }

    // Clean up
    output_task.abort();
    info!("WebSocket disconnected for terminal {}", terminal_id);
}

// ============================================================================
// Routes
// ============================================================================

/// Create routes for this module.
pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::{delete, get, post};

    axum::Router::new()
        .route(
            "/terminals",
            get(list_terminals_handler).post(create_terminal_handler),
        )
        .route("/terminals/{id}/write", post(write_terminal_handler))
        .route("/terminals/{id}/buffer", get(get_buffer_handler))
        .route("/terminals/{id}/resize", post(resize_terminal_handler))
        .route("/terminals/{id}/ws", get(ws_terminal_handler))
        .route("/terminals/{id}", delete(close_terminal_handler))
}
