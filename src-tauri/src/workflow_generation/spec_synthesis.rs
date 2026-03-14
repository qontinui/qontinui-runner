//! Spec Synthesis — Deterministic Verification Steps from Acceptance Criteria
//!
//! Bridges the gap between acceptance criteria and verification steps. Currently,
//! the builder agent implicitly maps criteria to steps via its prompt. This module
//! makes the mapping explicit and verifiable.
//!
//! No AI calls — purely deterministic mapping from criterion metadata to step JSON.
//!
//! ## Pipeline placement
//!
//! ```text
//! Specification → **Spec Synthesis** → Builder → Hardener → Validate
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::{debug, info, warn};

use super::specification::{AcceptanceCriteria, AcceptanceCriterion, VerificationMethod};

// ============================================================================
// Types
// ============================================================================

/// Result of synthesizing verification steps from acceptance criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisResult {
    /// Generated verification steps (as JSON values matching step schema).
    pub steps: Vec<Value>,
    /// Mapping from criterion ID to generated step IDs.
    pub criterion_step_map: HashMap<String, Vec<String>>,
    /// Criteria that could not be mapped to deterministic steps.
    pub unmapped_criteria: Vec<UnmappedCriterion>,
    /// Whether synthesis was successful.
    pub success: bool,
    /// Duration in ms.
    pub duration_ms: u64,
}

/// A criterion that could not be mapped to a deterministic verification step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmappedCriterion {
    /// The criterion ID.
    pub criterion_id: String,
    /// Why it couldn't be mapped.
    pub reason: String,
    /// Suggested step type (prompt) as fallback.
    pub fallback_step: Option<Value>,
}

/// Coverage statistics for acceptance criteria vs. workflow steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Total criteria count.
    pub total_criteria: usize,
    /// Criteria covered by at least one step.
    pub covered_count: usize,
    /// Criteria not covered by any step.
    pub uncovered: Vec<String>,
    /// Coverage percentage (0.0-1.0).
    pub coverage_ratio: f32,
}

// ============================================================================
// Discovery context helpers
// ============================================================================

/// Detected test runner from discovery context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestRunner {
    Jest,
    Vitest,
    Pytest,
    Cargo,
    Generic,
}

impl TestRunner {
    fn command_prefix(&self) -> &'static str {
        match self {
            TestRunner::Jest => "npx jest",
            TestRunner::Vitest => "npx vitest run",
            TestRunner::Pytest => "python -m pytest",
            TestRunner::Cargo => "cargo test",
            TestRunner::Generic => "npm test --",
        }
    }

    fn grep_flag(&self) -> &'static str {
        match self {
            TestRunner::Jest => "--testNamePattern",
            TestRunner::Vitest => "--testNamePattern",
            TestRunner::Pytest => "-k",
            TestRunner::Cargo => "--",
            TestRunner::Generic => "--grep",
        }
    }
}

/// Detect the test runner from discovery context by scanning for keywords.
fn detect_test_runner(discovery_context: &str) -> TestRunner {
    let ctx = discovery_context.to_lowercase();
    if ctx.contains("vitest") {
        TestRunner::Vitest
    } else if ctx.contains("jest") {
        TestRunner::Jest
    } else if ctx.contains("pytest") || ctx.contains("python") {
        TestRunner::Pytest
    } else if ctx.contains("cargo") || ctx.contains("rust") {
        TestRunner::Cargo
    } else {
        TestRunner::Generic
    }
}

/// Try to extract a working directory from the discovery context.
fn detect_working_dir(discovery_context: &str) -> Option<String> {
    // Look for common patterns like "frontend/", "src/", "packages/" in the context
    for keyword in &["working_dir", "cwd", "root_dir"] {
        if let Some(pos) = discovery_context.find(keyword) {
            let after = &discovery_context[pos..];
            // Try to extract a path-like value after the keyword
            if let Some(colon_pos) = after.find(':') {
                let value_start = colon_pos + 1;
                let rest = after[value_start..].trim_start();
                let end = rest.find(['"', '\'', ',', '\n', '}']).unwrap_or(rest.len());
                let path = rest[..end].trim().trim_matches('"').trim_matches('\'');
                if !path.is_empty() {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

// ============================================================================
// Core synthesis
// ============================================================================

/// Synthesize verification steps from acceptance criteria.
///
/// Deterministically maps each criterion to a verification step based on its
/// `method` field. Uses `discovery_context` to refine commands (e.g., choosing
/// the correct test runner). No AI calls are made.
pub fn synthesize_verification_steps(
    criteria: &AcceptanceCriteria,
    discovery_context: &str,
) -> SynthesisResult {
    let start = Instant::now();
    let test_runner = detect_test_runner(discovery_context);
    let working_dir = detect_working_dir(discovery_context);

    let mut steps: Vec<Value> = Vec::new();
    let mut criterion_step_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut unmapped_criteria: Vec<UnmappedCriterion> = Vec::new();
    let mut seen_commands: HashSet<String> = HashSet::new();

    debug!(
        "Synthesizing verification steps for {} criteria (test_runner={:?})",
        criteria.criteria.len(),
        test_runner
    );

    for criterion in &criteria.criteria {
        let step_id = format!("verify-{}", criterion.id);

        match criterion.method {
            VerificationMethod::Command => {
                let command = derive_command(criterion, &working_dir);
                let dedup_key = format!("command:{}", command);

                if seen_commands.contains(&dedup_key) {
                    debug!(
                        "Skipping duplicate command step for criterion '{}'",
                        criterion.id
                    );
                    // Still map the criterion to the existing step
                    if let Some(existing_step) = steps.iter().find(|s| {
                        s.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c == command)
                            .unwrap_or(false)
                    }) {
                        if let Some(existing_id) = existing_step.get("id").and_then(|v| v.as_str())
                        {
                            criterion_step_map
                                .entry(criterion.id.clone())
                                .or_default()
                                .push(existing_id.to_string());
                        }
                    }
                    continue;
                }
                seen_commands.insert(dedup_key);

                let mut step = json!({
                    "id": step_id,
                    "name": format!("Verify: {}", criterion.description),
                    "type": "command",
                    "phase": "verification",
                    "command": command,
                    "expected_exit_code": 0,
                    "criterion_ids": [&criterion.id]
                });

                if let Some(ref dir) = working_dir {
                    step.as_object_mut()
                        .unwrap()
                        .insert("working_dir".to_string(), json!(dir));
                }

                criterion_step_map
                    .entry(criterion.id.clone())
                    .or_default()
                    .push(step_id.clone());
                steps.push(step);
            }

            VerificationMethod::UiBridge => {
                let step = json!({
                    "id": step_id,
                    "name": format!("Verify: {}", criterion.description),
                    "type": "ui_bridge",
                    "phase": "verification",
                    "snapshot_assert": true,
                    "criterion_ids": [&criterion.id]
                });

                criterion_step_map
                    .entry(criterion.id.clone())
                    .or_default()
                    .push(step_id.clone());
                steps.push(step);
            }

            VerificationMethod::Test => {
                let command = format!(
                    "{} {} '{}'",
                    test_runner.command_prefix(),
                    test_runner.grep_flag(),
                    criterion.description.replace('\'', "\\'")
                );
                let dedup_key = format!("command:{}", command);

                if seen_commands.contains(&dedup_key) {
                    debug!(
                        "Skipping duplicate test step for criterion '{}'",
                        criterion.id
                    );
                    continue;
                }
                seen_commands.insert(dedup_key);

                let mut step = json!({
                    "id": step_id,
                    "name": format!("Test: {}", criterion.description),
                    "type": "command",
                    "phase": "verification",
                    "command": command,
                    "expected_exit_code": 0,
                    "criterion_ids": [&criterion.id]
                });

                if let Some(ref dir) = working_dir {
                    step.as_object_mut()
                        .unwrap()
                        .insert("working_dir".to_string(), json!(dir));
                }

                criterion_step_map
                    .entry(criterion.id.clone())
                    .or_default()
                    .push(step_id.clone());
                steps.push(step);
            }

            VerificationMethod::Manual => {
                debug!(
                    "Criterion '{}' is manual, adding to unmapped with fallback",
                    criterion.id
                );
                let fallback = json!({
                    "id": step_id,
                    "name": format!("Manual check: {}", criterion.description),
                    "type": "prompt",
                    "phase": "verification",
                    "prompt": format!(
                        "Manually verify: {}. Hint: {}",
                        criterion.description, criterion.verification_hint
                    ),
                    "criterion_ids": [&criterion.id]
                });

                unmapped_criteria.push(UnmappedCriterion {
                    criterion_id: criterion.id.clone(),
                    reason: "Manual verification method cannot be automated".to_string(),
                    fallback_step: Some(fallback),
                });
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let success = !steps.is_empty() || criteria.criteria.is_empty();

    info!(
        "Synthesized {} verification steps from {} criteria in {}ms ({} unmapped)",
        steps.len(),
        criteria.criteria.len(),
        duration_ms,
        unmapped_criteria.len()
    );

    SynthesisResult {
        steps,
        criterion_step_map,
        unmapped_criteria,
        success,
        duration_ms,
    }
}

/// Derive a shell command from a criterion's verification hint.
///
/// Attempts to extract a concrete command from the hint text. Falls back to
/// echoing the hint if no command can be extracted.
fn derive_command(criterion: &AcceptanceCriterion, working_dir: &Option<String>) -> String {
    let hint = &criterion.verification_hint;

    // Try to extract a backtick-delimited command from the hint
    if let Some(start) = hint.find('`') {
        if let Some(end) = hint[start + 1..].find('`') {
            let extracted = &hint[start + 1..start + 1 + end];
            if !extracted.is_empty() && (!extracted.contains(' ') || extracted.contains("--")) {
                return extracted.to_string();
            }
        }
    }

    // Look for common command patterns in the hint
    let hint_lower = hint.to_lowercase();
    if hint_lower.contains("tsc") || hint_lower.contains("typecheck") {
        return "npx tsc --noEmit".to_string();
    }
    if hint_lower.contains("eslint") || hint_lower.contains("lint") {
        return "npx eslint .".to_string();
    }
    if hint_lower.contains("prettier") || hint_lower.contains("format") {
        return "npx prettier --check .".to_string();
    }
    if hint_lower.contains("build") {
        return "npm run build".to_string();
    }
    if hint_lower.contains("curl") {
        // Extract the curl command if present
        if let Some(curl_start) = hint.find("curl") {
            let rest = &hint[curl_start..];
            let end = rest
                .find('\n')
                .or_else(|| rest.find('"'))
                .unwrap_or(rest.len());
            return rest[..end].trim().to_string();
        }
    }

    // API check pattern — construct a curl command
    if hint_lower.contains("api") || hint_lower.contains("endpoint") || hint_lower.contains("http")
    {
        // Try to extract a URL from the hint
        if let Some(url_start) = hint.find("http") {
            let rest = &hint[url_start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
                .unwrap_or(rest.len());
            let url = &rest[..end];
            return format!("curl -sf {}", url);
        }
    }

    // Fallback: use the hint as-is if it looks like a command, otherwise echo it
    let _ = working_dir; // reserved for future use
    if hint.starts_with("Run ") {
        // Strip "Run " prefix and backticks
        let cmd = hint.trim_start_matches("Run ").trim_matches('`').trim();
        if !cmd.is_empty() {
            return cmd.to_string();
        }
    }

    // Last resort: echo a pass/fail check
    format!("echo 'TODO: verify {}' && exit 1", criterion.id)
}

// ============================================================================
// Merge
// ============================================================================

/// Merge synthesized verification steps into an existing workflow.
///
/// For each synthesized step, checks if a step with matching `criterion_ids`
/// already exists in the workflow. If so, skips it (the builder already created
/// a step for that criterion). Otherwise, appends the step to
/// `verification_steps`.
pub fn merge_synthesized_steps(workflow_json: &mut Value, synthesis: &SynthesisResult) {
    let existing_criterion_ids = extract_criterion_ids_from_workflow(workflow_json);

    let verification_steps = workflow_json
        .get_mut("verification_steps")
        .and_then(|v| v.as_array_mut());

    let verification_steps = match verification_steps {
        Some(arr) => arr,
        None => {
            // Create the verification_steps array if it doesn't exist
            if let Some(obj) = workflow_json.as_object_mut() {
                obj.insert("verification_steps".to_string(), json!([]));
                obj.get_mut("verification_steps")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
            } else {
                warn!("Cannot merge synthesized steps: workflow is not a JSON object");
                return;
            }
        }
    };

    let mut merged = 0usize;
    let mut skipped = 0usize;

    for step in &synthesis.steps {
        let step_criterion_ids: HashSet<String> = step
            .get("criterion_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Check if any of this step's criterion_ids are already covered
        let already_covered = step_criterion_ids
            .iter()
            .any(|id| existing_criterion_ids.contains(id));

        if already_covered {
            skipped += 1;
        } else {
            verification_steps.push(step.clone());
            merged += 1;
        }
    }

    info!(
        "Merged {} synthesized steps, {} already covered by builder",
        merged, skipped
    );
}

/// Extract all criterion_ids referenced by steps in a workflow.
fn extract_criterion_ids_from_workflow(workflow_json: &Value) -> HashSet<String> {
    let mut ids = HashSet::new();

    for phase_key in &[
        "setup_steps",
        "execution_steps",
        "verification_steps",
        "teardown_steps",
    ] {
        if let Some(steps) = workflow_json.get(phase_key).and_then(|v| v.as_array()) {
            for step in steps {
                // Check both "criterion_ids" (array) and "criterion_id" (single string)
                if let Some(arr) = step.get("criterion_ids").and_then(|v| v.as_array()) {
                    for id in arr {
                        if let Some(s) = id.as_str() {
                            ids.insert(s.to_string());
                        }
                    }
                }
                if let Some(s) = step.get("criterion_id").and_then(|v| v.as_str()) {
                    ids.insert(s.to_string());
                }
            }
        }
    }

    ids
}

// ============================================================================
// Coverage report
// ============================================================================

/// Generate a coverage report comparing workflow steps against acceptance criteria.
///
/// Extracts all `criterion_ids` from steps across all workflow phases and compares
/// against the full set of criteria IDs.
pub fn coverage_report(workflow_json: &Value, criteria: &AcceptanceCriteria) -> CoverageReport {
    let covered_ids = extract_criterion_ids_from_workflow(workflow_json);

    let all_ids: Vec<String> = criteria.criteria.iter().map(|c| c.id.clone()).collect();
    let total_criteria = all_ids.len();

    let uncovered: Vec<String> = all_ids
        .into_iter()
        .filter(|id| !covered_ids.contains(id))
        .collect();

    let covered_count = total_criteria - uncovered.len();
    let coverage_ratio = if total_criteria > 0 {
        covered_count as f32 / total_criteria as f32
    } else {
        1.0
    };

    if !uncovered.is_empty() {
        warn!(
            "Criteria coverage: {}/{} ({:.0}%) — uncovered: {:?}",
            covered_count,
            total_criteria,
            coverage_ratio * 100.0,
            uncovered
        );
    } else {
        info!(
            "Criteria coverage: {}/{} (100%)",
            covered_count, total_criteria
        );
    }

    CoverageReport {
        total_criteria,
        covered_count,
        uncovered,
        coverage_ratio,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::specification::{CriterionPriority, VerificationMethod};

    fn sample_criteria() -> AcceptanceCriteria {
        AcceptanceCriteria {
            goal_summary: "Dark mode toggle works correctly".to_string(),
            criteria: vec![
                AcceptanceCriterion {
                    id: "typecheck-passes".to_string(),
                    description: "TypeScript compilation succeeds with no errors".to_string(),
                    method: VerificationMethod::Command,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Run `npx tsc --noEmit` in frontend/".to_string(),
                    category: "compilation".to_string(),
                },
                AcceptanceCriterion {
                    id: "toggle-renders".to_string(),
                    description: "Dark mode toggle button is visible on settings page".to_string(),
                    method: VerificationMethod::UiBridge,
                    priority: CriterionPriority::Critical,
                    verification_hint: "Assert element 'toggle-dark-mode' exists via UI Bridge"
                        .to_string(),
                    category: "ui-content".to_string(),
                },
                AcceptanceCriterion {
                    id: "unit-tests-pass".to_string(),
                    description: "Unit tests for dark mode pass".to_string(),
                    method: VerificationMethod::Test,
                    priority: CriterionPriority::Important,
                    verification_hint: "Run unit tests for dark mode component".to_string(),
                    category: "behavior".to_string(),
                },
                AcceptanceCriterion {
                    id: "manual-visual-review".to_string(),
                    description: "Colors look correct in dark mode".to_string(),
                    method: VerificationMethod::Manual,
                    priority: CriterionPriority::Optional,
                    verification_hint: "Visual inspection".to_string(),
                    category: "style".to_string(),
                },
            ],
            assumptions: vec![
                "Project uses TypeScript".to_string(),
                "Frontend runs on localhost:3001".to_string(),
            ],
        }
    }

    #[test]
    fn test_synthesize_generates_steps_for_automatable_criteria() {
        let criteria = sample_criteria();
        let result = synthesize_verification_steps(&criteria, "jest vitest");

        assert!(result.success);
        // 3 automatable criteria: command, ui_bridge, test
        assert_eq!(result.steps.len(), 3);
        // 1 unmapped (manual)
        assert_eq!(result.unmapped_criteria.len(), 1);
        assert_eq!(
            result.unmapped_criteria[0].criterion_id,
            "manual-visual-review"
        );
    }

    #[test]
    fn test_synthesize_command_step_structure() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Build succeeds".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "typecheck-passes".to_string(),
                description: "TypeScript compilation succeeds".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "Run `npx tsc --noEmit`".to_string(),
                category: "compilation".to_string(),
            }],
            assumptions: vec![],
        };

        let result = synthesize_verification_steps(&criteria, "");
        assert_eq!(result.steps.len(), 1);

        let step = &result.steps[0];
        assert_eq!(step["id"].as_str().unwrap(), "verify-typecheck-passes");
        assert_eq!(step["type"].as_str().unwrap(), "command");
        assert_eq!(step["phase"].as_str().unwrap(), "verification");
        assert_eq!(step["command"].as_str().unwrap(), "npx tsc --noEmit");
        assert_eq!(step["expected_exit_code"].as_i64().unwrap(), 0);
    }

    #[test]
    fn test_synthesize_ui_bridge_step() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Button renders".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "button-visible".to_string(),
                description: "Save button is visible".to_string(),
                method: VerificationMethod::UiBridge,
                priority: CriterionPriority::Critical,
                verification_hint: "Assert button exists".to_string(),
                category: "ui-content".to_string(),
            }],
            assumptions: vec![],
        };

        let result = synthesize_verification_steps(&criteria, "");
        let step = &result.steps[0];
        assert_eq!(step["type"].as_str().unwrap(), "ui_bridge");
        assert_eq!(step["snapshot_assert"].as_bool().unwrap(), true);
    }

    #[test]
    fn test_synthesize_test_step_uses_detected_runner() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Tests pass".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "tests-pass".to_string(),
                description: "Unit tests pass".to_string(),
                method: VerificationMethod::Test,
                priority: CriterionPriority::Critical,
                verification_hint: "Run tests".to_string(),
                category: "behavior".to_string(),
            }],
            assumptions: vec![],
        };

        // With vitest context
        let result = synthesize_verification_steps(&criteria, "project uses vitest for testing");
        let cmd = result.steps[0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("vitest"),
            "Expected vitest in command: {}",
            cmd
        );

        // With pytest context
        let result = synthesize_verification_steps(&criteria, "python project with pytest");
        let cmd = result.steps[0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("pytest"),
            "Expected pytest in command: {}",
            cmd
        );
    }

    #[test]
    fn test_synthesize_criterion_step_map() {
        let criteria = sample_criteria();
        let result = synthesize_verification_steps(&criteria, "");

        assert!(result.criterion_step_map.contains_key("typecheck-passes"));
        assert!(result.criterion_step_map.contains_key("toggle-renders"));
        assert!(result.criterion_step_map.contains_key("unit-tests-pass"));
        // manual criterion should NOT be in the map
        assert!(!result
            .criterion_step_map
            .contains_key("manual-visual-review"));
    }

    #[test]
    fn test_merge_skips_already_covered() {
        let criteria = sample_criteria();
        let synthesis = synthesize_verification_steps(&criteria, "");

        let mut workflow = json!({
            "verification_steps": [
                {
                    "id": "existing-step",
                    "type": "command",
                    "criterion_ids": ["typecheck-passes"]
                }
            ]
        });

        let initial_count = workflow["verification_steps"].as_array().unwrap().len();
        merge_synthesized_steps(&mut workflow, &synthesis);
        let final_count = workflow["verification_steps"].as_array().unwrap().len();

        // Should have added steps for toggle-renders and unit-tests-pass, but not typecheck-passes
        assert_eq!(final_count, initial_count + 2);
    }

    #[test]
    fn test_merge_creates_verification_steps_array() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Build succeeds".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "build-ok".to_string(),
                description: "Build succeeds".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "Run `npm run build`".to_string(),
                category: "compilation".to_string(),
            }],
            assumptions: vec![],
        };

        let synthesis = synthesize_verification_steps(&criteria, "");
        let mut workflow = json!({});

        merge_synthesized_steps(&mut workflow, &synthesis);
        assert!(workflow.get("verification_steps").is_some());
        assert_eq!(workflow["verification_steps"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_coverage_report_full_coverage() {
        let criteria = AcceptanceCriteria {
            goal_summary: "All covered".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "check-a".to_string(),
                description: "Check A".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run something".to_string(),
                category: "general".to_string(),
            }],
            assumptions: vec![],
        };

        let workflow = json!({
            "verification_steps": [
                { "criterion_ids": ["check-a"] }
            ]
        });

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.total_criteria, 1);
        assert_eq!(report.covered_count, 1);
        assert!(report.uncovered.is_empty());
        assert!((report.coverage_ratio - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_coverage_report_partial_coverage() {
        let criteria = sample_criteria();

        let workflow = json!({
            "verification_steps": [
                { "criterion_ids": ["typecheck-passes"] }
            ]
        });

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.total_criteria, 4);
        assert_eq!(report.covered_count, 1);
        assert_eq!(report.uncovered.len(), 3);
        assert!(report.coverage_ratio < 0.5);
    }

    #[test]
    fn test_coverage_report_empty_criteria() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Nothing".to_string(),
            criteria: vec![],
            assumptions: vec![],
        };
        let workflow = json!({});

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.total_criteria, 0);
        assert_eq!(report.covered_count, 0);
        assert!((report.coverage_ratio - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_coverage_report_checks_criterion_id_singular() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Check singular field".to_string(),
            criteria: vec![AcceptanceCriterion {
                id: "check-b".to_string(),
                description: "Check B".to_string(),
                method: VerificationMethod::Command,
                priority: CriterionPriority::Critical,
                verification_hint: "run it".to_string(),
                category: "general".to_string(),
            }],
            assumptions: vec![],
        };

        // Use singular criterion_id field (as the builder might produce)
        let workflow = json!({
            "verification_steps": [
                { "criterion_id": "check-b" }
            ]
        });

        let report = coverage_report(&workflow, &criteria);
        assert_eq!(report.covered_count, 1);
    }

    #[test]
    fn test_empty_criteria_synthesis() {
        let criteria = AcceptanceCriteria {
            goal_summary: "Nothing to do".to_string(),
            criteria: vec![],
            assumptions: vec![],
        };

        let result = synthesize_verification_steps(&criteria, "");
        assert!(result.success);
        assert!(result.steps.is_empty());
        assert!(result.unmapped_criteria.is_empty());
    }

    #[test]
    fn test_detect_test_runner() {
        assert_eq!(detect_test_runner("uses vitest"), TestRunner::Vitest);
        assert_eq!(detect_test_runner("jest config"), TestRunner::Jest);
        assert_eq!(
            detect_test_runner("python pytest project"),
            TestRunner::Pytest
        );
        assert_eq!(detect_test_runner("cargo rust"), TestRunner::Cargo);
        assert_eq!(detect_test_runner("unknown project"), TestRunner::Generic);
    }
}
