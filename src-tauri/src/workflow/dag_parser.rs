use super::dag_schema::*;
use crate::step_executor::executor_types::ExecutionStepConfig;
use std::collections::{HashMap, HashSet};

// ─────────────────────────────────────────────────────────────────────────────
// Error types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DagParseError {
    YamlError(String),
    ValidationErrors(Vec<DagValidationError>),
}

impl std::fmt::Display for DagParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DagParseError::YamlError(msg) => write!(f, "YAML parse error: {}", msg),
            DagParseError::ValidationErrors(errs) => {
                write!(f, "DAG validation errors ({}):", errs.len())?;
                for e in errs {
                    if let Some(ref id) = e.node_id {
                        write!(f, "\n  [{}] {}", id, e.message)?;
                    } else {
                        write!(f, "\n  {}", e.message)?;
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DagValidationError {
    pub node_id: Option<String>,
    pub message: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a YAML string into a [`DagWorkflowDef`].
pub fn parse_dag_workflow(yaml_str: &str) -> Result<DagWorkflowDef, DagParseError> {
    let def: DagWorkflowDef =
        serde_yaml::from_str(yaml_str).map_err(|e| DagParseError::YamlError(e.to_string()))?;

    validate_dag(&def).map_err(DagParseError::ValidationErrors)?;

    Ok(def)
}

/// Validate a parsed [`DagWorkflowDef`].
///
/// Returns `Ok(())` if valid, or a list of all validation errors.
pub fn validate_dag(def: &DagWorkflowDef) -> Result<(), Vec<DagValidationError>> {
    let mut errors: Vec<DagValidationError> = Vec::new();

    // Rule 7: at least one node
    if def.nodes.is_empty() {
        errors.push(DagValidationError {
            node_id: None,
            message: "Workflow must have at least one node".to_string(),
        });
    }

    let known_ids: HashSet<&str> = def.nodes.keys().map(String::as_str).collect();

    for (node_id, node) in &def.nodes {
        // Rule 1: exactly one type-discriminating field
        let discriminators: usize = [
            node.prompt.is_some(),
            node.command.is_some(),
            node.check_type.is_some(),
            node.ui_bridge_action.is_some(),
            node.a11y_action.is_some(),
            node.loop_body.is_some(),
            node.approval.is_some(),
            node.workflow_ref.is_some(),
            node.cancel_reason.is_some(),
        ]
        .iter()
        .filter(|&&b| b)
        .count();

        if discriminators != 1 {
            errors.push(DagValidationError {
                node_id: Some(node_id.clone()),
                message: format!(
                    "Node must have exactly one type-discriminating field set, found {}",
                    discriminators
                ),
            });
        }

        // Rule 2: depends_on references must exist
        for dep in &node.depends_on {
            if !known_ids.contains(dep.as_str()) {
                errors.push(DagValidationError {
                    node_id: Some(node_id.clone()),
                    message: format!("depends_on references unknown node '{}'", dep),
                });
            }
        }

        // Rule 3: when expressions must reference valid node IDs ($nodeId.field)
        if let Some(ref when_expr) = node.when {
            validate_when_refs(node_id, when_expr, &known_ids, &mut errors);
        }

        // Rule 4: loop_body node IDs must exist
        if let Some(ref body) = node.loop_body {
            for body_id in body {
                if !known_ids.contains(body_id.as_str()) {
                    errors.push(DagValidationError {
                        node_id: Some(node_id.clone()),
                        message: format!("loop_body references unknown node '{}'", body_id),
                    });
                }
            }
        }

        // Rule 5: approval.on_reject must reference a valid node ID
        if let Some(ref approval) = node.approval {
            if let Some(ref on_reject) = approval.on_reject {
                if !known_ids.contains(on_reject.as_str()) {
                    errors.push(DagValidationError {
                        node_id: Some(node_id.clone()),
                        message: format!(
                            "approval.on_reject references unknown node '{}'",
                            on_reject
                        ),
                    });
                }
            }
        }
    }

    // Rule 6: no cycles — build a simple adjacency list and check for cycles
    if errors.is_empty() {
        if let Err(cycle_err) = check_for_cycles(&def.nodes) {
            errors.push(DagValidationError {
                node_id: None,
                message: cycle_err,
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Convert DAG nodes to [`ExecutionStepConfig`] for backward-compatible execution.
pub fn dag_to_step_configs(def: &DagWorkflowDef) -> Result<Vec<ExecutionStepConfig>, String> {
    let mut configs: Vec<ExecutionStepConfig> = Vec::with_capacity(def.nodes.len());

    for (node_id, node) in &def.nodes {
        let effective_timeout = node.timeout_seconds.or(def.defaults.timeout_seconds);

        let (retry_count, retry_delay_ms) = node
            .retry
            .as_ref()
            .or(def.defaults.retry.as_ref())
            .map_or((None, None), |r| (Some(r.max_attempts), Some(r.delay_ms)));

        let effective_provider = node
            .context
            .as_ref()
            .and(def.defaults.provider.clone())
            .or_else(|| def.defaults.provider.clone());
        let effective_model = def.defaults.model.clone();

        let node_name = node.name.clone().unwrap_or_else(|| node_id.clone());
        let depends_on = if node.depends_on.is_empty() {
            None
        } else {
            Some(node.depends_on.clone())
        };

        let mut cfg = ExecutionStepConfig {
            id: Some(node_id.clone()),
            name: Some(node_name),
            depends_on,
            inputs: node.inputs.clone(),
            extract: node.extract.clone(),
            timeout_seconds: effective_timeout,
            retry_count,
            retry_delay_ms,
            provider: effective_provider,
            model: effective_model,
            ..ExecutionStepConfig::default()
        };

        // ── Type-discriminating mapping ───────────────────────────────────
        if let Some(ref prompt_text) = node.prompt {
            cfg.step_type = "prompt".to_string();
            cfg.prompt_content = Some(prompt_text.clone());
            cfg.prompt_mode = node.prompt_mode.clone();
        } else if let Some(ref cmd) = node.command {
            cfg.step_type = "command".to_string();
            cfg.command_mode = Some("shell".to_string());
            cfg.shell_command = Some(cmd.clone());
            cfg.shell_command_working_directory = node.working_directory.clone();
            cfg.shell_command_fail_on_error = node.fail_on_error;
        } else if let Some(ref check) = node.check_type {
            cfg.step_type = "command".to_string();
            cfg.command_mode = Some("check".to_string());
            cfg.check_type = Some(check.clone());
            cfg.check_group_id = node.check_group_id.clone();
        } else if let Some(ref action) = node.ui_bridge_action {
            cfg.step_type = "ui_bridge".to_string();
            cfg.ui_bridge_action = Some(action.clone());
            cfg.ui_bridge_url = node.ui_bridge_url.clone();
            cfg.ui_bridge_instruction = node.ui_bridge_instruction.clone();
        } else if let Some(ref action) = node.a11y_action {
            cfg.step_type = "native_accessibility".to_string();
            cfg.a11y_action = Some(action.clone());
            cfg.a11y_target = node.a11y_target.clone();
        } else if let Some(ref wf_ref) = node.workflow_ref {
            cfg.step_type = "workflow".to_string();
            cfg.ref_workflow_id = Some(wf_ref.clone());
            cfg.ref_workflow_inputs = node.workflow_inputs.clone();
        } else if node.loop_body.is_some() {
            // Placeholder — handled by Phase 2 DAG executor
            cfg.step_type = "dag_loop".to_string();
        } else if node.approval.is_some() {
            // Placeholder — handled by Phase 2 DAG executor
            cfg.step_type = "dag_approval".to_string();
        } else if node.cancel_reason.is_some() {
            // Placeholder — handled by Phase 2 DAG executor
            cfg.step_type = "dag_cancel".to_string();
        } else {
            return Err(format!(
                "Node '{}' has no recognised type-discriminating field",
                node_id
            ));
        }

        configs.push(cfg);
    }

    Ok(configs)
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Validate `$nodeId.field` references inside a `when` expression.
fn validate_when_refs(
    node_id: &str,
    when_expr: &str,
    known_ids: &HashSet<&str>,
    errors: &mut Vec<DagValidationError>,
) {
    // Find all tokens that start with '$'
    for token in when_expr.split_whitespace() {
        let token =
            token.trim_matches(|c: char| !c.is_alphanumeric() && c != '$' && c != '_' && c != '.');
        if token.starts_with('$') {
            // Strip leading '$', take first segment before '.'
            let raw = &token[1..];
            let ref_node = raw.split('.').next().unwrap_or("");
            if !ref_node.is_empty() && !known_ids.contains(ref_node) {
                errors.push(DagValidationError {
                    node_id: Some(node_id.to_string()),
                    message: format!(
                        "when expression references unknown node '{}' (in '{}')",
                        ref_node, when_expr
                    ),
                });
            }
        }
    }
}

/// Cycle detection using DFS coloring (white/gray/black).
fn check_for_cycles(nodes: &HashMap<String, DagNodeDef>) -> Result<(), String> {
    // Build adjacency: node_id -> list of dependency node_ids
    // (we traverse in the direction depends_on points)
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, node) in nodes {
        let deps: Vec<&str> = node.depends_on.iter().map(String::as_str).collect();
        adj.insert(id.as_str(), deps);
    }

    // 0 = unvisited, 1 = in-stack, 2 = done
    let mut color: HashMap<&str, u8> = HashMap::new();

    for start in nodes.keys() {
        if color.get(start.as_str()).copied().unwrap_or(0) == 0 {
            if let Err(cycle) = dfs_cycle(start.as_str(), &adj, &mut color) {
                return Err(format!("Cycle detected involving node '{}'", cycle));
            }
        }
    }

    Ok(())
}

fn dfs_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    color: &mut HashMap<&'a str, u8>,
) -> Result<(), String> {
    color.insert(node, 1); // gray — in stack

    if let Some(deps) = adj.get(node) {
        for &dep in deps {
            match color.get(dep).copied().unwrap_or(0) {
                1 => return Err(dep.to_string()), // back-edge → cycle
                0 => dfs_cycle(dep, adj, color)?,
                _ => {}
            }
        }
    }

    color.insert(node, 2); // black — done
    Ok(())
}
