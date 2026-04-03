//! Launch workflows via the Restate ingress API.
//!
//! Instead of spawning a workflow directly via `spawn_workflow_with_panic_guard`,
//! this module sends the workflow input to the Restate server, which then
//! calls back into the runner's service endpoint to execute it durably.

use std::time::Duration;
use tracing::{error, info};

use crate::step_executor::ExecutionStepConfig;
use super::config::RestateSettings;
use super::types::WorkflowInput;

/// Launch a workflow execution via the Restate ingress API.
///
/// The Restate server will call back into the runner's HTTP service endpoint
/// (QontinuiWorkflow::run handler) to execute the workflow durably.
///
/// The `execution_id` is used as the Restate workflow key, ensuring exactly-once
/// execution semantics — resubmitting with the same ID is idempotent.
pub async fn launch_workflow_via_restate(
    execution_id: &str,
    input: &WorkflowInput,
    ingress_url: &str,
) -> Result<(), String> {
    let url = format!(
        "{}/QontinuiWorkflow/{}/run/send",
        ingress_url, execution_id
    );

    info!(
        "Launching workflow via Restate: execution_id={}, url={}",
        execution_id, url
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(input)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Failed to send workflow to Restate: {}", e))?;

    if response.status().is_success() {
        info!(
            "Workflow {} submitted to Restate successfully",
            execution_id
        );
        Ok(())
    } else {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        error!(
            "Restate workflow submission failed ({}): {}",
            status, body
        );
        Err(format!(
            "Restate workflow submission failed ({}): {}",
            status, body
        ))
    }
}

/// Request to stop a running Restate workflow.
///
/// Sends a signal to the workflow's shared `request_stop` handler,
/// which sets a flag that the `run()` handler checks between iterations.
pub async fn request_workflow_stop(
    execution_id: &str,
    ingress_url: &str,
) -> Result<(), String> {
    let url = format!(
        "{}/QontinuiWorkflow/{}/request_stop/send",
        ingress_url, execution_id
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&serde_json::json!(null))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to send stop request to Restate: {}", e))?;

    if response.status().is_success() {
        info!("Stop request sent for workflow {}", execution_id);
        Ok(())
    } else {
        let status = response.status();
        Err(format!(
            "Restate stop request failed ({})",
            status
        ))
    }
}

/// Resolve an awakeable (e.g., approval gate response).
///
/// Sends the resolution to Restate's awakeable resolve endpoint,
/// which resumes the suspended workflow invocation.
pub async fn resolve_awakeable(
    awakeable_id: &str,
    payload: &serde_json::Value,
    ingress_url: &str,
) -> Result<(), String> {
    let url = format!(
        "{}/restate/awakeables/{}/resolve",
        ingress_url, awakeable_id
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(payload)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to resolve awakeable: {}", e))?;

    if response.status().is_success() {
        info!("Awakeable {} resolved", awakeable_id);
        Ok(())
    } else {
        let status = response.status();
        Err(format!(
            "Awakeable resolution failed ({})",
            status
        ))
    }
}

/// Check if Restate durable execution should be used for this workflow launch.
///
/// Returns `true` if:
/// 1. Restate is enabled in settings
/// 2. Restate server is healthy (admin port responding)
///
/// If `true`, the caller should use `launch_workflow_via_restate` instead of
/// `spawn_workflow_with_panic_guard`.
pub async fn should_use_restate(settings: &RestateSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    super::lifecycle::is_restate_healthy(settings.admin_port).await
}

/// Build a `WorkflowInput` from the serializable components.
///
/// This helper serializes the step configs and loop config into JSON strings
/// for transport across the Restate boundary.
pub fn build_workflow_input(
    loop_config_json: &str,
    setup_automation_steps: &[ExecutionStepConfig],
    setup_prompt_steps: &[ExecutionStepConfig],
    verification_steps: &[ExecutionStepConfig],
    agentic_steps: &[ExecutionStepConfig],
    completion_automation_steps: &[ExecutionStepConfig],
    completion_prompt_steps: &[ExecutionStepConfig],
) -> Result<WorkflowInput, String> {
    Ok(WorkflowInput {
        loop_config_json: loop_config_json.to_string(),
        setup_automation_steps_json: serde_json::to_string(setup_automation_steps)
            .map_err(|e| format!("Failed to serialize setup automation steps: {}", e))?,
        setup_prompt_steps_json: serde_json::to_string(setup_prompt_steps)
            .map_err(|e| format!("Failed to serialize setup prompt steps: {}", e))?,
        verification_steps_json: serde_json::to_string(verification_steps)
            .map_err(|e| format!("Failed to serialize verification steps: {}", e))?,
        agentic_steps_json: serde_json::to_string(agentic_steps)
            .map_err(|e| format!("Failed to serialize agentic steps: {}", e))?,
        completion_automation_steps_json: serde_json::to_string(completion_automation_steps)
            .map_err(|e| format!("Failed to serialize completion automation steps: {}", e))?,
        completion_prompt_steps_json: serde_json::to_string(completion_prompt_steps)
            .map_err(|e| format!("Failed to serialize completion prompt steps: {}", e))?,
    })
}

/// Extract a minimal JSON representation of the essential LoopConfig fields
/// for Restate transport. LoopConfig doesn't implement Serialize (too many
/// non-serializable runtime fields), so we extract only what's needed.
pub fn loop_config_to_json(
    max_iterations: u32,
    execution_id: &str,
    workflow_name: &str,
    workflow_id: &str,
    base_prompt: &str,
) -> String {
    serde_json::json!({
        "max_iterations": max_iterations,
        "execution_id": execution_id,
        "workflow_name": workflow_name,
        "workflow_id": workflow_id,
        "base_prompt": base_prompt,
    })
    .to_string()
}

/// Build a complete `WorkflowInput` from a LoopConfig by extracting steps
/// from stages. Most workflows store all steps inside stages (the direct
/// step parameters to `LoopController::run()` are `Vec::new()`).
///
/// For multi-stage workflows, all stages' steps are concatenated.
pub fn build_workflow_input_from_loop_config(
    loop_config: &crate::unified_workflow_executor::types::LoopConfig,
) -> Result<WorkflowInput, String> {
    let config_json = loop_config_to_json(
        loop_config.max_iterations,
        &loop_config.execution_id,
        &loop_config.workflow_name,
        &loop_config.workflow_id,
        &loop_config.base_prompt,
    );

    // Collect steps from all stages
    let mut setup_auto = Vec::new();
    let mut setup_prompt = Vec::new();
    let mut verification = Vec::new();
    let mut agentic = Vec::new();
    let mut completion_auto = Vec::new();
    let mut completion_prompt = Vec::new();

    for stage in &loop_config.stages {
        setup_auto.extend(stage.setup_automation_steps.iter().cloned());
        setup_prompt.extend(stage.setup_prompt_steps.iter().cloned());
        verification.extend(stage.verification_steps.iter().cloned());
        agentic.extend(stage.agentic_steps.iter().cloned());
        completion_auto.extend(stage.completion_automation_steps.iter().cloned());
        completion_prompt.extend(stage.completion_prompt_steps.iter().cloned());
    }

    build_workflow_input(
        &config_json,
        &setup_auto,
        &setup_prompt,
        &verification,
        &agentic,
        &completion_auto,
        &completion_prompt,
    )
}
