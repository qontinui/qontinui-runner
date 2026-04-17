//! Workflow Validation
//!
//! Validates generated workflows for structural correctness.

use crate::step_types::StepType;
use crate::unified_workflows::UnifiedWorkflow;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Classification of validation errors for structured error handling.
///
/// The fixer AI uses these kinds to classify errors and choose the right
/// fix strategy without parsing free-text messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationErrorKind {
    /// Unrecognized step type (not command/ui_bridge/prompt)
    InvalidStepType,
    /// Step type not allowed in the phase it's placed in
    InvalidPhase,
    /// Multiple mutually exclusive command mode fields set
    IncompatibleFields,
    /// Required field is missing (e.g., ui_bridge without action)
    MissingRequiredField,
    /// Field value not in valid enum set
    InvalidFieldValue,
    /// depends_on or input references a non-existent step
    InvalidReference,
    /// Structural issues: UUID format, empty name, etc.
    Structural,
}

impl std::fmt::Display for ValidationErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidStepType => write!(f, "invalid_step_type"),
            Self::InvalidPhase => write!(f, "invalid_phase"),
            Self::IncompatibleFields => write!(f, "incompatible_fields"),
            Self::MissingRequiredField => write!(f, "missing_required_field"),
            Self::InvalidFieldValue => write!(f, "invalid_field_value"),
            Self::InvalidReference => write!(f, "invalid_reference"),
            Self::Structural => write!(f, "structural"),
        }
    }
}

/// Validation errors for generated workflows
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    pub kind: ValidationErrorKind,
    pub field: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_name: Option<String>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.kind, self.field, self.message)
    }
}

/// Valid check_type enum values
const VALID_CHECK_TYPES: &[&str] = &[
    "lint",
    "format",
    "typecheck",
    "analyze",
    "security",
    "custom_command",
    "ci_cd",
    "http_status",
    "ai_review",
];

/// Valid test_type enum values
const VALID_TEST_TYPES: &[&str] = &[
    "playwright",
    "qontinui_vision",
    "python",
    "repository",
    "custom_command",
];

/// Valid ui_bridge action enum values.
///
/// This list MUST stay in sync with the action dispatch in
/// `src-tauri/src/step_executor/handlers/ui_bridge.rs`. When drift occurs, a
/// Builder that correctly emits a valid action gets its workflow flagged with
/// validation errors, which zeroes Factor 1 of the confidence score
/// (generator.rs ~line 1965) and the quality gate rejects the workflow with a
/// misleading "Low confidence score (0.00)" message. Dispatch handlers are at
/// ui_bridge.rs:918 (compare), :927 (element_action), :936 (wait_for_element),
/// :945 (wait), :959 (snapshot_assert), :975-1032 (navigate/execute/assert/
/// snapshot/action_plan).
const VALID_UI_BRIDGE_ACTIONS: &[&str] = &[
    "navigate",
    "execute",
    "assert",
    "snapshot",
    "snapshot_assert",
    "element_action",
    "wait_for_element",
    "wait",
    "compare",
    "action_plan",
];

/// Validate a generated workflow and return any errors
pub fn validate_workflow(workflow: &UnifiedWorkflow) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Validate basic fields
    if workflow.name.trim().is_empty() {
        errors.push(ValidationError {
            kind: ValidationErrorKind::Structural,
            field: "name".to_string(),
            message: "Workflow name cannot be empty".to_string(),
            step_name: None,
        });
    }

    // Validate ID is a valid UUID
    if Uuid::parse_str(&workflow.id).is_err() {
        errors.push(ValidationError {
            kind: ValidationErrorKind::Structural,
            field: "id".to_string(),
            message: format!("Invalid UUID format: {}", workflow.id),
            step_name: None,
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

    // Validate command field mutual exclusivity and enum values
    validate_command_step_fields(&workflow.setup_steps, "setup", &mut errors);
    validate_command_step_fields(&workflow.verification_steps, "verification", &mut errors);
    validate_command_step_fields(&workflow.completion_steps, "completion", &mut errors);

    // Validate ui_bridge step fields
    validate_ui_bridge_step_fields(&workflow.setup_steps, "setup", &mut errors);
    validate_ui_bridge_step_fields(&workflow.verification_steps, "verification", &mut errors);
    validate_ui_bridge_step_fields(&workflow.completion_steps, "completion", &mut errors);

    // Validate data flow: inputs, depends_on, and extract references
    let all_steps: Vec<&Value> = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.agentic_steps.iter())
        .chain(workflow.completion_steps.iter())
        .collect();
    validate_step_references(&all_steps, &mut errors);

    // Validate stages (multi-stage workflows)
    for (stage_idx, stage) in workflow.stages.iter().enumerate() {
        let stage_prefix = format!("stages[{}]", stage_idx);

        // Validate step IDs and phases
        validate_steps(
            &stage.setup_steps,
            &format!("{}.setup", stage_prefix),
            &mut errors,
        );
        validate_steps(
            &stage.verification_steps,
            &format!("{}.verification", stage_prefix),
            &mut errors,
        );
        validate_steps(
            &stage.agentic_steps,
            &format!("{}.agentic", stage_prefix),
            &mut errors,
        );
        validate_steps(
            &stage.completion_steps,
            &format!("{}.completion", stage_prefix),
            &mut errors,
        );

        // Validate phase constraints
        validate_phase_constraints(&stage.setup_steps, "setup", &mut errors);
        validate_phase_constraints(&stage.verification_steps, "verification", &mut errors);
        validate_phase_constraints(&stage.agentic_steps, "agentic", &mut errors);
        validate_phase_constraints(&stage.completion_steps, "completion", &mut errors);

        // Validate command fields
        validate_command_step_fields(
            &stage.setup_steps,
            &format!("{}.setup", stage_prefix),
            &mut errors,
        );
        validate_command_step_fields(
            &stage.verification_steps,
            &format!("{}.verification", stage_prefix),
            &mut errors,
        );
        validate_command_step_fields(
            &stage.completion_steps,
            &format!("{}.completion", stage_prefix),
            &mut errors,
        );

        // Validate ui_bridge fields
        validate_ui_bridge_step_fields(
            &stage.setup_steps,
            &format!("{}.setup", stage_prefix),
            &mut errors,
        );
        validate_ui_bridge_step_fields(
            &stage.verification_steps,
            &format!("{}.verification", stage_prefix),
            &mut errors,
        );
        validate_ui_bridge_step_fields(
            &stage.completion_steps,
            &format!("{}.completion", stage_prefix),
            &mut errors,
        );

        // Validate references within this stage
        let stage_all_steps: Vec<&Value> = stage
            .setup_steps
            .iter()
            .chain(stage.verification_steps.iter())
            .chain(stage.agentic_steps.iter())
            .chain(stage.completion_steps.iter())
            .collect();
        validate_step_references(&stage_all_steps, &mut errors);
    }

    // Validate timestamps
    if chrono::DateTime::parse_from_rfc3339(&workflow.created_at).is_err() {
        errors.push(ValidationError {
            kind: ValidationErrorKind::Structural,
            field: "created_at".to_string(),
            message: format!("Invalid ISO 8601 timestamp: {}", workflow.created_at),
            step_name: None,
        });
    }

    if chrono::DateTime::parse_from_rfc3339(&workflow.updated_at).is_err() {
        errors.push(ValidationError {
            kind: ValidationErrorKind::Structural,
            field: "modified_at".to_string(),
            message: format!("Invalid ISO 8601 timestamp: {}", workflow.updated_at),
            step_name: None,
        });
    }

    errors
}

fn step_name_from(step: &Value) -> Option<String> {
    step.get("name").and_then(|v| v.as_str()).map(String::from)
}

fn validate_steps(steps: &[Value], expected_phase: &str, errors: &mut Vec<ValidationError>) {
    // For stages, expected_phase is "stages[N].phase_name" — extract the base phase
    // name for comparison, since steps store plain phase values like "verification".
    let base_phase = expected_phase.rsplit('.').next().unwrap_or(expected_phase);

    for (i, step) in steps.iter().enumerate() {
        let sname = step_name_from(step);

        // Validate step ID
        if let Some(id) = step.get("id").and_then(|v| v.as_str()) {
            if Uuid::parse_str(id).is_err() {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::Structural,
                    field: format!("{}_steps[{}].id", expected_phase, i),
                    message: format!("Invalid UUID format: {}", id),
                    step_name: sname.clone(),
                });
            }
        } else {
            errors.push(ValidationError {
                kind: ValidationErrorKind::MissingRequiredField,
                field: format!("{}_steps[{}].id", expected_phase, i),
                message: "Step missing required 'id' field".to_string(),
                step_name: sname.clone(),
            });
        }

        // Step name is optional — fix_workflow() auto-generates it if missing

        // Validate phase matches array (compare against base phase, not prefixed path)
        if let Some(phase) = step.get("phase").and_then(|v| v.as_str()) {
            if phase != base_phase {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::Structural,
                    field: format!("{}_steps[{}].phase", expected_phase, i),
                    message: format!(
                        "Step phase '{}' doesn't match array '{}'. Phase field must match the array it's in.",
                        phase, base_phase
                    ),
                    step_name: sname.clone(),
                });
            }
        } else {
            errors.push(ValidationError {
                kind: ValidationErrorKind::MissingRequiredField,
                field: format!("{}_steps[{}].phase", expected_phase, i),
                message: "Step missing required 'phase' field".to_string(),
                step_name: sname,
            });
        }
    }
}

/// Returns the step types allowed in a given phase.
///
/// This is the single source of truth for phase constraints, used by both
/// validation and the metadata registry.
///
/// The 3 core step types are: command, ui_bridge, prompt.
pub fn allowed_types_for_phase(phase: &str) -> &'static [&'static str] {
    match phase {
        "setup" => &["command", "prompt", "ui_bridge"],
        "verification" => &["command", "ui_bridge", "prompt"],
        "completion" => &["command", "prompt", "ui_bridge"],
        "agentic" => &["prompt", "ui_bridge"],
        _ => &[],
    }
}

fn validate_phase_constraints(steps: &[Value], phase: &str, errors: &mut Vec<ValidationError>) {
    let allowed_types = allowed_types_for_phase(phase);
    let core_types = ["command", "ui_bridge", "prompt"];

    for (i, step) in steps.iter().enumerate() {
        if let Some(step_type_raw) = step.get("type").and_then(|v| v.as_str()) {
            // Normalize aliases before checking allowed-types. The step executor
            // recognizes many legacy/alias forms ("http" → command, "shell" →
            // command, "uibridge" → ui_bridge, etc.) via StepType::from_str_compat.
            // Without this normalization, the AI Builder legitimately emitting a
            // normalizable alias triggers InvalidStepType, every alias adds a
            // validation error, and 5+ errors zero Factor 1 of the confidence
            // score — rejecting a workflow that would otherwise execute fine.
            let canonical = StepType::from_str_compat(step_type_raw)
                .map(|t| t.as_str())
                .unwrap_or(step_type_raw);

            if !allowed_types.contains(&canonical) {
                let kind = if core_types.contains(&canonical) {
                    ValidationErrorKind::InvalidPhase
                } else {
                    ValidationErrorKind::InvalidStepType
                };
                errors.push(ValidationError {
                    kind,
                    field: format!("{}_steps[{}]", phase, i),
                    message: format!(
                        "Step type '{}' is not allowed in {} phase. Allowed types: {:?}",
                        step_type_raw, phase, allowed_types
                    ),
                    step_name: step_name_from(step),
                });
            }
        }
    }
}

/// Validate command step field mutual exclusivity and enum values.
///
/// For `command` type steps, validates:
/// 1. Only one of {check_type, check_group_id, test_type/test_id} is set (mutual exclusivity)
/// 2. check_type value is in the valid enum set
/// 3. test_type value is in the valid enum set
fn validate_command_step_fields(steps: &[Value], phase: &str, errors: &mut Vec<ValidationError>) {
    for (i, step) in steps.iter().enumerate() {
        let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if step_type != "command" {
            continue;
        }

        let sname = step_name_from(step);
        let field_prefix = format!("{}_steps[{}]", phase, i);

        let has_check_type = step.get("check_type").and_then(|v| v.as_str()).is_some();
        let has_check_group_id = step
            .get("check_group_id")
            .and_then(|v| v.as_str())
            .is_some();
        let has_test_type = step.get("test_type").and_then(|v| v.as_str()).is_some();
        let has_test_id = step.get("test_id").and_then(|v| v.as_str()).is_some();

        // Count how many mode-determining fields are set
        let mode_count = [
            has_check_type,
            has_check_group_id,
            has_test_type || has_test_id,
        ]
        .iter()
        .filter(|&&b| b)
        .count();

        if mode_count > 1 {
            let mut active_modes = Vec::new();
            if has_check_type {
                active_modes.push("check_type");
            }
            if has_check_group_id {
                active_modes.push("check_group_id");
            }
            if has_test_type || has_test_id {
                active_modes.push(if has_test_type {
                    "test_type"
                } else {
                    "test_id"
                });
            }
            errors.push(ValidationError {
                kind: ValidationErrorKind::IncompatibleFields,
                field: field_prefix.clone(),
                message: format!(
                    "Command step has multiple mode fields set: {}. Only one of {{check_type, check_group_id, test_type/test_id}} should be set.",
                    active_modes.join(", ")
                ),
                step_name: sname.clone(),
            });
        }

        // Validate mode is required
        let mode_value = step.get("mode").and_then(|v| v.as_str());
        if mode_value.is_none() {
            errors.push(ValidationError {
                kind: ValidationErrorKind::MissingRequiredField,
                field: format!("{}.mode", field_prefix),
                message: "Command step must have a 'mode' field. Valid values: shell, check, check_group, test".to_string(),
                step_name: sname.clone(),
            });
        }

        // Validate check_type enum
        if let Some(check_type) = step.get("check_type").and_then(|v| v.as_str()) {
            if !VALID_CHECK_TYPES.contains(&check_type) {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::InvalidFieldValue,
                    field: format!("{}.check_type", field_prefix),
                    message: format!(
                        "Invalid check_type '{}'. Valid values: {:?}",
                        check_type, VALID_CHECK_TYPES
                    ),
                    step_name: sname.clone(),
                });
            }
        }

        // Validate test_type enum
        if let Some(test_type) = step.get("test_type").and_then(|v| v.as_str()) {
            if !VALID_TEST_TYPES.contains(&test_type) {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::InvalidFieldValue,
                    field: format!("{}.test_type", field_prefix),
                    message: format!(
                        "Invalid test_type '{}'. Valid values: {:?}",
                        test_type, VALID_TEST_TYPES
                    ),
                    step_name: sname.clone(),
                });
            }
        }

        // Validate mode value and consistency
        if let Some(mode) = mode_value {
            let valid_modes = ["shell", "check", "check_group", "test"];
            if !valid_modes.contains(&mode) {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::InvalidFieldValue,
                    field: format!("{}.mode", field_prefix),
                    message: format!(
                        "Invalid command mode '{}'. Valid values: {:?}",
                        mode, valid_modes
                    ),
                    step_name: sname.clone(),
                });
            } else {
                // Check mode matches the fields that are set
                match mode {
                    "check" if !has_check_type => {
                        errors.push(ValidationError {
                            kind: ValidationErrorKind::IncompatibleFields,
                            field: format!("{}.mode", field_prefix),
                            message: "mode is 'check' but check_type is not set".to_string(),
                            step_name: sname.clone(),
                        });
                    }
                    "check_group" if !has_check_group_id => {
                        errors.push(ValidationError {
                            kind: ValidationErrorKind::IncompatibleFields,
                            field: format!("{}.mode", field_prefix),
                            message: "mode is 'check_group' but check_group_id is not set"
                                .to_string(),
                            step_name: sname.clone(),
                        });
                    }
                    "test" if !has_test_type && !has_test_id => {
                        errors.push(ValidationError {
                            kind: ValidationErrorKind::IncompatibleFields,
                            field: format!("{}.mode", field_prefix),
                            message: "mode is 'test' but neither test_type nor test_id is set"
                                .to_string(),
                            step_name: sname.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Validate ui_bridge step fields.
///
/// For `ui_bridge` type steps, validates:
/// 1. `action` field is required
/// 2. `action` value is in the valid enum set
fn validate_ui_bridge_step_fields(steps: &[Value], phase: &str, errors: &mut Vec<ValidationError>) {
    for (i, step) in steps.iter().enumerate() {
        let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if step_type != "ui_bridge" {
            continue;
        }

        let sname = step_name_from(step);
        let field_prefix = format!("{}_steps[{}]", phase, i);

        if let Some(action) = step.get("action").and_then(|v| v.as_str()) {
            if !VALID_UI_BRIDGE_ACTIONS.contains(&action) {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::InvalidFieldValue,
                    field: format!("{}.action", field_prefix),
                    message: format!(
                        "Invalid ui_bridge action '{}'. Valid values: {:?}",
                        action, VALID_UI_BRIDGE_ACTIONS
                    ),
                    step_name: sname,
                });
            }
        } else {
            errors.push(ValidationError {
                kind: ValidationErrorKind::MissingRequiredField,
                field: format!("{}.action", field_prefix),
                message: "ui_bridge step must have an 'action' field".to_string(),
                step_name: sname,
            });
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
        let sname = step_name_from(step);
        let name_display = sname.as_deref().unwrap_or("unknown");

        // Validate depends_on references
        if let Some(depends_on) = step.get("depends_on").and_then(|v| v.as_array()) {
            let mut deps = Vec::new();
            for (j, dep) in depends_on.iter().enumerate() {
                if let Some(dep_id) = dep.as_str() {
                    if !step_ids.contains(dep_id) {
                        errors.push(ValidationError {
                            kind: ValidationErrorKind::InvalidReference,
                            field: format!("step '{}' ({})", name_display, step_id),
                            message: format!(
                                "depends_on[{}] references non-existent step ID: {}",
                                j, dep_id
                            ),
                            step_name: sname.clone(),
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
                                        kind: ValidationErrorKind::InvalidReference,
                                        field: format!("step '{}' ({})", name_display, step_id),
                                        message: format!(
                                            "inputs.{} references non-existent step ID: {}",
                                            key, ref_step_id
                                        ),
                                        step_name: sname.clone(),
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
            kind: ValidationErrorKind::InvalidReference,
            field: "depends_on".to_string(),
            message: format!(
                "Circular dependency detected in depends_on chain: {}",
                cycle.join(" -> ")
            ),
            step_name: None,
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
            "command" if has_check_type => has_check = true,
            // Any other step type means this is NOT a pure check-group workflow
            _ => return false,
        }
    }

    has_check
}

/// Fix common issues in generated workflows
pub fn fix_workflow(workflow: &mut UnifiedWorkflow) {
    // Always assign a proper v4 UUID for the workflow ID
    if Uuid::parse_str(&workflow.id)
        .map(|u| u.get_version_num() != 4)
        .unwrap_or(true)
    {
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

    // Collect all steps, assign proper v4 UUIDs, fix phases and step types,
    // and update depends_on references across the entire workflow.
    let all_steps: Vec<&mut Vec<Value>> = vec![
        &mut workflow.setup_steps,
        &mut workflow.verification_steps,
        &mut workflow.agentic_steps,
        &mut workflow.completion_steps,
    ];
    let phases = ["setup", "verification", "agentic", "completion"];

    // Pass 1: Build old_id -> new_id mapping and replace IDs, fix phases and step types
    let mut id_map = std::collections::HashMap::new();
    for (steps, phase) in all_steps.iter().zip(phases.iter()) {
        let allowed_types = allowed_types_for_phase(phase);
        for step in steps.iter() {
            if let Value::Object(map) = step {
                if let Some(old_id) = map.get("id").and_then(|v| v.as_str()) {
                    let needs_new = Uuid::parse_str(old_id)
                        .map(|u| u.get_version_num() != 4)
                        .unwrap_or(true);
                    if needs_new {
                        id_map.insert(old_id.to_string(), Uuid::new_v4().to_string());
                    }
                }
            }
        }
        // Fix step types in this phase
        for step in steps.iter() {
            if let Value::Object(map) = step {
                let step_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if !step_type.is_empty() && !allowed_types.contains(&step_type) {
                    // Will be fixed in pass 2 (needs mutable access)
                }
            }
        }
    }

    // Pass 1b: Collect the set of known step IDs (after remapping) so we
    // can drop orphaned depends_on references in pass 2. The AI Builder
    // occasionally emits `depends_on: ["foo"]` where step `foo` was never
    // actually defined — those references become InvalidReference errors
    // that accumulate and zero Factor 1 of the confidence score. Dropping
    // unresolvable references lets the workflow still execute (unordered)
    // rather than being rejected wholesale.
    let known_ids: std::collections::HashSet<String> = {
        let mut set = std::collections::HashSet::new();
        for steps in [
            &workflow.setup_steps,
            &workflow.verification_steps,
            &workflow.agentic_steps,
            &workflow.completion_steps,
        ] {
            for step in steps.iter() {
                if let Some(id) = step.get("id").and_then(|v| v.as_str()) {
                    // Use the post-remap id when available
                    let effective = id_map.get(id).cloned().unwrap_or_else(|| id.to_string());
                    set.insert(effective);
                }
            }
        }
        set
    };

    // Pass 2: Apply ID replacements, fix phases, fix step types (needs re-borrow)
    let all_steps_mut: Vec<&mut Vec<Value>> = vec![
        &mut workflow.setup_steps,
        &mut workflow.verification_steps,
        &mut workflow.agentic_steps,
        &mut workflow.completion_steps,
    ];
    for (steps, phase) in all_steps_mut.into_iter().zip(phases.iter()) {
        let allowed_types = allowed_types_for_phase(phase);
        let default_type = if *phase == "agentic" {
            "prompt"
        } else {
            "command"
        };
        for step in steps.iter_mut() {
            if let Value::Object(map) = step {
                // Replace ID if it was in the mapping
                if let Some(old_id) = map.get("id").and_then(|v| v.as_str()).map(String::from) {
                    if let Some(new_id) = id_map.get(&old_id) {
                        map.insert("id".to_string(), Value::String(new_id.clone()));
                    }
                } else {
                    // Missing ID — assign a new one
                    map.insert("id".to_string(), Value::String(Uuid::new_v4().to_string()));
                }

                // Fix phase mismatch
                if map.get("phase").and_then(|v| v.as_str()) != Some(phase) {
                    map.insert("phase".to_string(), Value::String(phase.to_string()));
                }

                // Fix invalid step types
                if let Some(step_type) = map.get("type").and_then(|v| v.as_str()).map(String::from)
                {
                    if !allowed_types.contains(&step_type.as_str()) {
                        tracing::info!(
                            "Fixing invalid step type '{}' -> '{}' in {} phase",
                            step_type,
                            default_type,
                            phase
                        );
                        map.insert("type".to_string(), Value::String(default_type.to_string()));
                    }
                }

                // Auto-generate name for steps missing it
                let has_name = map
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                if !has_name {
                    let step_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("step");
                    let generated_name =
                        if let Some(prompt_text) = map.get("prompt").and_then(|v| v.as_str()) {
                            // Use first meaningful words of prompt
                            let words: Vec<&str> = prompt_text
                                .split_whitespace()
                                .filter(|w| w.len() > 1)
                                .take(6)
                                .collect();
                            if words.is_empty() {
                                format!("{} step ({})", phase, step_type)
                            } else {
                                words.join(" ")
                            }
                        } else if let Some(cmd) = map.get("command").and_then(|v| v.as_str()) {
                            // Use command basename
                            let base = cmd.split_whitespace().next().unwrap_or(cmd);
                            let base = base.rsplit('/').next().unwrap_or(base);
                            format!("Run {}", base)
                        } else {
                            format!("{} step ({})", phase, step_type)
                        };
                    tracing::info!(
                        "Auto-generating name '{}' for unnamed step in {} phase",
                        generated_name,
                        phase
                    );
                    map.insert("name".to_string(), Value::String(generated_name));
                }

                // Infer and set mode on command steps that are missing it
                let is_command = map.get("type").and_then(|v| v.as_str()) == Some("command");
                if is_command && !map.contains_key("mode") {
                    let inferred_mode = if map.contains_key("check_group_id") {
                        "check_group"
                    } else if map.contains_key("check_type") {
                        "check"
                    } else if map.contains_key("test_type") || map.contains_key("test_id") {
                        "test"
                    } else {
                        "shell"
                    };
                    tracing::info!(
                        "Inferring mode '{}' for command step in {} phase",
                        inferred_mode,
                        phase
                    );
                    map.insert("mode".to_string(), Value::String(inferred_mode.to_string()));
                }

                // Update depends_on references, remapping old ids and
                // dropping orphans that reference steps that don't exist.
                if let Some(Value::Array(deps)) = map.get_mut("depends_on") {
                    let mut kept: Vec<Value> = Vec::with_capacity(deps.len());
                    for dep in deps.iter() {
                        let Some(old_dep) = dep.as_str() else {
                            continue;
                        };
                        let remapped = id_map
                            .get(old_dep)
                            .cloned()
                            .unwrap_or_else(|| old_dep.to_string());
                        if known_ids.contains(&remapped) {
                            kept.push(Value::String(remapped));
                        } else {
                            tracing::info!(
                                "Dropping orphaned depends_on '{}' (step not found in workflow)",
                                old_dep
                            );
                        }
                    }
                    *deps = kept;
                }
            }
        }
    }

    // Fix stages (multi-stage workflows)
    for stage in workflow.stages.iter_mut() {
        let stage_steps: Vec<&mut Vec<Value>> = vec![
            &mut stage.setup_steps,
            &mut stage.verification_steps,
            &mut stage.agentic_steps,
            &mut stage.completion_steps,
        ];

        // Pass 1: Build ID map for this stage
        let mut stage_id_map = std::collections::HashMap::new();
        for steps in stage_steps.iter() {
            for step in steps.iter() {
                if let Value::Object(map) = step {
                    if let Some(old_id) = map.get("id").and_then(|v| v.as_str()) {
                        let needs_new = Uuid::parse_str(old_id)
                            .map(|u| u.get_version_num() != 4)
                            .unwrap_or(true);
                        if needs_new {
                            stage_id_map.insert(old_id.to_string(), Uuid::new_v4().to_string());
                        }
                    }
                }
            }
        }

        // Pass 2: Apply fixes (needs re-borrow)
        let stage_steps_mut: Vec<(&mut Vec<Value>, &str)> = vec![
            (&mut stage.setup_steps, "setup"),
            (&mut stage.verification_steps, "verification"),
            (&mut stage.agentic_steps, "agentic"),
            (&mut stage.completion_steps, "completion"),
        ];
        for (steps, phase) in stage_steps_mut {
            let allowed_types = allowed_types_for_phase(phase);
            let default_type = if phase == "agentic" {
                "prompt"
            } else {
                "command"
            };
            for step in steps.iter_mut() {
                if let Value::Object(map) = step {
                    // Replace ID if in mapping
                    if let Some(old_id) = map.get("id").and_then(|v| v.as_str()).map(String::from) {
                        if let Some(new_id) = stage_id_map.get(&old_id) {
                            map.insert("id".to_string(), Value::String(new_id.clone()));
                        }
                    } else {
                        map.insert("id".to_string(), Value::String(Uuid::new_v4().to_string()));
                    }

                    // Fix phase mismatch
                    if map.get("phase").and_then(|v| v.as_str()) != Some(phase) {
                        map.insert("phase".to_string(), Value::String(phase.to_string()));
                    }

                    // Fix invalid step types
                    if let Some(step_type) =
                        map.get("type").and_then(|v| v.as_str()).map(String::from)
                    {
                        if !allowed_types.contains(&step_type.as_str()) {
                            tracing::info!(
                                "Fixing invalid step type '{}' -> '{}' in stage {} phase",
                                step_type,
                                default_type,
                                phase
                            );
                            map.insert("type".to_string(), Value::String(default_type.to_string()));
                        }
                    }

                    // Auto-generate name for steps missing it (stages)
                    let has_name = map
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);
                    if !has_name {
                        let step_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("step");
                        let generated_name =
                            if let Some(prompt_text) = map.get("prompt").and_then(|v| v.as_str()) {
                                let words: Vec<&str> = prompt_text
                                    .split_whitespace()
                                    .filter(|w| w.len() > 1)
                                    .take(6)
                                    .collect();
                                if words.is_empty() {
                                    format!("{} step ({})", phase, step_type)
                                } else {
                                    words.join(" ")
                                }
                            } else if let Some(cmd) = map.get("command").and_then(|v| v.as_str()) {
                                let base = cmd.split_whitespace().next().unwrap_or(cmd);
                                let base = base.rsplit('/').next().unwrap_or(base);
                                format!("Run {}", base)
                            } else {
                                format!("{} step ({})", phase, step_type)
                            };
                        tracing::info!(
                            "Auto-generating name '{}' for unnamed step in stage {} phase",
                            generated_name,
                            phase
                        );
                        map.insert("name".to_string(), Value::String(generated_name));
                    }

                    // Infer and set mode on command steps that are missing it
                    let is_command = map.get("type").and_then(|v| v.as_str()) == Some("command");
                    if is_command && !map.contains_key("mode") {
                        let inferred_mode = if map.contains_key("check_group_id") {
                            "check_group"
                        } else if map.contains_key("check_type") {
                            "check"
                        } else if map.contains_key("test_type") || map.contains_key("test_id") {
                            "test"
                        } else {
                            "shell"
                        };
                        tracing::info!(
                            "Inferring mode '{}' for command step in stage {} phase",
                            inferred_mode,
                            phase
                        );
                        map.insert("mode".to_string(), Value::String(inferred_mode.to_string()));
                    }

                    // Update depends_on references
                    if let Some(Value::Array(deps)) = map.get_mut("depends_on") {
                        for dep in deps.iter_mut() {
                            if let Some(old_dep) = dep.as_str().map(String::from) {
                                if let Some(new_dep) = stage_id_map.get(&old_dep) {
                                    *dep = Value::String(new_dep.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Strip AI-hallucinated model/provider from generated workflows and stages.
    // The AI builder sometimes sets model names like "claude-sonnet-4" or "claude-opus-4"
    // which are not valid CLI model identifiers. The runner should use the user's
    // configured model/provider settings, not AI-generated ones.
    if workflow.model.is_some() {
        tracing::info!(
            "Stripping AI-generated model '{}' from workflow '{}' — runner will use user's configured model",
            workflow.model.as_deref().unwrap_or("?"),
            workflow.name
        );
        workflow.model = None;
    }
    if workflow.provider.is_some() {
        tracing::info!(
            "Stripping AI-generated provider '{}' from workflow '{}' — runner will use user's configured provider",
            workflow.provider.as_deref().unwrap_or("?"),
            workflow.name
        );
        workflow.provider = None;
    }
    for stage in workflow.stages.iter_mut() {
        if stage.model.is_some() {
            tracing::info!(
                "Stripping AI-generated model '{}' from stage '{}' — runner will use user's configured model",
                stage.model.as_deref().unwrap_or("?"),
                stage.name
            );
            stage.model = None;
        }
        if stage.provider.is_some() {
            tracing::info!(
                "Stripping AI-generated provider '{}' from stage '{}' — runner will use user's configured provider",
                stage.provider.as_deref().unwrap_or("?"),
                stage.name
            );
            stage.provider = None;
        }
    }

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

/// Validate that acceptance criteria are covered by verification steps.
///
/// Checks:
/// - Each automatable criterion has a matching verification step (by `criterion_id`)
/// - No verification step has an orphan `criterion_id` that doesn't match any criterion
///
/// Critical criteria missing coverage produce errors; important/optional produce warnings.
pub fn validate_criteria_coverage(
    workflow: &UnifiedWorkflow,
    criteria: &super::specification::AcceptanceCriteria,
) -> Vec<ValidationError> {
    use super::specification::{CriterionPriority, VerificationMethod};

    let mut errors = Vec::new();

    // Collect all criterion_ids from verification steps (top-level + stages)
    let mut found_criterion_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    let all_verification_steps: Vec<&Value> = workflow
        .verification_steps
        .iter()
        .chain(
            workflow
                .stages
                .iter()
                .flat_map(|s| s.verification_steps.iter()),
        )
        .collect();

    for step in &all_verification_steps {
        // Support single criterion_id
        if let Some(cid) = step.get("criterion_id").and_then(|v| v.as_str()) {
            found_criterion_ids.insert(cid.to_string());
        }
        // Support criterion_ids array
        if let Some(cids) = step.get("criterion_ids").and_then(|v| v.as_array()) {
            for cid in cids {
                if let Some(cid_str) = cid.as_str() {
                    found_criterion_ids.insert(cid_str.to_string());
                }
            }
        }
    }

    // Check each automatable criterion has a matching verification step
    let automatable: Vec<&super::specification::AcceptanceCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .collect();

    for criterion in &automatable {
        if !found_criterion_ids.contains(&criterion.id) {
            let (kind, severity_label) = match criterion.priority {
                CriterionPriority::Critical => {
                    (ValidationErrorKind::MissingRequiredField, "CRITICAL")
                }
                CriterionPriority::Important => (ValidationErrorKind::Structural, "important"),
                CriterionPriority::Optional => (ValidationErrorKind::Structural, "optional"),
            };
            errors.push(ValidationError {
                kind,
                field: format!("criterion_id:{}", criterion.id),
                message: format!(
                    "{} acceptance criterion '{}' ({}) has no matching verification step with criterion_id=\"{}\"",
                    severity_label, criterion.description, criterion.category, criterion.id
                ),
                step_name: None,
            });
        }
    }

    // Check for orphan criterion_ids on verification steps
    let valid_ids: std::collections::HashSet<&str> =
        criteria.criteria.iter().map(|c| c.id.as_str()).collect();

    for step in &all_verification_steps {
        if let Some(cid) = step.get("criterion_id").and_then(|v| v.as_str()) {
            if !valid_ids.contains(cid) {
                let step_name = step
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                errors.push(ValidationError {
                    kind: ValidationErrorKind::InvalidReference,
                    field: "criterion_id".to_string(),
                    message: format!(
                        "Verification step '{}' has criterion_id=\"{}\" which doesn't match any acceptance criterion",
                        step_name, cid
                    ),
                    step_name: Some(step_name),
                });
            }
        }
    }

    errors
}

/// Validate data contracts between stages.
///
/// For stages with `inputs`, checks that each required input has a matching
/// `output` from a prior stage. Missing outputs → Critical, unconsumed outputs → Info.
pub fn validate_data_contracts(workflow: &UnifiedWorkflow) -> Vec<super::revision::QualityFinding> {
    use super::revision::{FindingCategory, FindingSeverity, QualityFinding};
    use std::collections::HashSet;

    let mut findings = Vec::new();

    if workflow.stages.len() < 2 {
        return findings;
    }

    // Collect all outputs declared by each stage
    let mut available_outputs: HashSet<String> = HashSet::new();

    for (i, stage) in workflow.stages.iter().enumerate() {
        // Check inputs against available outputs
        if let Some(ref inputs) = stage.inputs {
            for input in inputs {
                if !available_outputs.contains(&input.key) {
                    let severity = if input.required {
                        FindingSeverity::Critical
                    } else {
                        FindingSeverity::Info
                    };
                    findings.push(QualityFinding {
                        finding_id: format!("data-contract-missing-{}-{}", i, input.key),
                        step_id: Some(stage.id.clone()),
                        severity,
                        category: FindingCategory::DataContractViolation,
                        description: format!(
                            "Stage '{}' requires input '{}' but no prior stage declares it as an output",
                            stage.name, input.key
                        ),
                        suggested_fix: Some(format!(
                            "Add an output with key '{}' to a stage that runs before '{}'",
                            input.key, stage.name
                        )),
                    });
                }
            }
        }

        // Register this stage's outputs for subsequent stages
        if let Some(ref outputs) = stage.outputs {
            for output in outputs {
                available_outputs.insert(output.key.clone());
            }
        }
    }

    // Check for unconsumed outputs (Info level)
    let consumed_keys: HashSet<String> = workflow
        .stages
        .iter()
        .filter_map(|s| s.inputs.as_ref())
        .flat_map(|inputs| inputs.iter().map(|i| i.key.clone()))
        .collect();

    for stage in &workflow.stages {
        if let Some(ref outputs) = stage.outputs {
            for output in outputs {
                if !consumed_keys.contains(&output.key) {
                    findings.push(QualityFinding {
                        finding_id: format!("data-contract-unconsumed-{}", output.key),
                        step_id: Some(stage.id.clone()),
                        severity: FindingSeverity::Info,
                        category: FindingCategory::DataContractViolation,
                        description: format!(
                            "Stage '{}' declares output '{}' but no downstream stage consumes it",
                            stage.name, output.key
                        ),
                        suggested_fix: None,
                    });
                }
            }
        }
    }

    findings
}

/// Validate required-flag discipline.
///
/// If a verification step is linked to a Critical acceptance criterion but has
/// `required: false`, that's a Critical finding.
pub fn validate_required_flag_discipline(
    workflow: &UnifiedWorkflow,
    criteria: Option<&super::specification::AcceptanceCriteria>,
) -> Vec<super::revision::QualityFinding> {
    use super::revision::{FindingCategory, FindingSeverity, QualityFinding};
    use super::specification::CriterionPriority;

    let mut findings = Vec::new();

    let criteria = match criteria {
        Some(c) => c,
        None => return findings,
    };

    // Build set of critical criterion IDs
    let critical_ids: std::collections::HashSet<&str> = criteria
        .criteria
        .iter()
        .filter(|c| c.priority == CriterionPriority::Critical)
        .map(|c| c.id.as_str())
        .collect();

    if critical_ids.is_empty() {
        return findings;
    }

    // Check all verification steps
    let all_verification_steps: Vec<&serde_json::Value> = workflow
        .verification_steps
        .iter()
        .chain(
            workflow
                .stages
                .iter()
                .flat_map(|s| s.verification_steps.iter()),
        )
        .collect();

    for step in all_verification_steps {
        let step_required = step
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(true); // default is true

        if step_required {
            continue; // already required, no issue
        }

        // Check if this step is linked to a critical criterion
        let mut linked_critical = false;

        if let Some(cid) = step.get("criterion_id").and_then(|v| v.as_str()) {
            if critical_ids.contains(cid) {
                linked_critical = true;
            }
        }

        if let Some(cids) = step.get("criterion_ids").and_then(|v| v.as_array()) {
            for cid in cids {
                if let Some(cid_str) = cid.as_str() {
                    if critical_ids.contains(cid_str) {
                        linked_critical = true;
                        break;
                    }
                }
            }
        }

        if linked_critical {
            let step_name = step
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let step_id = step.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");

            findings.push(QualityFinding {
                finding_id: format!("required-flag-{}", step_id),
                step_id: Some(step_id.to_string()),
                severity: FindingSeverity::Critical,
                category: FindingCategory::RequiredFlagViolation,
                description: format!(
                    "Verification step '{}' is linked to a Critical criterion but has required=false",
                    step_name
                ),
                suggested_fix: Some("Set required=true on this step".to_string()),
            });
        }
    }

    findings
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
            max_iterations: Some(10),
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
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
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: false,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            model_overrides: std::collections::HashMap::new(),
            flow_control_json: None,
            phase_timeouts_json: None,
            security_profile: None,
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
            max_iterations: Some(10),
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
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
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: false,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            model_overrides: std::collections::HashMap::new(),
            flow_control_json: None,
            phase_timeouts_json: None,
            security_profile: None,
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
    fn test_is_check_group_workflow_false_with_non_check_command() {
        let wf = make_check_group_workflow(
            vec![],
            vec![
                json!({"id": "c1", "type": "command", "check_type": "lint", "phase": "verification"}),
                json!({"id": "t1", "type": "command", "mode": "test", "test_type": "playwright", "phase": "verification"}),
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
                json!({"id": "c1", "type": "command", "check_type": "lint", "mode": "check", "phase": "verification", "name": "Lint"}),
                json!({"id": "t1", "type": "command", "mode": "test", "test_type": "playwright", "phase": "verification", "name": "Unit tests"}),
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

    #[test]
    fn test_fix_workflow_replaces_non_v4_uuids() {
        // AI-generated UUIDs that parse but aren't v4
        let fake_id_a = "a1b2c3d4-e5f6-1a7b-0c9d-0e1f2a3b4c5d"; // version 1, variant 0
        let fake_id_b = "b2c3d4e5-f6a7-2b8c-0d0e-1f2a3b4c5d6e"; // version 2, variant 0
        assert!(
            Uuid::parse_str(fake_id_a).is_ok(),
            "should parse as valid UUID format"
        );

        let mut wf = UnifiedWorkflow {
            id: Uuid::new_v4().to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            setup_steps: vec![
                json!({"id": fake_id_a, "type": "command", "phase": "setup", "name": "Step A"}),
            ],
            verification_steps: vec![
                json!({"id": fake_id_b, "type": "command", "phase": "verification", "name": "Step B", "depends_on": [fake_id_a]}),
            ],
            agentic_steps: vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "prompt", "phase": "agentic", "name": "Agent"}),
            ],
            completion_steps: vec![],
            max_iterations: Some(3),
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
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
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: false,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            model_overrides: std::collections::HashMap::new(),
            flow_control_json: None,
            phase_timeouts_json: None,
            security_profile: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        fix_workflow(&mut wf);

        // IDs should be replaced with proper v4 UUIDs
        let new_id_a = wf.setup_steps[0].get("id").unwrap().as_str().unwrap();
        let new_id_b = wf.verification_steps[0]
            .get("id")
            .unwrap()
            .as_str()
            .unwrap();
        assert_ne!(new_id_a, fake_id_a, "non-v4 UUID should be replaced");
        assert_ne!(new_id_b, fake_id_b, "non-v4 UUID should be replaced");
        assert_eq!(Uuid::parse_str(new_id_a).unwrap().get_version_num(), 4);
        assert_eq!(Uuid::parse_str(new_id_b).unwrap().get_version_num(), 4);

        // depends_on references should be updated to match the new ID
        let deps = wf.verification_steps[0]
            .get("depends_on")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(
            deps[0].as_str().unwrap(),
            new_id_a,
            "depends_on should reference new ID"
        );

        // Already-valid v4 UUIDs should be preserved
        let agentic_id = wf.agentic_steps[0].get("id").unwrap().as_str().unwrap();
        assert_eq!(Uuid::parse_str(agentic_id).unwrap().get_version_num(), 4);
    }

    #[test]
    fn test_fix_workflow_fixes_invalid_step_types() {
        let mut wf = UnifiedWorkflow {
            id: Uuid::new_v4().to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            setup_steps: vec![],
            verification_steps: vec![],
            agentic_steps: vec![],
            completion_steps: vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "save_workflow_artifact", "phase": "completion", "name": "Save"}),
            ],
            max_iterations: Some(3),
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
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
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: false,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            model_overrides: std::collections::HashMap::new(),
            flow_control_json: None,
            phase_timeouts_json: None,
            security_profile: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        fix_workflow(&mut wf);

        let step_type = wf.completion_steps[0]
            .get("type")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(
            step_type, "command",
            "invalid step type should be fixed to 'command'"
        );
    }

    // ====================================================================
    // Phase 2: Command field mutual exclusivity tests
    // ====================================================================

    #[test]
    fn test_command_mutual_exclusivity_check_and_test() {
        let mut errors = Vec::new();
        let steps = vec![json!({
            "type": "command",
            "name": "Bad step",
            "id": Uuid::new_v4().to_string(),
            "phase": "verification",
            "mode": "check",
            "check_type": "lint",
            "test_type": "playwright"
        })];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::IncompatibleFields
                    && e.message.contains("check_type")
                    && e.message.contains("test_type")),
            "Expected incompatible fields error for check_type + test_type, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_command_mutual_exclusivity_check_group_and_check() {
        let mut errors = Vec::new();
        let steps = vec![json!({
            "type": "command",
            "name": "Bad step",
            "id": Uuid::new_v4().to_string(),
            "phase": "verification",
            "mode": "check",
            "check_type": "lint",
            "check_group_id": "some-group-id"
        })];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::IncompatibleFields),
            "Expected incompatible fields error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_command_single_mode_is_valid() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "command", "name": "Check", "id": Uuid::new_v4().to_string(), "phase": "verification", "mode": "check", "check_type": "lint"}),
            json!({"type": "command", "name": "Test", "id": Uuid::new_v4().to_string(), "phase": "verification", "mode": "test", "test_type": "playwright"}),
            json!({"type": "command", "name": "Shell", "id": Uuid::new_v4().to_string(), "phase": "setup", "mode": "shell", "command": "echo ok"}),
            json!({"type": "command", "name": "Group", "id": Uuid::new_v4().to_string(), "phase": "verification", "mode": "check_group", "check_group_id": "gid"}),
        ];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors.is_empty(),
            "Expected no errors but got: {:?}",
            errors
        );
    }

    #[test]
    fn test_command_missing_mode() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "command", "name": "No mode", "id": Uuid::new_v4().to_string(), "phase": "setup", "command": "echo ok"}),
        ];
        validate_command_step_fields(&steps, "setup", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::MissingRequiredField
                    && e.field.contains("mode")),
            "Expected missing mode error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_command_invalid_check_type() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "command", "name": "Bad check", "id": Uuid::new_v4().to_string(), "phase": "verification", "mode": "check", "check_type": "banana"}),
        ];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::InvalidFieldValue
                    && e.message.contains("banana")),
            "Expected invalid check_type error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_command_invalid_test_type() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "command", "name": "Bad test", "id": Uuid::new_v4().to_string(), "phase": "verification", "mode": "test", "test_type": "cucumber"}),
        ];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::InvalidFieldValue
                    && e.message.contains("cucumber")),
            "Expected invalid test_type error, got: {:?}",
            errors
        );
    }

    // ====================================================================
    // ui_bridge validation tests
    // ====================================================================

    #[test]
    fn test_ui_bridge_missing_action() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "ui_bridge", "name": "No action", "id": Uuid::new_v4().to_string(), "phase": "verification"}),
        ];
        validate_ui_bridge_step_fields(&steps, "verification", &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::MissingRequiredField);
        assert!(errors[0].message.contains("action"));
    }

    #[test]
    fn test_ui_bridge_invalid_action() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "ui_bridge", "name": "Bad action", "id": Uuid::new_v4().to_string(), "phase": "verification", "action": "destroy"}),
        ];
        validate_ui_bridge_step_fields(&steps, "verification", &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::InvalidFieldValue);
        assert!(errors[0].message.contains("destroy"));
    }

    #[test]
    fn test_ui_bridge_valid_actions() {
        let mut errors = Vec::new();
        let steps: Vec<Value> = VALID_UI_BRIDGE_ACTIONS
            .iter()
            .enumerate()
            .map(|(i, action)| {
                json!({
                    "type": "ui_bridge",
                    "name": format!("Action {}", i),
                    "id": Uuid::new_v4().to_string(),
                    "phase": "verification",
                    "action": action
                })
            })
            .collect();
        validate_ui_bridge_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors.is_empty(),
            "Expected no errors but got: {:?}",
            errors
        );
    }

    // ====================================================================
    // Phase 3: Structured validation error kind tests
    // ====================================================================

    #[test]
    fn test_error_kind_display() {
        assert_eq!(
            ValidationErrorKind::InvalidStepType.to_string(),
            "invalid_step_type"
        );
        assert_eq!(
            ValidationErrorKind::IncompatibleFields.to_string(),
            "incompatible_fields"
        );
        assert_eq!(
            ValidationErrorKind::MissingRequiredField.to_string(),
            "missing_required_field"
        );
    }

    #[test]
    fn test_error_display_format() {
        let err = ValidationError {
            kind: ValidationErrorKind::IncompatibleFields,
            field: "verification_steps[0]".to_string(),
            message: "check_type and test_type both set".to_string(),
            step_name: Some("My Step".to_string()),
        };
        let display = err.to_string();
        assert!(display.contains("[incompatible_fields]"));
        assert!(display.contains("verification_steps[0]"));
        assert!(display.contains("check_type and test_type both set"));
    }

    #[test]
    fn test_error_serialization() {
        let err = ValidationError {
            kind: ValidationErrorKind::InvalidFieldValue,
            field: "setup_steps[0].check_type".to_string(),
            message: "Invalid check_type 'banana'".to_string(),
            step_name: Some("Bad Check".to_string()),
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "invalid_field_value");
        assert_eq!(json["field"], "setup_steps[0].check_type");
        assert_eq!(json["step_name"], "Bad Check");
    }

    #[test]
    fn test_phase_constraint_error_kind_invalid_type() {
        let mut errors = Vec::new();
        let steps = vec![
            json!({"type": "api_request", "name": "Legacy", "id": Uuid::new_v4().to_string(), "phase": "setup"}),
        ];
        validate_phase_constraints(&steps, "setup", &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::InvalidStepType);
    }

    #[test]
    fn test_phase_constraint_error_kind_wrong_phase() {
        let mut errors = Vec::new();
        // prompt is valid but not in agentic for... wait, it is. Let me use ui_bridge in agentic.
        let steps = vec![
            json!({"type": "ui_bridge", "name": "Not allowed", "id": Uuid::new_v4().to_string(), "phase": "agentic", "action": "snapshot"}),
        ];
        validate_phase_constraints(&steps, "agentic", &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::InvalidPhase);
    }

    #[test]
    fn test_mode_validation_check_without_check_type() {
        let mut errors = Vec::new();
        let steps = vec![json!({
            "type": "command",
            "name": "Mode mismatch",
            "id": Uuid::new_v4().to_string(),
            "phase": "verification",
            "mode": "check",
            "command": "echo hi"
        })];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::IncompatibleFields
                    && e.message.contains("check_type is not set")),
            "Expected incompatible fields error for check without check_type, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_mode_validation_invalid_mode_value() {
        let mut errors = Vec::new();
        let steps = vec![json!({
            "type": "command",
            "name": "Bad mode",
            "id": Uuid::new_v4().to_string(),
            "phase": "verification",
            "mode": "turbo"
        })];
        validate_command_step_fields(&steps, "verification", &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ValidationErrorKind::InvalidFieldValue
                    && e.message.contains("turbo")),
            "Expected invalid mode value error, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_fix_workflow_infers_mode_on_command_steps() {
        let mut wf = UnifiedWorkflow {
            id: Uuid::new_v4().to_string(),
            name: "Test".to_string(),
            description: "Test".to_string(),
            setup_steps: vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "command", "phase": "setup", "name": "Shell", "command": "echo ok"}),
            ],
            verification_steps: vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "command", "phase": "verification", "name": "Check", "check_type": "lint"}),
                json!({"id": Uuid::new_v4().to_string(), "type": "command", "phase": "verification", "name": "Test", "test_type": "playwright"}),
                json!({"id": Uuid::new_v4().to_string(), "type": "command", "phase": "verification", "name": "Group", "check_group_id": "gid"}),
            ],
            agentic_steps: vec![
                json!({"id": Uuid::new_v4().to_string(), "type": "prompt", "phase": "agentic", "name": "Fix"}),
            ],
            completion_steps: vec![],
            max_iterations: Some(3),
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
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
            stages: Vec::new(),
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: false,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            model_overrides: std::collections::HashMap::new(),
            flow_control_json: None,
            phase_timeouts_json: None,
            security_profile: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        fix_workflow(&mut wf);

        // Shell command should get mode "shell"
        assert_eq!(
            wf.setup_steps[0].get("mode").and_then(|v| v.as_str()),
            Some("shell")
        );
        // Check should get mode "check"
        assert_eq!(
            wf.verification_steps[0]
                .get("mode")
                .and_then(|v| v.as_str()),
            Some("check")
        );
        // Test should get mode "test"
        assert_eq!(
            wf.verification_steps[1]
                .get("mode")
                .and_then(|v| v.as_str()),
            Some("test")
        );
        // Check group should get mode "check_group"
        assert_eq!(
            wf.verification_steps[2]
                .get("mode")
                .and_then(|v| v.as_str()),
            Some("check_group")
        );
    }
}
