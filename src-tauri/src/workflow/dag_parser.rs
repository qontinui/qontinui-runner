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

        // Rule 8 (Phase 3): tool_policy deny tokens on an Agentic node must be
        // well-formed, so a policy never silently no-ops. A token that contains
        // ':' but has an EMPTY tool part or EMPTY command-prefix part (e.g.
        // "Bash:", ":foo", ":") is malformed and unenforceable as an
        // argument-scoped deny. Well-formed tool-name denies ("Write") and
        // well-formed "Tool:prefix" denies pass.
        if node.effective_kind() == NodeKind::Agentic {
            if let Some(ref policy) = node.tool_policy {
                if let Some(ref denies) = policy.deny {
                    for raw in denies {
                        let tok = raw.trim();
                        if tok.is_empty() {
                            continue;
                        }
                        if let Some((tool, prefix)) = tok.split_once(':') {
                            if tool.trim().is_empty() || prefix.trim().is_empty() {
                                errors.push(DagValidationError {
                                    node_id: Some(node_id.clone()),
                                    message: format!(
                                        "tool_policy deny '{}' is malformed (empty tool or command prefix) and cannot be enforced",
                                        tok
                                    ),
                                });
                            }
                        }
                    }
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

        // Tool-policy inheritance resolves HERE (purely & testably): only
        // Agentic nodes carry a policy. A per-node policy wins; otherwise the
        // workflow's `blueprint_defaults` is the fallback. Deterministic nodes
        // never carry a policy (they never spawn an LLM session).
        let resolved_tool_policy = if node.effective_kind() == NodeKind::Agentic {
            node.tool_policy
                .clone()
                .or_else(|| def.blueprint_defaults.clone())
        } else {
            None
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
            node_kind: Some(node.effective_kind()),
            tool_policy: resolved_tool_policy,
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

#[cfg(test)]
mod node_kind_tests {
    use super::*;
    use serde_json::json;

    fn workflow(value: serde_json::Value) -> DagWorkflowDef {
        serde_json::from_value(value).expect("fixture workflow should deserialize")
    }

    fn config_for<'a>(configs: &'a [ExecutionStepConfig], id: &str) -> &'a ExecutionStepConfig {
        configs
            .iter()
            .find(|c| c.id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no config for node id {id:?}"))
    }

    #[test]
    fn dag_to_step_configs_carries_inferred_node_kinds() {
        let def = workflow(json!({
            "name": "test-wf",
            "nodes": {
                "p": { "prompt": "do the thing" },
                "c": { "command": "echo hi" }
            }
        }));
        let configs = dag_to_step_configs(&def).expect("should build step configs");

        assert_eq!(
            config_for(&configs, "p").node_kind,
            Some(NodeKind::Agentic),
            "prompt node should carry Agentic"
        );
        assert_eq!(
            config_for(&configs, "c").node_kind,
            Some(NodeKind::Deterministic),
            "command node should carry Deterministic"
        );
    }

    // Plan-test (c): a malformed `Bash:` deny on an agentic node fails
    // validate_dag with a named error (no silent no-op).
    #[test]
    fn validate_dag_rejects_malformed_deny_token_on_agentic_node() {
        let def = workflow(json!({
            "name": "test-wf",
            "nodes": {
                "p": {
                    "prompt": "do the thing",
                    "tool_policy": { "deny": ["Bash:"] }
                }
            }
        }));
        let errs = validate_dag(&def).expect_err("malformed deny must fail validation");
        assert!(
            errs.iter().any(|e| e.node_id.as_deref() == Some("p")
                && e.message.contains("malformed")
                && e.message.contains("Bash:")),
            "expected a named malformed-deny error, got {errs:?}"
        );
    }

    #[test]
    fn validate_dag_accepts_well_formed_denies_on_agentic_node() {
        let def = workflow(json!({
            "name": "test-wf",
            "nodes": {
                "p": {
                    "prompt": "do the thing",
                    "tool_policy": {
                        "allow": ["Bash"],
                        "deny": ["Write", "Bash:git push --force"]
                    }
                }
            }
        }));
        assert!(validate_dag(&def).is_ok());
    }

    #[test]
    fn validate_dag_ignores_tool_policy_on_deterministic_node() {
        // A deterministic node carrying a malformed deny is not validated as a
        // tool policy (it would never spawn an LLM session). The discriminator
        // rule still applies — a command node with a tool_policy is fine.
        let def = workflow(json!({
            "name": "test-wf",
            "nodes": {
                "c": {
                    "command": "echo hi",
                    "tool_policy": { "deny": ["Bash:"] }
                }
            }
        }));
        assert!(
            validate_dag(&def).is_ok(),
            "tool_policy on a deterministic node should not trigger Rule 8"
        );
    }

    #[test]
    fn dag_to_step_configs_resolves_tool_policy_per_node_then_blueprint_default() {
        use crate::workflow::dag_schema::ViolationAction;
        let def = workflow(json!({
            "name": "test-wf",
            "blueprint_defaults": { "deny": ["Write"] },
            "nodes": {
                // inherits the workflow blueprint_defaults
                "inherit": { "prompt": "a" },
                // overrides with its own policy
                "own": {
                    "prompt": "b",
                    "tool_policy": { "allow": ["Read"], "on_violation": "fail_node" }
                },
                // deterministic: never carries a policy
                "det": { "command": "echo hi" }
            }
        }));
        let configs = dag_to_step_configs(&def).expect("should build step configs");

        let inherit = config_for(&configs, "inherit")
            .tool_policy
            .as_ref()
            .expect("inherited policy");
        assert_eq!(inherit.deny.as_deref(), Some(&["Write".to_string()][..]));

        let own = config_for(&configs, "own")
            .tool_policy
            .as_ref()
            .expect("own policy");
        assert_eq!(own.allow.as_deref(), Some(&["Read".to_string()][..]));
        assert_eq!(own.on_violation, ViolationAction::FailNode);

        assert!(
            config_for(&configs, "det").tool_policy.is_none(),
            "deterministic node must not carry a tool policy"
        );
    }

    #[test]
    fn dag_to_step_configs_honors_explicit_kind_on_prompt_node() {
        // A prompt node explicitly marked deterministic must carry Some(Deterministic).
        let def = workflow(json!({
            "name": "test-wf",
            "nodes": {
                "p": { "prompt": "do the thing", "kind": "deterministic" }
            }
        }));
        let configs = dag_to_step_configs(&def).expect("should build step configs");

        assert_eq!(
            config_for(&configs, "p").node_kind,
            Some(NodeKind::Deterministic),
            "explicit deterministic kind should override the prompt inference"
        );
    }
}
