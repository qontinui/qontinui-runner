use super::variable_engine::VariableStore;
use crate::workflow::dag_schema::TriggerRule;
use std::collections::HashMap;

/// Outcome of a node after execution.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeOutcome {
    Success,
    Failed,
    Skipped { reason: String },
    Cancelled { reason: String },
}

/// Decision on whether to execute a node.
#[derive(Debug)]
pub enum NodeDecision {
    Execute,
    Skip { reason: String },
    Cancel { reason: String },
}

/// Per-node execution context for DAG execution.
#[derive(Debug, Clone)]
pub struct DagNodeExecution {
    pub node_id: String,
    pub step_index: usize,
    pub trigger_rule: TriggerRule,
    pub when_condition: Option<String>,
}

/// The complete DAG execution plan with layers.
#[derive(Debug)]
pub struct DagExecutionPlan {
    pub layers: Vec<Vec<DagNodeExecution>>,
}

/// Evaluate whether a node should execute based on its trigger rule and upstream results.
pub fn evaluate_trigger_rule(
    rule: &TriggerRule,
    upstream_outcomes: &HashMap<String, NodeOutcome>,
    depends_on: &[String],
) -> NodeDecision {
    if depends_on.is_empty() {
        return NodeDecision::Execute;
    }

    let outcomes: Vec<&NodeOutcome> = depends_on
        .iter()
        .filter_map(|id| upstream_outcomes.get(id))
        .collect();

    // If any dependency hasn't completed yet, skip (shouldn't happen in layer execution)
    if outcomes.len() < depends_on.len() {
        return NodeDecision::Skip {
            reason: "Not all upstream dependencies have completed".to_string(),
        };
    }

    let successes = outcomes
        .iter()
        .filter(|o| matches!(o, NodeOutcome::Success))
        .count();
    let failures = outcomes
        .iter()
        .filter(|o| matches!(o, NodeOutcome::Failed))
        .count();
    let _cancelled = outcomes
        .iter()
        .filter(|o| matches!(o, NodeOutcome::Cancelled { .. }))
        .count();

    match rule {
        TriggerRule::AllSuccess => {
            if successes == depends_on.len() {
                NodeDecision::Execute
            } else {
                NodeDecision::Skip {
                    reason: format!("{} of {} upstream nodes failed", failures, depends_on.len()),
                }
            }
        }
        TriggerRule::OneSuccess => {
            if successes >= 1 {
                NodeDecision::Execute
            } else {
                NodeDecision::Skip {
                    reason: "No upstream nodes succeeded".to_string(),
                }
            }
        }
        TriggerRule::AllDone => {
            // Execute as long as all are done (regardless of success/failure)
            NodeDecision::Execute
        }
        TriggerRule::NoneFailedMinOneSuccess => {
            if failures == 0 && successes >= 1 {
                NodeDecision::Execute
            } else if failures > 0 {
                NodeDecision::Skip {
                    reason: format!("{} upstream nodes failed", failures),
                }
            } else {
                NodeDecision::Skip {
                    reason: "No upstream nodes succeeded".to_string(),
                }
            }
        }
    }
}

/// Evaluate a `when` condition using the variable store.
pub fn evaluate_when_condition(condition: &str, variables: &VariableStore) -> bool {
    variables.evaluate_condition(condition)
}

/// Decide whether a node should execute, combining trigger rule and when condition.
pub fn decide_node_execution(
    node: &DagNodeExecution,
    upstream_outcomes: &HashMap<String, NodeOutcome>,
    depends_on: &[String],
    variables: &VariableStore,
) -> NodeDecision {
    // First check trigger rule
    let trigger_decision = evaluate_trigger_rule(&node.trigger_rule, upstream_outcomes, depends_on);

    match trigger_decision {
        NodeDecision::Execute => {
            // Then check when condition
            if let Some(ref condition) = node.when_condition {
                if evaluate_when_condition(condition, variables) {
                    NodeDecision::Execute
                } else {
                    NodeDecision::Skip {
                        reason: format!("when condition '{}' evaluated to false", condition),
                    }
                }
            } else {
                NodeDecision::Execute
            }
        }
        other => other,
    }
}

/// Build a DagExecutionPlan from a DagWorkflowDef.
///
/// Uses the existing compute_execution_layers from dag.rs after converting
/// to ExecutionStepConfig, then enriches with DAG-specific metadata.
pub fn build_execution_plan(
    def: &crate::workflow::dag_schema::DagWorkflowDef,
) -> Result<DagExecutionPlan, String> {
    use crate::step_executor::dag::compute_execution_layers;
    use crate::workflow::dag_parser::dag_to_step_configs;

    let step_configs = dag_to_step_configs(def)?;
    let layers = compute_execution_layers(&step_configs)?;

    // Build node_id -> index map (index in step_configs vec)
    let node_ids: Vec<String> = step_configs
        .iter()
        .map(|s| s.id.clone().unwrap_or_default())
        .collect();

    // Enrich layers with DAG metadata
    let execution_layers = layers
        .into_iter()
        .map(|layer| {
            layer
                .into_iter()
                .map(|idx| {
                    let node_id = node_ids[idx].clone();
                    let node_def = def.nodes.get(&node_id);
                    DagNodeExecution {
                        node_id,
                        step_index: idx,
                        trigger_rule: node_def.map(|n| n.trigger_rule.clone()).unwrap_or_default(),
                        when_condition: node_def.and_then(|n| n.when.clone()),
                    }
                })
                .collect()
        })
        .collect();

    Ok(DagExecutionPlan {
        layers: execution_layers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::dag_schema::TriggerRule;

    fn outcomes(pairs: &[(&str, NodeOutcome)]) -> HashMap<String, NodeOutcome> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn test_all_success_passes_when_all_succeed() {
        let up = outcomes(&[("a", NodeOutcome::Success), ("b", NodeOutcome::Success)]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::AllSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Execute));
    }

    #[test]
    fn test_all_success_skips_when_one_fails() {
        let up = outcomes(&[("a", NodeOutcome::Success), ("b", NodeOutcome::Failed)]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::AllSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Skip { .. }));
    }

    #[test]
    fn test_one_success_passes_when_one_succeeds() {
        let up = outcomes(&[("a", NodeOutcome::Failed), ("b", NodeOutcome::Success)]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::OneSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Execute));
    }

    #[test]
    fn test_one_success_skips_when_none_succeed() {
        let up = outcomes(&[
            ("a", NodeOutcome::Failed),
            (
                "b",
                NodeOutcome::Skipped {
                    reason: "x".to_string(),
                },
            ),
        ]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::OneSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Skip { .. }));
    }

    #[test]
    fn test_all_done_always_executes() {
        let up = outcomes(&[("a", NodeOutcome::Failed), ("b", NodeOutcome::Success)]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::AllDone, &up, &deps);
        assert!(matches!(decision, NodeDecision::Execute));
    }

    #[test]
    fn test_none_failed_min_one_success_passes() {
        let up = outcomes(&[
            ("a", NodeOutcome::Success),
            (
                "b",
                NodeOutcome::Skipped {
                    reason: "x".to_string(),
                },
            ),
        ]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::NoneFailedMinOneSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Execute));
    }

    #[test]
    fn test_none_failed_min_one_success_skips_on_failure() {
        let up = outcomes(&[("a", NodeOutcome::Failed), ("b", NodeOutcome::Success)]);
        let deps = vec!["a".to_string(), "b".to_string()];
        let decision = evaluate_trigger_rule(&TriggerRule::NoneFailedMinOneSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Skip { .. }));
    }

    #[test]
    fn test_no_deps_always_executes() {
        let up: HashMap<String, NodeOutcome> = HashMap::new();
        let deps: Vec<String> = vec![];
        let decision = evaluate_trigger_rule(&TriggerRule::AllSuccess, &up, &deps);
        assert!(matches!(decision, NodeDecision::Execute));
    }

    #[test]
    fn test_when_condition_gates_execution() {
        use serde_json::json;
        let mut variables = VariableStore::new();
        variables.set_output("nodeA", json!({"flag": false}));

        let node = DagNodeExecution {
            node_id: "nodeB".to_string(),
            step_index: 1,
            trigger_rule: TriggerRule::AllSuccess,
            when_condition: Some("$nodeA.flag == 'true'".to_string()),
        };
        let up = outcomes(&[("nodeA", NodeOutcome::Success)]);
        let deps = vec!["nodeA".to_string()];
        let decision = decide_node_execution(&node, &up, &deps, &variables);
        assert!(matches!(decision, NodeDecision::Skip { .. }));
    }
}
