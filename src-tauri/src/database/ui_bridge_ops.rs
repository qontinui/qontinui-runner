//! CRUD operations for UI Bridge persistence tables.
//!
//! Covers: ui_bridge_elements, ui_bridge_events, ui_bridge_navigation_history,
//! and stall_events.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeElement {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub element_id: String,
    pub tag_name: Option<String>,
    pub element_type: Option<String>,
    pub bounds: Option<String>,
    pub visible: bool,
    pub enabled: bool,
    pub focused: bool,
    pub value: Option<String>,
    pub text_content: Option<String>,
    pub label: Option<String>,
    pub parent_id: Option<String>,
    pub children: Option<String>,
    pub actions: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeEvent {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub sequence: i64,
    pub event_type: String,
    pub element_id: Option<String>,
    pub state_id: Option<String>,
    pub transition_id: Option<String>,
    pub action: Option<String>,
    pub params: Option<String>,
    pub result: Option<String>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiBridgeNavigationEvent {
    pub id: i64,
    pub task_run_id: Option<i64>,
    pub timestamp: i64,
    pub target_states: String,
    pub path_found: bool,
    pub transitions_planned: Option<String>,
    pub transitions_executed: Option<String>,
    pub total_cost: Option<f64>,
    pub duration_ms: Option<f64>,
    pub success: bool,
    pub final_active_states: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StallEvent {
    pub id: String,
    pub task_run_id: String,
    pub iteration: i64,
    pub pattern_type: String,
    pub pattern_details: Option<String>,
    pub action_count: Option<i64>,
    pub intervention_action: Option<String>,
    pub intervention_result: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ElementReliability {
    pub element_id: String,
    pub total_interactions: i64,
    pub successful_interactions: i64,
    pub success_rate: f64,
    pub last_failure_reason: Option<String>,
    pub flaky: bool,
    pub recommended_confidence: f64,
}

/// A single time-window bucket in a decay curve.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecayCurveBucket {
    pub bucket: i64,
    pub total: i64,
    pub successes: i64,
    pub success_rate: f64,
}

/// Action type performance baseline.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionBaseline {
    pub action: String,
    pub count: i64,
    pub avg_duration_ms: Option<f64>,
    pub min_duration_ms: Option<f64>,
    pub max_duration_ms: Option<f64>,
    pub success_rate: f64,
}

/// A cluster of failures grouped by error message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailureCluster {
    pub error_message: String,
    pub count: i64,
    pub affected_elements: String,
}

/// Element with its bounds and success rate for fragility mapping.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ElementFragility {
    pub element_id: String,
    pub bounds: Option<String>,
    pub interaction_count: i64,
    pub success_rate: f64,
}

/// Automation regression: element+action that degraded between runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationRegression {
    pub element_id: String,
    pub action: String,
    pub prior_success_rate: f64,
    pub recent_success_rate: f64,
    pub delta: f64,
}

/// Stall frequency grouped by pattern type.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StallFrequency {
    pub pattern_type: String,
    pub count: i64,
}

/// Intervention effectiveness stats.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InterventionStats {
    pub intervention_action: String,
    pub total: i64,
    pub successful: i64,
    pub success_rate: f64,
}

/// Element with high interaction count but missing annotations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnnotationGap {
    pub element_id: String,
    pub interaction_count: i64,
    pub success_rate: f64,
    pub element_type: Option<String>,
    pub label: Option<String>,
}

/// Composite automation health score breakdown.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationHealthScore {
    pub overall_score: f64,
    pub element_success_rate: f64,
    pub regression_rate: f64,
    pub stall_frequency: f64,
    pub total_interactions: i64,
    pub total_elements: i64,
    pub total_stalls: i64,
}

/// Prioritized improvement recommendation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Recommendation {
    pub priority: u32,
    pub category: String,
    pub message: String,
    pub impact: String,
}
