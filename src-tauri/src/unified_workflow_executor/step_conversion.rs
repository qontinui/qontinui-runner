//! Step conversion utilities for transforming JSON step definitions into ExecutionStepConfig.
//!
//! This module contains standalone functions for:
//! - Variable substitution in step templates (`{{artifact_dir}}`, `{{execution_id}}`, `{{iteration}}`)
//! - Converting JSON Value arrays to typed `ExecutionStepConfig` vectors
//! - Extracting prompt-type steps separately from automation steps
//! - Phase assignment for steps originating from different workflow arrays

use crate::step_executor::{ExecutionStepConfig, StepPhase};

/// Variables available for substitution in step fields.
pub struct SubstitutionVars {
    pub artifact_dir: Option<String>,
    pub execution_id: String,
    pub iteration: u32,
}

/// Apply variable substitution to a JSON step value.
///
/// Replaces template variables in all string values within the JSON:
/// - `{{artifact_dir}}` -> artifact directory path (forward slashes)
/// - `{{execution_id}}` -> the task run ID
/// - `{{iteration}}` -> current iteration number
pub fn apply_variable_substitution(
    step: &serde_json::Value,
    vars: &SubstitutionVars,
) -> serde_json::Value {
    let mut json_str = serde_json::to_string(step).unwrap_or_default();

    if let Some(ref artifact_dir) = vars.artifact_dir {
        // Use forward slashes on all platforms for consistency
        let normalized = artifact_dir.replace('\\', "/");
        json_str = json_str.replace("{{artifact_dir}}", &normalized);
    }
    json_str = json_str.replace("{{execution_id}}", &vars.execution_id);
    json_str = json_str.replace("{{iteration}}", &vars.iteration.to_string());

    serde_json::from_str(&json_str).unwrap_or_else(|_| step.clone())
}

/// Apply variable substitution to a slice of JSON step values.
pub fn apply_substitution_to_steps(
    steps: &[serde_json::Value],
    vars: &SubstitutionVars,
) -> Vec<serde_json::Value> {
    steps
        .iter()
        .map(|s| apply_variable_substitution(s, vars))
        .collect()
}

/// Apply variable substitution to an ExecutionStepConfig's string fields.
///
/// Replaces `{{artifact_dir}}` and `{{execution_id}}` in all relevant
/// Option<String> fields. This is called after the artifact directory
/// is created but before steps are executed.
pub fn substitute_step_vars(
    step: &mut ExecutionStepConfig,
    artifact_dir: &str,
    execution_id: &str,
) {
    let sub = |s: &mut Option<String>| {
        if let Some(val) = s {
            if val.contains("{{artifact_dir}}") || val.contains("{{execution_id}}") {
                *val = val
                    .replace("{{artifact_dir}}", artifact_dir)
                    .replace("{{execution_id}}", execution_id);
            }
        }
    };

    sub(&mut step.output_path);
    sub(&mut step.input_path);
    sub(&mut step.ai_review_input_path);
    sub(&mut step.shell_command);
    sub(&mut step.shell_command_working_directory);
    sub(&mut step.check_command);
    sub(&mut step.check_working_directory);
    sub(&mut step.artifact_input_path);
    sub(&mut step.fixup_input_path);
    sub(&mut step.fixup_criteria_path);

    // Also substitute in prompt content (may reference artifact paths)
    if let Some(ref mut content) = step.prompt_content {
        if content.contains("{{artifact_dir}}") || content.contains("{{execution_id}}") {
            *content = content
                .replace("{{artifact_dir}}", artifact_dir)
                .replace("{{execution_id}}", execution_id);
        }
    }
}

pub fn convert_json_steps_to_execution_steps(
    steps: &[serde_json::Value],
    monitor: i32,
) -> Vec<ExecutionStepConfig> {
    convert_json_steps_with_phase(steps, monitor, None)
}

/// Convert JSON Value steps to ExecutionStepConfig with explicit phase.
///
/// Sets the explicit phase on all steps that don't already have one.
/// This is the preferred function for unified workflow execution.
pub fn convert_json_steps_with_phase(
    steps: &[serde_json::Value],
    _monitor: i32,
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    steps
        .iter()
        // Filter out prompt steps - they're handled separately to avoid duplicate logging
        .filter(|step| {
            let step_type = step
                .get("type")
                .or_else(|| step.get("step_type"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            !matches!(
                step_type,
                "prompt" | "ai_session" | "ai_prompt" | "run_prompt_sequence"
            )
        })
        .filter_map(|step| {
            let mut config =
                if let Ok(config) = serde_json::from_value::<ExecutionStepConfig>(step.clone()) {
                    config
                } else {
                    // Fall back to manual field extraction — preserve command, working directory,
                    // and other key fields so that check/test steps with inline commands still work
                    let step_type = step
                        .get("type")
                        .or_else(|| step.get("step_type"))
                        .and_then(|t| t.as_str())?;
                    ExecutionStepConfig {
                        step_type: step_type.to_string(),
                        name: step
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        id: step
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string()),
                        shell_command: step
                            .get("command")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        shell_command_working_directory: step
                            .get("working_directory")
                            .and_then(|w| w.as_str())
                            .map(|s| s.to_string()),
                        check_type: step
                            .get("check_type")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        test_type: step
                            .get("test_type")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        test_id: step
                            .get("test_id")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        ..Default::default()
                    }
                };

            // Set explicit phase if not already set
            if config.phase.is_none() {
                if let Some(phase_str) = explicit_phase {
                    if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                        config.set_phase(phase);
                    }
                }
            }

            Some(config)
        })
        .collect()
}

/// Convert ALL JSON steps (including prompt-type) to ExecutionStepConfig with explicit phase.
///
/// Unlike `convert_json_steps_with_phase` which filters out prompt steps,
/// this function preserves all step types in their original order.
/// This is needed for the verification phase where prompt-type steps
/// (AI-evaluated checks) must be included alongside automation steps.
pub fn convert_all_json_steps_with_phase(
    steps: &[serde_json::Value],
    _monitor: i32,
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    steps
        .iter()
        .filter_map(|step| {
            let mut config =
                if let Ok(config) = serde_json::from_value::<ExecutionStepConfig>(step.clone()) {
                    config
                } else {
                    // Fall back to manual field extraction — preserve command, working directory,
                    // and other key fields so that check/test steps with inline commands still work
                    let step_type = step
                        .get("type")
                        .or_else(|| step.get("step_type"))
                        .and_then(|t| t.as_str())?;
                    ExecutionStepConfig {
                        step_type: step_type.to_string(),
                        name: step
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string()),
                        id: step
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string()),
                        shell_command: step
                            .get("command")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        shell_command_working_directory: step
                            .get("working_directory")
                            .and_then(|w| w.as_str())
                            .map(|s| s.to_string()),
                        check_type: step
                            .get("check_type")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string()),
                        test_type: step
                            .get("test_type")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        test_id: step
                            .get("test_id")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        ..Default::default()
                    }
                };

            // Set explicit phase if not already set
            if config.phase.is_none() {
                if let Some(phase_str) = explicit_phase {
                    if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                        config.set_phase(phase);
                    }
                }
            }

            Some(config)
        })
        .collect()
}

/// Extract prompt steps from JSON Value array
///
/// If `explicit_phase` is provided, it will be set on all steps that don't
/// already have a phase specified.
pub fn extract_prompt_steps_from_json(steps: &[serde_json::Value]) -> Vec<ExecutionStepConfig> {
    extract_prompt_steps_with_phase(steps, None)
}

/// Extract prompt steps with explicit phase.
pub fn extract_prompt_steps_with_phase(
    steps: &[serde_json::Value],
    explicit_phase: Option<&str>,
) -> Vec<ExecutionStepConfig> {
    steps
        .iter()
        .filter(|step| {
            step.get("type")
                .or_else(|| step.get("step_type"))
                .and_then(|t| t.as_str())
                .map(|t| matches!(t, "prompt" | "ai_prompt" | "run_prompt_sequence"))
                .unwrap_or(false)
        })
        .filter_map(|step| {
            let mut config = serde_json::from_value::<ExecutionStepConfig>(step.clone()).ok()?;

            // Set explicit phase if not already set
            if config.phase.is_none() {
                if let Some(phase_str) = explicit_phase {
                    if let Some(phase) = StepPhase::from_str_opt(phase_str) {
                        config.set_phase(phase);
                    }
                }
            }

            Some(config)
        })
        .collect()
}
