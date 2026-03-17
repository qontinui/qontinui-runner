//! Type definitions for state machine config CRUD operations.

use serde::{Deserialize, Serialize};

// =============================================================================
// Config
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmConfig {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub render_count: i64,
    pub element_count: i64,
    pub include_html_ids: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSmConfigRequest {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSmConfigRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub render_count: Option<i64>,
    pub element_count: Option<i64>,
    pub include_html_ids: Option<bool>,
}

// =============================================================================
// State
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmState {
    pub id: String,
    pub config_id: String,
    pub state_id: String,
    pub name: String,
    pub description: Option<String>,
    pub element_ids: Vec<String>,
    pub render_ids: Vec<String>,
    pub confidence: f64,
    pub acceptance_criteria: Vec<String>,
    pub extra_metadata: serde_json::Value,
    pub domain_knowledge: Vec<SmDomainKnowledge>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmDomainKnowledge {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSmStateRequest {
    pub state_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub element_ids: Option<Vec<String>>,
    pub render_ids: Option<Vec<String>>,
    pub confidence: Option<f64>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub extra_metadata: Option<serde_json::Value>,
    pub domain_knowledge: Option<Vec<SmDomainKnowledge>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSmStateRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub element_ids: Option<Vec<String>>,
    pub render_ids: Option<Vec<String>>,
    pub confidence: Option<f64>,
    pub acceptance_criteria: Option<Vec<String>>,
    pub extra_metadata: Option<serde_json::Value>,
    pub domain_knowledge: Option<Vec<SmDomainKnowledge>>,
}

// =============================================================================
// Transition
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmTransition {
    pub id: String,
    pub config_id: String,
    pub transition_id: String,
    pub name: String,
    pub from_states: Vec<String>,
    pub activate_states: Vec<String>,
    pub exit_states: Vec<String>,
    pub actions: Vec<serde_json::Value>,
    pub path_cost: f64,
    pub stays_visible: bool,
    pub extra_metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSmTransitionRequest {
    pub transition_id: Option<String>,
    pub name: String,
    pub from_states: Vec<String>,
    pub activate_states: Vec<String>,
    pub exit_states: Vec<String>,
    pub actions: Vec<serde_json::Value>,
    pub path_cost: Option<f64>,
    pub stays_visible: Option<bool>,
    pub extra_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSmTransitionRequest {
    pub name: Option<String>,
    pub from_states: Option<Vec<String>>,
    pub activate_states: Option<Vec<String>>,
    pub exit_states: Option<Vec<String>>,
    pub actions: Option<Vec<serde_json::Value>>,
    pub path_cost: Option<f64>,
    pub stays_visible: Option<bool>,
    pub extra_metadata: Option<serde_json::Value>,
}

// =============================================================================
// Full Config (with states + transitions)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmConfigFull {
    #[serde(flatten)]
    pub config: SmConfig,
    pub states: Vec<SmState>,
    pub transitions: Vec<SmTransition>,
}

// =============================================================================
// Import/Export
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmImportRequest {
    pub name: Option<String>,
    pub config: serde_json::Value,
}

// =============================================================================
// Capture Screenshots
// =============================================================================

/// Metadata for a capture screenshot (returned without image data).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmCaptureScreenshotMeta {
    pub id: String,
    pub config_id: String,
    pub capture_index: i64,
    pub width: i64,
    pub height: i64,
    pub element_bounds_json: String,
    pub fingerprint_hashes_json: String,
    pub captured_at: String,
}

/// Full capture screenshot data for saving (includes base64 PNG that gets converted to WebP).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmCaptureScreenshotSave {
    pub capture_index: i64,
    pub screenshot_base64: String,
    pub width: i64,
    pub height: i64,
    pub element_bounds_json: String,
    pub fingerprint_hashes_json: String,
    pub captured_at: String,
}
