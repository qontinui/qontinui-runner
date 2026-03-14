//! Data types for fingerprint-based state discovery.
//!
//! These types mirror the Python `fingerprint_types.py` and TypeScript
//! `ui-bridge-types.ts` definitions, using camelCase JSON serialization
//! for compatibility with the frontend.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// Position Zone Constants
// =============================================================================

pub const GLOBAL_POSITION_ZONES: &[&str] = &["header", "footer"];
pub const BLOCKING_POSITION_ZONES: &[&str] = &["modal"];

// Zones excluded from exploration clicks (but elements are still captured)
pub const EXPLORATION_SKIP_ZONES: &[&str] = &["header", "footer", "fixed-top", "fixed-bottom"];

// =============================================================================
// Element Fingerprint
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementFingerprint {
    pub hash: String,
    pub structural_path: String,
    pub position_zone: String,
    pub landmark_context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landmark_label: Option<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub tag_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessible_name: Option<String>,
    #[serde(default)]
    pub size_category: String,
    #[serde(default)]
    pub relative_position: RelativePosition,
    #[serde(default)]
    pub is_repeating: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_pattern: Option<RepeatPattern>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelativePosition {
    #[serde(default)]
    pub top: f64,
    #[serde(default)]
    pub left: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepeatPattern {
    pub r#type: String,
    #[serde(default)]
    pub container_selector: String,
    #[serde(default)]
    pub item_selector: String,
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub total_count: usize,
    // Extra fields from TS (not in Python)
    #[serde(default)]
    pub container_tag: String,
    #[serde(default)]
    pub container_role: String,
    #[serde(default)]
    pub item_role: String,
}

// =============================================================================
// Capture & Transition Records
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRecord {
    pub capture_id: String,
    pub url: String,
    pub title: String,
    pub timestamp: i64,
    pub fingerprint_hashes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<TriggeredBy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggeredBy {
    pub action_type: String,
    pub target_fingerprint: String,
    pub previous_capture_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionRecord {
    pub action_id: String,
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_fingerprint: Option<String>,
    pub before_capture_id: String,
    pub after_capture_id: String,
    #[serde(default)]
    pub appeared_fingerprints: Vec<String>,
    #[serde(default)]
    pub disappeared_fingerprints: Vec<String>,
    pub timestamp: i64,
}

// =============================================================================
// Fingerprint Stats
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintStats {
    pub total_appearances: usize,
    pub capture_ids: Vec<String>,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: i64,
}

// =============================================================================
// State Candidate
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateCandidate {
    pub fingerprints: Vec<String>,
    pub cooccurrence_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landmark_context: Option<String>,
}

// =============================================================================
// Presence Matrix
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceMatrixEntry {
    pub capture_id: String,
    #[serde(default)]
    pub capture_index: usize,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    pub fingerprints: Vec<String>,
}

// =============================================================================
// Co-occurrence Export (full session data)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CooccurrenceExport {
    pub session_id: String,
    pub exported_at: i64,
    pub all_fingerprints: Vec<String>,
    pub fingerprint_details: HashMap<String, ElementFingerprint>,
    pub presence_matrix: Vec<PresenceMatrixEntry>,
    pub cooccurrence_counts: HashMap<String, HashMap<String, usize>>,
    pub fingerprint_stats: HashMap<String, FingerprintStats>,
    #[serde(default)]
    pub transitions: Vec<TransitionRecord>,
    #[serde(default)]
    pub state_candidates: Vec<StateCandidate>,
}

// =============================================================================
// Discovery Results
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredState {
    pub state_id: String,
    pub name: String,
    pub fingerprint_hashes: Vec<String>,
    #[serde(default)]
    pub element_ids: Vec<String>,
    #[serde(default = "default_main")]
    pub position_zone: String,
    #[serde(default)]
    pub landmark_context: String,
    #[serde(default)]
    pub is_global: bool,
    #[serde(default)]
    pub is_modal: bool,
    #[serde(default)]
    pub repeat_pattern_count: usize,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub observation_count: usize,
}

fn default_main() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTransition {
    pub from_state_id: String,
    pub to_state_id: String,
    pub action_type: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResult {
    pub states: Vec<DiscoveredState>,
    pub transitions: Vec<DiscoveredTransition>,
    pub statistics: DiscoveryStatistics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryStatistics {
    pub total_captures: usize,
    pub total_transitions: usize,
    pub unique_fingerprints: usize,
    pub discovered_states: usize,
    pub global_states: usize,
    pub modal_states: usize,
    pub discovered_transitions: usize,
}

// =============================================================================
// Exploration Config & Progress
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationConfig {
    #[serde(default = "default_max_interactions")]
    pub max_interactions: usize,
    #[serde(default = "default_settle_delay")]
    pub settle_delay_ms: u64,
    #[serde(default = "default_change_threshold")]
    pub change_threshold: f64,
    #[serde(default)]
    pub blocked_keywords: Vec<String>,
    #[serde(default = "default_viewport_width")]
    pub viewport_width: f64,
    #[serde(default = "default_viewport_height")]
    pub viewport_height: f64,
}

fn default_max_interactions() -> usize {
    50
}
fn default_settle_delay() -> u64 {
    500
}
fn default_change_threshold() -> f64 {
    0.3
}
fn default_viewport_width() -> f64 {
    1920.0
}
fn default_viewport_height() -> f64 {
    1080.0
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            max_interactions: 50,
            settle_delay_ms: 500,
            change_threshold: 0.3,
            blocked_keywords: vec![],
            viewport_width: 1920.0,
            viewport_height: 1080.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplorationProgress {
    pub current: usize,
    pub total: usize,
    pub current_element: Option<String>,
    pub status: String,
    pub captures: usize,
    pub unique_fingerprints: usize,
}
