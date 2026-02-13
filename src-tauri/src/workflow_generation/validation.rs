//! Workflow Validation
//!
//! Validates generated workflows for structural correctness.

use crate::unified_workflows::UnifiedWorkflow;
use serde_json::Value;
use uuid::Uuid;

/// Validation errors for generated workflows
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Validate a generated workflow and return any errors
pub fn validate_workflow(workflow: &UnifiedWorkflow) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Validate basic fields
    if workflow.name.trim().is_empty() {
        errors.push(ValidationError {
            field: "name".to_string(),
            message: "Workflow name cannot be empty".to_string(),
        });
    }

    // Validate ID is a valid UUID
    if Uuid::parse_str(&workflow.id).is_err() {
        errors.push(ValidationError {
            field: "id".to_string(),
            message: format!("Invalid UUID format: {}", workflow.id),
        });
    }

    // Validate step IDs and phases
    validate_steps(&workflow.setup_steps, "setup", &mut errors);
    validate_steps(&workflow.verification_steps, "verification", &mut errors);
    validate_steps(&workflow.agentic_steps, "agentic", &mut errors);
    validate_steps(&workflow.completion_steps, "completion", &mut errors);

    // Validate phase constraints for all steps (including agentic = prompt-only)
    validate_phase_constraints(&workflow.setup_steps, "setup", &mut errors);
    validate_phase_constraints(&workflow.verification_steps, "verification", &mut errors);
    validate_phase_constraints(&workflow.agentic_steps, "agentic", &mut errors);
    validate_phase_constraints(&workflow.completion_steps, "completion", &mut errors);

    // Validate timestamps
    if chrono::DateTime::parse_from_rfc3339(&workflow.created_at).is_err() {
        errors.push(ValidationError {
            field: "created_at".to_string(),
            message: format!("Invalid ISO 8601 timestamp: {}", workflow.created_at),
        });
    }

    if chrono::DateTime::parse_from_rfc3339(&workflow.updated_at).is_err() {
        errors.push(ValidationError {
            field: "modified_at".to_string(),
            message: format!("Invalid ISO 8601 timestamp: {}", workflow.updated_at),
        });
    }

    errors
}

fn validate_steps(steps: &[Value], expected_phase: &str, errors: &mut Vec<ValidationError>) {
    for (i, step) in steps.iter().enumerate() {
        // Validate step ID
        if let Some(id) = step.get("id").and_then(|v| v.as_str()) {
            if Uuid::parse_str(id).is_err() {
                errors.push(ValidationError {
                    field: format!("{}_steps[{}].id", expected_phase, i),
                    message: format!("Invalid UUID format: {}", id),
                });
            }
        } else {
            errors.push(ValidationError {
                field: format!("{}_steps[{}].id", expected_phase, i),
                message: "Step missing required 'id' field".to_string(),
            });
        }

        // Validate step name
        if let Some(name) = step.get("name").and_then(|v| v.as_str()) {
            if name.trim().is_empty() {
                errors.push(ValidationError {
                    field: format!("{}_steps[{}].name", expected_phase, i),
                    message: "Step name cannot be empty".to_string(),
                });
            }
        } else {
            errors.push(ValidationError {
                field: format!("{}_steps[{}].name", expected_phase, i),
                message: "Step missing required 'name' field".to_string(),
            });
        }

        // Validate phase matches array
        if let Some(phase) = step.get("phase").and_then(|v| v.as_str()) {
            if phase != expected_phase {
                errors.push(ValidationError {
                    field: format!("{}_steps[{}].phase", expected_phase, i),
                    message: format!(
                        "Step phase '{}' doesn't match array '{}'. Phase field must match the array it's in.",
                        phase, expected_phase
                    ),
                });
            }
        } else {
            errors.push(ValidationError {
                field: format!("{}_steps[{}].phase", expected_phase, i),
                message: "Step missing required 'phase' field".to_string(),
            });
        }
    }
}

/// Returns the step types allowed in a given phase.
///
/// This is the single source of truth for phase constraints, used by both
/// validation and the metadata registry.
pub fn allowed_types_for_phase(phase: &str) -> &'static [&'static str] {
    match phase {
        "setup" => &[
            "script",
            "state",
            "workflow_ref",
            "gui_action",
            "api_request",
            "mcp_call",
            "prompt",
            "shell_command",
            "check_group",
            "macro",
            "awas_discover",
            "awas_execute",
            "awas_check_support",
            "awas_list_actions",
        ],
        "verification" => &[
            "test",
            "check",
            "screenshot",
            "gui_action",
            "state",
            "workflow_ref",
            "api_request",
            "mcp_call",
            "prompt",
            "spec",
            "gate",
            "check_group",
            "macro",
            "awas_execute",
            "awas_list_actions",
            "awas_extract_elements",
        ],
        "completion" => &[
            "prompt",
            "script",
            "api_request",
            "mcp_call",
            "shell_command",
            "check_group",
            "macro",
        ],
        "agentic" => &["prompt"],
        _ => &[],
    }
}

fn validate_phase_constraints(steps: &[Value], phase: &str, errors: &mut Vec<ValidationError>) {
    let allowed_types = allowed_types_for_phase(phase);

    for (i, step) in steps.iter().enumerate() {
        if let Some(step_type) = step.get("type").and_then(|v| v.as_str()) {
            if !allowed_types.contains(&step_type) {
                errors.push(ValidationError {
                    field: format!("{}_steps[{}]", phase, i),
                    message: format!(
                        "Step type '{}' is not allowed in {} phase. Allowed types: {:?}",
                        step_type, phase, allowed_types
                    ),
                });
            }
        }
    }
}

/// Fix common issues in generated workflows
pub fn fix_workflow(workflow: &mut UnifiedWorkflow) {
    // Fix invalid UUIDs
    if Uuid::parse_str(&workflow.id).is_err() {
        workflow.id = Uuid::new_v4().to_string();
    }

    // Fix timestamps
    let now = chrono::Utc::now().to_rfc3339();
    if chrono::DateTime::parse_from_rfc3339(&workflow.created_at).is_err() {
        workflow.created_at = now.clone();
    }
    if chrono::DateTime::parse_from_rfc3339(&workflow.updated_at).is_err() {
        workflow.updated_at = now;
    }

    // Fix step IDs and phases
    fix_step_ids_and_phases(&mut workflow.setup_steps, "setup");
    fix_step_ids_and_phases(&mut workflow.verification_steps, "verification");
    fix_step_ids_and_phases(&mut workflow.agentic_steps, "agentic");
    fix_step_ids_and_phases(&mut workflow.completion_steps, "completion");
}

fn fix_step_ids_and_phases(steps: &mut [Value], phase: &str) {
    for step in steps.iter_mut() {
        if let Value::Object(map) = step {
            // Fix invalid UUIDs
            let needs_new_id = map
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| Uuid::parse_str(id).is_err())
                .unwrap_or(true);

            if needs_new_id {
                map.insert("id".to_string(), Value::String(Uuid::new_v4().to_string()));
            }

            // Fix phase mismatch
            let current_phase = map.get("phase").and_then(|v| v.as_str());
            if current_phase != Some(phase) {
                map.insert("phase".to_string(), Value::String(phase.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unified_workflows::LogSourceSelection;

    #[test]
    fn test_validate_empty_name() {
        let workflow = UnifiedWorkflow {
            id: Uuid::new_v4().to_string(),
            name: "".to_string(),
            description: "Test".to_string(),
            setup_steps: vec![],
            verification_steps: vec![],
            agentic_steps: vec![],
            completion_steps: vec![],
            max_iterations: 10,
            timeout_seconds: None,
            provider: None,
            model: None,
            log_source_selection: LogSourceSelection::default(),
            skip_ai_summary: false,
            category: "test".to_string(),
            tags: vec![],
            context_ids: vec![],
            disabled_context_ids: vec![],
            auto_include_contexts: true,
            prompt_template: None,
            log_watch_enabled: true,
            health_check_enabled: true,
            health_check_urls: vec![],
            preflight_check_enabled: true,
            generated_by_task_run_id: None,
            targeted_error_ids: vec![],
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let errors = validate_workflow(&workflow);
        assert!(errors.iter().any(|e| e.field == "name"));
    }
}
