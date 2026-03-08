//! Benchmark scoring for generator evaluation.
//!
//! Compares generated workflows against expected structure definitions to
//! produce scores across multiple dimensions (structure, content, step types).

use crate::unified_workflows::UnifiedWorkflow;
use serde::{Deserialize, Serialize};

/// Expected structure definition for a benchmark test case.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpectedStructure {
    /// Whether setup phase should have steps
    #[serde(default)]
    pub has_setup: Option<bool>,
    /// Whether verification phase should have steps
    #[serde(default)]
    pub has_verification: Option<bool>,
    /// Whether agentic phase should have steps
    #[serde(default)]
    pub has_agentic: Option<bool>,
    /// Whether completion phase should have steps
    #[serde(default)]
    pub has_completion: Option<bool>,

    /// Minimum expected setup steps
    #[serde(default)]
    pub min_setup_steps: Option<u32>,
    /// Minimum expected verification steps
    #[serde(default)]
    pub min_verification_steps: Option<u32>,
    /// Minimum expected agentic steps
    #[serde(default)]
    pub min_agentic_steps: Option<u32>,

    /// Expected step types to be present (e.g., "command", "ui_bridge", "prompt")
    #[serde(default)]
    pub expected_step_types: Vec<String>,

    /// Keywords expected in step names or commands
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Expected check_type values (e.g., "lint", "typecheck", "format")
    #[serde(default)]
    pub expected_check_types: Vec<String>,

    /// Expected test_type values (e.g., "playwright", "python")
    #[serde(default)]
    pub expected_test_types: Vec<String>,
}

/// Scoring result for a single benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkScore {
    /// Phase structure score (0.0 - 1.0)
    pub structure_score: f64,
    /// Content/keyword score (0.0 - 1.0)
    pub content_score: f64,
    /// Step type score (0.0 - 1.0)
    pub step_type_score: f64,
    /// Weighted overall score (0.0 - 1.0)
    pub overall_score: f64,
    /// Detailed breakdown of each scoring dimension
    pub breakdown: ScoreBreakdown,
}

/// Detailed breakdown of scoring for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub phase_checks: Vec<CheckResult>,
    pub step_count_checks: Vec<CheckResult>,
    pub step_type_checks: Vec<CheckResult>,
    pub keyword_checks: Vec<CheckResult>,
    pub check_type_checks: Vec<CheckResult>,
    pub test_type_checks: Vec<CheckResult>,
}

/// Result of a single check within scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// Score a generated workflow against expected structure.
pub fn score_workflow(workflow: &UnifiedWorkflow, expected: &ExpectedStructure) -> BenchmarkScore {
    let mut breakdown = ScoreBreakdown {
        phase_checks: vec![],
        step_count_checks: vec![],
        step_type_checks: vec![],
        keyword_checks: vec![],
        check_type_checks: vec![],
        test_type_checks: vec![],
    };

    // ── Structure scoring: phase presence + step counts ──

    let mut structure_checks = 0u32;
    let mut structure_passed = 0u32;

    // Phase presence
    if let Some(expected_setup) = expected.has_setup {
        let has = !workflow.setup_steps.is_empty();
        let passed = has == expected_setup;
        breakdown.phase_checks.push(CheckResult {
            name: "has_setup".into(),
            passed,
            detail: format!("expected={}, actual={}", expected_setup, has),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }
    if let Some(expected_verification) = expected.has_verification {
        let has = !workflow.verification_steps.is_empty();
        let passed = has == expected_verification;
        breakdown.phase_checks.push(CheckResult {
            name: "has_verification".into(),
            passed,
            detail: format!("expected={}, actual={}", expected_verification, has),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }
    if let Some(expected_agentic) = expected.has_agentic {
        let has = !workflow.agentic_steps.is_empty();
        let passed = has == expected_agentic;
        breakdown.phase_checks.push(CheckResult {
            name: "has_agentic".into(),
            passed,
            detail: format!("expected={}, actual={}", expected_agentic, has),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }
    if let Some(expected_completion) = expected.has_completion {
        let has = !workflow.completion_steps.is_empty();
        let passed = has == expected_completion;
        breakdown.phase_checks.push(CheckResult {
            name: "has_completion".into(),
            passed,
            detail: format!("expected={}, actual={}", expected_completion, has),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }

    // Step count checks
    if let Some(min_setup) = expected.min_setup_steps {
        let actual = workflow.setup_steps.len() as u32;
        let passed = actual >= min_setup;
        breakdown.step_count_checks.push(CheckResult {
            name: "min_setup_steps".into(),
            passed,
            detail: format!("min={}, actual={}", min_setup, actual),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }
    if let Some(min_verification) = expected.min_verification_steps {
        let actual = workflow.verification_steps.len() as u32;
        let passed = actual >= min_verification;
        breakdown.step_count_checks.push(CheckResult {
            name: "min_verification_steps".into(),
            passed,
            detail: format!("min={}, actual={}", min_verification, actual),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }
    if let Some(min_agentic) = expected.min_agentic_steps {
        let actual = workflow.agentic_steps.len() as u32;
        let passed = actual >= min_agentic;
        breakdown.step_count_checks.push(CheckResult {
            name: "min_agentic_steps".into(),
            passed,
            detail: format!("min={}, actual={}", min_agentic, actual),
        });
        structure_checks += 1;
        if passed {
            structure_passed += 1;
        }
    }

    let structure_score = if structure_checks > 0 {
        structure_passed as f64 / structure_checks as f64
    } else {
        1.0 // No structure checks = pass by default
    };

    // ── Content scoring: keywords found in step names/commands ──

    let all_step_text = collect_step_text(workflow);
    let mut keyword_total = 0u32;
    let mut keyword_found = 0u32;

    for keyword in &expected.keywords {
        let lower_keyword = keyword.to_lowercase();
        let found = all_step_text.iter().any(|t| t.contains(&lower_keyword));
        breakdown.keyword_checks.push(CheckResult {
            name: format!("keyword:{}", keyword),
            passed: found,
            detail: if found {
                "found".into()
            } else {
                "not found".into()
            },
        });
        keyword_total += 1;
        if found {
            keyword_found += 1;
        }
    }

    let content_score = if keyword_total > 0 {
        keyword_found as f64 / keyword_total as f64
    } else {
        1.0
    };

    // ── Step type scoring: correct types used ──

    let actual_step_types = collect_step_types(workflow);
    let actual_check_types = collect_check_types(workflow);
    let actual_test_types = collect_test_types(workflow);

    let mut type_total = 0u32;
    let mut type_found = 0u32;

    for expected_type in &expected.expected_step_types {
        let found = actual_step_types.contains(&expected_type.to_lowercase());
        breakdown.step_type_checks.push(CheckResult {
            name: format!("step_type:{}", expected_type),
            passed: found,
            detail: if found {
                "present".into()
            } else {
                "missing".into()
            },
        });
        type_total += 1;
        if found {
            type_found += 1;
        }
    }

    for expected_check in &expected.expected_check_types {
        let found = actual_check_types.contains(&expected_check.to_lowercase());
        breakdown.check_type_checks.push(CheckResult {
            name: format!("check_type:{}", expected_check),
            passed: found,
            detail: if found {
                "present".into()
            } else {
                "missing".into()
            },
        });
        type_total += 1;
        if found {
            type_found += 1;
        }
    }

    for expected_test in &expected.expected_test_types {
        let found = actual_test_types.contains(&expected_test.to_lowercase());
        breakdown.test_type_checks.push(CheckResult {
            name: format!("test_type:{}", expected_test),
            passed: found,
            detail: if found {
                "present".into()
            } else {
                "missing".into()
            },
        });
        type_total += 1;
        if found {
            type_found += 1;
        }
    }

    let step_type_score = if type_total > 0 {
        type_found as f64 / type_total as f64
    } else {
        1.0
    };

    // Weighted overall: 40% structure, 35% content, 25% step types
    let overall_score = structure_score * 0.4 + content_score * 0.35 + step_type_score * 0.25;

    BenchmarkScore {
        structure_score,
        content_score,
        step_type_score,
        overall_score,
        breakdown,
    }
}

/// Get a string field from a JSON step value.
fn get_step_str<'a>(step: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    step.get(field).and_then(|v| v.as_str())
}

/// Collect all searchable text from workflow steps (names, commands, prompts).
fn collect_step_text(workflow: &UnifiedWorkflow) -> Vec<String> {
    let mut texts = Vec::new();

    let all_steps = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.agentic_steps.iter())
        .chain(workflow.completion_steps.iter());

    for step in all_steps {
        if let Some(name) = get_step_str(step, "name") {
            texts.push(name.to_lowercase());
        }
        if let Some(cmd) = get_step_str(step, "command") {
            texts.push(cmd.to_lowercase());
        }
        if let Some(content) = get_step_str(step, "content") {
            texts.push(content.to_lowercase());
        }
        if let Some(desc) = get_step_str(step, "description") {
            texts.push(desc.to_lowercase());
        }
    }

    texts
}

/// Collect unique step types from all phases.
fn collect_step_types(workflow: &UnifiedWorkflow) -> Vec<String> {
    let mut types = std::collections::HashSet::new();

    let all_steps = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.agentic_steps.iter())
        .chain(workflow.completion_steps.iter());

    for step in all_steps {
        if let Some(st) = get_step_str(step, "step_type") {
            types.insert(st.to_lowercase());
        }
    }

    types.into_iter().collect()
}

/// Collect unique check_type values from command steps.
fn collect_check_types(workflow: &UnifiedWorkflow) -> Vec<String> {
    let mut types = std::collections::HashSet::new();

    let all_steps = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.completion_steps.iter());

    for step in all_steps {
        if let Some(ct) = get_step_str(step, "check_type") {
            types.insert(ct.to_lowercase());
        }
    }

    types.into_iter().collect()
}

/// Collect unique test_type values from command steps.
fn collect_test_types(workflow: &UnifiedWorkflow) -> Vec<String> {
    let mut types = std::collections::HashSet::new();

    let all_steps = workflow
        .setup_steps
        .iter()
        .chain(workflow.verification_steps.iter())
        .chain(workflow.completion_steps.iter());

    for step in all_steps {
        if let Some(tt) = get_step_str(step, "test_type") {
            types.insert(tt.to_lowercase());
        }
    }

    types.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_workflow() -> UnifiedWorkflow {
        serde_json::from_str(
            r#"{
                "id": "test-1",
                "name": "Test Workflow",
                "description": "A test",
                "category": "testing",
                "tags": [],
                "max_iterations": 5,
                "setup_steps": [
                    {"id": "s1", "name": "Install deps", "step_type": "command", "phase": "setup", "command": "npm install", "command_mode": "shell"}
                ],
                "verification_steps": [
                    {"id": "v1", "name": "Run lint", "step_type": "command", "phase": "verification", "command": "npm run lint", "check_type": "lint", "command_mode": "check"}
                ],
                "agentic_steps": [
                    {"id": "a1", "name": "Fix issues", "step_type": "prompt", "phase": "agentic", "content": "Fix the lint errors found in verification"}
                ],
                "completion_steps": [],
                "is_favorite": false
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn test_score_perfect_match() {
        let workflow = make_test_workflow();
        let expected = ExpectedStructure {
            has_setup: Some(true),
            has_verification: Some(true),
            has_agentic: Some(true),
            has_completion: Some(false),
            min_setup_steps: Some(1),
            min_verification_steps: Some(1),
            min_agentic_steps: Some(1),
            expected_step_types: vec!["command".into(), "prompt".into()],
            keywords: vec!["lint".into(), "npm".into()],
            expected_check_types: vec!["lint".into()],
            expected_test_types: vec![],
        };

        let score = score_workflow(&workflow, &expected);
        assert_eq!(score.structure_score, 1.0);
        assert_eq!(score.content_score, 1.0);
        assert_eq!(score.step_type_score, 1.0);
        assert!(score.overall_score > 0.99);
    }

    #[test]
    fn test_score_partial_match() {
        let workflow = make_test_workflow();
        let expected = ExpectedStructure {
            has_setup: Some(true),
            has_verification: Some(true),
            has_agentic: Some(true),
            has_completion: Some(true), // workflow has no completion steps
            min_setup_steps: None,
            min_verification_steps: None,
            min_agentic_steps: None,
            expected_step_types: vec!["command".into(), "ui_bridge".into()], // no ui_bridge
            keywords: vec!["lint".into(), "docker".into()],                  // no docker
            expected_check_types: vec![],
            expected_test_types: vec![],
        };

        let score = score_workflow(&workflow, &expected);
        assert!(score.structure_score < 1.0); // completion mismatch
        assert!(score.content_score < 1.0); // docker not found
        assert!(score.step_type_score < 1.0); // ui_bridge missing
        assert!(score.overall_score < 1.0);
    }

    #[test]
    fn test_score_no_expectations() {
        let workflow = make_test_workflow();
        let expected = ExpectedStructure {
            has_setup: None,
            has_verification: None,
            has_agentic: None,
            has_completion: None,
            min_setup_steps: None,
            min_verification_steps: None,
            min_agentic_steps: None,
            expected_step_types: vec![],
            keywords: vec![],
            expected_check_types: vec![],
            expected_test_types: vec![],
        };

        let score = score_workflow(&workflow, &expected);
        assert_eq!(score.overall_score, 1.0); // Everything passes by default
    }
}
