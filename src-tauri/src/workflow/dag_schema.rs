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
    /// Default tool policy inherited by agentic nodes that don't set their own.
    /// Genuinely new — composed in Phase 3, not read here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_defaults: Option<ToolPolicy>,
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

    // ── Blueprint typing (opt-in; back-compat: None ⇒ inferred) ──────────────
    /// Explicit node kind. If `None`, inferred via [`DagNodeDef::effective_kind`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NodeKind>,
    /// Tool policy, applied only when the effective kind is `Agentic`.
    /// `None` ⇒ no restriction. Inherits `DagWorkflowDef::blueprint_defaults`
    /// at resolution time (Phase 3), not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<ToolPolicy>,
}

impl DagNodeDef {
    /// Resolve the node's execution kind. An explicit `kind` always wins;
    /// otherwise it is inferred from the type-discriminating field — only a
    /// `prompt` node is agentic; every other node kind runs deterministically
    /// (it never spawns an LLM session itself).
    pub fn effective_kind(&self) -> NodeKind {
        if let Some(k) = self.kind {
            return k;
        }
        if self.prompt.is_some() {
            NodeKind::Agentic
        } else {
            NodeKind::Deterministic
        }
    }
}

fn default_trigger_rule() -> TriggerRule {
    TriggerRule::AllSuccess
}

/// Determines when a node should fire relative to its dependencies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TriggerRule {
    #[default]
    AllSuccess,
    OneSuccess,
    AllDone,
    NoneFailedMinOneSuccess,
}

/// Context isolation mode for a node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    Fresh,
    Shared,
}

/// Explicit node execution kind. Deterministic nodes never spawn an LLM
/// session; agentic nodes invoke the model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Deterministic,
    Agentic,
}

impl NodeKind {
    /// Stable lowercase string for telemetry stamping (matches serde repr).
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Deterministic => "deterministic",
            NodeKind::Agentic => "agentic",
        }
    }
}

/// Per-agentic-node tool policy. Applied only when the effective kind is
/// `Agentic`. `None` on a node ⇒ no restriction (today's behavior).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Allow-list of tool names. If `Some`, only these tools may be called.
    /// Empty/None ⇒ unrestricted (matches `ToolGuard` semantics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// Deny-list. Two self-classifying granularities (split on the FIRST `:`):
    ///  - bare tool name (no `:`), e.g. `"Write"` ⇒ tool-name deny.
    ///  - `"<Tool>:<command-prefix>"`, e.g. `"Bash:git push --force"` ⇒
    ///    command-argument-scoped deny.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
    /// What to do on a violation.
    #[serde(default)]
    pub on_violation: ViolationAction,
}

/// Action taken when a tool-policy violation is observed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ViolationAction {
    /// Skip the call and let the model continue (the CLI's native behavior
    /// under `--disallowed-tools`). This is the default.
    #[default]
    RejectCall,
    /// Hard-stop the node on a violation.
    FailNode,
}

/// A single deny entry, classified by granularity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyEntry {
    /// Block the whole tool (carrier: native `--disallowed-tools <name>`).
    ToolName(String),
    /// Block only matching invocations of an otherwise-allowed tool
    /// (carrier: a `Bash(<prefix>:*)` permission-rule specifier + the
    /// node's effective `ActionPolicy.blocked_commands`).
    CommandScoped {
        tool: String,
        command_prefix: String,
    },
}

impl ToolPolicy {
    /// Classify every deny entry by granularity. Tokens are trimmed; empty
    /// tokens are skipped. A token splits on its FIRST `:`.
    pub fn classified_denies(&self) -> Vec<DenyEntry> {
        let mut out = Vec::new();
        let Some(ref denies) = self.deny else {
            return out;
        };
        for raw in denies {
            let tok = raw.trim();
            if tok.is_empty() {
                continue;
            }
            match tok.split_once(':') {
                Some((tool, prefix)) => {
                    let tool = tool.trim();
                    let prefix = prefix.trim();
                    if tool.is_empty() || prefix.is_empty() {
                        // Malformed (e.g. ":foo" or "Bash:") — treat the whole
                        // token as a tool-name deny so it is never silently dropped.
                        out.push(DenyEntry::ToolName(tok.to_string()));
                    } else {
                        out.push(DenyEntry::CommandScoped {
                            tool: tool.to_string(),
                            command_prefix: prefix.to_string(),
                        });
                    }
                }
                None => out.push(DenyEntry::ToolName(tok.to_string())),
            }
        }
        out
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a `DagNodeDef` fixture from a JSON object, so tests stay readable
    /// despite the struct's many fields. Any field not present defaults via serde.
    fn node(value: serde_json::Value) -> DagNodeDef {
        serde_json::from_value(value).expect("fixture node should deserialize")
    }

    // ───────────────────────── effective_kind inference ──────────────────────

    #[test]
    fn effective_kind_prompt_node_is_agentic() {
        let n = node(json!({ "prompt": "do the thing" }));
        assert_eq!(n.effective_kind(), NodeKind::Agentic);
    }

    #[test]
    fn effective_kind_command_node_is_deterministic() {
        let n = node(json!({ "command": "echo hi" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_check_type_node_is_deterministic() {
        let n = node(json!({ "check_type": "lint" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_ui_bridge_node_is_deterministic() {
        let n = node(json!({ "ui_bridge_action": "click" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_a11y_node_is_deterministic() {
        let n = node(json!({ "a11y_action": "press" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_loop_node_is_deterministic() {
        let n = node(json!({ "loop_body": ["a", "b"] }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_approval_node_is_deterministic() {
        let n = node(json!({ "approval": { "message": "ok?" } }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_workflow_ref_node_is_deterministic() {
        let n = node(json!({ "workflow_ref": "sub.yaml" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_cancel_node_is_deterministic() {
        let n = node(json!({ "cancel_reason": "abort" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_empty_node_is_deterministic() {
        let n = node(json!({}));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_explicit_deterministic_overrides_prompt() {
        // Explicit kind wins even though a prompt is present.
        let n = node(json!({ "prompt": "do the thing", "kind": "deterministic" }));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn effective_kind_explicit_agentic_overrides_command() {
        // Explicit kind wins even though only a command is present.
        let n = node(json!({ "command": "echo hi", "kind": "agentic" }));
        assert_eq!(n.effective_kind(), NodeKind::Agentic);
    }

    // ───────────────────────── classified_denies ─────────────────────────────

    fn policy_with_deny(deny: Option<Vec<&str>>) -> ToolPolicy {
        ToolPolicy {
            allow: None,
            deny: deny.map(|v| v.into_iter().map(String::from).collect()),
            on_violation: ViolationAction::default(),
        }
    }

    #[test]
    fn classified_denies_bare_tool_name() {
        let p = policy_with_deny(Some(vec!["Write"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::ToolName("Write".to_string())]
        );
    }

    #[test]
    fn classified_denies_command_scoped() {
        let p = policy_with_deny(Some(vec!["Bash:git push --force"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::CommandScoped {
                tool: "Bash".to_string(),
                command_prefix: "git push --force".to_string(),
            }]
        );
    }

    #[test]
    fn classified_denies_command_scoped_simple() {
        let p = policy_with_deny(Some(vec!["Bash:rm -rf"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::CommandScoped {
                tool: "Bash".to_string(),
                command_prefix: "rm -rf".to_string(),
            }]
        );
    }

    #[test]
    fn classified_denies_mixed_preserves_order() {
        let p = policy_with_deny(Some(vec!["Write", "Bash:git push --force", "Edit"]));
        assert_eq!(
            p.classified_denies(),
            vec![
                DenyEntry::ToolName("Write".to_string()),
                DenyEntry::CommandScoped {
                    tool: "Bash".to_string(),
                    command_prefix: "git push --force".to_string(),
                },
                DenyEntry::ToolName("Edit".to_string()),
            ]
        );
    }

    #[test]
    fn classified_denies_trims_whitespace() {
        let p = policy_with_deny(Some(vec!["  Write  ", "  Bash : git push  "]));
        assert_eq!(
            p.classified_denies(),
            vec![
                DenyEntry::ToolName("Write".to_string()),
                DenyEntry::CommandScoped {
                    tool: "Bash".to_string(),
                    command_prefix: "git push".to_string(),
                },
            ]
        );
    }

    #[test]
    fn classified_denies_skips_empty_tokens() {
        let p = policy_with_deny(Some(vec!["", "   ", "Write"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::ToolName("Write".to_string())]
        );
    }

    #[test]
    fn classified_denies_malformed_trailing_colon_falls_back_to_tool_name() {
        // "Bash:" — empty prefix ⇒ never dropped, treated as tool-name deny.
        let p = policy_with_deny(Some(vec!["Bash:"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::ToolName("Bash:".to_string())]
        );
    }

    #[test]
    fn classified_denies_malformed_leading_colon_falls_back_to_tool_name() {
        // ":foo" — empty tool ⇒ never dropped, treated as tool-name deny.
        let p = policy_with_deny(Some(vec![":foo"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::ToolName(":foo".to_string())]
        );
    }

    #[test]
    fn classified_denies_none_is_empty() {
        let p = policy_with_deny(None);
        assert_eq!(p.classified_denies(), Vec::<DenyEntry>::new());
    }

    #[test]
    fn classified_denies_splits_on_first_colon_only() {
        // A prefix may itself contain a colon; only the FIRST colon splits.
        let p = policy_with_deny(Some(vec!["Bash:ssh user@host:cmd"]));
        assert_eq!(
            p.classified_denies(),
            vec![DenyEntry::CommandScoped {
                tool: "Bash".to_string(),
                command_prefix: "ssh user@host:cmd".to_string(),
            }]
        );
    }

    // ───────────────────────── serde round-trips ─────────────────────────────

    #[test]
    fn node_without_blueprint_fields_omits_keys_on_serialize() {
        let n = node(json!({ "command": "echo hi" }));
        assert!(n.kind.is_none());
        assert!(n.tool_policy.is_none());
        let serialized = serde_json::to_value(&n).expect("serialize");
        let obj = serialized.as_object().expect("object");
        assert!(
            !obj.contains_key("kind"),
            "kind should be skipped when None"
        );
        assert!(
            !obj.contains_key("tool_policy"),
            "tool_policy should be skipped when None"
        );
    }

    #[test]
    fn minimal_yaml_node_without_blueprint_fields_deserializes() {
        let yaml = r#"
command: "echo hi"
depends_on:
  - prev
"#;
        let n: DagNodeDef = serde_yaml::from_str(yaml).expect("yaml deserialize");
        assert!(n.kind.is_none());
        assert!(n.tool_policy.is_none());
        assert_eq!(n.command.as_deref(), Some("echo hi"));
        assert_eq!(n.effective_kind(), NodeKind::Deterministic);
    }

    #[test]
    fn node_with_tool_policy_round_trips_json() {
        let original = node(json!({
            "prompt": "do the thing",
            "kind": "agentic",
            "tool_policy": {
                "allow": ["Read", "Edit"],
                "deny": ["Write", "Bash:git push --force"],
                "on_violation": "fail_node"
            }
        }));
        let serialized = serde_json::to_string(&original).expect("serialize");
        let round: DagNodeDef = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(round.kind, Some(NodeKind::Agentic));
        assert_eq!(round.tool_policy, original.tool_policy);
        let tp = round.tool_policy.as_ref().expect("tool_policy");
        assert_eq!(tp.on_violation, ViolationAction::FailNode);
        assert_eq!(
            tp.classified_denies(),
            vec![
                DenyEntry::ToolName("Write".to_string()),
                DenyEntry::CommandScoped {
                    tool: "Bash".to_string(),
                    command_prefix: "git push --force".to_string(),
                },
            ]
        );
    }

    #[test]
    fn violation_action_defaults_to_reject_call() {
        let p = node(json!({
            "prompt": "x",
            "tool_policy": { "deny": ["Write"] }
        }))
        .tool_policy
        .expect("tool_policy");
        assert_eq!(p.on_violation, ViolationAction::RejectCall);
    }

    #[test]
    fn workflow_without_blueprint_defaults_omits_key() {
        let wf: DagWorkflowDef = serde_yaml::from_str(
            r#"
name: wf
nodes:
  a:
    command: "echo hi"
"#,
        )
        .expect("workflow deserialize");
        assert!(wf.blueprint_defaults.is_none());
        let serialized = serde_json::to_value(&wf).expect("serialize");
        assert!(!serialized
            .as_object()
            .expect("object")
            .contains_key("blueprint_defaults"));
    }

    // ───────────────────────── NodeKind::as_str (Phase 4) ────────────────────

    #[test]
    fn node_kind_as_str_returns_stable_strings() {
        assert_eq!(NodeKind::Deterministic.as_str(), "deterministic");
        assert_eq!(NodeKind::Agentic.as_str(), "agentic");
    }

    #[test]
    fn node_kind_as_str_matches_serde_repr() {
        // The telemetry string must equal the snake_case serde representation.
        let det = serde_json::to_value(NodeKind::Deterministic).expect("serialize");
        let agt = serde_json::to_value(NodeKind::Agentic).expect("serialize");
        assert_eq!(det.as_str(), Some(NodeKind::Deterministic.as_str()));
        assert_eq!(agt.as_str(), Some(NodeKind::Agentic.as_str()));
    }
}
