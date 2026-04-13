//! Bridge between Rust workflow executor and Python HTN planning layer.
//!
//! Provides functions for requesting HTN plans and reporting outcomes.
//! The Python planner lives in `multistate.planning` and is called via
//! subprocess/IPC.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Serialized world state for the Python HTN planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnWorldState {
    pub active_states: Vec<String>,
    pub available_transitions: Vec<String>,
    pub element_visible: std::collections::HashMap<String, bool>,
    pub element_values: std::collections::HashMap<String, String>,
}

/// A single action in an HTN plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnAction {
    pub name: String,
    pub args: Vec<serde_json::Value>,
}

/// Result of an HTN planning request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnPlanResult {
    pub success: bool,
    pub actions: Vec<HtnAction>,
    pub planning_time_ms: f64,
    pub nodes_explored: u32,
    pub error: Option<String>,
}

/// Outcome of plan execution, reported for meta-optimizer learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnPlanOutcome {
    pub plan_id: String,
    pub success: bool,
    pub steps_executed: u32,
    pub steps_succeeded: u32,
    pub replans: u32,
    pub total_time_ms: f64,
    pub error: Option<String>,
}

/// Configuration for HTN planning integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnConfig {
    /// Whether to attempt HTN planning before falling back to AI agent.
    pub enabled: bool,
    /// Maximum time to wait for planning (ms).
    pub planning_timeout_ms: u64,
    /// Maximum replans during execution.
    pub max_replans: u32,
    /// Python executable path (uses "python" if not set).
    pub python_path: Option<String>,
}

impl Default for HtnConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Off by default until method library matures
            planning_timeout_ms: 5000,
            max_replans: 5,
            python_path: None,
        }
    }
}

/// Request an HTN plan from the Python planner.
///
/// Calls `python -c "..."` to run the planner and returns the result.
/// This is a lightweight bridge — the heavy lifting is in Python.
pub async fn request_htn_plan(
    task: &str,
    world_state: &HtnWorldState,
    config: &HtnConfig,
) -> Result<HtnPlanResult, String> {
    let state_json = serde_json::to_string(world_state)
        .map_err(|e| format!("Failed to serialize world state: {}", e))?;

    let task_json = serde_json::to_string(&task)
        .map_err(|e| format!("Failed to serialize task: {}", e))?;

    // Build a Python script that creates the planner, runs it, and prints
    // JSON to stdout. We pass state and task as escaped JSON strings inside
    // the script.
    let python_script = format!(
        r#"
import json, sys
from multistate.planning.planner import HTNPlanner, WorldState
from multistate.planning.operators import STANDARD_OPERATORS
from multistate.planning.methods.generic import GENERIC_METHODS

state_data = json.loads({state_json_py})
task_tuple = tuple(json.loads({task_json_py}))

state = WorldState(
    active_states=set(state_data['active_states']),
    available_transitions=set(state_data['available_transitions']),
    element_visible=state_data.get('element_visible', {{}}),
    element_values=state_data.get('element_values', {{}}),
    blackboard={{}},
)

planner = HTNPlanner()
for name, op in STANDARD_OPERATORS.items():
    planner.register_operator(name, op)
for task_name, methods in GENERIC_METHODS.items():
    for m in methods:
        planner.register_method(task_name, m)

result = planner.find_plan(state, [task_tuple])
print(json.dumps({{
    'success': result.success,
    'actions': [list(a) for a in result.actions],
    'planning_time_ms': result.planning_time_ms,
    'nodes_explored': result.nodes_explored,
    'error': result.error,
}}))
"#,
        state_json_py = serde_json::to_string(&state_json)
            .map_err(|e| format!("Failed to double-serialize state: {}", e))?,
        task_json_py = serde_json::to_string(&task_json)
            .map_err(|e| format!("Failed to double-serialize task: {}", e))?,
    );

    let python = config.python_path.as_deref().unwrap_or("python");

    debug!("Running HTN planner via: {} -c <script>", python);

    let output = tokio::time::timeout(
        std::time::Duration::from_millis(config.planning_timeout_ms),
        tokio::process::Command::new(python)
            .args(["-c", &python_script])
            .output(),
    )
    .await
    .map_err(|_| "HTN planning timed out".to_string())?
    .map_err(|e| format!("Failed to run Python planner: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("HTN planner failed: {}", stderr);
        return Err(format!("HTN planner failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the raw JSON into an intermediate structure first, since the
    // Python planner returns actions as arrays [name, arg1, arg2, ...]
    // rather than {name, args} objects.
    let raw: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse planner output: {} (raw: {})", e, stdout))?;

    let success = raw["success"].as_bool().unwrap_or(false);
    let planning_time_ms = raw["planning_time_ms"].as_f64().unwrap_or(0.0);
    let nodes_explored = raw["nodes_explored"].as_u64().unwrap_or(0) as u32;
    let error = raw["error"].as_str().map(String::from);

    let actions = raw["actions"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|a| {
            let arr = a.as_array()?;
            let name = arr.first()?.as_str()?.to_string();
            let args = arr.get(1..).unwrap_or(&[]).to_vec();
            Some(HtnAction { name, args })
        })
        .collect();

    let result = HtnPlanResult {
        success,
        actions,
        planning_time_ms,
        nodes_explored,
        error,
    };

    info!(
        "HTN plan result: success={}, actions={}, time={:.1}ms",
        result.success,
        result.actions.len(),
        result.planning_time_ms,
    );

    Ok(result)
}

/// Report plan execution outcome for meta-optimizer learning.
///
/// Stores the outcome via structured logging. Database persistence will be
/// added when the meta-optimizer DB schema is extended for HTN-specific data.
pub fn report_plan_outcome(outcome: &HtnPlanOutcome) {
    info!(
        plan_id = %outcome.plan_id,
        success = outcome.success,
        steps_executed = outcome.steps_executed,
        steps_succeeded = outcome.steps_succeeded,
        replans = outcome.replans,
        total_time_ms = outcome.total_time_ms,
        "HTN plan outcome",
    );
    // TODO: Store in learning_outcomes table when meta-optimizer DB schema is extended
    // For now, structured logging captures the data for analysis
    debug!("HTN plan outcome details: {:?}", outcome);
}

/// Check if HTN planning should be attempted for the current context.
///
/// Returns `true` if HTN is enabled and the current state has enough
/// structure (active states, transitions) to make planning worthwhile.
pub fn should_attempt_htn(config: &HtnConfig, world_state: &HtnWorldState) -> bool {
    if !config.enabled {
        return false;
    }
    // Need at least some active states and transitions for planning to be useful
    !world_state.active_states.is_empty() && !world_state.available_transitions.is_empty()
}
