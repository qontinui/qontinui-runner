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

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tracing::{error, info};

use super::CommandResponse;

/// Element state as returned from the React UI Bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementState {
    pub visible: bool,
    pub enabled: bool,
    pub focused: bool,
    pub rect: ElementRect,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
}

/// Element bounding rectangle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// Element identifier info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementIdentifier {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awas_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_id: Option<String>,
    pub xpath: String,
    pub selector: String,
}

/// Registered element info (serializable subset of RegisteredElement)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeElement {
    pub id: String,
    #[serde(rename = "type")]
    pub element_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_actions: Option<Vec<String>>,
    pub identifier: ElementIdentifier,
    pub state: ElementState,
    pub registered_at: i64,
    pub mounted: bool,
}

/// Registered component info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeComponent {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub actions: Vec<ComponentActionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_ids: Option<Vec<String>>,
    pub registered_at: i64,
    pub mounted: bool,
}

/// Component action info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentActionInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Action request for elements
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementActionRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_options: Option<WaitOptions>,
}

/// Action request for components
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentActionRequest {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Wait options for actions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u32>,
}

/// Action response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element_state: Option<ElementState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    pub duration_ms: u64,
    pub timestamp: i64,
}

/// Discovery request options
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

/// Discovered element info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredElement {
    pub id: String,
    #[serde(rename = "type")]
    pub element_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub tag_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessible_name: Option<String>,
    pub actions: Vec<String>,
    pub state: ElementState,
    pub registered: bool,
}

/// Discovery response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResponse {
    pub elements: Vec<DiscoveredElement>,
    pub total: usize,
    pub duration_ms: u64,
    pub timestamp: i64,
}

/// UI Bridge snapshot (full state)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIBridgeSnapshot {
    pub timestamp: i64,
    pub elements: Vec<UIBridgeElement>,
    pub components: Vec<UIBridgeComponent>,
    pub workflows: Vec<WorkflowInfo>,
}

/// Workflow info for snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub step_count: usize,
}

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    .map_err(|e| format!("Failed to emit UI Bridge request: {}", e))?;

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
    _state: State<'_, Arc<super::AppState>>,
    cooccurrence_export: serde_json::Value,
    config: Option<FingerprintDiscoveryConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Discovering states from fingerprints (Rust)");

    // Run on a blocking thread since the discovery may be CPU-intensive
    tokio::task::spawn_blocking(move || -> Result<CommandResponse, String> {
        // Deserialize the co-occurrence export
        let export: crate::exploration::types::CooccurrenceExport =
            serde_json::from_value(cooccurrence_export)
                .map_err(|e| format!("Failed to parse co-occurrence export: {}", e))?;

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

        let data = serde_json::to_value(&result)
            .map_err(|e| format!("Failed to serialize result: {}", e))?;

        Ok(CommandResponse {
            success: true,
            message: Some("Fingerprint state discovery complete".to_string()),
            data: Some(data),
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
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
            .map_err(|e| format!("Failed to reload webview: {}", e))?;
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
    state: State<'_, Arc<super::AppState>>,
    runner_url: Option<String>,
    config: Option<UIBridgeExplorationConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Running automatic exploration");

    let app_state = state.inner().clone();
    let self_url = crate::mcp::types::get_self_base_url(&state);

    tokio::task::spawn_blocking(move || -> Result<CommandResponse, String> {
        let mut executor_lock = crate::safe_lock::safe_lock_or_recover(
            &app_state.extraction_executor,
            "extraction_executor",
        );

        if let Some(ref mut executor) = *executor_lock {
            executor.ensure_started().map_err(|e| {
                error!("Failed to start extraction executor: {}", e);
                e
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
                .map_err(|e| e.to_string())?;

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
    .map_err(|e| format!("Task join error: {}", e))?
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
    state: State<'_, Arc<super::AppState>>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Stopping exploration");

    let app_state = state.inner().clone();

    let response_result = tokio::task::spawn_blocking(
        move || -> Result<crate::executor::lifecycle::CommandResponseResult, String> {
            let mut executor_lock = crate::safe_lock::safe_lock_or_recover(
                &app_state.extraction_executor,
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
                    .map_err(|e| e.to_string())
            } else {
                Err("Extraction executor not initialized".to_string())
            }
        },
    )
    .await
    .map_err(|e| format!("Task join error: {}", e))??;

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
    state: State<'_, Arc<super::AppState>>,
    config: Option<crate::exploration::ExplorationConfig>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Starting native Rust exploration");

    let sdk_conn = state.sdk_connection.clone();
    let config = config.unwrap_or_default();

    let mut engine = crate::exploration::ExplorationEngine::new();

    // Store cancel token so it can be cancelled from another command
    {
        let mut cancel_guard = state.exploration_cancel.lock().await;
        *cancel_guard = Some(engine.cancel_token());
    }

    let result = engine.explore(&sdk_conn, config).await;

    // Clear cancel token
    {
        let mut cancel_guard = state.exploration_cancel.lock().await;
        *cancel_guard = None;
    }

    let result = result?;

    let result_json = serde_json::to_value(&result)
        .map_err(|e| format!("Failed to serialize discovery result: {}", e))?;

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
    state: State<'_, Arc<super::AppState>>,
) -> Result<CommandResponse, String> {
    info!("UI Bridge: Stopping native exploration");

    let cancel_guard = state.exploration_cancel.lock().await;
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
        serde_json::from_value(cooccurrence_export)
            .map_err(|e| format!("Failed to parse co-occurrence export: {}", e))?;

    let discovery_config = config.unwrap_or_default();
    let mut discovery = crate::exploration::FingerprintStateDiscovery::new(discovery_config);
    discovery.load_cooccurrence_export(&export);
    discovery.discover_states();
    let result = discovery.into_result();

    let result_json =
        serde_json::to_value(&result).map_err(|e| format!("Failed to serialize result: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Discovery complete: {} states",
            result.states.len()
        )),
        data: Some(result_json),
    })
}
