//! Types for the reflection workflow system.
//!
//! Defines the core data structures for tracking fixes applied by reflection
//! workflows and their effectiveness over time.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A fix applied by a reflection workflow after analyzing a completed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionFix {
    pub id: String,
    /// The task run that was analyzed
    pub source_task_run_id: String,
    /// The reflection workflow's own task run
    pub reflection_task_run_id: String,
    /// FK to task_run_findings (if fix addresses a specific finding)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_finding_id: Option<String>,
    /// FK to task_knowledge (if fix addresses a specific knowledge entry)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_knowledge_id: Option<String>,
    /// What kind of fix was applied
    pub fix_type: String,
    /// Human-readable description of the fix
    pub fix_description: String,
    /// File that was changed (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_changed: Option<String>,
    /// Previous value (for diffing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_value: Option<String>,
    /// New value (for diffing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<String>,
    /// Confidence level of the fix
    pub confidence: String,
    /// Content hash for deduplication across reflection runs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Current status of the fix
    pub status: String,
    /// Effectiveness evaluation result (NULL until evaluated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effectiveness: Option<String>,
    /// Evidence supporting the effectiveness evaluation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effectiveness_evidence: Option<String>,
    /// When the fix was applied
    pub applied_at: String,
    /// When effectiveness was evaluated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated_at: Option<String>,
    pub created_at: String,
    /// Which generation agent's output likely caused this issue
    /// (e.g. "specification", "builder", "verification", "hardener")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent: Option<String>,
    /// Root cause diagnosis and reasoning behind the fix
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Other approaches considered and why they were rejected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternatives_considered: Option<String>,
    /// Scope of this fix: 'workflow', 'project', or 'universal'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection_scope: Option<String>,
    /// Project path for project/workflow scoped fixes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    /// When this universal pattern applies (e.g., "web apps using React")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applicability_context: Option<String>,
}

/// Types of fixes that a reflection workflow can apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixType {
    /// Updated knowledge base content
    KnowledgeBaseUpdate,
    /// Rewrote a workflow step
    WorkflowStepRewrite,
    /// Fixed a UI selector
    SelectorFix,
    /// Updated tool configuration
    ToolConfigUpdate,
    /// Added missing context information
    ContextAddition,
    /// Clarified ambiguous instructions
    InstructionClarification,

    // --- Project-scoped fix types (project reflection) ---
    /// Environment requirements: deps, env vars, runtime versions, services
    ProjectEnvironment,
    /// Codebase structure: where tests live, framework patterns, build system
    ProjectArchitecture,
    /// Test infrastructure: runner, fixture patterns, setup/teardown
    ProjectTestPattern,
    /// Recurring friction: common failures, flaky areas, known quirks
    ProjectRecurringIssue,

    // --- UI Bridge-scoped fix types (UI Bridge reflection) ---
    /// Element discovery: labeling, grouping, hierarchy, semantic roles
    UiBridgeDiscovery,
    /// Snapshot format: structure, AI-friendliness, noise reduction
    UiBridgeSnapshot,
    /// Action reliability: feedback, error recovery, new actions
    UiBridgeAction,
    /// SDK integration: new endpoints, capabilities, configuration
    UiBridgeSdkFeature,
    /// Prompt/workflow guidance: better instructions for AI to use UI Bridge
    UiBridgePromptGuidance,
}

impl FixType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::KnowledgeBaseUpdate => "knowledge_base_update",
            Self::WorkflowStepRewrite => "workflow_step_rewrite",
            Self::SelectorFix => "selector_fix",
            Self::ToolConfigUpdate => "tool_config_update",
            Self::ContextAddition => "context_addition",
            Self::InstructionClarification => "instruction_clarification",
            Self::ProjectEnvironment => "project_environment",
            Self::ProjectArchitecture => "project_architecture",
            Self::ProjectTestPattern => "project_test_pattern",
            Self::ProjectRecurringIssue => "project_recurring_issue",
            Self::UiBridgeDiscovery => "ui_bridge_discovery",
            Self::UiBridgeSnapshot => "ui_bridge_snapshot",
            Self::UiBridgeAction => "ui_bridge_action",
            Self::UiBridgeSdkFeature => "ui_bridge_sdk_feature",
            Self::UiBridgePromptGuidance => "ui_bridge_prompt_guidance",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "knowledge_base_update" | "kb_update" => Some(Self::KnowledgeBaseUpdate),
            "workflow_step_rewrite" | "step_rewrite" => Some(Self::WorkflowStepRewrite),
            "selector_fix" | "selector" => Some(Self::SelectorFix),
            "tool_config_update" | "tool_config" => Some(Self::ToolConfigUpdate),
            "context_addition" | "context" => Some(Self::ContextAddition),
            "instruction_clarification" | "clarification" => Some(Self::InstructionClarification),
            "project_environment" | "proj_env" => Some(Self::ProjectEnvironment),
            "project_architecture" | "proj_arch" => Some(Self::ProjectArchitecture),
            "project_test_pattern" | "proj_test" => Some(Self::ProjectTestPattern),
            "project_recurring_issue" | "proj_issue" => Some(Self::ProjectRecurringIssue),
            "ui_bridge_discovery" | "ub_discovery" => Some(Self::UiBridgeDiscovery),
            "ui_bridge_snapshot" | "ub_snapshot" => Some(Self::UiBridgeSnapshot),
            "ui_bridge_action" | "ub_action" => Some(Self::UiBridgeAction),
            "ui_bridge_sdk_feature" | "ub_sdk_feature" => Some(Self::UiBridgeSdkFeature),
            "ui_bridge_prompt_guidance" | "ub_prompt" => Some(Self::UiBridgePromptGuidance),
            _ => None,
        }
    }

    /// Returns true if this is a project-scoped fix type (from project reflection).
    pub fn is_project_scoped(&self) -> bool {
        matches!(
            self,
            Self::ProjectEnvironment
                | Self::ProjectArchitecture
                | Self::ProjectTestPattern
                | Self::ProjectRecurringIssue
        )
    }

    /// Returns true if this is a UI Bridge-scoped fix type (from UI Bridge reflection).
    pub fn is_ui_bridge_scoped(&self) -> bool {
        matches!(
            self,
            Self::UiBridgeDiscovery
                | Self::UiBridgeSnapshot
                | Self::UiBridgeAction
                | Self::UiBridgeSdkFeature
                | Self::UiBridgePromptGuidance
        )
    }
}

impl fmt::Display for FixType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Status of a reflection fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixStatus {
    /// Fix has been applied
    Applied,
    /// Fix was reverted (caused issues)
    Reverted,
    /// Fix was replaced by a newer fix
    Superseded,
}

impl FixStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Reverted => "reverted",
            Self::Superseded => "superseded",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "applied" => Some(Self::Applied),
            "reverted" => Some(Self::Reverted),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

impl fmt::Display for FixStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Effectiveness evaluation result for a reflection fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixEffectiveness {
    /// The fix resolved the issue (did not recur)
    Effective,
    /// The fix did not resolve the issue (recurred)
    Ineffective,
    /// The fix introduced new problems
    CausedRegression,
    /// Not enough data to determine effectiveness
    Inconclusive,
}

impl FixEffectiveness {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Effective => "effective",
            Self::Ineffective => "ineffective",
            Self::CausedRegression => "caused_regression",
            Self::Inconclusive => "inconclusive",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "effective" => Some(Self::Effective),
            "ineffective" => Some(Self::Ineffective),
            "caused_regression" | "regression" => Some(Self::CausedRegression),
            "inconclusive" => Some(Self::Inconclusive),
            _ => None,
        }
    }
}

impl fmt::Display for FixEffectiveness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The exposure counts the fix evaluator (`evaluate_pending_fixes`) writes to
/// `effectiveness_evidence` as JSON beside every verdict it writes. One struct
/// is both the writer's shape and the readers' parse, so a reader cannot drift
/// from the keys the evaluator writes.
///
/// `effectiveness_evidence` is also free text when a person sets a verdict by
/// hand (`PUT /reflection-fixes/:id/effectiveness`), so a reader parses it with
/// [`describe_effectiveness_evidence`], which falls back to the raw text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExposureEvidence {
    /// The verdict these counts decided ([`FixEffectiveness::as_str`]).
    pub verdict: String,
    /// The window the counts were taken over, in SQL terms.
    pub window: String,
    /// Completed runs of the fix's workflow after the fix was applied.
    pub later_runs: i64,
    /// Later runs a fix needs before it is judged at all.
    pub min_later_runs: i64,
    /// The fix names the finding it addressed.
    pub source_finding: bool,
    /// That finding carries a signature, so a recurrence is observable.
    pub source_signature_known: bool,
    /// Later runs whose findings carry the source signature again.
    pub recurrences: i64,
    /// The most later runs any one signature new since the fix appeared in.
    pub new_signature_peak_runs: i64,
    /// Later runs a new signature needs to count as a regression.
    pub min_later_runs_for_regression: i64,
}

impl ExposureEvidence {
    /// One sentence naming the counts the verdict rests on — the readable
    /// form the reflection AI context and the UI show instead of the JSON.
    pub fn summary(&self) -> String {
        let runs = self.later_runs;
        let observability = if !self.source_finding {
            "the fix names no source finding, so a recurrence cannot be observed".to_string()
        } else if !self.source_signature_known {
            "its source finding carries no signature, so a recurrence cannot be observed"
                .to_string()
        } else {
            format!(
                "the source finding recurred in {} of them",
                self.recurrences
            )
        };
        format!(
            "{} after {} later run{} (needs {}): {}; the most later runs any new signature appeared in was {} (regression at {})",
            self.verdict,
            runs,
            if runs == 1 { "" } else { "s" },
            self.min_later_runs,
            observability,
            self.new_signature_peak_runs,
            self.min_later_runs_for_regression,
        )
    }
}

/// The readable form of a fix's `effectiveness_evidence`: the evaluator's
/// [`ExposureEvidence`] as its [`ExposureEvidence::summary`], anything else
/// (hand-set free text) unchanged.
pub fn describe_effectiveness_evidence(raw: &str) -> String {
    match serde_json::from_str::<ExposureEvidence>(raw) {
        Ok(evidence) => evidence.summary(),
        Err(_) => raw.to_string(),
    }
}

/// Aggregated effectiveness report for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivenessReport {
    pub workflow_name: String,
    pub total_fixes: u32,
    pub effective_count: u32,
    pub ineffective_count: u32,
    pub regression_count: u32,
    pub inconclusive_count: u32,
    pub unevaluated_count: u32,
    pub effectiveness_rate: f64,
    pub fixes: Vec<ReflectionFix>,
}

/// Input for creating a new reflection fix.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateReflectionFixInput {
    pub source_task_run_id: String,
    pub reflection_task_run_id: String,
    #[serde(default)]
    pub source_finding_id: Option<String>,
    #[serde(default)]
    pub source_knowledge_id: Option<String>,
    pub fix_type: String,
    pub fix_description: String,
    #[serde(default)]
    pub file_changed: Option<String>,
    #[serde(default)]
    pub old_value: Option<String>,
    #[serde(default)]
    pub new_value: Option<String>,
    #[serde(default = "default_confidence")]
    pub confidence: String,
    /// Which generation agent's output likely caused this issue
    #[serde(default)]
    pub source_agent: Option<String>,
    /// Root cause diagnosis and reasoning behind the fix
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Other approaches considered and why they were rejected
    #[serde(default)]
    pub alternatives_considered: Option<String>,
}

fn default_confidence() -> String {
    "medium".to_string()
}

/// Input for updating fix effectiveness.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateEffectivenessInput {
    pub effectiveness: String,
    #[serde(default)]
    pub effectiveness_evidence: Option<String>,
}

/// Input for updating fix status.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFixStatusInput {
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(source_finding: bool, signature: bool, later: i64, recur: i64) -> ExposureEvidence {
        ExposureEvidence {
            verdict: "ineffective".to_string(),
            window: "task_runs.completed_at > reflection_fixes.applied_at".to_string(),
            later_runs: later,
            min_later_runs: 3,
            source_finding,
            source_signature_known: signature,
            recurrences: recur,
            new_signature_peak_runs: 1,
            min_later_runs_for_regression: 2,
        }
    }

    #[test]
    fn evaluator_evidence_reads_as_the_counts_it_rests_on() {
        let raw = serde_json::to_string(&evidence(true, true, 5, 2)).unwrap();
        assert_eq!(
            describe_effectiveness_evidence(&raw),
            "ineffective after 5 later runs (needs 3): the source finding recurred in 2 of them; \
             the most later runs any new signature appeared in was 1 (regression at 2)"
        );
    }

    #[test]
    fn an_unobservable_recurrence_says_why() {
        assert!(evidence(false, false, 3, 0)
            .summary()
            .contains("names no source finding"));
        assert!(evidence(true, false, 3, 0)
            .summary()
            .contains("carries no signature"));
        assert!(evidence(true, true, 1, 0)
            .summary()
            .contains("after 1 later run "));
    }

    #[test]
    fn hand_set_or_partial_evidence_is_shown_unchanged() {
        for raw in [
            "Issue recurred in run 42",
            r#"{"verdict":"effective","later_runs":3}"#,
            "",
        ] {
            assert_eq!(describe_effectiveness_evidence(raw), raw);
        }
    }
}
