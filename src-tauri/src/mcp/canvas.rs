//! Canvas API Module
//!
//! Provides HTTP endpoints for agent-to-UI (A2UI) canvas panels.
//! The AI agent can POST structured JSON panels that are rendered
//! in the runner frontend's Canvas dashboard widget.

use axum::{
    extract::{Path, State},
    response::Json,
    routing::{get, post},
    Router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Emitter;
use tracing::{info, warn};

use crate::event_system::AppEvent;
use crate::mcp::types::{ApiResponse, ApiState};

/// Maximum number of active panels per task run.
const MAX_PANELS_PER_TASK_RUN: usize = 50;

/// Maximum payload size per panel data (1MB).
const MAX_PANEL_DATA_SIZE: usize = 1_048_576;

/// Allowed component types (validated server-side).
const ALLOWED_COMPONENTS: &[&str] = &[
    "Markdown",
    "CodeDiff",
    "Table",
    "FileTree",
    "KeyValue",
    "Terminal",
    "Alert",
    "Timeline",
    "ProgressChart",
    "FindingList",
    "Checklist",
    "SummaryStats",
    "StateTimeline",
    "Waterfall",
    "Sparkline",
    "WaffleChart",
    "PhaseTimeline",
    "IterationComparison",
    "StepDurationChart",
    "PhaseDistribution",
    "DependencyGraph",
    "CostBreakdown",
    "MissionBrief",
    "AcceptanceCriteria",
];

/// In-memory canvas state.
pub struct CanvasState {
    panels: HashMap<String, StoredPanel>,
    task_run_id: Option<String>,
}

impl CanvasState {
    pub fn new() -> Self {
        Self {
            panels: HashMap::new(),
            task_run_id: None,
        }
    }

    /// Clear all panels (used when starting a new task run).
    pub fn clear(&mut self) {
        self.panels.clear();
        self.task_run_id = None;
    }

    /// Insert or update a panel in the canvas state.
    pub fn insert_panel(&mut self, panel: StoredPanel) {
        self.panels.insert(panel.panel_id.clone(), panel);
    }

    /// Set the task run ID for the current canvas state.
    pub fn set_task_run_id(&mut self, id: Option<String>) {
        self.task_run_id = id;
    }

    /// Get the number of panels in the canvas state.
    pub fn panel_count(&self) -> usize {
        self.panels.len()
    }
}

/// A stored canvas panel.
///
/// Wire-format mirror of `qontinui_types::app_events::CanvasPanel`. The
/// canonical wire shape lives in qontinui-schemas so the schemars→TS
/// pipeline can emit a typed `CanvasPanel` for the `canvas-update` Tauri
/// event listener (`useCanvasData.ts`).
///
/// `data` stays `serde_json::Value` because each `component` type has a
/// different inner shape — see the per-component data schemas in
/// `qontinui-schemas/ts/src/canvas/index.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StoredPanel {
    pub panel_id: String,
    pub component: String,
    pub title: String,
    pub data: serde_json::Value,
    pub priority: i32,
    pub size: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub task_run_id: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Request body for creating/updating a panel.
#[derive(Debug, Deserialize)]
pub struct CreatePanelRequest {
    /// Agent-chosen unique ID (allows update-in-place).
    pub panel_id: String,
    /// Must be in ALLOWED_COMPONENTS.
    pub component: String,
    /// Panel header text.
    pub title: String,
    /// Component-specific payload.
    pub data: serde_json::Value,
    /// Display ordering (lower = first, default 50).
    #[serde(default = "default_priority")]
    pub priority: Option<i32>,
    /// Panel size: "compact" | "normal" | "large".
    #[serde(default)]
    pub size: Option<String>,
    /// Optional group name for organizing panels into sections.
    #[serde(default)]
    pub group: Option<String>,
    /// Optional task run ID (auto-detected from running task if not provided).
    #[serde(default)]
    pub task_run_id: Option<String>,
    /// Auto-dismiss in seconds (0 = permanent, default 0). Reserved for future use.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

fn default_priority() -> Option<i32> {
    Some(50)
}

/// Request body for patching panel data (e.g., interactive checklist updates).
#[derive(Debug, Deserialize)]
pub struct PatchPanelRequest {
    pub data: serde_json::Value,
}

/// Response for a panel operation.
#[derive(Debug, Serialize)]
pub struct PanelResponse {
    pub panel_id: String,
    pub action: String,
}

/// Create or update a canvas panel.
async fn create_or_update_panel(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreatePanelRequest>,
) -> Json<ApiResponse<PanelResponse>> {
    // Validate component type
    if !ALLOWED_COMPONENTS.contains(&request.component.as_str()) {
        return Json(ApiResponse::error(format!(
            "Unknown component type '{}'. Allowed: {}",
            request.component,
            ALLOWED_COMPONENTS.join(", ")
        )));
    }

    // Validate panel_id is not empty
    if request.panel_id.trim().is_empty() {
        return Json(ApiResponse::error("panel_id cannot be empty"));
    }

    // Validate data size
    let data_str = request.data.to_string();
    if data_str.len() > MAX_PANEL_DATA_SIZE {
        return Json(ApiResponse::error(format!(
            "Panel data exceeds maximum size of {} bytes",
            MAX_PANEL_DATA_SIZE
        )));
    }

    // Validate size
    let size = request.size.unwrap_or_else(|| "normal".to_string());
    if !["compact", "normal", "large"].contains(&size.as_str()) {
        return Json(ApiResponse::error(
            "Invalid size. Must be 'compact', 'normal', or 'large'",
        ));
    }

    // Determine task_run_id: use provided, or detect from running task
    let task_run_id = if let Some(id) = request.task_run_id {
        id
    } else {
        // Try to find the currently running task run
        let runs = state
            .app_state
            .pg_db
            .get_running_task_runs(None)
            .await
            .unwrap_or_default();
        if !runs.is_empty() {
            runs[0].id.clone()
        } else {
            "unknown".to_string()
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    let priority = request.priority.unwrap_or(50);

    let panel = StoredPanel {
        panel_id: request.panel_id.clone(),
        component: request.component,
        title: request.title,
        data: request.data,
        priority,
        size,
        group: request.group,
        task_run_id: task_run_id.clone(),
        created_at: now.clone(),
        updated_at: now,
    };

    // Determine if this is a create or update
    let action;
    {
        let mut canvas = state.app_state.canvas_state.write().await;

        // Check panel limit (only for new panels)
        if !canvas.panels.contains_key(&panel.panel_id)
            && canvas.panels.len() >= MAX_PANELS_PER_TASK_RUN
        {
            return Json(ApiResponse::error(format!(
                "Maximum panel limit ({}) reached",
                MAX_PANELS_PER_TASK_RUN
            )));
        }

        action = if canvas.panels.contains_key(&panel.panel_id) {
            "update"
        } else {
            "create"
        };

        canvas.task_run_id = Some(task_run_id.clone());
        canvas.panels.insert(panel.panel_id.clone(), panel.clone());
    }

    // Emit canvas-update event. `panel` now ships as a typed `StoredPanel`
    // instead of a `serde_json::Value` so the schemars→TS pipeline produces
    // a typed payload — wire format unchanged (snake_case fields).
    let event = AppEvent::CanvasUpdate {
        action: action.to_string(),
        panel_id: panel.panel_id.clone(),
        panel: Some(panel.clone()),
        task_run_id: Some(task_run_id),
    };
    if let Err(e) = state.app_handle.emit(event.event_name(), &event) {
        warn!("Failed to emit canvas-update event: {}", e);
    }

    info!(
        "Canvas panel '{}' {}: {} ({})",
        panel.panel_id, action, panel.title, panel.component
    );

    Json(ApiResponse::success(PanelResponse {
        panel_id: panel.panel_id,
        action: action.to_string(),
    }))
}

/// List all active panels for the current task run.
async fn list_panels(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<Vec<StoredPanel>>> {
    let canvas = state.app_state.canvas_state.read().await;
    let mut panels: Vec<StoredPanel> = canvas.panels.values().cloned().collect();
    panels.sort_by_key(|p| p.priority);
    Json(ApiResponse::success(panels))
}

/// Get a single panel by ID.
async fn get_panel(
    State(state): State<Arc<ApiState>>,
    Path(panel_id): Path<String>,
) -> Json<ApiResponse<Option<StoredPanel>>> {
    let canvas = state.app_state.canvas_state.read().await;
    let panel = canvas.panels.get(&panel_id).cloned();
    Json(ApiResponse::success(panel))
}

/// Delete a single panel.
async fn delete_panel(
    State(state): State<Arc<ApiState>>,
    Path(panel_id): Path<String>,
) -> Json<ApiResponse<PanelResponse>> {
    let task_run_id;
    {
        let mut canvas = state.app_state.canvas_state.write().await;
        task_run_id = canvas.task_run_id.clone();
        canvas.panels.remove(&panel_id);
    }

    // Emit canvas-update event
    let event = AppEvent::CanvasUpdate {
        action: "delete".to_string(),
        panel_id: panel_id.clone(),
        panel: None,
        task_run_id,
    };
    if let Err(e) = state.app_handle.emit(event.event_name(), &event) {
        warn!("Failed to emit canvas-update delete event: {}", e);
    }

    info!("Canvas panel '{}' deleted", panel_id);

    Json(ApiResponse::success(PanelResponse {
        panel_id,
        action: "delete".to_string(),
    }))
}

/// Clear all panels.
async fn clear_panels(State(state): State<Arc<ApiState>>) -> Json<ApiResponse<PanelResponse>> {
    let task_run_id;
    {
        let mut canvas = state.app_state.canvas_state.write().await;
        task_run_id = canvas.task_run_id.clone();
        canvas.clear();
    }

    // Emit canvas-update event
    let event = AppEvent::CanvasUpdate {
        action: "clear".to_string(),
        panel_id: String::new(),
        panel: None,
        task_run_id,
    };
    if let Err(e) = state.app_handle.emit(event.event_name(), &event) {
        warn!("Failed to emit canvas-update clear event: {}", e);
    }

    info!("All canvas panels cleared");

    Json(ApiResponse::success(PanelResponse {
        panel_id: String::new(),
        action: "clear".to_string(),
    }))
}

/// Patch panel data (e.g., for interactive checklist updates from the frontend).
async fn patch_panel_data(
    State(state): State<Arc<ApiState>>,
    Path(panel_id): Path<String>,
    Json(request): Json<PatchPanelRequest>,
) -> Json<ApiResponse<PanelResponse>> {
    let now = chrono::Utc::now().to_rfc3339();
    let task_run_id;
    let updated_panel;

    {
        let mut canvas = state.app_state.canvas_state.write().await;
        let panel = match canvas.panels.get_mut(&panel_id) {
            Some(p) => p,
            None => {
                return Json(ApiResponse::error(format!(
                    "Panel '{}' not found",
                    panel_id
                )));
            }
        };

        panel.data = request.data;
        panel.updated_at = now;
        task_run_id = Some(panel.task_run_id.clone());
        updated_panel = panel.clone();
    }

    // Emit canvas-update event with the typed `StoredPanel` payload.
    let event = AppEvent::CanvasUpdate {
        action: "update".to_string(),
        panel_id: panel_id.clone(),
        panel: Some(updated_panel),
        task_run_id,
    };
    if let Err(e) = state.app_handle.emit(event.event_name(), &event) {
        warn!("Failed to emit canvas-update event for patch: {}", e);
    }

    Json(ApiResponse::success(PanelResponse {
        panel_id,
        action: "update".to_string(),
    }))
}

/// Get historical panels for a completed task run.
async fn get_historical_panels(
    State(_state): State<Arc<ApiState>>,
    Path(_task_run_id): Path<String>,
) -> Json<ApiResponse<Vec<StoredPanel>>> {
    // SQLite checkpoint_db removed — historical panels not yet migrated to PG
    Json(ApiResponse::success(vec![]))
}

/// Define routes for the canvas module.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/canvas/panels",
            post(create_or_update_panel)
                .get(list_panels)
                .delete(clear_panels),
        )
        .route(
            "/canvas/panels/history/{task_run_id}",
            get(get_historical_panels),
        )
        .route(
            "/canvas/panels/{panel_id}",
            get(get_panel).delete(delete_panel).patch(patch_panel_data),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_components() {
        assert!(ALLOWED_COMPONENTS.contains(&"Markdown"));
        assert!(ALLOWED_COMPONENTS.contains(&"Table"));
        assert!(ALLOWED_COMPONENTS.contains(&"CodeDiff"));
        assert!(ALLOWED_COMPONENTS.contains(&"SummaryStats"));
        assert!(ALLOWED_COMPONENTS.contains(&"StateTimeline"));
        assert!(ALLOWED_COMPONENTS.contains(&"Waterfall"));
        assert!(ALLOWED_COMPONENTS.contains(&"Sparkline"));
        assert!(ALLOWED_COMPONENTS.contains(&"WaffleChart"));
        assert!(ALLOWED_COMPONENTS.contains(&"AcceptanceCriteria"));
        assert!(!ALLOWED_COMPONENTS.contains(&"arbitrary_html"));
    }

    #[test]
    fn test_canvas_state_clear() {
        let mut state = CanvasState::new();
        state.panels.insert(
            "test".to_string(),
            StoredPanel {
                panel_id: "test".to_string(),
                component: "Markdown".to_string(),
                title: "Test".to_string(),
                data: serde_json::json!({"content": "hello"}),
                priority: 50,
                size: "normal".to_string(),
                group: None,
                task_run_id: "task-1".to_string(),
                created_at: "2024-01-01".to_string(),
                updated_at: "2024-01-01".to_string(),
            },
        );
        state.task_run_id = Some("task-1".to_string());

        assert_eq!(state.panels.len(), 1);
        state.clear();
        assert_eq!(state.panels.len(), 0);
        assert!(state.task_run_id.is_none());
    }
}
