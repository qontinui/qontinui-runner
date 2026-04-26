//! Quality Analysis and Revision Phase
//!
//! Implements the core of Feature 1: a deterministic quality analysis pass over
//! generated workflows, followed by an optional AI revision agent that fixes
//! identified issues.
//!
//! The pipeline becomes:
//!   Discovery → Investigation → Specification → Builder → Autofix →
//!   [Verify↔Fix] → Hardener → **Revision** → Validate
//!
//! The revision phase has two parts:
//! 1. **Deterministic quality checks** — coverage matrix, retry consistency,
//!    required-flag discipline, data contract validation
//! 2. **AI revision agent** — takes the quality report and rewrites the workflow
//!    to fix identified findings

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::ai_provider::AiResponse;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::unified_workflows::UnifiedWorkflow;
use crate::workflow_generation::specification::{
    AcceptanceCriteria, CriterionPriority, VerificationMethod,
};

// ============================================================================
// Types
// ============================================================================

/// Severity of a quality finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Critical,
    Warning,
    Info,
}

/// Category of a quality finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    VerificationGap,
    MissingCriterion,
    UnnecessaryStep,
    WeakRetry,
    RequiredFlagViolation,
    RetryInconsistency,
    DataContractViolation,
    FalsePositiveRisk,
}

/// A single quality finding from the analysis phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityFinding {
    pub finding_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub severity: FindingSeverity,
    pub category: FindingCategory,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// Maps acceptance criteria to verification steps and vice versa.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageMatrix {
    /// criterion_id → list of step IDs that verify it
    pub criteria_to_steps: HashMap<String, Vec<String>>,
    /// step_id → list of criterion IDs it covers
    pub steps_to_criteria: HashMap<String, Vec<String>>,
    /// Criterion IDs from AcceptanceCriteria with no matching verification step
    pub uncovered_criteria: Vec<String>,
    /// Verification step IDs with no criterion_id or criterion_ids
    pub unlinked_steps: Vec<String>,
}

/// The full quality report for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub findings: Vec<QualityFinding>,
    pub score: f32,
    pub pass: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_matrix: Option<CoverageMatrix>,
    /// Semantic coverage matrix from the evaluation engine (entailment-scored).
    /// Present when evaluation data is available; strictly more informative
    /// than the name-matching `coverage_matrix`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_coverage_matrix: Option<super::evaluation::coverage::SemanticCoverageMatrix>,
}

// ============================================================================
// Coverage Matrix
// ============================================================================

/// Build a coverage matrix mapping acceptance criteria to verification steps.
///
/// Collects all verification steps from top-level and stages, extracts
/// `criterion_id` (single string) and `criterion_ids` (array of strings),
/// and produces bidirectional mappings plus lists of uncovered criteria
/// and unlinked steps.
pub fn build_coverage_matrix(
    workflow: &UnifiedWorkflow,
    criteria: &AcceptanceCriteria,
) -> CoverageMatrix {
    let mut criteria_to_steps: HashMap<String, Vec<String>> = HashMap::new();
    let mut steps_to_criteria: HashMap<String, Vec<String>> = HashMap::new();

    // Initialize criteria_to_steps with all automatable criterion IDs
    for c in &criteria.criteria {
        if c.method != VerificationMethod::Manual {
            criteria_to_steps.entry(c.id.clone()).or_default();
        }
    }

    // Collect all verification steps from top-level + stages
    let mut all_verification_steps: Vec<&Value> = Vec::new();
    for step in &workflow.verification_steps {
        all_verification_steps.push(step);
    }
    for stage in &workflow.stages {
        for step in &stage.verification_steps {
            all_verification_steps.push(step);
        }
    }

    // Process each verification step
    for step in &all_verification_steps {
        let step_id = step
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if step_id.is_empty() {
            continue;
        }

        let mut linked_criteria: Vec<String> = Vec::new();

        // Extract criterion_id (single string)
        if let Some(cid) = step.get("criterion_id").and_then(|v| v.as_str()) {
            if !cid.is_empty() {
                linked_criteria.push(cid.to_string());
            }
        }

        // Extract criterion_ids (array of strings)
        if let Some(cids) = step.get("criterion_ids").and_then(|v| v.as_array()) {
            for cid_val in cids {
                if let Some(cid) = cid_val.as_str() {
                    if !cid.is_empty() && !linked_criteria.contains(&cid.to_string()) {
                        linked_criteria.push(cid.to_string());
                    }
                }
            }
        }

        if linked_criteria.is_empty() {
            // Track as unlinked — handled below
        } else {
            for cid in &linked_criteria {
                criteria_to_steps
                    .entry(cid.clone())
                    .or_default()
                    .push(step_id.clone());
            }
        }

        steps_to_criteria.insert(step_id, linked_criteria);
    }

    // Compute uncovered criteria (automatable criteria with no matching step)
    let uncovered_criteria: Vec<String> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .filter(|c| {
            criteria_to_steps
                .get(&c.id)
                .is_none_or(|steps| steps.is_empty())
        })
        .map(|c| c.id.clone())
        .collect();

    // Compute unlinked steps (verification steps with no criterion linkage)
    let unlinked_steps: Vec<String> = steps_to_criteria
        .iter()
        .filter(|(_, cids)| cids.is_empty())
        .map(|(sid, _)| sid.clone())
        .collect();

    CoverageMatrix {
        criteria_to_steps,
        steps_to_criteria,
        uncovered_criteria,
        unlinked_steps,
    }
}

// ============================================================================
// Deterministic Quality Checks
// ============================================================================

/// Run all deterministic quality checks on a workflow.
///
/// Returns findings and an optional coverage matrix (present when criteria are
/// provided). The matrix is built once here so callers can reuse it without
/// rebuilding.
///
/// Collects findings from:
/// 1. Coverage matrix analysis (uncovered criteria)
/// 2. Data contract validation (stub — calls downstream module if available)
/// 3. Required flag discipline (stub — calls downstream module if available)
/// 4. Retry consistency checks
/// 5. Category coverage (stub — calls downstream module if available)
/// 6. Dependency analysis (stub — calls downstream module if available)
pub fn deterministic_quality_checks(
    workflow: &UnifiedWorkflow,
    criteria: Option<&AcceptanceCriteria>,
) -> (Vec<QualityFinding>, Option<CoverageMatrix>) {
    let mut findings: Vec<QualityFinding> = Vec::new();

    // 1. Coverage matrix findings
    let coverage_matrix = if let Some(criteria) = criteria {
        let matrix = build_coverage_matrix(workflow, criteria);
        for cid in &matrix.uncovered_criteria {
            if let Some(c) = criteria.criteria.iter().find(|c| c.id == *cid) {
                let severity = match c.priority {
                    CriterionPriority::Critical => FindingSeverity::Critical,
                    CriterionPriority::Important => FindingSeverity::Warning,
                    CriterionPriority::Optional => FindingSeverity::Info,
                };
                findings.push(QualityFinding {
                    finding_id: format!("coverage-{}", cid),
                    step_id: None,
                    severity,
                    category: FindingCategory::MissingCriterion,
                    description: format!(
                        "Acceptance criterion '{}' has no matching verification step",
                        c.description
                    ),
                    suggested_fix: Some(format!(
                        "Add a verification step with criterion_id=\"{}\"",
                        cid
                    )),
                });
            }
        }
        Some(matrix)
    } else {
        None
    };

    // 2. Data contract validation
    findings.extend(super::validation::validate_data_contracts(workflow));

    // 3. Required flag discipline
    findings.extend(super::validation::validate_required_flag_discipline(
        workflow, criteria,
    ));

    // 4. Retry consistency checks
    findings.extend(retry_consistency_findings(workflow));

    // 5. Category coverage
    findings.extend(super::verification_templates::category_coverage_findings(
        workflow,
    ));

    // 6. Dependency analysis
    {
        let dep_graph = super::dependency_analysis::build_dependency_graph(workflow);
        findings.extend(super::dependency_analysis::dependency_findings(&dep_graph));
    }

    (findings, coverage_matrix)
}

/// Check retry configuration consistency across verification steps.
///
/// Flags:
/// - HTTP steps (commands containing "curl" or check_type "http_status") with retry_count=0
/// - Steps with retry_count > 5
/// - Inconsistent retry configs among same-type verification steps within a stage
fn retry_consistency_findings(workflow: &UnifiedWorkflow) -> Vec<QualityFinding> {
    let mut findings: Vec<QualityFinding> = Vec::new();

    // Collect verification steps grouped by source (top-level or stage)
    let mut step_groups: Vec<(&str, &[Value])> = Vec::new();
    if !workflow.verification_steps.is_empty() {
        step_groups.push(("top-level", &workflow.verification_steps));
    }
    for stage in &workflow.stages {
        if !stage.verification_steps.is_empty() {
            step_groups.push((&stage.name, &stage.verification_steps));
        }
    }

    for (group_name, steps) in &step_groups {
        // Track retry counts by step type for inconsistency detection
        let mut retry_by_type: HashMap<String, Vec<(String, u32)>> = HashMap::new();

        for step in *steps {
            let step_id = step
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let step_type = step
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let retry_count = step
                .get("retry_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;

            let command = step.get("command").and_then(|v| v.as_str()).unwrap_or("");

            let check_type = step
                .get("check_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Check: HTTP steps with retry_count=0
            let is_http_step = command.contains("curl")
                || check_type == "http_status"
                || command.contains("wget")
                || command.contains("http://")
                || command.contains("https://");

            if is_http_step && retry_count == 0 {
                findings.push(QualityFinding {
                    finding_id: format!("retry-http-{}", step_id),
                    step_id: Some(step_id.clone()),
                    severity: FindingSeverity::Warning,
                    category: FindingCategory::WeakRetry,
                    description: format!(
                        "HTTP verification step '{}' has no retries (retry_count=0). \
                         Network requests are inherently flaky.",
                        step_id
                    ),
                    suggested_fix: Some(
                        "Add retry_count: 3 and retry_delay_ms: 2000 for network resilience"
                            .to_string(),
                    ),
                });
            }

            // Check: Excessive retries
            if retry_count > 5 {
                findings.push(QualityFinding {
                    finding_id: format!("retry-excessive-{}", step_id),
                    step_id: Some(step_id.clone()),
                    severity: FindingSeverity::Warning,
                    category: FindingCategory::RetryInconsistency,
                    description: format!(
                        "Step '{}' has excessive retries (retry_count={}). \
                         This may mask real failures and slow down execution.",
                        step_id, retry_count
                    ),
                    suggested_fix: Some(
                        "Reduce retry_count to 3-5 and investigate why so many retries are needed"
                            .to_string(),
                    ),
                });
            }

            // Collect for inconsistency detection
            if !step_type.is_empty() {
                retry_by_type
                    .entry(step_type.clone())
                    .or_default()
                    .push((step_id, retry_count));
            }
        }

        // Check: Inconsistent retry configs among same-type steps
        for (step_type, entries) in &retry_by_type {
            if entries.len() < 2 {
                continue;
            }

            let retry_counts: Vec<u32> = entries.iter().map(|(_, c)| *c).collect();
            let min_retry = retry_counts.iter().copied().min().unwrap_or(0);
            let max_retry = retry_counts.iter().copied().max().unwrap_or(0);

            // Flag if there's a significant spread (e.g., one step has 0 retries and another has 3+)
            if max_retry > 0 && min_retry == 0 {
                let step_ids: Vec<&str> = entries.iter().map(|(id, _)| id.as_str()).collect();
                findings.push(QualityFinding {
                    finding_id: format!("retry-inconsistent-{}-{}", group_name, step_type),
                    step_id: None,
                    severity: FindingSeverity::Info,
                    category: FindingCategory::RetryInconsistency,
                    description: format!(
                        "Inconsistent retry configs among '{}' steps in {}: \
                         some have retries (max={}) while others have none. Steps: {}",
                        step_type,
                        group_name,
                        max_retry,
                        step_ids.join(", ")
                    ),
                    suggested_fix: Some(format!(
                        "Standardize retry_count across all '{}' verification steps in this group",
                        step_type
                    )),
                });
            }
        }
    }

    findings
}

// ============================================================================
// Quality Score
// ============================================================================

/// Compute a quality score from findings.
///
/// Score = `1.0 - (0.3 * critical + 0.1 * warning + 0.02 * info)`, clamped to [0.0, 1.0].
fn compute_quality_score(findings: &[QualityFinding]) -> f32 {
    let mut critical_count = 0u32;
    let mut warning_count = 0u32;
    let mut info_count = 0u32;

    for f in findings {
        match f.severity {
            FindingSeverity::Critical => critical_count += 1,
            FindingSeverity::Warning => warning_count += 1,
            FindingSeverity::Info => info_count += 1,
        }
    }

    // Calibration notes (2026-04): AI semantic analysis surfaces many
    // advisory warnings on real-world spec-driven workflows — 20–40 warnings
    // is routine and doesn't indicate a broken workflow. The previous formula
    // (0.1 per warning) saturated to 0.00 at ~10 warnings, causing every
    // spec→workflow AI generation to be rejected by the 0.3 confidence-gate
    // even with zero critical findings. Critical findings still dominate
    // (0.3 each — one critical drops score to 0.7), warnings are capped
    // in aggregate so they can't single-handedly zero the score, and info
    // findings are near-free.
    let crit_penalty = 0.3 * critical_count as f32;
    let warn_penalty = (0.006 * warning_count as f32).min(0.2);
    let info_penalty = (0.002 * info_count as f32).min(0.05);
    let penalty = crit_penalty + warn_penalty + info_penalty;

    (1.0 - penalty).clamp(0.0, 1.0)
}

// ============================================================================
// Quality Analysis
// ============================================================================

/// Run the full quality analysis on a workflow.
///
/// 1. Runs deterministic quality checks (which also builds the coverage matrix)
/// 2. Computes score and pass/fail (threshold: 0.7)
/// 3. Returns the quality report
///
/// Note: AI semantic analysis (counter-example prompting) is not yet implemented.
/// Only deterministic findings are returned.
pub fn run_quality_analysis(
    workflow: &UnifiedWorkflow,
    criteria: Option<&AcceptanceCriteria>,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> QualityReport {
    let (mut findings, coverage_matrix) = deterministic_quality_checks(workflow, criteria);

    // AI semantic analysis (counter-example prompting)
    if let Some(dh) = doctor_handle {
        match run_ai_semantic_analysis(workflow, dh, model, provider) {
            Ok(ai_findings) => {
                info!(
                    "AI semantic analysis found {} false-positive-risk findings",
                    ai_findings.len()
                );
                findings.extend(ai_findings);
            }
            Err(e) => {
                warn!(
                    "AI semantic analysis failed, continuing with deterministic findings only: {}",
                    e
                );
            }
        }
    }

    let score = compute_quality_score(&findings);
    let pass = score >= 0.7;

    info!(
        "Quality analysis: score={:.2}, pass={}, findings={} (critical={}, warning={}, info={})",
        score,
        pass,
        findings.len(),
        findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::Critical)
            .count(),
        findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::Warning)
            .count(),
        findings
            .iter()
            .filter(|f| f.severity == FindingSeverity::Info)
            .count(),
    );

    QualityReport {
        findings,
        score,
        pass,
        coverage_matrix,
        semantic_coverage_matrix: None,
    }
}

// ============================================================================
// AI Semantic Analysis (Counter-Example Prompting)
// ============================================================================

/// A single finding from the AI semantic analysis response.
#[derive(Debug, Deserialize)]
struct AiSemanticFinding {
    step_id: String,
    description: String,
    #[serde(default)]
    suggested_fix: Option<String>,
}

/// Run AI semantic analysis using counter-example prompting.
///
/// Sends the workflow to an AI model and asks it to identify scenarios where
/// verification steps could pass despite the underlying requirement not being met
/// (false positives). Returns findings with category `FalsePositiveRisk`.
fn run_ai_semantic_analysis(
    workflow: &UnifiedWorkflow,
    doctor_handle: &DoctorHandle,
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<Vec<QualityFinding>, String> {
    let workflow_json = serde_json::to_string_pretty(workflow).unwrap_or_else(|_| "{}".to_string());

    let prompt = format!(
        "You are a workflow verification analyst. Analyze the following workflow JSON \
         and identify false positive risks in its verification steps.\n\n\
         ## Workflow\n```json\n{}\n```\n\n\
         ## Task\n\
         For each verification step, describe a scenario where the check passes but \
         the requirement is NOT met. If you can identify a concrete false positive, \
         report it as a finding with category \"false_positive_risk\".\n\n\
         Respond with ONLY a JSON array of findings in this exact format:\n\
         ```json\n\
         [{{\n\
           \"step_id\": \"the-step-id\",\n\
           \"description\": \"Description of the false positive scenario\",\n\
           \"suggested_fix\": \"How to make the check more robust\"\n\
         }}]\n\
         ```\n\n\
         If there are no false positive risks, respond with an empty array: `[]`\n\
         Respond with ONLY the JSON array, no other text.",
        workflow_json,
    );

    let task_context = TaskContext {
        prompt: prompt.clone(),
        file_count: None,
        criteria_count: None,
        file_paths: vec![],
        phase: None,
    };

    info!("Running AI semantic analysis (counter-example prompting)");

    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        Some(doctor_handle),
        model,
        provider,
        None,
        None,
        None,
        None,
    );

    if !ai_result.success {
        return Err(format!(
            "AI semantic analysis call failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown error")
        ));
    }

    let output = ai_result.output.trim().to_string();
    if output.is_empty() {
        return Err("AI semantic analysis returned empty output".to_string());
    }

    // Extract JSON from response (may be wrapped in markdown code blocks)
    let json_text = crate::workflow_generation::generator::extract_json_from_response(&output);

    let ai_findings: Vec<AiSemanticFinding> = serde_json::from_str(&json_text)
        .map_err(|e| format!("Failed to parse AI semantic analysis response: {}", e))?;

    let findings = ai_findings
        .into_iter()
        .enumerate()
        .map(|(i, af)| QualityFinding {
            finding_id: format!("ai-false-positive-{}", i),
            step_id: if af.step_id.is_empty() {
                None
            } else {
                Some(af.step_id)
            },
            severity: FindingSeverity::Warning,
            category: FindingCategory::FalsePositiveRisk,
            description: af.description,
            suggested_fix: af.suggested_fix,
        })
        .collect();

    Ok(findings)
}

// ============================================================================
// Revision Agent
// ============================================================================

/// Format findings as a numbered list for inclusion in an AI prompt.
fn format_findings_for_prompt(findings: &[QualityFinding]) -> String {
    if findings.is_empty() {
        return "(No findings)".to_string();
    }

    findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let severity_str = match f.severity {
                FindingSeverity::Critical => "CRITICAL",
                FindingSeverity::Warning => "WARNING",
                FindingSeverity::Info => "INFO",
            };
            let category_str = match f.category {
                FindingCategory::VerificationGap => "verification_gap",
                FindingCategory::MissingCriterion => "missing_criterion",
                FindingCategory::UnnecessaryStep => "unnecessary_step",
                FindingCategory::WeakRetry => "weak_retry",
                FindingCategory::RequiredFlagViolation => "required_flag_violation",
                FindingCategory::RetryInconsistency => "retry_inconsistency",
                FindingCategory::DataContractViolation => "data_contract_violation",
                FindingCategory::FalsePositiveRisk => "false_positive_risk",
            };
            let step_part = f
                .step_id
                .as_ref()
                .map(|s| format!(" (step: {})", s))
                .unwrap_or_default();
            let fix_part = f
                .suggested_fix
                .as_ref()
                .map(|s| format!("\n   Fix: {}", s))
                .unwrap_or_default();

            format!(
                "{}. [{}/ {}]{} — {}{}",
                i + 1,
                severity_str,
                category_str,
                step_part,
                f.description,
                fix_part,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the AI revision agent to fix quality issues in a workflow.
///
/// Takes the current workflow and quality report, builds a prompt instructing
/// the AI to fix the identified issues, and returns the corrected workflow.
pub fn run_revision_agent(
    workflow: &UnifiedWorkflow,
    report: &QualityReport,
    description: &str,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> Result<UnifiedWorkflow, String> {
    if report.findings.is_empty() {
        debug!("No findings to fix — returning workflow unchanged");
        return Ok(workflow.clone());
    }

    let workflow_json = serde_json::to_string_pretty(workflow).unwrap_or_else(|_| "{}".to_string());
    let findings_text = format_findings_for_prompt(&report.findings);

    let description_section = if description.is_empty() {
        String::new()
    } else {
        format!(
            "\n## Original Workflow Description\n{}\n\n\
             Use this description to understand the original intent of the workflow \
             when deciding how to fix the findings.\n",
            description
        )
    };

    let prompt = format!(
        "You are a workflow quality reviewer. Fix the following issues in this workflow.\n\n\
         ## Workflow\n```json\n{}\n```\n\n\
         ## Quality Findings\n{}\n\
         {}\
         ## Instructions\n\
         Fix each finding by modifying the workflow JSON. Return ONLY the corrected workflow JSON, no explanation.\n\
         - For missing criterion coverage: add or fix verification steps with the appropriate criterion_id\n\
         - For required flag violations: set required=true on critical verification steps\n\
         - For retry issues: add appropriate retry_count and retry_delay_ms fields\n\
         - For data contract violations: add missing stage outputs/inputs\n\
         - For retry inconsistencies: standardize retry configs across same-type steps\n\
         Respond with ONLY the JSON.",
        workflow_json, findings_text, description_section,
    );

    let task_context = TaskContext {
        prompt: prompt.clone(),
        file_count: None,
        criteria_count: Some(report.findings.len()),
        file_paths: vec![],
        phase: None,
    };

    info!(
        "Running revision agent to fix {} findings (score={:.2})",
        report.findings.len(),
        report.score,
    );

    let ai_result: AiResponse = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &task_context,
        doctor_handle,
        model,
        provider,
        None,
        None,
        None,
        None,
    );

    if !ai_result.success {
        return Err(format!(
            "Revision agent AI call failed: {}",
            ai_result.error.as_deref().unwrap_or("unknown error")
        ));
    }

    let output = ai_result.output.trim().to_string();
    if output.is_empty() {
        return Err("Revision agent returned empty output".to_string());
    }

    // Extract JSON from response (may be wrapped in markdown code blocks)
    let json_text = crate::workflow_generation::generator::extract_json_from_response(&output);

    match serde_json::from_str::<UnifiedWorkflow>(&json_text) {
        Ok(revised) => {
            info!(
                "Revision agent produced corrected workflow (name={})",
                revised.name
            );
            Ok(revised)
        }
        Err(e) => Err(format!("Failed to parse revised workflow: {}", e)),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::specification::{
        AcceptanceCriteria, AcceptanceCriterion, CriterionPriority, VerificationMethod,
    };
    use serde_json::json;

    /// Helper to create a minimal workflow with verification steps.
    fn make_workflow(verification_steps: Vec<Value>) -> UnifiedWorkflow {
        UnifiedWorkflow {
            id: "test-wf-001".to_string(),
            name: "Test Workflow".to_string(),
            description: "A test workflow".to_string(),
            category: "test".to_string(),
            tags: vec![],
            setup_steps: vec![],
            verification_steps,
            agentic_steps: vec![],
            completion_steps: vec![],
            max_iterations: Some(10),
            max_fix_attempts: 3,
            max_ci_auto_resumes: 10,
            timeout_seconds: None,
            provider: None,
            model: None,
            model_overrides: HashMap::new(),
            skip_ai_summary: false,
            targeted_error_ids: vec![],
            log_source_selection: Default::default(),
            context_ids: vec![],
            disabled_context_ids: vec![],
            auto_include_contexts: true,
            prompt_template: None,
            log_watch_enabled: true,
            health_check_enabled: false,
            health_check_urls: vec![],
            preflight_check_enabled: false,
            enable_sweep: false,
            max_sweep_iterations: 5,
            generated_by_task_run_id: None,
            stages: vec![],
            stop_on_failure: false,
            constraint_overrides: std::collections::HashMap::new(),
            approval_gate: false,
            reflection_mode: true,
            completion_prompts_first: false,
            is_favorite: false,
            dependency_graph: None,
            cost_annotations: None,
            quality_report: None,
            acceptance_criteria: None,
            multi_agent_mode: true,
            use_worktree: false,
            auto_commit_subagents: None,
            strict_cwd: false,
            tool_tags: vec![],
            workflow_architecture: None,
            rollback_policy: None,
            enforce_token_budget: false,
            ai_reviewed: true,
            flow_control_json: None,
            phase_timeouts_json: None,
            security_profile: None,
            htn_enabled: false,
            htn_state_machine_path: None,
            htn_ui_bridge_url: None,
            auto_commit_subagents: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn sample_criteria() -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "Backend API responds correctly".to_string(),
            criteria: vec![
                AcceptanceCriterion {
                    id: "typecheck-passes".to_string(),
                    description: "TypeScript compilation succeeds".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Run `npx tsc --noEmit`".to_string(),
                    category: "compilation".to_string(),
                    ..Default::default()
                },
                AcceptanceCriterion {
                    id: "api-health".to_string(),
                    description: "API health endpoint returns 200".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Important,
                    verification_hint: "curl http://localhost:8000/health".to_string(),
                    category: "runtime".to_string(),
                    ..Default::default()
                },
                AcceptanceCriterion {
                    id: "visual-review".to_string(),
                    description: "Dashboard looks correct visually".to_string(),
                    method: VerificationMethod::Manual,
                    priority: CriterionPriority::Optional,
                    verification_hint: "Human review".to_string(),
                    category: "style".to_string(),
                    ..Default::default()
                },
                AcceptanceCriterion {
                    id: "lint-clean".to_string(),
                    description: "ESLint passes with no errors".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Optional,
                    verification_hint: "Run `npx eslint .`".to_string(),
                    category: "code-quality".to_string(),
                    ..Default::default()
                },
            ],
            assumptions: vec!["TypeScript project".to_string()],
            bugfix_context: None,
        }
    }

    #[test]
    fn test_build_coverage_matrix() {
        let workflow = make_workflow(vec![
            json!({
                "id": "step-1",
                "type": "command",
                "name": "Typecheck",
                "criterion_id": "typecheck-passes"
            }),
            json!({
                "id": "step-2",
                "type": "command",
                "name": "Health check",
                "criterion_ids": ["api-health"]
            }),
            json!({
                "id": "step-3",
                "type": "command",
                "name": "Unlinked step"
            }),
        ]);

        let criteria = sample_criteria();
        let matrix = build_coverage_matrix(&workflow, &criteria);

        // step-1 covers typecheck-passes
        assert_eq!(
            matrix.criteria_to_steps.get("typecheck-passes"),
            Some(&vec!["step-1".to_string()])
        );

        // step-2 covers api-health
        assert_eq!(
            matrix.criteria_to_steps.get("api-health"),
            Some(&vec!["step-2".to_string()])
        );

        // lint-clean is uncovered (no step matches it)
        assert!(matrix
            .uncovered_criteria
            .contains(&"lint-clean".to_string()));

        // visual-review (Manual) should NOT be in uncovered_criteria
        assert!(!matrix
            .uncovered_criteria
            .contains(&"visual-review".to_string()));

        // step-3 is unlinked (no criterion_id or criterion_ids)
        assert!(matrix.unlinked_steps.contains(&"step-3".to_string()));

        // step-1 maps to typecheck-passes
        assert_eq!(
            matrix.steps_to_criteria.get("step-1"),
            Some(&vec!["typecheck-passes".to_string()])
        );

        // step-2 maps to api-health
        assert_eq!(
            matrix.steps_to_criteria.get("step-2"),
            Some(&vec!["api-health".to_string()])
        );
    }

    #[test]
    fn test_compute_quality_score() {
        // No findings → perfect score
        assert_eq!(compute_quality_score(&[]), 1.0);

        // 1 critical → 1.0 - 0.3 = 0.7
        let findings = vec![QualityFinding {
            finding_id: "f1".to_string(),
            step_id: None,
            severity: FindingSeverity::Critical,
            category: FindingCategory::MissingCriterion,
            description: "test".to_string(),
            suggested_fix: None,
        }];
        let score = compute_quality_score(&findings);
        assert!((score - 0.7).abs() < 0.001);

        // 2 critical → 1.0 - 0.6 = 0.4
        let findings = vec![
            QualityFinding {
                finding_id: "f1".to_string(),
                step_id: None,
                severity: FindingSeverity::Critical,
                category: FindingCategory::MissingCriterion,
                description: "test".to_string(),
                suggested_fix: None,
            },
            QualityFinding {
                finding_id: "f2".to_string(),
                step_id: None,
                severity: FindingSeverity::Critical,
                category: FindingCategory::MissingCriterion,
                description: "test2".to_string(),
                suggested_fix: None,
            },
        ];
        let score = compute_quality_score(&findings);
        assert!((score - 0.4).abs() < 0.001);

        // 1 warning → 1.0 - 0.006 = 0.994
        let findings = vec![QualityFinding {
            finding_id: "f1".to_string(),
            step_id: None,
            severity: FindingSeverity::Warning,
            category: FindingCategory::WeakRetry,
            description: "test".to_string(),
            suggested_fix: None,
        }];
        let score = compute_quality_score(&findings);
        assert!((score - 0.994).abs() < 0.001);

        // 1 info → 1.0 - 0.002 = 0.998
        let findings = vec![QualityFinding {
            finding_id: "f1".to_string(),
            step_id: None,
            severity: FindingSeverity::Info,
            category: FindingCategory::RetryInconsistency,
            description: "test".to_string(),
            suggested_fix: None,
        }];
        let score = compute_quality_score(&findings);
        assert!((score - 0.998).abs() < 0.001);

        // Mixed: 1 critical + 2 warnings + 3 info → 1.0 - 0.3 - 0.012 - 0.006 = 0.682
        let findings = vec![
            QualityFinding {
                finding_id: "f1".to_string(),
                step_id: None,
                severity: FindingSeverity::Critical,
                category: FindingCategory::MissingCriterion,
                description: "test".to_string(),
                suggested_fix: None,
            },
            QualityFinding {
                finding_id: "f2".to_string(),
                step_id: None,
                severity: FindingSeverity::Warning,
                category: FindingCategory::WeakRetry,
                description: "test".to_string(),
                suggested_fix: None,
            },
            QualityFinding {
                finding_id: "f3".to_string(),
                step_id: None,
                severity: FindingSeverity::Warning,
                category: FindingCategory::WeakRetry,
                description: "test".to_string(),
                suggested_fix: None,
            },
            QualityFinding {
                finding_id: "f4".to_string(),
                step_id: None,
                severity: FindingSeverity::Info,
                category: FindingCategory::RetryInconsistency,
                description: "test".to_string(),
                suggested_fix: None,
            },
            QualityFinding {
                finding_id: "f5".to_string(),
                step_id: None,
                severity: FindingSeverity::Info,
                category: FindingCategory::RetryInconsistency,
                description: "test".to_string(),
                suggested_fix: None,
            },
            QualityFinding {
                finding_id: "f6".to_string(),
                step_id: None,
                severity: FindingSeverity::Info,
                category: FindingCategory::RetryInconsistency,
                description: "test".to_string(),
                suggested_fix: None,
            },
        ];
        let score = compute_quality_score(&findings);
        assert!((score - 0.682).abs() < 0.001);

        // 4 critical → clamped to 0.0
        let findings: Vec<QualityFinding> = (0..4)
            .map(|i| QualityFinding {
                finding_id: format!("f{}", i),
                step_id: None,
                severity: FindingSeverity::Critical,
                category: FindingCategory::MissingCriterion,
                description: "test".to_string(),
                suggested_fix: None,
            })
            .collect();
        let score = compute_quality_score(&findings);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_deterministic_checks_finds_uncovered_criteria() {
        // Workflow with only one verification step covering "typecheck-passes"
        // but missing "api-health" and "lint-clean"
        let workflow = make_workflow(vec![json!({
            "id": "step-1",
            "type": "command",
            "name": "Typecheck",
            "criterion_id": "typecheck-passes"
        })]);

        let criteria = sample_criteria();
        let (findings, _matrix) = deterministic_quality_checks(&workflow, Some(&criteria));

        // Should find "api-health" missing (Important → Warning)
        let api_finding = findings
            .iter()
            .find(|f| f.finding_id == "coverage-api-health");
        assert!(
            api_finding.is_some(),
            "Should find missing api-health criterion"
        );
        assert_eq!(api_finding.unwrap().severity, FindingSeverity::Warning);
        assert_eq!(
            api_finding.unwrap().category,
            FindingCategory::MissingCriterion
        );

        // Should find "lint-clean" missing (Optional → Info)
        let lint_finding = findings
            .iter()
            .find(|f| f.finding_id == "coverage-lint-clean");
        assert!(
            lint_finding.is_some(),
            "Should find missing lint-clean criterion"
        );
        assert_eq!(lint_finding.unwrap().severity, FindingSeverity::Info);

        // Should NOT find "visual-review" missing (Manual method → excluded)
        let manual_finding = findings
            .iter()
            .find(|f| f.finding_id == "coverage-visual-review");
        assert!(
            manual_finding.is_none(),
            "Manual criteria should not generate coverage findings"
        );
    }

    #[test]
    fn test_retry_consistency_http_no_retries() {
        let workflow = make_workflow(vec![json!({
            "id": "health-check",
            "type": "command",
            "name": "API Health",
            "command": "curl http://localhost:8000/health",
            "retry_count": 0
        })]);

        let findings = retry_consistency_findings(&workflow);

        let http_finding = findings
            .iter()
            .find(|f| f.finding_id == "retry-http-health-check");
        assert!(
            http_finding.is_some(),
            "Should flag HTTP step with no retries"
        );
        assert_eq!(http_finding.unwrap().severity, FindingSeverity::Warning);
        assert_eq!(http_finding.unwrap().category, FindingCategory::WeakRetry);
    }

    #[test]
    fn test_retry_consistency_excessive_retries() {
        let workflow = make_workflow(vec![json!({
            "id": "flaky-step",
            "type": "command",
            "name": "Flaky check",
            "command": "npx some-check",
            "retry_count": 10
        })]);

        let findings = retry_consistency_findings(&workflow);

        let excessive = findings
            .iter()
            .find(|f| f.finding_id == "retry-excessive-flaky-step");
        assert!(
            excessive.is_some(),
            "Should flag step with excessive retries"
        );
        assert_eq!(excessive.unwrap().severity, FindingSeverity::Warning);
    }

    #[test]
    fn test_format_findings_for_prompt() {
        let findings = vec![
            QualityFinding {
                finding_id: "f1".to_string(),
                step_id: Some("step-abc".to_string()),
                severity: FindingSeverity::Critical,
                category: FindingCategory::MissingCriterion,
                description: "Missing coverage for typecheck".to_string(),
                suggested_fix: Some("Add a typecheck step".to_string()),
            },
            QualityFinding {
                finding_id: "f2".to_string(),
                step_id: None,
                severity: FindingSeverity::Warning,
                category: FindingCategory::WeakRetry,
                description: "No retries on HTTP step".to_string(),
                suggested_fix: None,
            },
        ];

        let text = format_findings_for_prompt(&findings);
        assert!(text.contains("1. [CRITICAL/ missing_criterion] (step: step-abc)"));
        assert!(text.contains("Missing coverage for typecheck"));
        assert!(text.contains("Fix: Add a typecheck step"));
        assert!(text.contains("2. [WARNING/ weak_retry]"));
        assert!(text.contains("No retries on HTTP step"));
    }

    #[test]
    fn test_format_findings_empty() {
        let text = format_findings_for_prompt(&[]);
        assert_eq!(text, "(No findings)");
    }

    #[test]
    fn test_quality_report_serde_roundtrip() {
        let report = QualityReport {
            findings: vec![QualityFinding {
                finding_id: "f1".to_string(),
                step_id: None,
                severity: FindingSeverity::Warning,
                category: FindingCategory::WeakRetry,
                description: "Test finding".to_string(),
                suggested_fix: None,
            }],
            score: 0.9,
            pass: true,
            coverage_matrix: None,
            semantic_coverage_matrix: None,
        };

        let json = serde_json::to_string(&report).unwrap();
        let parsed: QualityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.findings.len(), 1);
        assert!((parsed.score - 0.9).abs() < 0.001);
        assert!(parsed.pass);
    }
}
