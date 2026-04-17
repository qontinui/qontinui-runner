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
    /// UI Bridge URL for querying element state (e.g., "http://localhost:1420").
    /// If None, HTN runs in plan-only mode without actual GUI execution.
    pub ui_bridge_url: Option<String>,
    /// UI Bridge target type: "web", "desktop", "mobile".
    pub target_type: Option<String>,
    /// Optional path to a serialized StateManager JSON file.
    /// When provided, the planner uses the saved state machine; otherwise empty.
    pub state_machine_path: Option<String>,
}

impl Default for HtnConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Off by default until method library matures
            planning_timeout_ms: 5000,
            max_replans: 5,
            python_path: None,
            ui_bridge_url: None,
            target_type: None,
            state_machine_path: None,
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

    let task_json =
        serde_json::to_string(&task).map_err(|e| format!("Failed to serialize task: {}", e))?;

    // Build a Python script that creates the planner, runs it, and prints
    // JSON to stdout. We pass state and task as escaped JSON strings inside
    // the script.
    let multistate_src =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../multistate/src");
    let multistate_src_str = multistate_src.display().to_string().replace('\\', "\\\\");

    let python_script = format!(
        r#"
import sys; sys.path.insert(0, r'{multistate_src_str}')
import json
from multistate.planning.planner import HTNPlanner, WorldState
from multistate.planning.operators import STANDARD_OPERATORS
from multistate.planning.methods.generic import GENERIC_METHODS

state_data = json.loads({state_json_py})
task_raw = json.loads({task_json_py})
if isinstance(task_raw, str):
    task_tuple = (task_raw,)
elif isinstance(task_raw, list):
    task_tuple = tuple(task_raw)
else:
    task_tuple = (str(task_raw),)

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
        multistate_src_str = multistate_src_str,
        state_json_py = serde_json::to_string(&state_json)
            .map_err(|e| format!("Failed to double-serialize state: {}", e))?,
        task_json_py = serde_json::to_string(&task_json)
            .map_err(|e| format!("Failed to double-serialize task: {}", e))?,
    );

    let python = config.python_path.as_deref().unwrap_or("python");

    debug!("Running HTN planner via: {} -c <script>", python);

    let mut child = tokio::process::Command::new(python)
        .args(["-c", &python_script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn Python planner: {}", e))?;

    let output = match tokio::time::timeout(
        std::time::Duration::from_millis(config.planning_timeout_ms),
        async {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let status = child.wait().await?;
            let mut stdout_bytes = Vec::new();
            if let Some(mut out) = stdout {
                tokio::io::AsyncReadExt::read_to_end(&mut out, &mut stdout_bytes).await?;
            }
            let mut stderr_bytes = Vec::new();
            if let Some(mut err) = stderr {
                tokio::io::AsyncReadExt::read_to_end(&mut err, &mut stderr_bytes).await?;
            }
            Ok::<std::process::Output, std::io::Error>(std::process::Output {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            })
        },
    )
    .await
    {
        Ok(result) => result.map_err(|e| format!("Python planner failed: {}", e))?,
        Err(_) => {
            let _ = child.kill().await;
            return Err("HTN planning timed out".to_string());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("HTN planner failed: {}", stderr);
        return Err(format!("HTN planner failed: {}", stderr));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout_str
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");

    // Parse the raw JSON into an intermediate structure first, since the
    // Python planner returns actions as arrays [name, arg1, arg2, ...]
    // rather than {name, args} objects.
    let raw: serde_json::Value = serde_json::from_str(json_line).map_err(|e| {
        format!(
            "Failed to parse planner output: {} (raw: {})",
            e, stdout_str
        )
    })?;

    let success = raw["success"].as_bool().unwrap_or(false);
    let planning_time_ms = raw["planning_time_ms"].as_f64().unwrap_or(0.0);
    let nodes_explored = raw["nodes_explored"].as_u64().unwrap_or(0) as u32;
    let error = raw["error"].as_str().map(String::from);

    let empty_actions = vec![];
    let actions = raw["actions"]
        .as_array()
        .unwrap_or(&empty_actions)
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
/// Returns `true` if HTN is enabled. World state is now built inside the
/// Python subprocess (via `execute_htn_attempt`), so we no longer require
/// pre-populated state from Rust.
pub fn should_attempt_htn(config: &HtnConfig) -> bool {
    config.enabled
}

/// Result of a full HTN plan-and-execute attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HtnExecutionResult {
    /// Whether planning found a viable plan.
    pub plan_found: bool,
    /// Whether plan execution succeeded (only meaningful if plan_found).
    pub execution_success: bool,
    /// Number of plan actions.
    pub plan_actions: u32,
    /// Number of steps that executed successfully.
    pub steps_succeeded: u32,
    /// Number of replans during execution.
    pub replans: u32,
    /// Total time in ms (planning + execution).
    pub total_time_ms: f64,
    /// Summary of what was done (for AI context in case of failure).
    pub summary: String,
    /// Error message if failed.
    pub error: Option<String>,
}

/// Attempt to plan AND execute an HTN fix for the given failure context.
///
/// This is a self-contained call that runs a Python script which:
/// 1. Initializes HAL and optionally connects to UI Bridge
/// 2. Snapshots the current world state
/// 3. Plans using all registered operators and methods
/// 4. Executes the plan with replanning on state divergence
/// 5. Returns the execution result as JSON
///
/// Returns `Ok(HtnExecutionResult)` with success/failure and details.
/// Returns `Err(String)` if the Python subprocess fails entirely.
pub async fn execute_htn_attempt(
    task_description: &str,
    config: &HtnConfig,
) -> Result<HtnExecutionResult, String> {
    let qontinui_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../qontinui/src");
    let multistate_src =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../multistate/src");

    // Build stdin JSON config for the CLI
    let stdin_config = serde_json::json!({
        "task": task_description,
        "ui_bridge_url": config.ui_bridge_url,
        "target_type": config.target_type.as_deref().unwrap_or("web"),
        "state_machine_path": config.state_machine_path,
        "planning_timeout_ms": config.planning_timeout_ms,
        "max_replans": config.max_replans,
    });
    let stdin_json = stdin_config.to_string();

    let python = config.python_path.as_deref().unwrap_or("python");
    debug!(
        "Running HTN via: {} -m qontinui.planning_integration",
        python
    );

    // Build PYTHONPATH with both src trees
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut pythonpath = format!(
        "{}{}{}",
        qontinui_src.display(),
        sep,
        multistate_src.display(),
    );
    if let Ok(existing) = std::env::var("PYTHONPATH") {
        if !existing.is_empty() {
            pythonpath = format!("{}{}{}", pythonpath, sep, existing);
        }
    }

    let mut child = tokio::process::Command::new(python)
        .args(["-m", "qontinui.planning_integration"])
        .env("PYTHONPATH", pythonpath)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn Python HTN CLI: {}", e))?;

    // Write config to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        if let Err(e) = stdin.write_all(stdin_json.as_bytes()).await {
            let _ = child.kill().await;
            return Err(format!("Failed to write HTN config to stdin: {}", e));
        }
        // Dropping stdin closes it, signaling EOF to Python
        drop(stdin);
    }

    let output = match tokio::time::timeout(
        std::time::Duration::from_millis(config.planning_timeout_ms),
        async {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let status = child.wait().await?;
            let mut stdout_bytes = Vec::new();
            if let Some(mut out) = stdout {
                tokio::io::AsyncReadExt::read_to_end(&mut out, &mut stdout_bytes).await?;
            }
            let mut stderr_bytes = Vec::new();
            if let Some(mut err) = stderr {
                tokio::io::AsyncReadExt::read_to_end(&mut err, &mut stderr_bytes).await?;
            }
            Ok::<std::process::Output, std::io::Error>(std::process::Output {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            })
        },
    )
    .await
    {
        Ok(result) => result.map_err(|e| format!("Python HTN CLI failed: {}", e))?,
        Err(_) => {
            let _ = child.kill().await;
            return Err("HTN execute attempt timed out".to_string());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("HTN CLI exited with error: {}", stderr);
        return Err(format!("HTN CLI failed: {}", stderr));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_line = stdout_str
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");

    let result: HtnExecutionResult = serde_json::from_str(json_line).map_err(|e| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!(
            "Failed to parse HTN CLI output: {} (stdout: {}, stderr: {})",
            e, stdout_str, stderr,
        )
    })?;

    info!(
        "HTN result: plan_found={}, exec_success={}, actions={}, succeeded={}/{}, replans={}, time={:.1}ms",
        result.plan_found,
        result.execution_success,
        result.plan_actions,
        result.steps_succeeded,
        result.plan_actions,
        result.replans,
        result.total_time_ms,
    );

    Ok(result)
}
