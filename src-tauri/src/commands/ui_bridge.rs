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

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tracing::info;

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
