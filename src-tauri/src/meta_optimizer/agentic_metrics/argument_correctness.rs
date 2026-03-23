//! Argument Correctness metric — deterministic scorer.
//!
//! Evaluates the correctness of arguments passed to pipeline agents by
//! validating that input/output snapshots contain expected fields per agent type.
//!
//! This is a structural validation (does the JSON have the right shape?) not
//! a semantic one (is the content correct?). Semantic correctness is handled
//! by LLM-as-judge metrics in Phase 3.
//!
//! Agent type expectations:
//! - **spec_analyst**: input has task_description; output has criteria[]
//! - **locator**: input has criteria; output has file_paths[]
//! - **implementer**: input has file_paths, criteria; output has changes[]
//! - **verifier**: input has criteria; output has results[]
//! - **snapshot**: input has url or selector; output has image_data or screenshot
//!
//! Scoring:
//! - 1.0 — all expected fields present and non-empty
//! - 0.5–0.9 — some fields present
//! - 0.0 — no traces available or zero iterations

use super::{AgenticMetric, MetricResult};

/// Input data for argument correctness scoring.
#[derive(Debug, Clone)]
pub struct ArgumentCorrectnessInput {
    /// Pipeline agent traces as (agent_type, input_json, output_json) tuples.
    /// Empty if no traces exist (traditional architecture, or empty traces issue).
    pub traces: Vec<TraceSnapshot>,
    /// Whether the run had zero iterations.
    pub zero_iterations: bool,
}

/// A simplified trace snapshot for validation.
#[derive(Debug, Clone)]
pub struct TraceSnapshot {
    pub agent_type: String,
    pub input_snapshot: serde_json::Value,
    pub output_snapshot: serde_json::Value,
}

/// Compute the argument correctness score.
pub fn score(input: &ArgumentCorrectnessInput) -> MetricResult {
    let (score_val, rationale, confidence) = compute(input);

    MetricResult {
        metric: AgenticMetric::ArgumentCorrectness,
        score: score_val,
        confidence,
        rationale,
        is_llm_judged: false,
        model_used: None,
    }
}

fn compute(input: &ArgumentCorrectnessInput) -> (f64, String, f64) {
    if input.zero_iterations {
        return (
            0.0,
            "Zero iterations — no agent traces to validate".into(),
            0.95,
        );
    }

    if input.traces.is_empty() {
        // No traces available — common for traditional architecture runs.
        // This is a known gap (pipeline_agent_traces empty), not a failure.
        return (
            0.5,
            "No pipeline agent traces available — neutral score (traditional architecture)".into(),
            0.2,
        );
    }

    let mut total_checks = 0;
    let mut passed_checks = 0;
    let mut details = Vec::new();

    for trace in &input.traces {
        let (agent_passed, agent_total, _agent_detail) = validate_agent_trace(trace);
        passed_checks += agent_passed;
        total_checks += agent_total;
        if agent_passed < agent_total {
            details.push(format!(
                "{}:{}/{}",
                trace.agent_type, agent_passed, agent_total
            ));
        }
    }

    if total_checks == 0 {
        return (0.5, "No validatable fields found in traces".into(), 0.3);
    }

    let score_val = passed_checks as f64 / total_checks as f64;
    let confidence = 0.75 + (input.traces.len().min(4) as f64 * 0.05); // More traces = more confidence

    let rationale = if details.is_empty() {
        format!(
            "All {}/{} structural checks passed across {} agent traces",
            passed_checks,
            total_checks,
            input.traces.len()
        )
    } else {
        format!(
            "{}/{} structural checks passed. Issues: [{}]",
            passed_checks,
            total_checks,
            details.join(", ")
        )
    };

    (score_val, rationale, confidence.min(0.95))
}

/// Validate a single agent trace against expected schema for its type.
/// Returns (passed_checks, total_checks, detail_string).
fn validate_agent_trace(trace: &TraceSnapshot) -> (u32, u32, String) {
    match trace.agent_type.as_str() {
        "spec_analyst" => validate_spec_analyst(trace),
        "locator" => validate_locator(trace),
        "implementer" => validate_implementer(trace),
        "verifier" => validate_verifier(trace),
        "snapshot" => validate_snapshot(trace),
        _ => {
            // Unknown agent type — just check non-empty input/output
            let mut passed = 0u32;
            let total = 2u32;
            if !trace.input_snapshot.is_null() && trace.input_snapshot != serde_json::json!({}) {
                passed += 1;
            }
            if !trace.output_snapshot.is_null() && trace.output_snapshot != serde_json::json!({}) {
                passed += 1;
            }
            (
                passed,
                total,
                format!("unknown agent '{}': basic check", trace.agent_type),
            )
        }
    }
}

fn validate_spec_analyst(trace: &TraceSnapshot) -> (u32, u32, String) {
    let mut passed = 0u32;
    let total = 3u32;

    // Input should have task description or goal
    if has_nonempty_field(
        &trace.input_snapshot,
        &["task_description", "goal", "prompt", "description"],
    ) {
        passed += 1;
    }
    // Output should have criteria array
    if has_array_field(
        &trace.output_snapshot,
        &["criteria", "acceptance_criteria", "requirements"],
    ) {
        passed += 1;
    }
    // Output should be non-empty
    if !trace.output_snapshot.is_null() && trace.output_snapshot != serde_json::json!({}) {
        passed += 1;
    }

    (passed, total, "spec_analyst".into())
}

fn validate_locator(trace: &TraceSnapshot) -> (u32, u32, String) {
    let mut passed = 0u32;
    let total = 3u32;

    // Input should have criteria or task context
    if has_nonempty_field(
        &trace.input_snapshot,
        &["criteria", "task_description", "context"],
    ) {
        passed += 1;
    }
    // Output should have file paths
    if has_array_field(
        &trace.output_snapshot,
        &["file_paths", "files", "paths", "relevant_files"],
    ) {
        passed += 1;
    }
    // Output should be non-empty
    if !trace.output_snapshot.is_null() && trace.output_snapshot != serde_json::json!({}) {
        passed += 1;
    }

    (passed, total, "locator".into())
}

fn validate_implementer(trace: &TraceSnapshot) -> (u32, u32, String) {
    let mut passed = 0u32;
    let total = 4u32;

    // Input should have file paths
    if has_array_field(&trace.input_snapshot, &["file_paths", "files", "paths"]) {
        passed += 1;
    }
    // Input should have criteria
    if has_nonempty_field(&trace.input_snapshot, &["criteria", "requirements", "task"]) {
        passed += 1;
    }
    // Output should have changes
    if has_array_field(
        &trace.output_snapshot,
        &["changes", "modifications", "edits"],
    ) || has_nonempty_field(&trace.output_snapshot, &["diff", "patch", "output"])
    {
        passed += 1;
    }
    // Output should be non-empty
    if !trace.output_snapshot.is_null() && trace.output_snapshot != serde_json::json!({}) {
        passed += 1;
    }

    (passed, total, "implementer".into())
}

fn validate_verifier(trace: &TraceSnapshot) -> (u32, u32, String) {
    let mut passed = 0u32;
    let total = 3u32;

    // Input should have criteria to verify
    if has_nonempty_field(&trace.input_snapshot, &["criteria", "checks", "assertions"]) {
        passed += 1;
    }
    // Output should have results
    if has_array_field(
        &trace.output_snapshot,
        &["results", "criterion_results", "checks"],
    ) || has_nonempty_field(&trace.output_snapshot, &["passed", "verdict", "all_passed"])
    {
        passed += 1;
    }
    // Output should be non-empty
    if !trace.output_snapshot.is_null() && trace.output_snapshot != serde_json::json!({}) {
        passed += 1;
    }

    (passed, total, "verifier".into())
}

fn validate_snapshot(trace: &TraceSnapshot) -> (u32, u32, String) {
    let mut passed = 0u32;
    let total = 3u32;

    // Input should have URL or selector
    if has_nonempty_field(
        &trace.input_snapshot,
        &["url", "selector", "page_url", "target"],
    ) {
        passed += 1;
    }
    // Output should have image data
    if has_nonempty_field(
        &trace.output_snapshot,
        &["image_data", "screenshot", "base64", "path"],
    ) {
        passed += 1;
    }
    // Output should be non-empty
    if !trace.output_snapshot.is_null() && trace.output_snapshot != serde_json::json!({}) {
        passed += 1;
    }

    (passed, total, "snapshot".into())
}

/// Check if any of the given field names exist and are non-empty strings in the JSON.
fn has_nonempty_field(json: &serde_json::Value, field_names: &[&str]) -> bool {
    if let Some(obj) = json.as_object() {
        for name in field_names {
            if let Some(val) = obj.get(*name) {
                match val {
                    serde_json::Value::String(s) => {
                        if !s.is_empty() {
                            return true;
                        }
                    }
                    serde_json::Value::Null => continue,
                    _ => return true, // Non-null, non-string value (object, number, bool)
                }
            }
        }
    }
    false
}

/// Check if any of the given field names exist and are non-empty arrays in the JSON.
fn has_array_field(json: &serde_json::Value, field_names: &[&str]) -> bool {
    if let Some(obj) = json.as_object() {
        for name in field_names {
            if let Some(serde_json::Value::Array(arr)) = obj.get(*name) {
                if !arr.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn trace(
        agent_type: &str,
        input: serde_json::Value,
        output: serde_json::Value,
    ) -> TraceSnapshot {
        TraceSnapshot {
            agent_type: agent_type.to_string(),
            input_snapshot: input,
            output_snapshot: output,
        }
    }

    #[test]
    fn test_zero_iterations() {
        let input = ArgumentCorrectnessInput {
            traces: vec![],
            zero_iterations: true,
        };
        let result = score(&input);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn test_no_traces() {
        let input = ArgumentCorrectnessInput {
            traces: vec![],
            zero_iterations: false,
        };
        let result = score(&input);
        assert_eq!(result.score, 0.5);
        assert!(result.confidence < 0.5);
    }

    #[test]
    fn test_perfect_spec_analyst() {
        let input = ArgumentCorrectnessInput {
            traces: vec![trace(
                "spec_analyst",
                json!({"task_description": "Add login page"}),
                json!({"criteria": [{"id": "c1", "description": "Login form exists"}]}),
            )],
            zero_iterations: false,
        };
        let result = score(&input);
        assert_eq!(result.score, 1.0);
    }

    #[test]
    fn test_partial_implementer() {
        let input = ArgumentCorrectnessInput {
            traces: vec![trace(
                "implementer",
                json!({"file_paths": ["/src/main.rs"]}), // has file_paths but no criteria
                json!({"changes": [{"file": "main.rs"}]}),
            )],
            zero_iterations: false,
        };
        let result = score(&input);
        // 3/4 checks pass: file_paths yes, criteria no, changes yes, non-empty yes
        assert!((result.score - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_empty_output() {
        let input = ArgumentCorrectnessInput {
            traces: vec![trace(
                "verifier",
                json!({"criteria": [{"id": "c1"}]}),
                json!({}),
            )],
            zero_iterations: false,
        };
        let result = score(&input);
        // 1/3: criteria present, but no results and empty output
        assert!((result.score - 0.333).abs() < 0.01);
    }

    #[test]
    fn test_multiple_traces_blended() {
        let input = ArgumentCorrectnessInput {
            traces: vec![
                trace(
                    "spec_analyst",
                    json!({"task_description": "Fix bug"}),
                    json!({"criteria": [{"id": "c1"}]}),
                ),
                trace(
                    "implementer",
                    json!({"file_paths": ["/src/main.rs"], "criteria": [{"id": "c1"}]}),
                    json!({"changes": [{"file": "main.rs"}]}),
                ),
            ],
            zero_iterations: false,
        };
        let result = score(&input);
        // spec_analyst: 3/3, implementer: 4/4 → 7/7 = 1.0
        assert!((result.score - 1.0).abs() < 0.01);
    }
}
