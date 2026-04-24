//! Inter-Agent Schema Contracts (Phase 5)
//!
//! Defines typed input/output contracts per agent role in the multi-agent pipeline.
//! These contract types formalize the data flows between pipeline agents:
//!
//! - **Spec Analyst** produces acceptance criteria with complexity assessment
//! - **Locator** maps criteria to code locations with dependency information
//! - **Implementer** produces file changes with reasoning and confidence
//! - **Verifier** produces verification results with pass/fail status
//!
//! Contract types wrap and mirror the existing pipeline types
//! (`PipelineAcceptanceCriterion`, `LocatedCriterion`, `ImplementerChange`,
//! `VerifierFailure`) rather than replacing them, ensuring backward compatibility.
//!
//! ## Usage
//!
//! Contract types are populated by `ContractValidator` after validating raw JSON
//! agent output against the corresponding JSON Schema. They are stored as optional
//! typed fields on `PipelineContext` alongside the existing ad-hoc fields.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agentic_verification::{CodeLocation, LocatedCriterion, PipelineAcceptanceCriterion};
use crate::orchestrator::structured_output::ConfidenceLevel;
use crate::unified_workflow_executor::agentic_output::FileChange;
use crate::unified_workflow_executor::multi_agent_pipeline_loop::{
    ImplementerChange, VerifierFailure,
};
use crate::workflow_generation::complexity::ComplexityLevel;

// ============================================================================
// Agent Role Identifier
// ============================================================================

/// Identifies a pipeline agent role for contract lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRole {
    SpecAnalyst,
    Locator,
    Implementer,
    Verifier,
}

impl AgentRole {
    /// String key used in pipeline traces and configuration.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SpecAnalyst => "spec_analyst",
            Self::Locator => "locator",
            Self::Implementer => "implementer",
            Self::Verifier => "verifier",
        }
    }

    /// Parse from a string key (e.g. from pipeline trace `agent_type`).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "spec_analyst" => Some(Self::SpecAnalyst),
            "locator" => Some(Self::Locator),
            "implementer" => Some(Self::Implementer),
            "verifier" => Some(Self::Verifier),
            _ => None,
        }
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Spec Analyst Contract
// ============================================================================

/// Output contract for the Spec Analyst agent.
///
/// The Spec Analyst decomposes a task specification into structured acceptance
/// criteria with dependency ordering and complexity assessment. This contract
/// formalizes the output that downstream agents (Locator, Implementer) consume.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpecAnalystOutput {
    /// Structured acceptance criteria extracted from the specification.
    /// Each criterion has a unique ID, description, verification method,
    /// and dependency pointers for DAG construction.
    pub criteria: Vec<PipelineAcceptanceCriterion>,

    /// Overall complexity assessment of the task.
    pub complexity_assessment: ComplexityLevel,

    /// Number of independent dependency subtrees identified.
    /// Informs the pipeline about potential parallelism.
    #[serde(default)]
    pub subtree_count_estimate: u32,

    /// Free-form rationale for the complexity assessment.
    #[serde(default)]
    pub complexity_rationale: String,
}

// ============================================================================
// Locator Contract
// ============================================================================

/// A file dependency edge identified by the Locator agent.
///
/// Represents a directional dependency between two files, e.g.
/// "component.tsx imports types from types.ts".
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileDependency {
    /// Source file path (the file that depends on another).
    pub from_path: String,
    /// Target file path (the file being depended on).
    pub to_path: String,
    /// Nature of the dependency: "import", "type_reference", "runtime_call", "test_for".
    pub dependency_type: String,
}

/// Output contract for the Locator agent.
///
/// The Locator maps acceptance criteria to concrete code locations (files,
/// components, line ranges). This contract formalizes the location data
/// that the Implementer agent uses to target its changes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LocatorOutput {
    /// Criteria with their located code targets.
    /// Each entry maps one acceptance criterion to its target and related files.
    pub located_criteria: Vec<LocatedCriterion>,

    /// File dependency graph edges discovered during location analysis.
    /// Used to detect cross-file impact and inform change ordering.
    #[serde(default)]
    pub file_dependency_graph: Vec<FileDependency>,

    /// Files that were analyzed but not matched to any criterion.
    /// Useful for debugging locator coverage gaps.
    #[serde(default)]
    pub unmatched_files_scanned: u32,
}

// ============================================================================
// Implementer Contract
// ============================================================================

/// Output contract for the Implementer agent.
///
/// The Implementer makes code changes to fulfill acceptance criteria assigned
/// to it. This contract formalizes the change record, including which files
/// were modified, reasoning, and confidence level.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImplementerOutput {
    /// Structured change records per subtree/level/attempt.
    /// Mirrors the existing `ImplementerChange` tracking in `PipelineContext`.
    pub changes: Vec<ImplementerChange>,

    /// File-level changes made (paths and actions).
    /// Complements `changes` with concrete file paths for the Verifier.
    #[serde(default)]
    pub file_changes: Vec<FileChange>,

    /// Free-form reasoning explaining the implementation approach.
    #[serde(default)]
    pub reasoning: String,

    /// Implementer's confidence in the correctness of its changes.
    #[serde(default)]
    pub confidence: ConfidenceLevel,

    /// Criteria IDs that the implementer believes are now satisfied.
    #[serde(default)]
    pub satisfied_criteria: Vec<String>,

    /// Criteria IDs that could not be addressed (with reasons in `reasoning`).
    #[serde(default)]
    pub unaddressed_criteria: Vec<String>,
}

// ============================================================================
// Verifier Contract
// ============================================================================

/// A single verification result for one acceptance criterion.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VerificationResult {
    /// The criterion ID being verified.
    pub criterion_id: String,
    /// Whether verification passed.
    pub passed: bool,
    /// Verification method used (matches criterion's `verification_method`).
    pub method: String,
    /// Details if failed (empty or absent if passed).
    #[serde(default)]
    pub failure_details: String,
    /// Confidence in the verification result (for AI-evaluated criteria).
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// Output contract for the Verifier agent.
///
/// The Verifier checks whether acceptance criteria are satisfied after the
/// Implementer has made its changes. This contract formalizes the verification
/// results, including per-criterion pass/fail status and failure details.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VerifierOutput {
    /// Per-criterion verification results.
    pub results: Vec<VerificationResult>,

    /// Whether all criteria passed verification.
    pub overall_pass: bool,

    /// Detailed failure records (mirrors `VerifierFailure` in `PipelineContext`).
    /// Contains richer failure context than the boolean in `results`.
    #[serde(default)]
    pub failure_details: Vec<VerifierFailure>,

    /// Number of criteria that passed.
    #[serde(default)]
    pub pass_count: u32,

    /// Number of criteria that failed.
    #[serde(default)]
    pub fail_count: u32,
}

// ============================================================================
// Conversion Helpers
// ============================================================================

impl SpecAnalystOutput {
    /// Build from existing pipeline context spec results.
    pub fn from_pipeline_data(
        criteria: Vec<PipelineAcceptanceCriterion>,
        complexity: ComplexityLevel,
    ) -> Self {
        let subtree_estimate = estimate_subtree_count(&criteria);
        Self {
            criteria,
            complexity_assessment: complexity,
            subtree_count_estimate: subtree_estimate,
            complexity_rationale: String::new(),
        }
    }
}

impl LocatorOutput {
    /// Build from existing pipeline context locator results.
    pub fn from_pipeline_data(located_criteria: Vec<LocatedCriterion>) -> Self {
        Self {
            located_criteria,
            file_dependency_graph: Vec::new(),
            unmatched_files_scanned: 0,
        }
    }
}

impl ImplementerOutput {
    /// Build from existing pipeline context implementer changes.
    pub fn from_pipeline_data(changes: Vec<ImplementerChange>) -> Self {
        let satisfied: Vec<String> = changes
            .iter()
            .filter(|c| c.success)
            .flat_map(|c| c.criteria_ids.clone())
            .collect();
        Self {
            changes,
            file_changes: Vec::new(),
            reasoning: String::new(),
            confidence: ConfidenceLevel::Medium,
            satisfied_criteria: satisfied,
            unaddressed_criteria: Vec::new(),
        }
    }
}

impl VerifierOutput {
    /// Build from existing pipeline context verifier failures and the full criteria list.
    pub fn from_pipeline_data(failures: &[VerifierFailure], all_criteria_ids: &[String]) -> Self {
        let failed_ids: std::collections::HashSet<&str> =
            failures.iter().map(|f| f.criterion_id.as_str()).collect();

        let results: Vec<VerificationResult> = all_criteria_ids
            .iter()
            .map(|id| {
                let passed = !failed_ids.contains(id.as_str());
                let failure = failures.iter().find(|f| f.criterion_id == *id);
                VerificationResult {
                    criterion_id: id.clone(),
                    passed,
                    method: failure.map(|f| f.method.clone()).unwrap_or_default(),
                    failure_details: failure.map(|f| f.details.clone()).unwrap_or_default(),
                    confidence: None,
                }
            })
            .collect();

        let pass_count = results.iter().filter(|r| r.passed).count() as u32;
        let fail_count = results.iter().filter(|r| !r.passed).count() as u32;
        let overall_pass = fail_count == 0;

        Self {
            results,
            overall_pass,
            failure_details: failures.to_vec(),
            pass_count,
            fail_count,
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Estimate the number of independent subtrees from criteria dependencies.
fn estimate_subtree_count(criteria: &[PipelineAcceptanceCriterion]) -> u32 {
    if criteria.is_empty() {
        return 0;
    }
    // Simple heuristic: criteria with no dependencies that are not depended on
    // by anything form their own subtrees. For a real count, use the DAG builder.
    let all_ids: std::collections::HashSet<&str> = criteria.iter().map(|c| c.id.as_str()).collect();
    let depended_on: std::collections::HashSet<&str> = criteria
        .iter()
        .flat_map(|c| c.depends_on.iter())
        .filter(|d| all_ids.contains(d.as_str()))
        .map(|d| d.as_str())
        .collect();

    // Count root criteria (no internal dependencies pointing to them that also have no deps)
    let roots = criteria
        .iter()
        .filter(|c| c.depends_on.iter().all(|d| !all_ids.contains(d.as_str())))
        .count();

    // Rough estimate: each root with no dependents is an independent subtree
    let independent = criteria
        .iter()
        .filter(|c| {
            c.depends_on.iter().all(|d| !all_ids.contains(d.as_str()))
                && !depended_on.contains(c.id.as_str())
        })
        .count();

    // At least 1 subtree, and connected roots form fewer subtrees than total roots
    std::cmp::max(1, if independent > 0 { independent } else { roots }) as u32
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_criterion(id: &str, deps: Vec<String>) -> PipelineAcceptanceCriterion {
        PipelineAcceptanceCriterion {
            id: id.to_string(),
            spec_assertion_id: format!("assert_{}", id),
            spec_group_id: "group_1".to_string(),
            description: format!("Test criterion {}", id),
            criterion_type: "deterministic".to_string(),
            verification_method: "test_output".to_string(),
            depends_on: deps,
            target_elements: vec![],
            estimated_complexity: "simple".to_string(),
            severity: "major".to_string(),
            enabled: true,
        }
    }

    #[test]
    fn test_spec_analyst_output_schema_generation() {
        let schema = schemars::schema_for!(SpecAnalystOutput);
        let json = serde_json::to_value(&schema).unwrap();
        // Should have properties for criteria, complexity_assessment, etc.
        assert!(json.get("properties").is_some() || json.get("$defs").is_some());
    }

    #[test]
    fn test_spec_analyst_output_roundtrip() {
        let output = SpecAnalystOutput::from_pipeline_data(
            vec![
                make_criterion("c1", vec![]),
                make_criterion("c2", vec!["c1".into()]),
            ],
            ComplexityLevel::Moderate,
        );
        let json = serde_json::to_value(&output).unwrap();
        let parsed: SpecAnalystOutput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.criteria.len(), 2);
        assert_eq!(parsed.complexity_assessment, ComplexityLevel::Moderate);
    }

    #[test]
    fn test_locator_output_roundtrip() {
        let output = LocatorOutput::from_pipeline_data(vec![]);
        let json = serde_json::to_value(&output).unwrap();
        let parsed: LocatorOutput = serde_json::from_value(json).unwrap();
        assert!(parsed.located_criteria.is_empty());
    }

    #[test]
    fn test_implementer_output_from_pipeline_data() {
        let changes = vec![ImplementerChange {
            subtree_id: "s0".into(),
            level: 0,
            attempt: 1,
            criteria_ids: vec!["c1".into(), "c2".into()],
            success: true,
            tokens_in: 100,
            tokens_out: 50,
        }];
        let output = ImplementerOutput::from_pipeline_data(changes);
        assert_eq!(output.satisfied_criteria, vec!["c1", "c2"]);
        assert!(output.unaddressed_criteria.is_empty());
    }

    #[test]
    fn test_verifier_output_from_pipeline_data() {
        let failures = vec![VerifierFailure {
            criterion_id: "c2".into(),
            method: "test_output".into(),
            details: "assertion failed".into(),
            attempt: 1,
        }];
        let all_ids = vec!["c1".into(), "c2".into(), "c3".into()];
        let output = VerifierOutput::from_pipeline_data(&failures, &all_ids);
        assert!(!output.overall_pass);
        assert_eq!(output.pass_count, 2);
        assert_eq!(output.fail_count, 1);
        assert_eq!(output.results.len(), 3);
    }

    #[test]
    fn test_verifier_output_all_pass() {
        let output = VerifierOutput::from_pipeline_data(&[], &["c1".into()]);
        assert!(output.overall_pass);
        assert_eq!(output.pass_count, 1);
        assert_eq!(output.fail_count, 0);
    }

    #[test]
    fn test_estimate_subtree_count() {
        // Two independent criteria -> 2 subtrees
        let criteria = vec![make_criterion("a", vec![]), make_criterion("b", vec![])];
        assert_eq!(estimate_subtree_count(&criteria), 2);

        // Two dependent criteria -> 1 subtree
        let criteria = vec![
            make_criterion("a", vec![]),
            make_criterion("b", vec!["a".into()]),
        ];
        assert_eq!(estimate_subtree_count(&criteria), 1);

        // Empty -> 0
        assert_eq!(estimate_subtree_count(&[]), 0);
    }

    #[test]
    fn test_agent_role_roundtrip() {
        for role in [
            AgentRole::SpecAnalyst,
            AgentRole::Locator,
            AgentRole::Implementer,
            AgentRole::Verifier,
        ] {
            let s = role.as_str();
            let parsed = AgentRole::from_str(s).unwrap();
            assert_eq!(parsed, role);
        }
        assert!(AgentRole::from_str("unknown").is_none());
    }
}
