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
///
/// # Every rate is nullable, and `null` means UNKNOWN
///
/// A rate whose denominator is zero was not measured. It is `None` here and
/// serializes as JSON `null` — never `0.0`, and never a "conservative" `1.0`.
/// `overall_score` is `None` whenever any input term is, because the declared
/// weighted formula has two `1 - rate` terms and a `+ 0.20` base: evaluated on
/// unknowns treated as zero it returns `0.70` for a completely empty window,
/// which the card painted as a green-ish "Good". That is a confident-looking
/// value with no provenance behind it, which fleet policy
/// `verification-and-evidence` `unknown-must-not-render-as-a-default` forbids.
///
/// The counts are **measured facts** and stay bare integers. In particular
/// `total_stalls` reports its true value even when `stall_frequency` is `null`
/// for want of a denominator — the fact that stalls happened is not in doubt,
/// only the rate is.
///
/// The trailing four fields are the payload's own coverage statement, so a
/// consumer can see what a `null` rests on instead of assuming.
/// `unknown_fields` is the machine-actionable discriminator: the names of the
/// fields that came back `null`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationHealthScore {
    /// `null` unless all three rates below are known.
    pub overall_score: Option<f64>,
    /// `null` when `total_interactions == 0`.
    pub element_success_rate: Option<f64>,
    /// `null` when `regression_eligible_pairs == 0`.
    pub regression_rate: Option<f64>,
    /// `null` when `total_interactions == 0`, **including** when
    /// `total_stalls > 0`.
    pub stall_frequency: Option<f64>,
    pub total_interactions: i64,
    pub total_elements: i64,
    pub total_stalls: i64,
    /// The `regression_rate` denominator — `(element, action)` pairs with
    /// enough samples to be judged at all. Exposed because it is the one
    /// denominator not otherwise visible in this payload.
    pub regression_eligible_pairs: i64,
    /// The `?days=` window actually applied.
    pub window_days: u32,
    /// The cutoff instant the query filtered on, epoch milliseconds.
    pub window_start_epoch_ms: i64,
    /// Names of the fields above that are `null`. Empty when everything was
    /// measured.
    pub unknown_fields: Vec<String>,
}

/// Prioritized improvement recommendation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Recommendation {
    pub priority: u32,
    pub category: String,
    pub message: String,
    pub impact: String,
}
