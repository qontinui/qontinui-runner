//! MCP API Server
//!
//! Provides an HTTP API for the MCP server to communicate with the runner.
//! This allows Claude Code (running in WSL) to control the Windows runner.
//!
//! # Multi-Monitor Coordinate System
//!
//! Windows uses a "virtual desktop" coordinate system where all monitors are combined
//! into one large coordinate space. The primary monitor is usually at (0, 0), and other
//! monitors can have negative coordinates if positioned to the left or above.
//!
//! ## Example 3-Monitor Setup:
//! ```text
//!     Left Monitor        Primary Monitor       Right Monitor
//!     (-1920, 702)        (0, 0)                (3840, 702)
//!     1920x1080           3840x2160             1920x1080
//!
//!     Virtual Desktop Origin: (-1920, 0) - the minimum X and Y across all monitors
//!     Virtual Desktop Size: 7680x2160
//! ```
//!
//! ## Key Insight: FIND vs CLICK Coordinates
//!
//! When the FIND action captures a screenshot, it captures the **entire virtual desktop**
//! (all monitors combined). The coordinates returned by FIND are relative to the
//! **virtual desktop origin** (the minimum X, minimum Y point across all monitors).
//!
//! When a CLICK action targets the FIND result, pyautogui needs **absolute virtual
//! desktop coordinates** to position the mouse correctly.

#![allow(dead_code)]
//!
//! ## The Offset Calculation
//!
//! The `monitor_offset_x` and `monitor_offset_y` values passed to Python represent
//! the **virtual desktop origin** - NOT a specific monitor's position.
//!
//! ```text
//! Example: User clicks on left monitor at FIND result (65, 1372)
//!
//! Virtual desktop origin: (-1920, 0)  ← minimum X and Y across all monitors
//! FIND result (relative to screenshot): (65, 1372)
//! Final absolute coordinates: (65 + -1920, 1372 + 0) = (-1855, 1372)
//!
//! This correctly places the click on the left monitor!
//! ```
//!
//! ## Common Pitfall (Fixed)
//!
//! Previously, the code incorrectly used the **specific monitor's position** as the offset.
//! For the left monitor at (-1920, 702), this added 702 to the Y coordinate, causing clicks
//! to land on the wrong monitor (702 pixels too low).
//!
//! The fix: Always calculate the virtual desktop origin (min X, min Y across all monitors)
//! regardless of which monitor is specified, because FIND always captures the full virtual desktop.

use axum::{
    response::Json,
    routing::{get, post},
    Router,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::Emitter;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::action_service::UnifiedActionService;
use crate::commands::rag::RAGState;
use crate::commands::AppState;
use crate::config_storage::ConfigStorage;
use crate::mcp::awas::{
    awas_check_support, awas_discover, awas_execute, awas_extract_elements, awas_list_actions,
};

use crate::mcp::shared::get_workspace_paths_internal;
use crate::mcp::types::{ApiResponse, ApiState};

/// Health check endpoint
async fn health() -> Json<ApiResponse<String>> {
    Json(ApiResponse::success("ok".to_string()))
}

/// Create the API router
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
) -> Router {
    // Get dev_logs path for session manager
    let dev_logs_path = get_workspace_paths_internal()
        .map(|(_, dev_logs, _)| dev_logs)
        .unwrap_or_else(|_| std::path::PathBuf::from(".dev-logs"));

    // Ensure dev_logs directory exists
    let _ = std::fs::create_dir_all(&dev_logs_path);

    // Initialize config storage (graceful degradation if directory creation fails)
    let config_storage = match ConfigStorage::new() {
        Ok(storage) => {
            info!("Config storage initialized successfully");
            Arc::new(tokio::sync::Mutex::new(storage))
        }
        Err(e) => {
            warn!(
                "Config storage initialization failed (non-fatal): {}. Using degraded mode.",
                e
            );
            Arc::new(tokio::sync::Mutex::new(ConfigStorage::new_degraded()))
        }
    };

    // Create UnifiedActionService for deterministic execution
    let action_service = Arc::new(UnifiedActionService::new(
        app_state.clone(),
        config_storage.clone(),
    ));

    let api_state = Arc::new(ApiState {
        app_state,
        rag_state,
        app_handle: app_handle.clone(),
        current_config_id: std::sync::Mutex::new(None),
        config_storage,
        action_service,
        current_ai_pids: Arc::new(std::sync::Mutex::new(Vec::new())),
        extraction_state: Arc::new(crate::mcp::extraction::ExtractionState::new()),
        sdk_connection: Arc::new(tokio::sync::Mutex::new(
            crate::mcp::sdk_client::SdkConnectionManager::new(),
        )),
        ui_bridge_pending: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        doctor_handle: None, // Doctor handle accessed via app_state.doctor_handle when needed
    });

    // Set up UI Bridge response listener
    // This listens for "ui-bridge-response" events from the React frontend
    // and delivers responses to waiting HTTP handlers
    {
        let pending = api_state.ui_bridge_pending.clone();
        let handle = app_handle.clone();

        // We need to use tauri's listen which returns a sync result
        // The listener callback will be called on the main thread

        use tauri::Listener;

        let pending_for_listener = pending.clone();
        let _listener_id = handle.listen("ui-bridge-response", move |event| {
            let pending = pending_for_listener.clone();

            // Parse the response payload
            if let Ok(response) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                // Spawn a task to handle the response since we need async
                let runtime = tokio::runtime::Handle::try_current();
                if let Ok(rt) = runtime {
                    rt.spawn(async move {
                        crate::mcp::ui_bridge::handle_ui_bridge_response(pending, response).await;
                    });
                } else {
                    warn!("UI Bridge: No tokio runtime available for response handling");
                }
            } else {
                warn!(
                    "UI Bridge: Failed to parse response payload: {}",
                    event.payload()
                );
            }
        });
        info!("UI Bridge: Response listener set up");
    }

    // Resume interrupted unified workflows on startup
    let state_for_resume = api_state.clone();
    let resume_config_storage = api_state.config_storage.clone();
    let resume_pid_tracker = api_state.current_ai_pids.clone();
    tokio::spawn(async move {
        // Small delay to let the server fully start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Note: We no longer use global_auto_continue here.
        // Each workflow's per-task auto_continue setting determines whether it gets resumed.
        // The global setting is now only used for the UI toggle, not startup resume logic.

        // Log to debug file
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crate::paths::get_workflow_debug_log_path())
        {
            use std::io::Write;
            let _ = writeln!(
                f,
                "[{}] STARTUP_RESUME_CHECK: Processing interrupted workflows (per-task auto_continue)",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
            );
        }

        // Process interrupted workflows - each workflow's per-task auto_continue setting
        // determines whether it gets resumed or marked as failed
        let resume_config = crate::unified_workflow_executor::ResumeConfig {
            resume_enabled: true, // Let the function check per-task auto_continue
        };

        let count = crate::unified_workflow_executor::resume_interrupted_workflows(
            state_for_resume.app_state.checkpoint_db.clone(),
            state_for_resume.app_state.clone(),
            resume_config_storage,
            state_for_resume.app_handle.clone(),
            resume_pid_tracker,
            resume_config,
        )
        .await;

        if count > 0 {
            info!(
                "Processed {} interrupted unified workflow(s) on startup",
                count
            );
        }
    });

    // Sync workflows from web backend on startup (background task)
    {
        let db = api_state.app_state.checkpoint_db.clone();
        tokio::spawn(async move {
            // Wait for server to start and auth to be available
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            match crate::mcp::web_backend_workflows::sync_workflows_from_backend(&db).await {
                Ok(count) => {
                    if count > 0 {
                        info!("Synced {} workflows from web backend", count);
                    }
                }
                Err(e) => {
                    warn!("Workflow sync from backend skipped: {}", e);
                }
            }
        });
    }

    // CORS: Permissive (allow any origin) is intentional.
    // This localhost-only API (port 9876) must be accessible from:
    //   - The Tauri webview (tauri://localhost origin)
    //   - External MCP clients (Claude Desktop, Cursor, etc.)
    //   - WSL environments
    // Adding origin restrictions would break MCP client compatibility.
    // Security is enforced by binding to localhost, not by CORS.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Local routes
        .route("/health", get(health))
        // AWAS routes (imported directly)
        .route("/awas/discover", post(awas_discover))
        .route("/awas/execute", post(awas_execute))
        .route("/awas/check-support", post(awas_check_support))
        .route("/awas/actions", get(awas_list_actions))
        .route("/awas/extract-elements", post(awas_extract_elements))
        // Module routes
        .merge(crate::mcp::ai_generation::routes())
        .merge(crate::mcp::api_requests::routes())
        .merge(crate::mcp::app_discovery::routes())
        .merge(crate::mcp::automation_runs::routes())
        .merge(crate::mcp::checkpoints::routes())
        .merge(crate::mcp::checks::routes())
        .merge(crate::mcp::configs::routes())
        .merge(crate::mcp::contexts::routes())
        .merge(crate::mcp::dom_capture::routes())
        .merge(crate::mcp::error_monitor::routes())
        .merge(crate::mcp::extraction::routes())
        .merge(crate::mcp::findings_api::routes())
        .merge(crate::mcp::generation_rules_api::routes())
        .merge(crate::mcp::hooks::routes())
        .merge(crate::mcp::interaction_recording::routes())
        .merge(crate::mcp::log_sources::routes())
        .merge(crate::mcp::macros::routes())
        .merge(crate::mcp::mcp_servers::routes())
        .merge(crate::mcp::misc::routes())
        .merge(crate::mcp::models::routes())
        .merge(crate::mcp::monitors::routes())
        .merge(crate::mcp::playwright::routes())
        .merge(crate::mcp::processes::routes())
        .merge(crate::mcp::prompts::routes())
        .merge(crate::mcp::query_tool::routes())
        .merge(crate::mcp::rag::routes())
        .merge(crate::mcp::recordings::routes())
        .merge(crate::mcp::reflection_api::routes())
        .merge(crate::mcp::saved_api_requests::routes())
        .merge(crate::mcp::scheduler::routes())
        .merge(crate::mcp::sdk_client::routes())
        .merge(crate::mcp::prompt_snippets::routes())
        .merge(crate::mcp::settings::routes())
        .merge(crate::mcp::shell_commands::routes())
        .merge(crate::mcp::state_explorer::routes())
        .merge(crate::mcp::state_machine::routes())
        .merge(crate::mcp::step_type_knowledge_api::routes())
        .merge(crate::mcp::step_type_metadata_api::routes())
        .merge(crate::mcp::task_runs::routes())
        .merge(crate::mcp::testing::routes())
        .merge(crate::mcp::ui_bridge::routes())
        .merge(crate::mcp::unified_workflows::routes())
        .merge(crate::mcp::verification_tests::routes())
        .merge(crate::mcp::websocket::routes())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
        .with_state(api_state)
}

/// Try to bind to a port with SO_REUSEADDR
fn try_bind_port(port: u16) -> Result<std::net::TcpListener, std::io::Error> {
    // Create socket with SO_REUSEADDR to allow binding even if there are zombie connections
    // This is necessary on Windows where TIME_WAIT/CLOSE_WAIT sockets can block port binding
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    socket.listen(1024)?;
    Ok(socket.into())
}

/// Start the MCP API server
pub async fn start_server(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let emitter = app_handle.clone();
    let api_ready_flag = app_state.clone();
    let router = create_router(app_state, rag_state, app_handle);

    // Try the requested port first, then fallback ports if zombie connections are blocking
    // This can happen on Windows when previous process crashes leave orphaned sockets
    let ports_to_try = [port, port + 1, port + 2];
    let mut last_error = None;

    for try_port in ports_to_try {
        match try_bind_port(try_port) {
            Ok(std_listener) => {
                let listener = tokio::net::TcpListener::from_std(std_listener)?;
                if try_port != port {
                    warn!(
                        "Primary port {} was blocked, using fallback port {}. \
                         Restart the app after zombie connections clear.",
                        port, try_port
                    );
                }
                info!("MCP API server listening on port {}", try_port);

                // Signal that the API is ready for requests
                api_ready_flag.api_ready.store(true, Ordering::Relaxed);
                if let Err(e) = emitter.emit("api-ready", try_port) {
                    warn!("Failed to emit api-ready event: {}", e);
                } else {
                    info!("Emitted api-ready event (port {})", try_port);
                }

                axum::serve(listener, router).await?;
                return Ok(());
            }
            Err(e) => {
                warn!("Failed to bind to port {}: {}", try_port, e);
                last_error = Some(e);
            }
        }
    }

    Err(Box::new(last_error.unwrap_or_else(|| {
        std::io::Error::other("All ports failed")
    })))
}
