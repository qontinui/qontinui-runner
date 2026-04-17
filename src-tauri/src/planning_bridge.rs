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
    /// Maximum time for the entire HTN attempt (planning + execution) in ms.
    pub timeout_ms: u64,
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
    /// Optional path to a directory of HTN method JSON files.
    /// When provided, methods are loaded alongside the built-in generic methods.
    pub methods_directory: Option<String>,
}

impl Default for HtnConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Off by default until method library matures
            timeout_ms: 15000,
            max_replans: 5,
            python_path: None,
            ui_bridge_url: None,
            target_type: None,
            state_machine_path: None,
            methods_directory: None,
        }
    }
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
        "methods_directory": config.methods_directory,
        "timeout_ms": config.timeout_ms,
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
        std::time::Duration::from_millis(config.timeout_ms),
        async {
            let stdout_handle = child.stdout.take();
            let stderr_handle = child.stderr.take();

            // Read stdout and stderr concurrently BEFORE waiting,
            // to avoid deadlock when pipe buffers fill up.
            let (stdout_result, stderr_result) = tokio::join!(
                async {
                    let mut bytes = Vec::new();
                    if let Some(mut out) = stdout_handle {
                        tokio::io::AsyncReadExt::read_to_end(&mut out, &mut bytes).await.ok();
                    }
                    bytes
                },
                async {
                    let mut bytes = Vec::new();
                    if let Some(mut err) = stderr_handle {
                        tokio::io::AsyncReadExt::read_to_end(&mut err, &mut bytes).await.ok();
                    }
                    bytes
                },
            );

            // Now wait for the child to finish
            let status = child.wait().await?;

            Ok::<std::process::Output, std::io::Error>(std::process::Output {
                status,
                stdout: stdout_result,
                stderr: stderr_result,
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
