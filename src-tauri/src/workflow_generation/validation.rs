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

    // Validate data flow: inputs, depends_on, and extract references
    let all_steps: Vec<&Value> = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.agentic_steps.iter())
        .chain(workflow.completion_steps.iter())
        .collect();
    validate_step_references(&all_steps, &mut errors);

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
///
/// The 4 core step types are: command, test, ui_bridge, prompt.
pub fn allowed_types_for_phase(phase: &str) -> &'static [&'static str] {
    match phase {
        "setup" => &["command", "prompt", "ui_bridge"],
        "verification" => &["command", "test", "ui_bridge", "prompt"],
        "completion" => &["command", "prompt", "ui_bridge"],
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

/// Validate that `inputs`, `depends_on`, and `extract` references point to valid step IDs.
///
/// Checks:
/// - All step IDs in `depends_on` arrays reference existing step IDs
/// - All step ID references in `inputs` values (via `${step_id.field}` syntax) reference existing step IDs
/// - No circular dependencies exist in `depends_on` chains
pub fn validate_step_references(all_steps: &[&Value], errors: &mut Vec<ValidationError>) {
    // Collect all step IDs
    let step_ids: std::collections::HashSet<String> = all_steps
        .iter()
        .filter_map(|step| step.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    // Build adjacency list for cycle detection
    let mut adjacency: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for step in all_steps {
        let step_id = match step.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let step_name = step
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Validate depends_on references
        if let Some(depends_on) = step.get("depends_on").and_then(|v| v.as_array()) {
            let mut deps = Vec::new();
            for (j, dep) in depends_on.iter().enumerate() {
                if let Some(dep_id) = dep.as_str() {
                    if !step_ids.contains(dep_id) {
                        errors.push(ValidationError {
                            field: format!("step '{}' ({})", step_name, step_id),
                            message: format!(
                                "depends_on[{}] references non-existent step ID: {}",
                                j, dep_id
                            ),
                        });
                    }
                    deps.push(dep_id.to_string());
                }
            }
            adjacency.insert(step_id.clone(), deps);
        }

        // Validate inputs references (look for ${step_id.field} patterns)
        if let Some(inputs) = step.get("inputs").and_then(|v| v.as_object()) {
            for (key, value) in inputs {
                if let Some(val_str) = value.as_str() {
                    // Extract step ID from ${step_id.field} pattern
                    let mut start = 0;
                    while let Some(pos) = val_str[start..].find("${") {
                        let abs_pos = start + pos + 2;
                        if let Some(end) = val_str[abs_pos..].find('}') {
                            let ref_expr = &val_str[abs_pos..abs_pos + end];
                            if let Some(ref_step_id) = ref_expr.split('.').next() {
                                if !ref_step_id.is_empty() && !step_ids.contains(ref_step_id) {
                                    errors.push(ValidationError {
                                        field: format!("step '{}' ({})", step_name, step_id),
                                        message: format!(
                                            "inputs.{} references non-existent step ID: {}",
                                            key, ref_step_id
                                        ),
                                    });
                                }
                            }
                            start = abs_pos + end + 1;
                        } else {
                            break;
                        }
                    }
                }
            }
        }
    }

    // Detect cycles in depends_on
    if let Some(cycle) = detect_cycles(&adjacency) {
        errors.push(ValidationError {
            field: "depends_on".to_string(),
            message: format!(
                "Circular dependency detected in depends_on chain: {}",
                cycle.join(" -> ")
            ),
        });
    }
}

/// Detect cycles in the dependency graph using DFS.
///
/// Returns the cycle path if one is found, or None if the graph is acyclic.
pub fn detect_cycles(
    adjacency: &std::collections::HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    use std::collections::HashSet;

    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();
    let mut stack = Vec::new();

    for node in adjacency.keys() {
        if !visited.contains(node) {
            if let Some(cycle) =
                dfs_detect_cycle(node, adjacency, &mut visited, &mut in_stack, &mut stack)
            {
                return Some(cycle);
            }
        }
    }

    None
}

fn dfs_detect_cycle(
    node: &str,
    adjacency: &std::collections::HashMap<String, Vec<String>>,
    visited: &mut std::collections::HashSet<String>,
    in_stack: &mut std::collections::HashSet<String>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    visited.insert(node.to_string());
    in_stack.insert(node.to_string());
    stack.push(node.to_string());

    if let Some(neighbors) = adjacency.get(node) {
        for neighbor in neighbors {
            if !visited.contains(neighbor.as_str()) {
                if let Some(cycle) = dfs_detect_cycle(neighbor, adjacency, visited, in_stack, stack)
                {
                    return Some(cycle);
                }
            } else if in_stack.contains(neighbor.as_str()) {
                // Found a cycle — extract the cycle path
                let cycle_start = stack.iter().position(|n| n == neighbor).unwrap_or(0);
                let mut cycle: Vec<String> = stack[cycle_start..].to_vec();
                cycle.push(neighbor.clone());
                return Some(cycle);
            }
        }
    }

    stack.pop();
    in_stack.remove(node);
    None
}

/// Check if a workflow is a check-group workflow (all verification steps are command steps with check_type).
///
/// A check-group workflow should have ONLY `command` type steps (with check_type set) in verification,
/// with no setup or completion steps. The AI builder sometimes adds unnecessary setup/completion
/// steps despite instructions — this function detects check-group workflows so we can
/// deterministically strip those extra phases.
fn is_check_group_workflow(workflow: &UnifiedWorkflow) -> bool {
    // Must have at least one verification step
    if workflow.verification_steps.is_empty() {
        return false;
    }

    // Must have only command steps with check_type in verification
    let mut has_check = false;

    for step in &workflow.verification_steps {
        let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let has_check_type = step.get("check_type").is_some();
        match step_type {
            // Legacy "check" type or new "command" type with check_type field
            "check" => has_check = true,
            "command" if has_check_type => has_check = true,
            // Any other step type means this is NOT a pure check-group workflow
            _ => return false,
        }
    }

    has_check
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

    // For check-group workflows, strip setup and completion steps.
    // The AI builder often adds unnecessary setup/completion steps despite generation rules.
    // This deterministic fix ensures check-group workflows are clean.
    if is_check_group_workflow(workflow) {
        if !workflow.setup_steps.is_empty() {
            tracing::info!(
                "Stripping {} setup step(s) from check-group workflow '{}'",
                workflow.setup_steps.len(),
                workflow.name
            );
            workflow.setup_steps.clear();
        }
        if !workflow.completion_steps.is_empty() {
            tracing::info!(
                "Stripping {} completion step(s) from check-group workflow '{}'",
                workflow.completion_steps.len(),
                workflow.name
            );
            workflow.completion_steps.clear();
        }
    }
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
    use serde_json::json;

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
            enable_sweep: false,
            max_sweep_iterations: 5,
            generated_by_task_run_id: None,
            targeted_error_ids: vec![],
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        let errors = validate_workflow(&workflow);
        assert!(errors.iter().any(|e| e.field == "name"));
    }

    #[test]
    fn test_validate_step_references_valid() {
        let steps = vec![
            json!({"id": "step-1", "name": "First", "type": "command", "phase": "setup"}),
            json!({"id": "step-2", "name": "Second", "type": "command", "phase": "setup", "depends_on": ["step-1"]}),
        ];
        let step_refs: Vec<&Value> = steps.iter().collect();
        let mut errors = Vec::new();
        validate_step_references(&step_refs, &mut errors);
        assert!(
            errors.is_empty(),
            "Expected no errors but got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_step_references_invalid_depends_on() {
        let steps = vec![
            json!({"id": "step-1", "name": "First", "type": "command", "phase": "setup"}),
            json!({"id": "step-2", "name": "Second", "type": "command", "phase": "setup", "depends_on": ["nonexistent"]}),
        ];
        let step_refs: Vec<&Value> = steps.iter().collect();
        let mut errors = Vec::new();
        validate_step_references(&step_refs, &mut errors);
        assert!(errors.iter().any(|e| e.message.contains("nonexistent")));
    }

    #[test]
    fn test_validate_step_references_invalid_inputs() {
        let steps = vec![
            json!({"id": "step-1", "name": "First", "type": "command", "phase": "setup"}),
            json!({"id": "step-2", "name": "Second", "type": "command", "phase": "setup", "inputs": {"token": "${bad-id.token}"}}),
        ];
        let step_refs: Vec<&Value> = steps.iter().collect();
        let mut errors = Vec::new();
        validate_step_references(&step_refs, &mut errors);
        assert!(errors.iter().any(|e| e.message.contains("bad-id")));
    }

    #[test]
    fn test_detect_cycles_no_cycle() {
        let mut adjacency = std::collections::HashMap::new();
        adjacency.insert("a".to_string(), vec!["b".to_string()]);
        adjacency.insert("b".to_string(), vec!["c".to_string()]);
        adjacency.insert("c".to_string(), vec![]);
        assert!(detect_cycles(&adjacency).is_none());
    }

    #[test]
    fn test_detect_cycles_with_cycle() {
        let mut adjacency = std::collections::HashMap::new();
        adjacency.insert("a".to_string(), vec!["b".to_string()]);
        adjacency.insert("b".to_string(), vec!["c".to_string()]);
        adjacency.insert("c".to_string(), vec!["a".to_string()]);
        let cycle = detect_cycles(&adjacency);
        assert!(cycle.is_some(), "Expected a cycle to be detected");
    }

    #[test]
    fn test_detect_cycles_self_reference() {
        let mut adjacency = std::collections::HashMap::new();
        adjacency.insert("a".to_string(), vec!["a".to_string()]);
        let cycle = detect_cycles(&adjacency);
        assert!(cycle.is_some(), "Expected self-referencing cycle");
    }

    fn make_check_group_workflow(
        setup: Vec<Value>,
        verification: Vec<Value>,
        completion: Vec<Value>,
    ) -> UnifiedWorkflow {
        UnifiedWorkflow {
            id: Uuid::new_v4().to_string(),
            name: "Test Check Group".to_string(),
            description: "Test".to_string(),
            setup_steps: setup,
            verification_steps: verification,
            agentic_steps: vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "prompt", "phase": "agentic", "name": "Fix issues"}),
            ],
            completion_steps: completion,
            max_iterations: 10,
            timeout_seconds: None,
            provider: None,
            model: None,
            log_source_selection: LogSourceSelection::default(),
            skip_ai_summary: false,
            category: "check-group".to_string(),
            tags: vec![],
            context_ids: vec![],
            disabled_context_ids: vec![],
            auto_include_contexts: true,
            prompt_template: None,
            log_watch_enabled: true,
            health_check_enabled: true,
            health_check_urls: vec![],
            preflight_check_enabled: true,
            enable_sweep: false,
            max_sweep_iterations: 5,
            generated_by_task_run_id: None,
            targeted_error_ids: vec![],
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_is_check_group_workflow_true() {
        let wf = make_check_group_workflow(
            vec![],
            vec![
                json!({"id": "c1", "type": "command", "check_type": "lint", "phase": "verification"}),
                json!({"id": "c2", "type": "command", "check_type": "typecheck", "phase": "verification"}),
            ],
            vec![],
        );
        assert!(is_check_group_workflow(&wf));
    }

    #[test]
    fn test_is_check_group_workflow_false_with_test_step() {
        let wf = make_check_group_workflow(
            vec![],
            vec![
                json!({"id": "c1", "type": "command", "check_type": "lint", "phase": "verification"}),
                json!({"id": "t1", "type": "test", "phase": "verification"}),
            ],
            vec![],
        );
        assert!(!is_check_group_workflow(&wf));
    }

    #[test]
    fn test_fix_workflow_strips_setup_completion_for_check_group() {
        let mut wf = make_check_group_workflow(
            vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "command", "phase": "setup", "name": "Install deps"}),
            ],
            vec![
                json!({"id": "c1", "type": "command", "check_type": "lint", "phase": "verification", "name": "Lint"}),
                json!({"id": "c2", "type": "command", "check_type": "typecheck", "phase": "verification", "name": "Typecheck"}),
            ],
            vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "prompt", "phase": "completion", "name": "Summary"}),
            ],
        );

        assert_eq!(wf.setup_steps.len(), 1);
        assert_eq!(wf.completion_steps.len(), 1);

        fix_workflow(&mut wf);

        assert_eq!(wf.setup_steps.len(), 0);
        assert_eq!(wf.completion_steps.len(), 0);
    }

    #[test]
    fn test_fix_workflow_preserves_setup_for_non_check_group() {
        let mut wf = make_check_group_workflow(
            vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "command", "phase": "setup", "name": "Install deps"}),
            ],
            vec![
                json!({"id": "c1", "type": "command", "check_type": "lint", "phase": "verification", "name": "Lint"}),
                json!({"id": "t1", "type": "test", "phase": "verification", "name": "Unit tests"}),
            ],
            vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "prompt", "phase": "completion", "name": "Summary"}),
            ],
        );

        fix_workflow(&mut wf);

        // Not a check-group (has test step), so setup/completion preserved
        assert_eq!(wf.setup_steps.len(), 1);
        assert_eq!(wf.completion_steps.len(), 1);
    }
}
