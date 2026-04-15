//! DAG Workflow Runtime — orchestrates execution of a parsed DAG workflow.
//!
//! This module provides the execution control flow for DAG workflows:
//! 1. Parse YAML → DagWorkflowDef
//! 2. Build execution plan (topological layers)
//! 3. For each layer, evaluate node decisions (trigger rules, when conditions)
//! 4. Track node outcomes and variable propagation
//! 5. Integrate with event log for crash recovery

use crate::workflow::dag_executor::{
    build_execution_plan, decide_node_execution, DagExecutionPlan, NodeDecision, NodeOutcome,
};
use crate::workflow::dag_schema::DagWorkflowDef;
use crate::workflow::variable_engine::VariableStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The runtime state of a DAG workflow execution.
#[derive(Debug)]
pub struct DagRuntime {
    /// The parsed workflow definition.
    pub definition: DagWorkflowDef,
    /// The execution plan (topological layers of nodes).
    pub plan: DagExecutionPlan,
    /// Variable store tracking node outputs.
    pub variables: VariableStore,
    /// Outcomes of completed/skipped/failed nodes.
    pub outcomes: HashMap<String, NodeOutcome>,
    /// Current layer index (0-based).
    pub current_layer: usize,
    /// Whether the workflow has finished (all layers processed or cancelled).
    pub finished: bool,
    /// If the workflow was cancelled, the reason.
    pub cancel_reason: Option<String>,
}

/// A node that is ready to execute in the current layer.
#[derive(Debug, Clone)]
pub struct ReadyNode {
    pub node_id: String,
    pub step_index: usize,
}

/// A node that was skipped due to trigger rule or when condition.
#[derive(Debug, Clone)]
pub struct SkippedNode {
    pub node_id: String,
    pub reason: String,
}

/// Result of advancing the runtime by one layer.
#[derive(Debug)]
pub enum LayerAdvance {
    /// Nodes ready for execution (caller should execute them and report back).
    Ready {
        layer_index: usize,
        nodes: Vec<ReadyNode>,
        skipped: Vec<SkippedNode>,
    },
    /// Workflow is complete (all layers processed).
    Complete {
        success: bool,
        outcomes: HashMap<String, NodeOutcome>,
    },
    /// Workflow was cancelled by a cancel node.
    Cancelled { reason: String },
}

/// Summary of a completed DAG workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagWorkflowResult {
    pub success: bool,
    pub total_nodes: usize,
    pub completed_nodes: usize,
    pub failed_nodes: usize,
    pub skipped_nodes: usize,
    pub cancelled: bool,
    pub cancel_reason: Option<String>,
    /// Per-node metrics for learning_outcomes.dag_node_metrics.
    pub node_metrics: Vec<DagNodeMetric>,
}

/// Per-node execution metric for the meta-optimizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNodeMetric {
    pub node_id: String,
    pub outcome: String, // "success", "failed", "skipped", "cancelled"
    pub duration_ms: Option<u64>,
    pub retry_count: u32,
    pub was_skipped: bool,
}

impl DagRuntime {
    /// Create a new runtime from a parsed workflow definition.
    pub fn new(definition: DagWorkflowDef) -> Result<Self, String> {
        let plan = build_execution_plan(&definition)?;
        Ok(Self {
            definition,
            plan,
            variables: VariableStore::new(),
            outcomes: HashMap::new(),
            current_layer: 0,
            finished: false,
            cancel_reason: None,
        })
    }

    /// Create a runtime from a YAML string (parse + validate + build plan).
    pub fn from_yaml(yaml_str: &str) -> Result<Self, String> {
        use crate::workflow::dag_parser::parse_dag_workflow;
        let def = parse_dag_workflow(yaml_str).map_err(|e| format!("{}", e))?;
        Self::new(def)
    }

    /// Advance to the next layer. Returns what actions the caller should take.
    ///
    /// The caller is responsible for executing the ReadyNodes and then calling
    /// `report_node_outcome()` for each before calling `advance()` again.
    pub fn advance(&mut self) -> LayerAdvance {
        if self.finished {
            return LayerAdvance::Complete {
                success: self.is_success(),
                outcomes: self.outcomes.clone(),
            };
        }

        if self.current_layer >= self.plan.layers.len() {
            self.finished = true;
            return LayerAdvance::Complete {
                success: self.is_success(),
                outcomes: self.outcomes.clone(),
            };
        }

        let layer = &self.plan.layers[self.current_layer];
        let mut ready = Vec::new();
        let mut skipped = Vec::new();

        for node_exec in layer {
            // Check for cancel nodes first
            if let Some(node_def) = self.definition.nodes.get(&node_exec.node_id) {
                if node_def.cancel_reason.is_some() {
                    let reason = node_def.cancel_reason.clone().unwrap_or_default();
                    self.cancel_reason = Some(reason.clone());
                    self.finished = true;
                    self.outcomes.insert(
                        node_exec.node_id.clone(),
                        NodeOutcome::Cancelled {
                            reason: reason.clone(),
                        },
                    );
                    return LayerAdvance::Cancelled { reason };
                }
            }

            // Evaluate whether this node should execute
            let depends_on: Vec<String> = self
                .definition
                .nodes
                .get(&node_exec.node_id)
                .map(|n| n.depends_on.clone())
                .unwrap_or_default();

            let decision =
                decide_node_execution(node_exec, &self.outcomes, &depends_on, &self.variables);

            match decision {
                NodeDecision::Execute => {
                    ready.push(ReadyNode {
                        node_id: node_exec.node_id.clone(),
                        step_index: node_exec.step_index,
                    });
                }
                NodeDecision::Skip { reason } => {
                    self.outcomes.insert(
                        node_exec.node_id.clone(),
                        NodeOutcome::Skipped {
                            reason: reason.clone(),
                        },
                    );
                    skipped.push(SkippedNode {
                        node_id: node_exec.node_id.clone(),
                        reason,
                    });
                }
                NodeDecision::Cancel { reason } => {
                    self.cancel_reason = Some(reason.clone());
                    self.finished = true;
                    self.outcomes.insert(
                        node_exec.node_id.clone(),
                        NodeOutcome::Cancelled {
                            reason: reason.clone(),
                        },
                    );
                    return LayerAdvance::Cancelled { reason };
                }
            }
        }

        let layer_index = self.current_layer;
        self.current_layer += 1;

        // If no nodes are ready and all were skipped, continue to next layer automatically
        if ready.is_empty() && !skipped.is_empty() {
            return self.advance();
        }

        if ready.is_empty() && skipped.is_empty() {
            self.finished = true;
            return LayerAdvance::Complete {
                success: self.is_success(),
                outcomes: self.outcomes.clone(),
            };
        }

        LayerAdvance::Ready {
            layer_index,
            nodes: ready,
            skipped,
        }
    }

    /// Report the outcome of a node execution.
    ///
    /// Must be called for every ReadyNode before the next `advance()`.
    pub fn report_node_outcome(
        &mut self,
        node_id: &str,
        outcome: NodeOutcome,
        output: Option<serde_json::Value>,
    ) {
        self.outcomes.insert(node_id.to_string(), outcome);
        if let Some(output) = output {
            self.variables.set_output(node_id, output);
        }
    }

    /// Check if the workflow succeeded (no failures, no cancellation).
    pub fn is_success(&self) -> bool {
        self.cancel_reason.is_none()
            && !self
                .outcomes
                .values()
                .any(|o| matches!(o, NodeOutcome::Failed))
    }

    /// Build the final result summary.
    ///
    /// `node_retry_counts` maps node_id → number of retries (attempts - 1).
    /// Pass an empty map if retry tracking is not needed.
    pub fn build_result(
        &self,
        node_durations: &HashMap<String, u64>,
        node_retry_counts: &HashMap<String, u32>,
    ) -> DagWorkflowResult {
        let total_nodes = self.definition.nodes.len();
        let mut completed = 0;
        let mut failed = 0;
        let mut skipped_count = 0;
        let mut node_metrics = Vec::new();

        for (node_id, outcome) in &self.outcomes {
            let (outcome_str, was_skipped) = match outcome {
                NodeOutcome::Success => {
                    completed += 1;
                    ("success", false)
                }
                NodeOutcome::Failed => {
                    failed += 1;
                    ("failed", false)
                }
                NodeOutcome::Skipped { .. } => {
                    skipped_count += 1;
                    ("skipped", true)
                }
                NodeOutcome::Cancelled { .. } => ("cancelled", false),
            };
            node_metrics.push(DagNodeMetric {
                node_id: node_id.clone(),
                outcome: outcome_str.to_string(),
                duration_ms: node_durations.get(node_id).copied(),
                retry_count: node_retry_counts.get(node_id).copied().unwrap_or(0),
                was_skipped,
            });
        }

        DagWorkflowResult {
            success: self.is_success(),
            total_nodes,
            completed_nodes: completed,
            failed_nodes: failed,
            skipped_nodes: skipped_count,
            cancelled: self.cancel_reason.is_some(),
            cancel_reason: self.cancel_reason.clone(),
            node_metrics,
        }
    }

    /// Get the variable store (for external access).
    pub fn variable_store(&self) -> &VariableStore {
        &self.variables
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SIMPLE_TWO_NODE_YAML: &str = r#"
name: test-workflow
nodes:
  nodeA:
    prompt: "Do task A"
  nodeB:
    prompt: "Do task B"
    depends_on: [nodeA]
"#;

    const THREE_NODE_PARALLEL_YAML: &str = r#"
name: parallel-workflow
nodes:
  nodeA:
    prompt: "Task A"
  nodeB:
    prompt: "Task B"
  nodeC:
    prompt: "Task C"
    depends_on: [nodeA, nodeB]
"#;

    const CANCEL_NODE_YAML: &str = r#"
name: cancel-workflow
nodes:
  nodeA:
    prompt: "Task A"
  cancel_node:
    cancel_reason: "Workflow cancelled by policy"
    depends_on: [nodeA]
"#;

    const WHEN_CONDITION_YAML: &str = r#"
name: conditional-workflow
nodes:
  nodeA:
    prompt: "Task A"
  nodeB:
    prompt: "Task B (conditional)"
    depends_on: [nodeA]
    when: "$nodeA.proceed == 'yes'"
"#;

    const ALL_DONE_TRIGGER_YAML: &str = r#"
name: all-done-workflow
nodes:
  nodeA:
    prompt: "Task A"
  nodeB:
    prompt: "Task B"
  nodeC:
    prompt: "Task C (runs regardless)"
    depends_on: [nodeA, nodeB]
    trigger_rule: all_done
"#;

    /// Helper: drive the runtime to completion, simulating node execution.
    /// `node_results` maps node_id → (success, optional output).
    fn drive_to_completion(
        runtime: &mut DagRuntime,
        node_results: &HashMap<&str, (bool, Option<serde_json::Value>)>,
    ) -> DagWorkflowResult {
        loop {
            match runtime.advance() {
                LayerAdvance::Ready { nodes, .. } => {
                    for ready_node in nodes {
                        let (success, output) = node_results
                            .get(ready_node.node_id.as_str())
                            .cloned()
                            .unwrap_or((true, None));
                        let outcome = if success {
                            NodeOutcome::Success
                        } else {
                            NodeOutcome::Failed
                        };
                        runtime.report_node_outcome(&ready_node.node_id, outcome, output);
                    }
                }
                LayerAdvance::Complete { .. } | LayerAdvance::Cancelled { .. } => {
                    return runtime.build_result(&HashMap::new(), &HashMap::new());
                }
            }
        }
    }

    #[test]
    fn test_simple_two_node_success() {
        let mut runtime =
            DagRuntime::from_yaml(SIMPLE_TWO_NODE_YAML).expect("should parse and build plan");

        let mut results = HashMap::new();
        results.insert("nodeA", (true, None));
        results.insert("nodeB", (true, None));

        let result = drive_to_completion(&mut runtime, &results);

        assert!(result.success);
        assert_eq!(result.completed_nodes, 2);
        assert_eq!(result.failed_nodes, 0);
        assert_eq!(result.skipped_nodes, 0);
        assert!(!result.cancelled);
    }

    #[test]
    fn test_failure_propagates_skip() {
        // nodeA fails → nodeB (depends on nodeA with AllSuccess) should be skipped
        let mut runtime = DagRuntime::from_yaml(SIMPLE_TWO_NODE_YAML).expect("should parse");

        let mut results = HashMap::new();
        results.insert("nodeA", (false, None)); // nodeA fails
        results.insert("nodeB", (true, None)); // won't run

        let result = drive_to_completion(&mut runtime, &results);

        assert!(!result.success);
        assert_eq!(result.failed_nodes, 1);
        assert_eq!(result.skipped_nodes, 1);
    }

    #[test]
    fn test_parallel_nodes_then_join() {
        let mut runtime = DagRuntime::from_yaml(THREE_NODE_PARALLEL_YAML).expect("should parse");

        let mut results = HashMap::new();
        results.insert("nodeA", (true, None));
        results.insert("nodeB", (true, None));
        results.insert("nodeC", (true, None));

        let result = drive_to_completion(&mut runtime, &results);

        assert!(result.success);
        assert_eq!(result.completed_nodes, 3);
    }

    #[test]
    fn test_cancel_node_stops_workflow() {
        let mut runtime = DagRuntime::from_yaml(CANCEL_NODE_YAML).expect("should parse");

        // First advance: nodeA is ready
        let advance1 = runtime.advance();
        assert!(matches!(advance1, LayerAdvance::Ready { .. }));

        if let LayerAdvance::Ready { nodes, .. } = advance1 {
            for n in nodes {
                runtime.report_node_outcome(&n.node_id, NodeOutcome::Success, None);
            }
        }

        // Second advance: cancel_node is in the next layer, should cancel
        let advance2 = runtime.advance();
        assert!(matches!(advance2, LayerAdvance::Cancelled { .. }));

        if let LayerAdvance::Cancelled { reason } = advance2 {
            assert!(
                reason.contains("cancelled")
                    || reason.contains("Cancelled")
                    || reason.contains("policy")
            );
        }

        assert!(runtime.cancel_reason.is_some());
    }

    #[test]
    fn test_when_condition_skips_node_on_false() {
        let mut runtime = DagRuntime::from_yaml(WHEN_CONDITION_YAML).expect("should parse");

        // nodeA returns output where proceed != 'yes'
        let advance1 = runtime.advance();
        if let LayerAdvance::Ready { nodes, .. } = advance1 {
            for n in nodes {
                runtime.report_node_outcome(
                    &n.node_id,
                    NodeOutcome::Success,
                    Some(json!({"proceed": "no"})),
                );
            }
        }

        // nodeB should be skipped because when condition is false
        let result = drive_to_completion(&mut runtime, &HashMap::new());

        assert!(result.success); // no failures
        assert_eq!(result.skipped_nodes, 1); // nodeB skipped
    }

    #[test]
    fn test_when_condition_executes_node_on_true() {
        let mut runtime = DagRuntime::from_yaml(WHEN_CONDITION_YAML).expect("should parse");

        let mut results = HashMap::new();
        results.insert("nodeA", (true, Some(json!({"proceed": "yes"}))));
        results.insert("nodeB", (true, None));

        let result = drive_to_completion(&mut runtime, &results);

        assert!(result.success);
        assert_eq!(result.completed_nodes, 2);
        assert_eq!(result.skipped_nodes, 0);
    }

    #[test]
    fn test_all_done_trigger_runs_after_failure() {
        let mut runtime = DagRuntime::from_yaml(ALL_DONE_TRIGGER_YAML).expect("should parse");

        // nodeA fails, nodeB succeeds, nodeC has all_done so runs anyway
        let mut results = HashMap::new();
        results.insert("nodeA", (false, None));
        results.insert("nodeB", (true, None));
        results.insert("nodeC", (true, None));

        let result = drive_to_completion(&mut runtime, &results);

        // nodeC ran (all_done), nodeA failed, nodeB succeeded
        assert_eq!(result.failed_nodes, 1);
        assert_eq!(result.completed_nodes, 2); // nodeB + nodeC
        assert_eq!(result.skipped_nodes, 0);
        // Overall success is false (nodeA failed)
        assert!(!result.success);
    }

    #[test]
    fn test_build_result_metrics() {
        let mut runtime = DagRuntime::from_yaml(SIMPLE_TWO_NODE_YAML).expect("should parse");

        let mut results = HashMap::new();
        results.insert("nodeA", (true, None));
        results.insert("nodeB", (true, None));

        drive_to_completion(&mut runtime, &results);

        let mut durations = HashMap::new();
        durations.insert("nodeA".to_string(), 150u64);
        durations.insert("nodeB".to_string(), 200u64);

        let result = runtime.build_result(&durations, &HashMap::new());

        assert_eq!(result.node_metrics.len(), 2);
        for metric in &result.node_metrics {
            assert_eq!(metric.outcome, "success");
            assert!(!metric.was_skipped);
            assert!(metric.duration_ms.is_some());
        }
    }

    #[test]
    fn test_advance_after_finished_returns_complete() {
        let mut runtime = DagRuntime::from_yaml(SIMPLE_TWO_NODE_YAML).expect("should parse");

        let mut results = HashMap::new();
        results.insert("nodeA", (true, None));
        results.insert("nodeB", (true, None));

        drive_to_completion(&mut runtime, &results);

        // Calling advance again after finished should return Complete immediately
        let extra = runtime.advance();
        assert!(matches!(extra, LayerAdvance::Complete { .. }));
    }

    #[test]
    fn test_variable_propagation() {
        let mut runtime = DagRuntime::from_yaml(SIMPLE_TWO_NODE_YAML).expect("should parse");

        let advance1 = runtime.advance();
        if let LayerAdvance::Ready { nodes, .. } = advance1 {
            for n in nodes {
                runtime.report_node_outcome(
                    &n.node_id,
                    NodeOutcome::Success,
                    Some(json!({"result": "value_from_a"})),
                );
            }
        }

        // Variable store should have nodeA's output
        let stored = runtime.variable_store().get_output("nodeA");
        assert!(stored.is_some());
        assert_eq!(stored.unwrap()["result"], "value_from_a");
    }

    #[test]
    fn test_is_success_false_on_failure() {
        let mut runtime = DagRuntime::from_yaml(SIMPLE_TWO_NODE_YAML).expect("should parse");

        let mut results = HashMap::new();
        results.insert("nodeA", (false, None));

        drive_to_completion(&mut runtime, &results);

        assert!(!runtime.is_success());
    }

    #[test]
    fn test_is_success_false_on_cancel() {
        let mut runtime = DagRuntime::from_yaml(CANCEL_NODE_YAML).expect("should parse");

        let advance1 = runtime.advance();
        if let LayerAdvance::Ready { nodes, .. } = advance1 {
            for n in nodes {
                runtime.report_node_outcome(&n.node_id, NodeOutcome::Success, None);
            }
        }
        runtime.advance(); // triggers cancel

        assert!(!runtime.is_success());
        assert!(runtime.cancel_reason.is_some());
    }
}
