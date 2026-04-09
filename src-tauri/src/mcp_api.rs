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

use async_graphql_axum::{GraphQL, GraphQLSubscription};
use axum::{
    response::Json,
    routing::{get, post},
    Router,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{Emitter, Manager};
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
use crate::mcp::types::ApiState;

/// Cached embedding-service health probe.  Calls GET /api/embeddings/status
/// at most once every 30 seconds.  Returns a JSON value suitable for inlining
/// into the `/health` response.
async fn embedding_service_health() -> serde_json::Value {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    static LAST_CHECK_MS: AtomicU64 = AtomicU64::new(0);
    static LAST_REACHABLE: AtomicBool = AtomicBool::new(false);
    static LAST_ERROR: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
        std::sync::OnceLock::new();

    let url = crate::database::embedding_client::EmbeddingClient::default_url();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let prev = LAST_CHECK_MS.load(Ordering::Relaxed);
    let stale = now_ms.saturating_sub(prev) > 30_000;

    if stale
        && LAST_CHECK_MS
            .compare_exchange(prev, now_ms, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        // Status endpoint is at the same base minus the last path segment.
        let status_url = url.replace("/compute-text", "/status");
        let ok = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
        {
            Ok(c) => match c.get(&status_url).send().await {
                Ok(r) => r.status().is_success(),
                Err(e) => {
                    let err_mtx = LAST_ERROR.get_or_init(|| std::sync::Mutex::new(None));
                    if let Ok(mut g) = err_mtx.lock() {
                        *g = Some(e.to_string());
                    }
                    false
                }
            },
            Err(e) => {
                let err_mtx = LAST_ERROR.get_or_init(|| std::sync::Mutex::new(None));
                if let Ok(mut g) = err_mtx.lock() {
                    *g = Some(format!("Failed to build HTTP client: {e}"));
                }
                false
            }
        };
        LAST_REACHABLE.store(ok, Ordering::Release);
        if ok {
            let err_mtx = LAST_ERROR.get_or_init(|| std::sync::Mutex::new(None));
            if let Ok(mut g) = err_mtx.lock() {
                *g = None;
            }
        }
    }

    let reachable = LAST_REACHABLE.load(Ordering::Acquire);
    let err_msg = LAST_ERROR
        .get()
        .and_then(|m| m.lock().ok())
        .and_then(|g| g.clone());

    serde_json::json!({
        "reachable": reachable,
        "url": url,
        "lastCheckMs": LAST_CHECK_MS.load(Ordering::Relaxed),
        "lastErrorMessage": err_msg,
    })
}

/// Health check endpoint.
/// Includes `uiBridge` metadata so the app discovery scanner can detect the runner.
/// Returns rich diagnostics: frontend responsiveness, uptime, circuit breaker state.
async fn health(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> Json<serde_json::Value> {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let last_pong = state.ui_bridge_last_pong.load(Ordering::Relaxed);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let pong_age_ms = if last_pong > 0 { now_ms - last_pong } else { 0 };
    let responsive = last_pong > 0 && pong_age_ms < 15000;

    let pending_count = state
        .ui_bridge_pending_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let circuit_breaker_state = state.ui_bridge_circuit_breaker.get_state().await;

    let status = if last_pong > 0 { "ok" } else { "starting" };
    let console_errors = state.ui_bridge_console_error_count.load(Ordering::Relaxed);

    // AI provider circuit breaker states
    let ai_provider_states: Vec<serde_json::Value> =
        crate::ai_provider::circuit_breaker::all_provider_states()
            .into_iter()
            .map(|(key, cb_state)| {
                let available = crate::ai_provider::circuit_breaker::is_provider_available(&key);
                serde_json::json!({
                    "providerKey": key,
                    "state": cb_state.to_string(),
                    "available": available,
                })
            })
            .collect();

    // When the frontend has not connected after 30s of uptime, auto-attach a
    // native window screenshot so health consumers (agents, supervisor) can see
    // what the webview is actually showing (e.g., ERR_CONNECTION_REFUSED).
    let diagnostic_screenshot = if last_pong == 0 && uptime_secs >= 30 {
        crate::mcp::ui_bridge::capture_runner_window_base64(&state).await
    } else {
        None
    };

    // Embedding service health probe (cached, refreshed every 30s).
    let embedding_health = embedding_service_health().await;

    let mut data = serde_json::json!({
        "status": status,
        "ready": last_pong > 0,
        "responsive": responsive,
        "lastHeartbeat": last_pong,
        "heartbeatAgeMs": pong_age_ms,
        "uptimeSeconds": uptime_secs,
        "pendingRequests": pending_count,
        "circuitBreaker": format!("{:?}", circuit_breaker_state),
        "consoleErrorCount": console_errors,
        "aiProviderCircuitBreakers": ai_provider_states,
        "embeddingService": embedding_health,
    });

    if let Some((screenshot, width, height)) = diagnostic_screenshot {
        data.as_object_mut().unwrap().insert(
            "diagnosticScreenshot".to_string(),
            serde_json::json!({
                "screenshot": screenshot,
                "width": width,
                "height": height,
                "reason": "Frontend SDK has not connected after 30s of uptime"
            }),
        );
    }

    Json(serde_json::json!({
        "success": true,
        "data": data,
        "uiBridge": {
            "appId": "qontinui-runner",
            "appName": "Qontinui Runner",
            "appType": "desktop",
            "framework": "tauri",
            "capabilities": ["control", "renderLog", "debug"],
        },
        "timestamp": now_ms,
    }))
}

/// Create the API router
pub fn create_router(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    instance_manager: Arc<crate::instance_manager::InstanceManager>,
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

    let current_ai_pids = app_state.ai_pid_tracker.clone();
    let shared_sdk_connection = app_state.sdk_connection.clone();
    let api_state = Arc::new(ApiState {
        app_state,
        rag_state,
        app_handle: app_handle.clone(),
        current_config_id: std::sync::Mutex::new(None),
        config_storage,
        action_service,
        current_ai_pids,
        extraction_state: Arc::new(crate::mcp::extraction::ExtractionState::new()),
        sdk_connection: shared_sdk_connection,
        ui_bridge_pending: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        ui_bridge_pending_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ui_bridge_circuit_breaker: Arc::new(crate::mcp::ui_bridge::UiBridgeCircuitBreaker::new()),
        ui_bridge_semaphore: Arc::new(tokio::sync::Semaphore::new(6)),
        ui_bridge_last_pong: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ui_bridge_ready: Arc::new(tokio::sync::Notify::new()),
        ui_bridge_dedup: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        ui_bridge_console_error_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        ui_bridge_render_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        ui_bridge_last_discovered: Arc::new(tokio::sync::RwLock::new(None)),
        doctor_handle: None, // Doctor handle accessed via app_state.doctor_handle when needed
        started_at: std::time::Instant::now(),
        instance_manager,
        ui_bridge_event_sequence: std::sync::atomic::AtomicI64::new(0),
        knowledge_graph_cache: Arc::new(tokio::sync::RwLock::new(None)),
        graph_cache_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        accessibility_manager: Arc::new(tokio::sync::Mutex::new(
            qontinui_runner_lib::accessibility::AccessibilityManager::new(),
        )),
    });

    // Set up UI Bridge response listener
    // This listens for "ui-bridge-response" events from the React frontend
    // and delivers responses to waiting HTTP handlers
    {
        let pending = api_state.ui_bridge_pending.clone();
        let pending_count = api_state.ui_bridge_pending_count.clone();
        let handle = app_handle.clone();

        // We need to use tauri's listen which returns a sync result
        // The listener callback will be called on the main thread

        use tauri::Listener;

        let pending_for_listener = pending.clone();
        let pending_count_for_listener = pending_count.clone();
        let _listener_id = handle.listen("ui-bridge-response", move |event| {
            let pending = pending_for_listener.clone();
            let pending_count = pending_count_for_listener.clone();

            // Parse the response payload
            if let Ok(response) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                // Spawn a task to handle the response since we need async
                let runtime = tokio::runtime::Handle::try_current();
                if let Ok(rt) = runtime {
                    rt.spawn(async move {
                        crate::mcp::ui_bridge::handle_ui_bridge_response(
                            pending,
                            pending_count,
                            response,
                        )
                        .await;
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

    // Set up UI Bridge pong listener for frontend liveness tracking
    {
        let last_pong = api_state.ui_bridge_last_pong.clone();
        let ready = api_state.ui_bridge_ready.clone();
        let handle = app_handle.clone();

        use tauri::Listener;

        handle.listen("ui-bridge-pong", move |_event| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            last_pong.store(now, std::sync::atomic::Ordering::Relaxed);
            // Unblock any requests waiting for frontend readiness
            ready.notify_waiters();
        });
    }

    // Start UI Bridge ping task (every 3s)
    {
        let handle = app_handle.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;
                let _ = handle.emit(
                    "ui-bridge-ping",
                    serde_json::json!({ "timestamp": chrono::Utc::now().timestamp_millis() }),
                );
            }
        });
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

    // Resume interrupted chat sessions on startup
    {
        let chat_handle = app_handle.clone();
        // Access session manager from Tauri state (managed separately from AppState)
        let chat_sm: Arc<crate::claude_session::SessionManager> = app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();
        tokio::spawn(async move {
            // Wait a bit longer than unified workflows to let the server fully start
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let count = crate::commands::ai_session::resume_ai_sessions(chat_sm, chat_handle).await;

            if count > 0 {
                info!("Resumed {} AI session(s) on startup", count);
            }
        });
    }

    // Auto-start cloud relay if configured
    {
        let relay_api_state = api_state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            crate::mcp::backend_relay::commands::auto_start_cloud_relay(relay_api_state).await;
        });
    }

    // Sync workflows from web backend on startup (background task)
    {
        let sync_pg_db = api_state.app_state.pg_db.clone();
        tokio::spawn(async move {
            // Wait for server to start and auth to be available
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

            match crate::mcp::web_backend_workflows::sync_workflows_from_backend(&sync_pg_db).await
            {
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

    // Start zombie task run sweep (detects and cleans up stale "running" tasks)
    {
        let sweep_handle = app_handle.clone();
        let sweep_sm: Arc<crate::claude_session::SessionManager> = app_handle
            .state::<Arc<crate::claude_session::SessionManager>>()
            .inner()
            .clone();
        crate::zombie_sweep::start_zombie_sweep(sweep_sm, sweep_handle);
    }

    // Periodic file registry cleanup (sweep stale entries every 60s)
    {
        let cleanup_registry = api_state.app_state.file_registry_manager.clone();
        let cleanup_db = api_state.app_state.pg_db.clone();
        tokio::spawn(async move {
            // Wait for server to fully start
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            loop {
                cleanup_registry.cleanup_stale(&cleanup_db).await;
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }
        });
    }

    // One-time audit event cleanup based on audit_retention_days setting
    {
        let pg_db = api_state.app_state.pg_db.clone();
        tokio::spawn(async move {
            // Wait for server startup to settle
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            let settings = crate::settings::get_security_settings();
            if settings.audit_retention_days > 0 {
                match pg_db
                    .cleanup_old_audit_events(settings.audit_retention_days)
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(
                            "Cleaned up {} old audit events (retention: {} days)",
                            n,
                            settings.audit_retention_days
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Audit event cleanup failed: {}", e);
                    }
                }
            }
        });
    }

    // Start trigger service (event-driven workflow automation)
    {
        let trigger_app_state = api_state.app_state.clone();
        let trigger_config_storage = api_state.config_storage.clone();
        let trigger_handle = app_handle.clone();
        let trigger_pids = api_state.current_ai_pids.clone();
        tokio::spawn(async move {
            // Wait for server to be ready
            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
            crate::trigger_system::start_trigger_service(
                trigger_app_state,
                trigger_config_storage,
                trigger_handle,
                trigger_pids,
            )
            .await;
        });
    }

    // Start cascade event buffer (collects cascade detection events for /cascade/events)
    crate::mcp::cascade::start_buffer_task(&api_state);

    // Build GraphQL schema with ApiState as context data
    let graphql_schema = crate::graphql::build_schema(api_state.clone());

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

    // GraphQL sub-router with concurrency limit (max 20 concurrent GraphQL requests).
    // This prevents a burst of expensive queries from starving REST endpoints.
    // WebSocket subscriptions are excluded — they're long-lived by design.
    let graphql_routes = Router::new()
        .route(
            "/graphql",
            get(crate::graphql::schema::graphiql_handler)
                .post(crate::graphql::schema::graphql_handler),
        )
        .layer(tower::limit::ConcurrencyLimitLayer::new(20));

    Router::new()
        // GraphQL endpoints (typed API alongside REST)
        .merge(graphql_routes)
        .route_service(
            "/graphql/ws",
            GraphQLSubscription::new(graphql_schema.clone()),
        )
        // Local routes
        .route("/health", get(health))
        .route("/ui-bridge/health", get(health))
        // AWAS routes (imported directly)
        .route("/awas/discover", post(awas_discover))
        .route("/awas/execute", post(awas_execute))
        .route("/awas/check-support", post(awas_check_support))
        .route("/awas/actions", get(awas_list_actions))
        .route("/awas/extract-elements", post(awas_extract_elements))
        // Module routes
        .merge(crate::mcp::accessibility::routes())
        .merge(crate::mcp::canvas::routes())
        .merge(crate::mcp::cascade::routes())
        .merge(crate::mcp::ai_generation::routes())
        .merge(crate::mcp::api_requests::routes())
        .merge(crate::mcp::app_discovery::routes())
        .merge(crate::mcp::automation_runs::routes())
        .merge(crate::mcp::comparison_api::routes())
        .merge(crate::mcp::checks::routes())
        .merge(crate::mcp::configs::routes())
        .merge(crate::mcp::constraints_api::routes())
        .merge(crate::mcp::contexts::routes())
        .merge(crate::mcp::development_intelligence::routes())
        .merge(crate::mcp::dom_capture::routes())
        .merge(crate::mcp::error_monitor::routes())
        .merge(crate::mcp::extraction::routes())
        .merge(crate::mcp::file_registry::routes())
        .merge(crate::mcp::findings_api::routes())
        .merge(crate::mcp::generation_rules_api::routes())
        .merge(crate::mcp::meta_optimizer_api::routes())
        .merge(crate::mcp::generator_eval::routes())
        .merge(crate::mcp::step_evaluation_api::routes())
        .merge(crate::mcp::hooks::routes())
        .merge(crate::mcp::inngest::routes())
        .merge(crate::mcp::api_spec_verify::routes())
        .merge(crate::mcp::headless_browser::routes())
        .merge(crate::mcp::interaction_recording::routes())
        .merge(crate::mcp::log_sources::routes())
        .merge(crate::mcp::macros::routes())
        .merge(crate::mcp::mcp_servers::routes())
        .merge(crate::mcp::misc::routes())
        .merge(crate::mcp::ai_session::routes())
        .merge(crate::mcp::auto_continue::routes())
        .merge(crate::mcp::backup_restore::routes())
        .merge(crate::mcp::playwright_collection::routes())
        .merge(crate::mcp::models::routes())
        .merge(crate::mcp::monitors::routes())
        .merge(crate::mcp::orchestration_loop_api::routes())
        .merge(crate::mcp::playwright::routes())
        .merge(crate::mcp::processes::routes())
        .merge(crate::mcp::provider_health::routes())
        .merge(crate::mcp::prompts::routes())
        .merge(crate::mcp::prompt_home::routes())
        .merge(crate::mcp::query_tool::routes())
        .merge(crate::mcp::queue::routes())
        .merge(crate::mcp::rag::routes())
        .merge(crate::mcp::recordings::routes())
        .merge(crate::mcp::reflection_api::routes())
        .merge(crate::mcp::graph_api::routes())
        .merge(crate::mcp::observations_api::routes())
        .merge(crate::mcp::entity_profiles_api::routes())
        .merge(crate::mcp::online_learning_api::routes())
        .merge(crate::mcp::memory_consolidation_api::routes())
        .merge(crate::mcp::query_memory_tool::routes())
        .merge(crate::mcp::decision_trail_api::routes())
        .merge(crate::mcp::saved_api_requests::routes())
        .merge(crate::mcp::scheduler::routes())
        .merge(crate::mcp::sdk_client::routes())
        .merge(crate::mcp::prompt_snippets::routes())
        .merge(crate::mcp::settings::routes())
        .merge(crate::mcp::shell_commands::routes())
        .merge(crate::mcp::skills::routes())
        .merge(crate::mcp::state_explorer::routes())
        .merge(crate::mcp::state_machine::routes())
        .merge(crate::mcp::gui_config::routes())
        .merge(crate::mcp::image_quality_tests::routes())
        .merge(crate::mcp::step_type_knowledge_api::routes())
        .merge(crate::mcp::step_type_metadata_api::routes())
        .merge(crate::mcp::task_run_inspection::routes())
        .merge(crate::mcp::task_runs::routes())
        .merge(crate::mcp::terminals::routes())
        .merge(crate::mcp::testing::routes())
        .merge(crate::mcp::triggers::routes())
        .merge(crate::mcp::ui_bridge::routes())
        .merge(crate::mcp::ui_bridge_integration::routes())
        .merge(crate::mcp::unified_workflows::routes())
        .merge(crate::mcp::verification_tests::routes())
        .merge(crate::mcp::websocket::routes())
        .merge(crate::mcp::worktrees::routes())
        .merge(crate::mcp::token_analytics::routes())
        .merge(crate::mcp::otel_status::routes())
        .merge(crate::mcp::container_status::routes())
        .merge(crate::mcp::security_audit::routes())
        .merge(crate::mcp::knowledge_acquisition_api::routes())
        .merge(crate::mcp::session_recap::routes())
        .merge(crate::mcp::api_surface::routes())
        .merge(crate::mcp::api_surface_diff::routes())
        .merge(crate::mcp::prm_export::routes())
        .merge(crate::mcp::restate_api::routes())
        .merge(crate::mcp::hitl::routes())
        .merge(crate::mcp::streaming::routes())
        .route("/cloud-relay/start", post(cloud_relay_start))
        .route("/cloud-relay/status", get(cloud_relay_status))
        .layer(axum::middleware::from_fn(
            crate::middleware::trace_propagation_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(RequestBodyLimitLayer::new(100 * 1024 * 1024))
        .layer(axum::Extension(graphql_schema))
        .with_state(api_state)
}

/// HTTP endpoint to manually start the cloud relay
async fn cloud_relay_start(
    axum::extract::State(state): axum::extract::State<Arc<ApiState>>,
) -> axum::Json<serde_json::Value> {
    crate::mcp::backend_relay::commands::auto_start_cloud_relay(state).await;
    axum::Json(serde_json::json!({"status": "started"}))
}

/// HTTP endpoint to check cloud relay status
async fn cloud_relay_status() -> axum::Json<serde_json::Value> {
    let status = crate::mcp::backend_relay::commands::get_cloud_relay_status_internal().await;
    axum::Json(status)
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
    instance_manager: Arc<crate::instance_manager::InstanceManager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let emitter = app_handle.clone();
    let api_ready_flag = app_state.clone();
    let router = create_router(app_state, rag_state, app_handle, instance_manager);

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

                // Store the actual bound port in AppState
                api_ready_flag.api_port.store(try_port, Ordering::Relaxed);

                // Port stored in api_ready_flag.api_port above; PG queries accept runner_port as parameter.

                // Signal that the API is ready for requests
                api_ready_flag.api_ready.store(true, Ordering::Relaxed);
                if let Err(e) = emitter.emit("api-ready", try_port) {
                    warn!("Failed to emit api-ready event: {}", e);
                } else {
                    info!("Emitted api-ready event (port {})", try_port);
                }

                // Update window title if using non-default port or instance name
                let default_port = crate::mcp::types::MCP_API_PORT;
                let instance_name = std::env::var("QONTINUI_INSTANCE_NAME").ok();
                let needs_title_update = try_port != default_port || instance_name.is_some();
                if needs_title_update {
                    let title = match instance_name {
                        Some(name) => format!("Qontinui Runner — {} [:{}]", name, try_port),
                        None => format!("Qontinui Runner [:{}]", try_port),
                    };
                    if let Some(window) = emitter.get_webview_window("main") {
                        if let Err(e) = window.set_title(&title) {
                            warn!("Failed to set window title: {}", e);
                        } else {
                            info!("Window title set to: {}", title);
                        }
                    }
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
