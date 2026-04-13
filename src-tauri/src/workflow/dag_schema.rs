use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level YAML workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagWorkflowDef {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub nodes: HashMap<String, DagNodeDef>,
    #[serde(default)]
    pub defaults: NodeDefaults,
    #[serde(default)]
    pub settings: WorkflowSettings,
}

/// Default values applied to nodes that don't specify the field themselves.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeDefaults {
    pub timeout_seconds: Option<u64>,
    pub retry: Option<RetryConfig>,
    pub context: Option<ContextMode>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Workflow-level settings controlling orchestration behaviour.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowSettings {
    pub max_iterations: Option<u32>,
    pub rollback_policy: Option<String>,
    pub isolation: Option<String>,
    pub approval_gate: Option<bool>,
    pub cross_workflow_learning: Option<bool>,
}

/// A single node in the DAG.
///
/// Exactly one "type-discriminating" field must be set:
/// `prompt`, `command`, `check_type`, `ui_bridge_action`, `a11y_action`,
/// `loop_body`, `approval`, `workflow_ref`, or `cancel_reason`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNodeDef {
    pub name: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub when: Option<String>,
    #[serde(default = "default_trigger_rule")]
    pub trigger_rule: TriggerRule,
    pub retry: Option<RetryConfig>,
    pub timeout_seconds: Option<u64>,
    pub context: Option<ContextMode>,

    // ── PromptNode ──────────────────────────────────────────────────────────
    pub prompt: Option<String>,
    pub prompt_mode: Option<String>,

    // ── CommandNode ─────────────────────────────────────────────────────────
    pub command: Option<String>,
    pub working_directory: Option<String>,
    pub fail_on_error: Option<bool>,

    // ── CheckNode ───────────────────────────────────────────────────────────
    pub check_type: Option<String>,
    pub check_group_id: Option<String>,

    // ── UiBridgeNode ────────────────────────────────────────────────────────
    pub ui_bridge_action: Option<String>,
    pub ui_bridge_url: Option<String>,
    pub ui_bridge_instruction: Option<String>,

    // ── NativeAccessibilityNode ─────────────────────────────────────────────
    pub a11y_action: Option<String>,
    pub a11y_target: Option<String>,

    // ── LoopNode ────────────────────────────────────────────────────────────
    pub loop_body: Option<Vec<String>>,
    pub until: Option<String>,
    pub until_bash: Option<String>,
    pub max_loop_iterations: Option<u32>,
    pub commit_interval: Option<u32>,

    // ── ApprovalNode ────────────────────────────────────────────────────────
    pub approval: Option<ApprovalConfig>,

    // ── WorkflowRefNode ─────────────────────────────────────────────────────
    pub workflow_ref: Option<String>,
    pub workflow_inputs: Option<HashMap<String, String>>,

    // ── CancelNode ──────────────────────────────────────────────────────────
    pub cancel_reason: Option<String>,

    // ── Data flow ───────────────────────────────────────────────────────────
    pub inputs: Option<HashMap<String, String>>,
    pub extract: Option<HashMap<String, String>>,
}

fn default_trigger_rule() -> TriggerRule {
    TriggerRule::AllSuccess
}

/// Determines when a node should fire relative to its dependencies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerRule {
    AllSuccess,
    OneSuccess,
    AllDone,
    NoneFailedMinOneSuccess,
}

impl Default for TriggerRule {
    fn default() -> Self {
        Self::AllSuccess
    }
}

/// Context isolation mode for a node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    Fresh,
    Shared,
}

/// Retry configuration for a node or workflow-level defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    #[serde(default)]
    pub delay_ms: u64,
    pub backoff_multiplier: Option<f64>,
}

/// Configuration for a human-approval gate node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalConfig {
    pub message: String,
    pub on_reject: Option<String>,
    pub timeout_seconds: Option<u64>,
}
