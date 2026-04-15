//! DAG context utilities — connects fresh_context and sub_workflow
//! modules to the DAG execution pipeline.

use serde_json::Value;
use std::collections::HashMap;

use crate::workflow::dag_schema::{ContextMode, DagNodeDef};
use crate::workflow::fresh_context::{build_fresh_context_prompt, IterationDiffSummary};
use crate::workflow::sub_workflow::validate_sub_workflow;
use crate::workflow::variable_engine::VariableStore;

/// Check if a node requires fresh context and build the appropriate prompt.
///
/// Returns `Some(fresh_prompt)` if the node has `context: fresh`,
/// or `None` if it should use shared context (default).
pub fn resolve_node_context(
    node: &DagNodeDef,
    base_prompt: &str,
    iteration: u32,
    variables: &VariableStore,
    verification_failures: &[String],
    iteration_diffs: &[IterationDiffSummary],
) -> Option<String> {
    match &node.context {
        Some(ContextMode::Fresh) => {
            // Convert VariableStore outputs to a simple HashMap<String, Value>
            // for the fresh context builder
            let var_map = variables_as_map(variables, node);
            Some(build_fresh_context_prompt(
                base_prompt,
                iteration,
                &var_map,
                verification_failures,
                iteration_diffs,
            ))
        }
        _ => None, // Shared context or not specified — use default
    }
}

/// Validate that a workflow_ref node can be executed at the given depth.
///
/// Returns Ok(()) if the sub-workflow invocation is allowed,
/// or Err with a human-readable error message.
pub fn validate_node_sub_workflow(node: &DagNodeDef, current_depth: u32) -> Result<(), String> {
    if let Some(ref wf_ref) = node.workflow_ref {
        validate_sub_workflow(wf_ref, current_depth, None).map_err(|e| format!("{}", e))
    } else {
        Ok(()) // Not a workflow_ref node
    }
}

/// Extract a simplified variable map from the VariableStore for fresh context.
///
/// Only includes variables from nodes that this node depends on (via inputs).
fn variables_as_map(variables: &VariableStore, node: &DagNodeDef) -> HashMap<String, Value> {
    let mut map = HashMap::new();

    // If the node has explicit inputs, resolve those
    if let Some(ref inputs) = node.inputs {
        for (name, reference) in inputs {
            if let Some(val) = variables.resolve_reference(reference) {
                map.insert(name.clone(), val);
            }
        }
    }

    // Also include outputs from depends_on nodes
    for dep_id in &node.depends_on {
        if let Some(output) = variables.get_output(dep_id) {
            map.insert(dep_id.clone(), output.clone());
        }
    }

    map
}

/// Context for a single node execution, including any fresh context prompt.
#[derive(Debug, Clone)]
pub struct NodeExecutionContext {
    /// If set, use this prompt instead of the shared session.
    pub fresh_prompt: Option<String>,
    /// Current sub-workflow depth for recursion checking.
    pub sub_workflow_depth: u32,
    /// Whether this node is a sub-workflow invocation.
    pub is_sub_workflow: bool,
}

impl NodeExecutionContext {
    /// Build context for a node.
    pub fn for_node(
        node: &DagNodeDef,
        base_prompt: &str,
        iteration: u32,
        variables: &VariableStore,
        verification_failures: &[String],
        iteration_diffs: &[IterationDiffSummary],
        current_depth: u32,
    ) -> Result<Self, String> {
        // Validate sub-workflow if applicable
        validate_node_sub_workflow(node, current_depth)?;

        let fresh_prompt = resolve_node_context(
            node,
            base_prompt,
            iteration,
            variables,
            verification_failures,
            iteration_diffs,
        );

        Ok(Self {
            fresh_prompt,
            sub_workflow_depth: current_depth,
            is_sub_workflow: node.workflow_ref.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::dag_schema::{ContextMode, DagNodeDef, TriggerRule};
    use crate::workflow::sub_workflow::MAX_SUB_WORKFLOW_DEPTH;
    use serde_json::json;

    /// Construct a minimal DagNodeDef with sensible defaults.
    fn make_node() -> DagNodeDef {
        DagNodeDef {
            name: None,
            depends_on: vec![],
            when: None,
            trigger_rule: TriggerRule::AllSuccess,
            retry: None,
            timeout_seconds: None,
            context: None,
            prompt: Some("Do the thing.".to_string()),
            prompt_mode: None,
            command: None,
            working_directory: None,
            fail_on_error: None,
            check_type: None,
            check_group_id: None,
            ui_bridge_action: None,
            ui_bridge_url: None,
            ui_bridge_instruction: None,
            a11y_action: None,
            a11y_target: None,
            loop_body: None,
            until: None,
            until_bash: None,
            max_loop_iterations: None,
            commit_interval: None,
            approval: None,
            workflow_ref: None,
            workflow_inputs: None,
            cancel_reason: None,
            inputs: None,
            extract: None,
        }
    }

    // ── resolve_node_context ─────────────────────────────────────────────────

    #[test]
    fn test_resolve_node_context_none_for_default() {
        let node = make_node(); // context: None
        let store = VariableStore::new();
        let result = resolve_node_context(&node, "Base prompt.", 1, &store, &[], &[]);
        assert!(result.is_none(), "default context should return None");
    }

    #[test]
    fn test_resolve_node_context_none_for_shared() {
        let mut node = make_node();
        node.context = Some(ContextMode::Shared);
        let store = VariableStore::new();
        let result = resolve_node_context(&node, "Base prompt.", 1, &store, &[], &[]);
        assert!(result.is_none(), "shared context should return None");
    }

    #[test]
    fn test_resolve_node_context_some_for_fresh_empty_vars() {
        let mut node = make_node();
        node.context = Some(ContextMode::Fresh);
        let store = VariableStore::new();
        let result = resolve_node_context(&node, "My base.", 3, &store, &[], &[]);
        assert!(result.is_some(), "fresh context must return Some");
        let prompt = result.unwrap();
        assert!(prompt.contains("Iteration 3"));
        assert!(prompt.contains("My base."));
    }

    #[test]
    fn test_resolve_node_context_fresh_with_populated_variables() {
        let mut node = make_node();
        node.context = Some(ContextMode::Fresh);
        node.depends_on = vec!["step_a".to_string()];

        let mut store = VariableStore::new();
        store.set_output("step_a", json!({"score": 77}));

        let result = resolve_node_context(&node, "Check score.", 2, &store, &[], &[]);
        assert!(result.is_some());
        let prompt = result.unwrap();
        // The dep output should be included in the context
        assert!(prompt.contains("step_a"));
        assert!(prompt.contains("Iteration 2"));
        assert!(prompt.contains("Check score."));
    }

    #[test]
    fn test_resolve_node_context_fresh_with_explicit_inputs() {
        let mut node = make_node();
        node.context = Some(ContextMode::Fresh);

        let mut inputs = HashMap::new();
        inputs.insert("my_score".to_string(), "$nodeA.score".to_string());
        node.inputs = Some(inputs);

        let mut store = VariableStore::new();
        store.set_output("nodeA", json!({"score": 99}));

        let result = resolve_node_context(&node, "Use score.", 1, &store, &[], &[]);
        assert!(result.is_some());
        let prompt = result.unwrap();
        assert!(prompt.contains("my_score"));
    }

    // ── validate_node_sub_workflow ───────────────────────────────────────────

    #[test]
    fn test_validate_node_sub_workflow_passes_for_non_workflow_ref() {
        let node = make_node(); // workflow_ref: None
        assert!(validate_node_sub_workflow(&node, 5).is_ok());
    }

    #[test]
    fn test_validate_node_sub_workflow_passes_at_normal_depth() {
        let mut node = make_node();
        node.workflow_ref = Some("child.yaml".to_string());
        assert!(validate_node_sub_workflow(&node, 3).is_ok());
    }

    #[test]
    fn test_validate_node_sub_workflow_fails_at_max_depth() {
        let mut node = make_node();
        node.workflow_ref = Some("child.yaml".to_string());
        let result = validate_node_sub_workflow(&node, MAX_SUB_WORKFLOW_DEPTH);
        assert!(result.is_err(), "should fail at max depth");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("exceeds maximum"),
            "error should mention max depth: {}",
            msg
        );
    }

    // ── NodeExecutionContext::for_node ───────────────────────────────────────

    #[test]
    fn test_node_execution_context_regular_node_succeeds() {
        let node = make_node();
        let store = VariableStore::new();
        let ctx = NodeExecutionContext::for_node(&node, "Do work.", 1, &store, &[], &[], 0);
        assert!(ctx.is_ok());
        let ctx = ctx.unwrap();
        assert!(
            ctx.fresh_prompt.is_none(),
            "default context → no fresh prompt"
        );
        assert!(!ctx.is_sub_workflow);
        assert_eq!(ctx.sub_workflow_depth, 0);
    }

    #[test]
    fn test_node_execution_context_fresh_context_node() {
        let mut node = make_node();
        node.context = Some(ContextMode::Fresh);
        let store = VariableStore::new();
        let ctx = NodeExecutionContext::for_node(&node, "Fresh run.", 5, &store, &[], &[], 2);
        assert!(ctx.is_ok());
        let ctx = ctx.unwrap();
        assert!(ctx.fresh_prompt.is_some());
        assert_eq!(ctx.sub_workflow_depth, 2);
        assert!(!ctx.is_sub_workflow);
    }

    #[test]
    fn test_node_execution_context_sub_workflow_node_ok() {
        let mut node = make_node();
        node.workflow_ref = Some("child.yaml".to_string());
        let store = VariableStore::new();
        let ctx = NodeExecutionContext::for_node(&node, "Run child.", 1, &store, &[], &[], 1);
        assert!(ctx.is_ok());
        let ctx = ctx.unwrap();
        assert!(ctx.is_sub_workflow);
    }

    #[test]
    fn test_node_execution_context_deep_sub_workflow_fails() {
        let mut node = make_node();
        node.workflow_ref = Some("child.yaml".to_string());
        let store = VariableStore::new();
        let result = NodeExecutionContext::for_node(
            &node,
            "Too deep.",
            1,
            &store,
            &[],
            &[],
            MAX_SUB_WORKFLOW_DEPTH,
        );
        assert!(result.is_err(), "should fail when depth exceeds maximum");
    }
}
