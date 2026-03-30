//! Cross-Artifact Consistency Checking
//!
//! Validates that the Builder's output (UnifiedWorkflow) is consistent with
//! the Specification's output (AcceptanceCriteria). Catches gaps between
//! what was specified and what was generated.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::specification::{AcceptanceCriteria, AcceptanceCriterion, CriterionPriority};

/// Result of cross-artifact consistency analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyReport {
    /// Individual issues found
    pub issues: Vec<ConsistencyIssue>,
    /// Overall consistency score (0.0 = completely inconsistent, 1.0 = fully consistent)
    pub score: f64,
    /// Total criteria checked
    pub criteria_checked: usize,
    /// Criteria with at least one matching step
    pub criteria_covered: usize,
}

/// A single consistency issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyIssue {
    /// Type of issue
    pub kind: ConsistencyIssueKind,
    /// Human-readable description
    pub description: String,
    /// Related criterion ID (if applicable)
    pub criterion_id: Option<String>,
    /// Related step name (if applicable)
    pub step_name: Option<String>,
    /// Severity: "error" or "warning"
    pub severity: String,
}

/// Types of consistency issues.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsistencyIssueKind {
    /// A criterion exists but no verification step references it
    UnverifiedCriterion,
    /// A critical criterion has no matching verification step
    CriticalCriterionUncovered,
    /// A verification step doesn't relate to any criterion
    OrphanStep,
    /// Step uses technology/pattern that contradicts the constitution
    ConstitutionViolation,
    /// Criterion priority doesn't match step's required flag
    PriorityMismatch,
    /// Step's verification_hint contradicts the step's actual command
    HintContradiction,
}

/// Check consistency between acceptance criteria and generated workflow steps.
///
/// `workflow_json` is the raw JSON of the UnifiedWorkflow.
/// `constitution` is the optional project constitution text.
pub fn check_consistency(
    criteria: &AcceptanceCriteria,
    workflow_json: &serde_json::Value,
    constitution: Option<&str>,
) -> ConsistencyReport {
    let mut issues = Vec::new();

    // Extract verification step names/commands from workflow JSON
    let verification_steps = extract_verification_steps(workflow_json);

    let criteria_count = criteria.criteria.len();
    let mut covered_count = 0;

    // Check each criterion has a matching verification step
    for criterion in &criteria.criteria {
        let has_match = verification_steps
            .iter()
            .any(|step| step_matches_criterion(step, criterion));

        if has_match {
            covered_count += 1;
        } else {
            let severity = match criterion.priority {
                CriterionPriority::Critical => {
                    issues.push(ConsistencyIssue {
                        kind: ConsistencyIssueKind::CriticalCriterionUncovered,
                        description: format!(
                            "Critical criterion '{}' ({}) has no matching verification step",
                            criterion.id, criterion.description
                        ),
                        criterion_id: Some(criterion.id.clone()),
                        step_name: None,
                        severity: "error".to_string(),
                    });
                    "error"
                }
                _ => {
                    issues.push(ConsistencyIssue {
                        kind: ConsistencyIssueKind::UnverifiedCriterion,
                        description: format!(
                            "Criterion '{}' ({}) has no matching verification step",
                            criterion.id, criterion.description
                        ),
                        criterion_id: Some(criterion.id.clone()),
                        step_name: None,
                        severity: "warning".to_string(),
                    });
                    "warning"
                }
            };
            debug!(
                criterion_id = %criterion.id,
                severity,
                "Unverified criterion"
            );
        }

        // Check priority/required flag consistency
        check_priority_consistency(criterion, &verification_steps, &mut issues);

        // Check hint contradictions: step command contradicts the verification hint
        check_hint_contradictions(criterion, &verification_steps, &mut issues);
    }

    // Check for orphan steps (verification steps that don't match any criterion)
    for step in &verification_steps {
        let matches_any = criteria
            .criteria
            .iter()
            .any(|c| step_matches_criterion(step, c));
        if !matches_any {
            issues.push(ConsistencyIssue {
                kind: ConsistencyIssueKind::OrphanStep,
                description: format!(
                    "Verification step '{}' doesn't match any acceptance criterion",
                    step.name
                ),
                criterion_id: None,
                step_name: Some(step.name.clone()),
                severity: "warning".to_string(),
            });
        }
    }

    // Check constitution violations if a constitution is provided
    if let Some(constitution) = constitution {
        check_constitution_violations(constitution, &verification_steps, &mut issues);
    }

    let score = if criteria_count == 0 {
        1.0
    } else {
        let error_count = issues.iter().filter(|i| i.severity == "error").count();
        let warning_count = issues.iter().filter(|i| i.severity == "warning").count();
        let penalty = (error_count as f64 * 0.15 + warning_count as f64 * 0.05).min(1.0);
        (1.0 - penalty).max(0.0)
    };

    if !issues.is_empty() {
        warn!(
            issue_count = issues.len(),
            score,
            "Consistency check found issues"
        );
    }

    ConsistencyReport {
        issues,
        score,
        criteria_checked: criteria_count,
        criteria_covered: covered_count,
    }
}

/// Format consistency issues for injection into the Fixer agent's prompt.
pub fn format_consistency_issues_for_fixer(report: &ConsistencyReport) -> String {
    if report.issues.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Cross-Artifact Consistency Issues\n\n");
    output.push_str("The following inconsistencies were found between the acceptance criteria and the generated workflow:\n\n");

    for issue in &report.issues {
        let icon = if issue.severity == "error" {
            "ERROR"
        } else {
            "WARN"
        };
        output.push_str(&format!("- **[{}]** {}\n", icon, issue.description));
        if let Some(ref cid) = issue.criterion_id {
            output.push_str(&format!("  Criterion: {}\n", cid));
        }
        if let Some(ref sname) = issue.step_name {
            output.push_str(&format!("  Step: {}\n", sname));
        }
    }

    output.push_str(&format!(
        "\nCoverage: {}/{} criteria covered. Score: {:.2}\n",
        report.criteria_covered, report.criteria_checked, report.score
    ));

    output
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Extracted info about a verification step for matching.
struct ExtractedStep {
    name: String,
    command: Option<String>,
    check_type: Option<String>,
    required: bool,
}

/// Extract verification steps from workflow JSON.
fn extract_verification_steps(workflow: &serde_json::Value) -> Vec<ExtractedStep> {
    let mut steps = Vec::new();

    if let Some(ver_steps) = workflow
        .get("verification_steps")
        .and_then(|v| v.as_array())
    {
        for step in ver_steps {
            let name = step
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let command = step
                .get("shell_command")
                .or_else(|| step.get("command"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let check_type = step
                .get("check_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let required = step
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            steps.push(ExtractedStep {
                name,
                command,
                check_type,
                required,
            });
        }
    }

    // Also check setup steps for setup-type verification (e.g., state-driven criteria)
    if let Some(setup_steps) = workflow.get("setup_steps").and_then(|v| v.as_array()) {
        for step in setup_steps {
            let name = step
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let command = step
                .get("shell_command")
                .or_else(|| step.get("command"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let check_type = step
                .get("check_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let required = step
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            steps.push(ExtractedStep {
                name,
                command,
                check_type,
                required,
            });
        }
    }

    steps
}

/// Check if a step matches a criterion via keyword overlap.
fn step_matches_criterion(step: &ExtractedStep, criterion: &AcceptanceCriterion) -> bool {
    let step_text = format!(
        "{} {}",
        step.name.to_lowercase(),
        step.command.as_deref().unwrap_or("").to_lowercase()
    );
    let criterion_text = format!(
        "{} {} {}",
        criterion.id.to_lowercase().replace('-', " "),
        criterion.description.to_lowercase(),
        criterion.verification_hint.to_lowercase()
    );

    // Extract significant words (3+ chars) from criterion
    let criterion_words: Vec<&str> = criterion_text
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .filter(|w| {
            ![
                "the", "and", "for", "that", "with", "from", "this", "shall", "should", "must",
            ]
            .contains(w)
        })
        .collect();

    if criterion_words.is_empty() {
        return false;
    }

    // Count how many significant criterion words appear in the step text
    let matches = criterion_words
        .iter()
        .filter(|w| step_text.contains(**w))
        .count();
    let match_ratio = matches as f64 / criterion_words.len() as f64;

    // Also check if check_type matches criterion category
    if let Some(ref ct) = step.check_type {
        let ct_lower = ct.to_lowercase();
        if criterion.category.to_lowercase().contains(&ct_lower)
            || ct_lower.contains(&criterion.category.to_lowercase())
        {
            return true;
        }
    }

    match_ratio >= 0.3
}

/// Check priority/required flag consistency.
fn check_priority_consistency(
    criterion: &AcceptanceCriterion,
    steps: &[ExtractedStep],
    issues: &mut Vec<ConsistencyIssue>,
) {
    // Find matching steps
    let matching: Vec<&ExtractedStep> = steps
        .iter()
        .filter(|s| step_matches_criterion(s, criterion))
        .collect();

    for step in &matching {
        // Critical criteria should have required=true steps
        if criterion.priority == CriterionPriority::Critical && !step.required {
            issues.push(ConsistencyIssue {
                kind: ConsistencyIssueKind::PriorityMismatch,
                description: format!(
                    "Critical criterion '{}' is verified by step '{}' which has required=false",
                    criterion.id, step.name
                ),
                criterion_id: Some(criterion.id.clone()),
                step_name: Some(step.name.clone()),
                severity: "warning".to_string(),
            });
        }
        // Optional criteria should have required=false steps
        if criterion.priority == CriterionPriority::Optional && step.required {
            issues.push(ConsistencyIssue {
                kind: ConsistencyIssueKind::PriorityMismatch,
                description: format!(
                    "Optional criterion '{}' is verified by step '{}' which has required=true — consider setting required=false",
                    criterion.id, step.name
                ),
                criterion_id: Some(criterion.id.clone()),
                step_name: Some(step.name.clone()),
                severity: "warning".to_string(),
            });
        }
    }
}

/// Check for hint contradictions: step's command contradicts the criterion's verification_hint.
///
/// Examples: hint says "run typecheck" but step runs a linter; hint says "check file exists"
/// but step runs a content grep; hint says "test endpoint returns 200" but step curls a
/// different endpoint.
fn check_hint_contradictions(
    criterion: &AcceptanceCriterion,
    steps: &[ExtractedStep],
    issues: &mut Vec<ConsistencyIssue>,
) {
    if criterion.verification_hint.trim().is_empty() {
        return;
    }

    let hint_lower = criterion.verification_hint.to_lowercase();
    let matching: Vec<&ExtractedStep> = steps
        .iter()
        .filter(|s| step_matches_criterion(s, criterion))
        .collect();

    for step in &matching {
        if let Some(ref cmd) = step.command {
            let cmd_lower = cmd.to_lowercase();

            // Contradiction: hint says typecheck but step runs lint
            if (hint_lower.contains("typecheck") || hint_lower.contains("type check") || hint_lower.contains("tsc"))
                && !cmd_lower.contains("tsc")
                && !cmd_lower.contains("typecheck")
                && !cmd_lower.contains("type-check")
                && (cmd_lower.contains("eslint") || cmd_lower.contains("lint"))
            {
                issues.push(ConsistencyIssue {
                    kind: ConsistencyIssueKind::HintContradiction,
                    description: format!(
                        "Criterion '{}' hint says '{}' but step '{}' runs a linter instead of a typechecker",
                        criterion.id, criterion.verification_hint, step.name
                    ),
                    criterion_id: Some(criterion.id.clone()),
                    step_name: Some(step.name.clone()),
                    severity: "warning".to_string(),
                });
            }

            // Contradiction: hint says lint but step runs typecheck
            if (hint_lower.contains("lint") || hint_lower.contains("eslint"))
                && !cmd_lower.contains("lint")
                && !cmd_lower.contains("eslint")
                && (cmd_lower.contains("tsc") || cmd_lower.contains("typecheck"))
            {
                issues.push(ConsistencyIssue {
                    kind: ConsistencyIssueKind::HintContradiction,
                    description: format!(
                        "Criterion '{}' hint says '{}' but step '{}' runs a typechecker instead of a linter",
                        criterion.id, criterion.verification_hint, step.name
                    ),
                    criterion_id: Some(criterion.id.clone()),
                    step_name: Some(step.name.clone()),
                    severity: "warning".to_string(),
                });
            }

            // Contradiction: hint says "test" but step runs "build"
            if (hint_lower.contains("run test") || hint_lower.contains("unit test") || hint_lower.contains("pytest") || hint_lower.contains("vitest"))
                && !cmd_lower.contains("test")
                && (cmd_lower.contains("build") || cmd_lower.contains("compile"))
            {
                issues.push(ConsistencyIssue {
                    kind: ConsistencyIssueKind::HintContradiction,
                    description: format!(
                        "Criterion '{}' hint says '{}' but step '{}' runs a build command instead of tests",
                        criterion.id, criterion.verification_hint, step.name
                    ),
                    criterion_id: Some(criterion.id.clone()),
                    step_name: Some(step.name.clone()),
                    severity: "warning".to_string(),
                });
            }
        }
    }
}

/// Check for constitution violations in verification steps.
fn check_constitution_violations(
    constitution: &str,
    steps: &[ExtractedStep],
    issues: &mut Vec<ConsistencyIssue>,
) {
    let constitution_lower = constitution.to_lowercase();

    // Check for common anti-patterns based on constitution content
    for step in steps {
        if let Some(ref cmd) = step.command {
            let cmd_lower = cmd.to_lowercase();

            // If constitution says "TypeScript strict mode" and step uses --skipLibCheck
            if constitution_lower.contains("strict mode") && cmd_lower.contains("--skiplibcheck") {
                issues.push(ConsistencyIssue {
                    kind: ConsistencyIssueKind::ConstitutionViolation,
                    description: format!(
                        "Step '{}' uses --skipLibCheck which contradicts the constitution's strict mode requirement",
                        step.name
                    ),
                    criterion_id: None,
                    step_name: Some(step.name.clone()),
                    severity: "warning".to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::specification::*;
    use super::*;
    use serde_json::json;

    fn make_criteria(ids: &[(&str, CriterionPriority)]) -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "Test goal".to_string(),
            criteria: ids
                .iter()
                .map(|(id, priority)| AcceptanceCriterion {
                    id: id.to_string(),
                    description: format!("Test criterion for {}", id),
                    method: VerificationMethod::Command,
                    priority: priority.clone(),
                    verification_hint: format!("Check {}", id),
                    category: "testing".to_string(),
                    ..Default::default()
                })
                .collect(),
            assumptions: vec![],
            bugfix_context: None,
        }
    }

    fn make_workflow(step_names: &[&str]) -> serde_json::Value {
        json!({
            "verification_steps": step_names.iter().map(|name| json!({
                "name": name,
                "type": "command",
                "shell_command": format!("check-{}", name.to_lowercase().replace(' ', "-")),
                "required": true
            })).collect::<Vec<_>>(),
            "setup_steps": []
        })
    }

    #[test]
    fn test_full_coverage() {
        let criteria = make_criteria(&[
            ("typecheck", CriterionPriority::Critical),
            ("lint-check", CriterionPriority::Important),
        ]);
        let workflow = make_workflow(&["typecheck verification", "lint check step"]);
        let report = check_consistency(&criteria, &workflow, None);
        assert_eq!(report.criteria_covered, 2);
        assert!(report.score > 0.8);
    }

    #[test]
    fn test_missing_critical_criterion() {
        let criteria = make_criteria(&[("auth-login", CriterionPriority::Critical)]);
        let workflow = make_workflow(&["unrelated step"]);
        let report = check_consistency(&criteria, &workflow, None);
        assert_eq!(report.criteria_covered, 0);
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == ConsistencyIssueKind::CriticalCriterionUncovered));
        assert!(report.score < 0.9);
    }

    #[test]
    fn test_orphan_step() {
        let criteria = make_criteria(&[]);
        let workflow = make_workflow(&["some random step"]);
        let report = check_consistency(&criteria, &workflow, None);
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == ConsistencyIssueKind::OrphanStep));
    }

    #[test]
    fn test_format_for_fixer() {
        let report = ConsistencyReport {
            issues: vec![ConsistencyIssue {
                kind: ConsistencyIssueKind::CriticalCriterionUncovered,
                description: "Critical criterion 'auth' not covered".to_string(),
                criterion_id: Some("auth".to_string()),
                step_name: None,
                severity: "error".to_string(),
            }],
            score: 0.85,
            criteria_checked: 1,
            criteria_covered: 0,
        };
        let formatted = format_consistency_issues_for_fixer(&report);
        assert!(formatted.contains("ERROR"));
        assert!(formatted.contains("auth"));
    }
}
