//! UI Bridge commands
//!
//! This module provides Tauri IPC commands for bridging the UI Bridge React
//! registry to external HTTP APIs. It allows external tools (like Claude Code)
//! to interact with the React UI through the Axum HTTP server.
//!
//! # Architecture
//!
//! ```text
//! External HTTP Client (e.g., Claude Code)
//!     ↓ HTTP
//! Axum Server (mcp_api.rs /ui-bridge/* routes)
//!     ↓ Tauri emit/listen
//! React Frontend (UIBridgeProvider)
//!     ↓ Returns response
//! Axum Server → HTTP Response
//! ```
//!
//! # Commands
//!
//! - `ui_bridge_get_elements` - Get all registered UI elements
//! - `ui_bridge_get_element` - Get a specific element by ID
//! - `ui_bridge_execute_action` - Execute an action on an element
//! - `ui_bridge_get_components` - Get all registered components
//! - `ui_bridge_get_component` - Get a specific component by ID
//! - `ui_bridge_execute_component_action` - Execute an action on a component
//! - `ui_bridge_discover` - Discover controllable elements in the UI
//! - `ui_bridge_get_snapshot` - Get a full snapshot of the UI bridge state
//! - `ui_bridge_discover_states_from_fingerprints` - Discover states from fingerprint co-occurrence data

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Manager, State};
use tracing::{error, info};

// Fully migrated to compartment state (Workstream C).
// `ui_bridge_run_exploration_native` uses multi-State (Bridge + Integration).
// `ui_bridge_run_exploration` uses multi-State (Bridge + Health).
use super::compartments::BridgeCompartment;
use super::CommandResponse;

// Re-export wire-format DTOs from qontinui-types (canonical source of truth).
pub use qontinui_types::ui_bridge::{
    ActionResponse, ComponentActionInfo, ComponentActionRequest, DiscoveredElement,
    DiscoveryRequest, DiscoveryResponse, ElementActionRequest, ElementIdentifier, ElementRect,
    ElementState, UIBridgeComponent, UIBridgeElement, UIBridgeSnapshot, WaitOptions, WorkflowInfo,
};

/// Get all registered UI elements from the React UI Bridge.
///
/// This command emits an event to the React frontend and waits for the response.
/// The React UIBridgeProvider should listen for this event and respond with
/// the current registered elements.
///
/// # Arguments
/// * `app` - The Tauri app handle for emitting events
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with elements data
/// * `Err(String)` - Error message if the operation fails
#[tauri::command]
pub async fn ui_bridge_get_elements(app: tauri::AppHandle) -> Result<CommandResponse, String> {
    info!("UI Bridge: Getting all elements");

    // Emit request to React frontend
    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "get_elements"
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    // For now, return a placeholder response.
    // The actual implementation will use a channel/oneshot to wait for the React response.
    // This requires setting up a listener in the frontend that responds to these events.
    Ok(CommandResponse {
        success: true,
        message: Some("Request sent to UI Bridge".to_string()),
        data: Some(serde_json::json!({
            "note": "The React frontend should respond with the elements via the ui-bridge-response event"
        })),
    })
}

/// Get a specific element by ID.
#[tauri::command]
pub async fn ui_bridge_get_element(
    app: tauri::AppHandle,
    element_id: String,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Getting element {}", element_id);

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "get_element",
            "elementId": element_id
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Request sent for element {}", element_id)),
        data: None,
    })
}

/// Execute an action on an element.
#[tauri::command]
pub async fn ui_bridge_execute_action(
    app: tauri::AppHandle,
    element_id: String,
    action: ElementActionRequest,
) -> Result<CommandResponse, String> {
    info!(
        "UI Bridge: Executing action {} on element {}",
        action.action, element_id
    );

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "execute_action",
            "elementId": element_id,
            "action": action
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Action {} sent for element {}",
            action.action, element_id
        )),
        data: None,
    })
}

/// Get all registered components.
#[tauri::command]
pub async fn ui_bridge_get_components(app: tauri::AppHandle) -> Result<CommandResponse, String> {
    info!("UI Bridge: Getting all components");

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "get_components"
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Request sent for components".to_string()),
        data: None,
    })
}

/// Get a specific component by ID.
#[tauri::command]
pub async fn ui_bridge_get_component(
    app: tauri::AppHandle,
    component_id: String,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Getting component {}", component_id);

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "get_component",
            "componentId": component_id
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Request sent for component {}", component_id)),
        data: None,
    })
}

/// Execute an action on a component.
#[tauri::command]
pub async fn ui_bridge_execute_component_action(
    app: tauri::AppHandle,
    component_id: String,
    action_id: String,
    params: Option<serde_json::Value>,
) -> Result<CommandResponse, String> {
    info!(
        "UI Bridge: Executing action {} on component {}",
        action_id, component_id
    );

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "execute_component_action",
            "componentId": component_id,
            "actionId": action_id,
            "params": params
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Action {} sent for component {}",
            action_id, component_id
        )),
        data: None,
    })
}

/// Discover controllable elements in the UI.
#[tauri::command]
pub async fn ui_bridge_discover(
    app: tauri::AppHandle,
    options: Option<DiscoveryRequest>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Discovering elements with options {:?}", options);

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "discover",
            "options": options
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Discovery request sent".to_string()),
        data: None,
    })
}

/// Get a full snapshot of the UI Bridge state.
#[tauri::command]
pub async fn ui_bridge_get_snapshot(app: tauri::AppHandle) -> Result<CommandResponse, String> {
    info!("UI Bridge: Getting snapshot");

    app.emit(
        "ui-bridge-request",
        serde_json::json!({
            "type": "get_snapshot"
        }),
    )
    .map_err(|e: tauri::Error| String::from(AppError::from(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some("Snapshot request sent".to_string()),
        data: None,
    })
}

/// Configuration for fingerprint state discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintDiscoveryConfig {
    /// Minimum co-occurrence rate for grouping (0.0-1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_cooccurrence_rate: Option<f64>,
}

/// Discovered state from fingerprint analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredFingerprintState {
    pub state_id: String,
    pub name: String,
    pub fingerprint_hashes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_ids: Option<Vec<String>>,
    pub position_zone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landmark_context: Option<String>,
    pub is_global: bool,
    pub is_modal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_pattern_count: Option<i32>,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_count: Option<i32>,
}

/// State transition from fingerprint analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredStateTransition {
    pub from_state_id: String,
    pub to_state_id: String,
    pub action_type: String,
    pub count: i32,
}

/// Discovery statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryStatistics {
    pub total_captures: i32,
    pub total_transitions: i32,
    pub unique_fingerprints: i32,
    pub discovered_states: i32,
    pub global_states: i32,
    pub modal_states: i32,
    pub discovered_transitions: i32,
}

/// Response from fingerprint state discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintDiscoveryResult {
    pub states: Vec<DiscoveredFingerprintState>,
    pub transitions: Vec<DiscoveredStateTransition>,
    pub statistics: DiscoveryStatistics,
}

/// Discover states from fingerprint co-occurrence data.
///
/// This command takes co-occurrence export data from the UI Bridge capture session
/// and runs the FingerprintStateDiscovery algorithm from the qontinui library
/// to discover states based on element fingerprints.
///
/// # Arguments
/// * `state` - The application state containing the extraction executor
/// * `cooccurrence_export` - The co-occurrence export data from UI Bridge
/// * `config` - Optional discovery configuration
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with discovered states and transitions
/// * `Err(String)` - Error message if discovery fails
#[tauri::command]
pub async fn ui_bridge_discover_states_from_fingerprints(
    _state: State<'_, BridgeCompartment>,
    cooccurrence_export: serde_json::Value,
    config: Option<FingerprintDiscoveryConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Discovering states from fingerprints (Rust)");

    // Run on a blocking thread since the discovery may be CPU-intensive
    tokio::task::spawn_blocking(move || -> Result<CommandResponse, AppError> {
        // Deserialize the co-occurrence export — serde_json::Error converts via `?`.
        let export: crate::exploration::types::CooccurrenceExport =
            serde_json::from_value(cooccurrence_export)?;

        // Build discovery config
        let discovery_config = if let Some(c) = config {
            let mut dc = crate::exploration::discovery::DiscoveryConfig::default();
            if let Some(rate) = c.min_cooccurrence_rate {
                dc.min_cooccurrence_rate = rate;
            }
            dc
        } else {
            crate::exploration::discovery::DiscoveryConfig::default()
        };

        // Run Rust-native discovery
        let mut discovery =
            crate::exploration::discovery::FingerprintStateDiscovery::new(discovery_config);
        discovery.load_cooccurrence_export(&export);
        discovery.discover_states();
        let result = discovery.into_result();

        info!(
            "Discovery complete: {} states, {} transitions",
            result.states.len(),
            result.transitions.len()
        );

        let data = serde_json::to_value(&result)?;

        Ok(CommandResponse {
            success: true,
            message: Some("Fingerprint state discovery complete".to_string()),
            data: Some(data),
        })
    })
    .await
    .map_err(|e| String::from(AppError::from(e)))?
    .map_err(String::from)
}

/// Reload the runner's webview.
///
/// Calls `location.reload()` on the webview to recover from frozen states
/// (e.g., loading screen stuck after Vite fails to mount).
///
/// # Returns
/// * `Ok(CommandResponse)` - Success if reload was triggered
/// * `Err(String)` - Error if reload could not be initiated
#[tauri::command]
pub async fn ui_bridge_reload_webview(app: tauri::AppHandle) -> Result<CommandResponse, String> {
    info!("UI Bridge: Reloading webview");

    // Get all webview windows and reload the main one
    if let Some(window) = app.get_webview_window(qontinui_runner_lib::get_main_window_label()) {
        window
            .eval("location.reload()")
            .map_err(|e| String::from(AppError::from(e)))?;
        Ok(CommandResponse {
            success: true,
            message: Some("Webview reload triggered".to_string()),
            data: None,
        })
    } else {
        Err("Main webview window not found".to_string())
    }
}

/// Configuration for UI Bridge exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeExplorationConfig {
    /// Maximum navigation depth from starting page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i32>,
    /// Maximum elements to interact with per page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_elements_per_page: Option<i32>,
    /// Maximum total elements to explore
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_elements: Option<i32>,
    /// Delay between actions in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_delay_ms: Option<i32>,
    /// Keywords in element text/id to skip (e.g., "delete", "logout")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_keywords: Option<Vec<String>>,
    /// Keywords that are always safe to interact with
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_keywords: Option<Vec<String>>,
    /// Whether to capture screenshots with snapshots
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_screenshots: Option<bool>,
}

/// Result of UI Bridge exploration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeExplorationResult {
    /// Unique identifier for this exploration
    pub exploration_id: String,
    /// Total unique elements discovered
    pub elements_discovered: i32,
    /// Total elements interacted with
    pub elements_explored: i32,
    /// Errors encountered during exploration
    pub errors: Vec<String>,
    /// State discovery result (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_discovery_result: Option<serde_json::Value>,
    /// Raw cooccurrence export data (for further processing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooccurrence_export: Option<serde_json::Value>,
}

/// Run automatic UI Bridge exploration via the Python library
///
/// This command connects to the browser via the extension and systematically
/// explores interactive elements, building a co-occurrence export for
/// fingerprint-based state discovery.
///
/// # Arguments
/// * `runner_url` - URL of the runner (default: http://localhost:9876)
/// * `config` - Optional exploration configuration
///
/// # Returns
/// Exploration result with discovered states and statistics
#[tauri::command]
pub async fn ui_bridge_run_exploration(
    state: State<'_, BridgeCompartment>,
    health: State<'_, super::compartments::HealthCompartment>,
    runner_url: Option<String>,
    config: Option<UIBridgeExplorationConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Running automatic exploration");

    let bridge_compartment = state.inner().clone();
    let self_url = {
        let port = health
            .api_port()
            .load(std::sync::atomic::Ordering::Relaxed);
        format!("http://localhost:{}", port)
    };

    tokio::task::spawn_blocking(move || -> Result<CommandResponse, String> {
        let mut executor_lock = crate::safe_lock::safe_lock_or_recover(
            bridge_compartment.extraction_executor(),
            "extraction_executor",
        );

        if let Some(ref mut executor) = *executor_lock {
            executor.ensure_started().map_err(|e| {
                error!("Failed to start extraction executor: {}", e);
                String::from(AppError::ExecutorError(e))
            })?;

            let params = json!({
                "runner_url": runner_url.unwrap_or(self_url),
                "config": config.map(|c| json!({
                    "max_depth": c.max_depth.unwrap_or(2),
                    "max_elements_per_page": c.max_elements_per_page.unwrap_or(20),
                    "max_total_elements": c.max_total_elements.unwrap_or(100),
                    "action_delay_ms": c.action_delay_ms.unwrap_or(500),
                    "blocked_keywords": c.blocked_keywords.unwrap_or_default(),
                    "safe_keywords": c.safe_keywords.unwrap_or_default(),
                    "capture_screenshots": c.capture_screenshots.unwrap_or(false),
                })),
            });

            let response_result = executor
                .send_command_and_wait(
                    "run_ui_bridge_exploration",
                    Some(params),
                    std::time::Duration::from_secs(300),
                )
                .map_err(|e| String::from(AppError::ExecutorError(e)))?;

            if response_result.success {
                Ok(CommandResponse {
                    success: true,
                    message: Some("UI Bridge exploration complete".to_string()),
                    data: response_result.data,
                })
            } else {
                let error_msg = response_result
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string());
                Err(format!("Exploration failed: {}", error_msg))
            }
        } else {
            Err("Extraction executor not initialized".to_string())
        }
    })
    .await
    .map_err(|e| String::from(AppError::from(e)))?
}

/// Stop a running UI Bridge exploration
///
/// Signals the Python exploration process to stop gracefully.
/// The exploration will complete its current action, export any partial results,
/// and return what was discovered so far.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success if stop was requested
/// * `Err(String)` - Error if stop could not be initiated
#[tauri::command]
pub async fn ui_bridge_stop_exploration(
    state: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Stopping exploration");

    let bridge_compartment = state.inner().clone();

    let response_result = tokio::task::spawn_blocking(
        move || -> Result<crate::executor::lifecycle::CommandResponseResult, String> {
            let mut executor_lock = crate::safe_lock::safe_lock_or_recover(
                bridge_compartment.extraction_executor(),
                "extraction_executor",
            );

            if let Some(ref mut executor) = *executor_lock {
                if !executor.is_running() {
                    return Err("Extraction executor not running".to_string());
                }

                executor
                    .send_command_and_wait(
                        "stop_ui_bridge_exploration",
                        None,
                        std::time::Duration::from_secs(10),
                    )
                    .map_err(|e| String::from(AppError::ExecutorError(e)))
            } else {
                Err("Extraction executor not initialized".to_string())
            }
        },
    )
    .await
    .map_err(|e| String::from(AppError::from(e)))??;

    if response_result.success {
        Ok(CommandResponse {
            success: true,
            message: Some("Exploration stop requested".to_string()),
            data: response_result.data,
        })
    } else {
        let error_msg = response_result
            .error
            .unwrap_or_else(|| "Unknown error".to_string());
        Err(format!("Failed to stop exploration: {}", error_msg))
    }
}

// =============================================================================
// Native Rust Exploration (no Python dependency)
// =============================================================================

/// Run exploration using the native Rust engine.
///
/// This starts a background exploration task that fetches snapshots,
/// generates fingerprints, clicks elements across pages, and runs
/// state discovery. Results are returned when exploration completes.
///
/// Unlike the Python-based exploration, this runs entirely in Rust
/// and survives page navigation in the runner's webview.
#[tauri::command]
pub async fn ui_bridge_run_exploration_native(
    bridge: State<'_, BridgeCompartment>,
    integration: State<'_, super::compartments::IntegrationCompartment>,
    config: Option<crate::exploration::ExplorationConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Starting native Rust exploration");

    let sdk_conn = integration.sdk_connection().clone();
    let config = config.unwrap_or_default();

    let mut engine = crate::exploration::ExplorationEngine::new();

    // Store cancel token so it can be cancelled from another command
    {
        let mut cancel_guard = bridge.exploration_cancel().lock().await;
        *cancel_guard = Some(engine.cancel_token());
    }

    let result = engine.explore(&sdk_conn, config).await;

    // Clear cancel token
    {
        let mut cancel_guard = bridge.exploration_cancel().lock().await;
        *cancel_guard = None;
    }

    let result = result?;

    let result_json = serde_json::to_value(&result).map_err(|e| {
        String::from(AppError::JsonError(e))
    })?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Exploration complete: {} states discovered",
            result.states.len()
        )),
        data: Some(result_json),
    })
}

/// Stop a running native exploration.
#[tauri::command]
pub async fn ui_bridge_stop_exploration_native(
    state: State<'_, BridgeCompartment>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Stopping native exploration");

    let cancel_guard = state.exploration_cancel().lock().await;
    if let Some(ref token) = *cancel_guard {
        token.cancel();
        Ok(CommandResponse {
            success: true,
            message: Some("Exploration cancel requested".to_string()),
            data: None,
        })
    } else {
        Err("No native exploration is currently running".to_string())
    }
}

/// Run state discovery from fingerprint co-occurrence data using the native Rust engine.
///
/// Accepts optional config for tuning discovery parameters (e.g., min_cooccurrence_rate).
#[tauri::command]
pub async fn ui_bridge_discover_states_native(
    cooccurrence_export: serde_json::Value,
    config: Option<crate::exploration::discovery::DiscoveryConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Running native Rust state discovery");

    let export: crate::exploration::CooccurrenceExport =
        serde_json::from_value(cooccurrence_export).map_err(|e| {
            String::from(AppError::ParseError(format!(
                "Failed to parse co-occurrence export: {}",
                e
            )))
        })?;

    let discovery_config = config.unwrap_or_default();
    let mut discovery = crate::exploration::FingerprintStateDiscovery::new(discovery_config);
    discovery.load_cooccurrence_export(&export);
    discovery.discover_states();
    let result = discovery.into_result();

    let result_json =
        serde_json::to_value(&result).map_err(|e| String::from(AppError::JsonError(e)))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Discovery complete: {} states",
            result.states.len()
        )),
        data: Some(result_json),
    })
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// Non-generic because handlers accept concrete `tauri::AppHandle`.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    PluginBuilder::<tauri::Wry>::new("qontinui_ui_bridge")
        .invoke_handler(tauri::generate_handler![
            ui_bridge_get_elements,
            ui_bridge_get_element,
            ui_bridge_execute_action,
            ui_bridge_get_components,
            ui_bridge_get_component,
            ui_bridge_execute_component_action,
            ui_bridge_discover,
            ui_bridge_get_snapshot,
            ui_bridge_discover_states_from_fingerprints,
            ui_bridge_run_exploration,
            ui_bridge_stop_exploration,
            ui_bridge_reload_webview,
            ui_bridge_run_exploration_native,
            ui_bridge_stop_exploration_native,
            ui_bridge_discover_states_native,
        ])
        .build()
}
