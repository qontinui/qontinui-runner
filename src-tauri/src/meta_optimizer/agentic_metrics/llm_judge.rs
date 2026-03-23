//! Shared LLM-as-judge infrastructure for qualitative metrics.
//!
//! Provides a batched evaluation pattern: a single LLM call scores multiple
//! metrics (PlanAdherence, GoalAccuracy, PlanQuality) to minimize API costs.
//!
//! Uses the runner's existing `ai_provider::routing` for LLM calls and parses
//! structured JSON responses containing scores and rationales.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{AgenticMetric, MetricResult};

/// Input data for LLM-as-judge evaluation.
///
/// Gathered from `task_runs` (prompt, execution_steps_json, summary, goal_achieved)
/// and optionally from `task_run_events` for execution trace details.
#[derive(Debug, Clone)]
pub struct LlmJudgeInput {
    pub task_run_id: String,
    /// The original task prompt / goal description.
    pub task_prompt: String,
    /// The planned execution steps (JSON from execution_steps_json).
    pub execution_plan: Option<String>,
    /// AI-generated summary of what happened.
    pub outcome_summary: Option<String>,
    /// Whether the goal was achieved (from task_runs.goal_achieved).
    pub goal_achieved: Option<bool>,
    /// Status: 'complete', 'failed', 'stopped'.
    pub status: String,
    /// Brief execution trace: key events from task_run_events.
    pub execution_trace_summary: Option<String>,
}

/// A single metric score parsed from the LLM judge response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JudgeMetricScore {
    metric: String,
    score: f64,
    rationale: String,
}

/// The full structured response expected from the LLM judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JudgeResponse {
    scores: Vec<JudgeMetricScore>,
}

/// Build the batched evaluation prompt for all three LLM-judged metrics.
fn build_judge_prompt(input: &LlmJudgeInput) -> String {
    let plan_section = input
        .execution_plan
        .as_deref()
        .unwrap_or("(no execution plan available)");

    let summary_section = input
        .outcome_summary
        .as_deref()
        .unwrap_or("(no outcome summary available)");

    let goal_status = match input.goal_achieved {
        Some(true) => "Goal was achieved",
        Some(false) => "Goal was NOT achieved",
        None => "Goal achievement unknown",
    };

    let trace_section = input
        .execution_trace_summary
        .as_deref()
        .unwrap_or("(no execution trace available)");

    format!(
        r#"You are an agentic workflow evaluator. Score the following workflow execution on three dimensions.
Each score must be a number between 0.0 and 1.0. Provide a one-sentence rationale for each.

## Task Goal
{task_prompt}

## Execution Plan
{plan_section}

## Execution Trace (key events)
{trace_section}

## Outcome
Status: {status}
{goal_status}
Summary: {summary_section}

Respond with ONLY a JSON object in this exact format (no markdown fences, no extra text):
{{"scores": [
  {{"metric": "plan_adherence", "score": <0.0-1.0>, "rationale": "<one sentence>"}},
  {{"metric": "goal_accuracy", "score": <0.0-1.0>, "rationale": "<one sentence>"}},
  {{"metric": "plan_quality", "score": <0.0-1.0>, "rationale": "<one sentence>"}}
]}}"#,
        task_prompt = input.task_prompt,
        plan_section = plan_section,
        trace_section = trace_section,
        status = input.status,
        goal_status = goal_status,
        summary_section = summary_section,
    )
}

/// Parse the LLM response into metric results.
///
/// Handles common response variations: JSON with/without markdown fences,
/// partial responses, and malformed JSON.
fn parse_judge_response(response: &str, model_used: &str) -> Vec<MetricResult> {
    // Try to extract JSON from response (may be wrapped in markdown fences)
    let json_str = extract_json_from_response(response);

    let parsed: JudgeResponse = match serde_json::from_str(&json_str) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to parse LLM judge response: {}. Raw: {}", e, &response[..response.len().min(200)]);
            return fallback_scores(model_used, "Failed to parse LLM response");
        }
    };

    let mut results = Vec::new();

    for judge_score in &parsed.scores {
        let metric = match judge_score.metric.as_str() {
            "plan_adherence" => AgenticMetric::PlanAdherence,
            "goal_accuracy" => AgenticMetric::GoalAccuracy,
            "plan_quality" => AgenticMetric::PlanQuality,
            other => {
                debug!("Ignoring unknown metric from LLM judge: {}", other);
                continue;
            }
        };

        let score = judge_score.score.clamp(0.0, 1.0);

        results.push(MetricResult {
            metric,
            score,
            confidence: 0.7, // LLM-judged scores have moderate confidence
            rationale: judge_score.rationale.clone(),
            is_llm_judged: true,
            model_used: Some(model_used.to_string()),
        });
    }

    // Ensure we got all three metrics; fill missing ones
    for expected in &[
        AgenticMetric::PlanAdherence,
        AgenticMetric::GoalAccuracy,
        AgenticMetric::PlanQuality,
    ] {
        if !results.iter().any(|r| r.metric == *expected) {
            results.push(MetricResult {
                metric: *expected,
                score: 0.5,
                confidence: 0.3,
                rationale: "Metric missing from LLM judge response — neutral score".into(),
                is_llm_judged: true,
                model_used: Some(model_used.to_string()),
            });
        }
    }

    results
}

/// Extract JSON object from a response that may contain markdown fences or surrounding text.
///
/// Public so that `rag_judge` can reuse this.
pub fn extract_json_from_response(response: &str) -> String {
    let trimmed = response.trim();

    // Try stripping markdown code fences
    if let Some(start) = trimmed.find("```json") {
        if let Some(end) = trimmed[start + 7..].find("```") {
            return trimmed[start + 7..start + 7 + end].trim().to_string();
        }
    }
    if let Some(start) = trimmed.find("```") {
        if let Some(end) = trimmed[start + 3..].find("```") {
            let inner = trimmed[start + 3..start + 3 + end].trim();
            if inner.starts_with('{') {
                return inner.to_string();
            }
        }
    }

    // Try finding the outermost JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return trimmed[start..=end].to_string();
        }
    }

    trimmed.to_string()
}

/// Fallback scores when LLM response can't be parsed.
fn fallback_scores(model_used: &str, reason: &str) -> Vec<MetricResult> {
    vec![
        AgenticMetric::PlanAdherence,
        AgenticMetric::GoalAccuracy,
        AgenticMetric::PlanQuality,
    ]
    .into_iter()
    .map(|metric| MetricResult {
        metric,
        score: 0.5,
        confidence: 0.1,
        rationale: format!("LLM judge fallback: {}", reason),
        is_llm_judged: true,
        model_used: Some(model_used.to_string()),
    })
    .collect()
}

/// Determine whether a run should be LLM-judged.
///
/// Only runs with iterations > 0 and ambiguous outcomes justify the LLM cost.
/// Clear successes and zero-iteration crashes are well-handled by deterministic metrics.
pub fn should_llm_judge(
    iterations: u32,
    verification_passed: bool,
    was_stopped: bool,
) -> bool {
    // Skip zero-iteration crashes — deterministic metrics handle these
    if iterations == 0 {
        return false;
    }

    // Always judge partial completions — most ambiguous
    if was_stopped {
        return true;
    }

    // Judge failures that attempted work — useful signal
    if !verification_passed {
        return true;
    }

    // Successful runs with many iterations may have interesting quality signals
    if iterations >= 3 {
        return true;
    }

    // Simple successes (1-2 iterations, passed) don't need LLM judging
    false
}

/// Run the LLM judge evaluation synchronously.
///
/// This is a blocking call that makes one LLM API request.
/// Should be called from `tokio::task::spawn_blocking` in async contexts.
pub fn evaluate_sync(input: &LlmJudgeInput) -> Vec<MetricResult> {
    use crate::ai_provider::routing::run_prompt_with_routing;
    use crate::ai_router::TaskContext;

    let prompt = build_judge_prompt(input);
    let context = TaskContext::from_prompt(&prompt);

    info!(
        "Running LLM judge evaluation for task {} ({} chars prompt)",
        input.task_run_id,
        prompt.len()
    );

    let response = run_prompt_with_routing(&prompt, &context, None);

    if !response.success {
        let error = response.error.unwrap_or_else(|| "Unknown error".into());
        warn!("LLM judge call failed for task {}: {}", input.task_run_id, error);
        return fallback_scores("unknown", &error);
    }

    let model = "routed"; // Model is selected by routing; exact model not exposed in AiResponse
    let results = parse_judge_response(&response.output, model);

    info!(
        "LLM judge scored task {}: {}",
        input.task_run_id,
        results
            .iter()
            .map(|r| format!("{}={:.2}", r.metric, r.score))
            .collect::<Vec<_>>()
            .join(", ")
    );

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_plain() {
        let input = r#"{"scores": [{"metric": "plan_adherence", "score": 0.8, "rationale": "good"}]}"#;
        let result = extract_json_from_response(input);
        assert!(result.starts_with('{'));
        assert!(result.contains("plan_adherence"));
    }

    #[test]
    fn test_extract_json_with_fences() {
        let input = "Here's the evaluation:\n```json\n{\"scores\": []}\n```\nDone.";
        let result = extract_json_from_response(input);
        assert_eq!(result, "{\"scores\": []}");
    }

    #[test]
    fn test_extract_json_with_surrounding_text() {
        let input = "Let me evaluate. {\"scores\": []} That's my assessment.";
        let result = extract_json_from_response(input);
        assert_eq!(result, "{\"scores\": []}");
    }

    #[test]
    fn test_parse_judge_response_valid() {
        let response = r#"{"scores": [
            {"metric": "plan_adherence", "score": 0.85, "rationale": "Plan was followed closely"},
            {"metric": "goal_accuracy", "score": 0.9, "rationale": "Goal mostly achieved"},
            {"metric": "plan_quality", "score": 0.7, "rationale": "Plan was reasonable"}
        ]}"#;
        let results = parse_judge_response(response, "test-model");
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].metric, AgenticMetric::PlanAdherence);
        assert!((results[0].score - 0.85).abs() < 0.001);
        assert!(results[0].is_llm_judged);
        assert_eq!(results[0].model_used.as_deref(), Some("test-model"));
    }

    #[test]
    fn test_parse_judge_response_missing_metric() {
        let response = r#"{"scores": [
            {"metric": "goal_accuracy", "score": 0.9, "rationale": "Good"}
        ]}"#;
        let results = parse_judge_response(response, "test");
        assert_eq!(results.len(), 3); // Missing ones filled with 0.5
        let pa = results.iter().find(|r| r.metric == AgenticMetric::PlanAdherence).unwrap();
        assert_eq!(pa.score, 0.5);
        assert!(pa.confidence < 0.5);
    }

    #[test]
    fn test_parse_judge_response_invalid_json() {
        let results = parse_judge_response("not json at all", "test");
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.score == 0.5));
        assert!(results.iter().all(|r| r.confidence < 0.2));
    }

    #[test]
    fn test_parse_judge_response_clamps_scores() {
        let response = r#"{"scores": [
            {"metric": "plan_adherence", "score": 1.5, "rationale": "Over"},
            {"metric": "goal_accuracy", "score": -0.3, "rationale": "Under"},
            {"metric": "plan_quality", "score": 0.5, "rationale": "Normal"}
        ]}"#;
        let results = parse_judge_response(response, "test");
        assert_eq!(results[0].score, 1.0); // Clamped from 1.5
        assert_eq!(results[1].score, 0.0); // Clamped from -0.3
        assert_eq!(results[2].score, 0.5);
    }

    #[test]
    fn test_should_llm_judge() {
        // Zero iterations — never judge
        assert!(!should_llm_judge(0, false, false));
        assert!(!should_llm_judge(0, true, false));

        // Stopped by user — always judge
        assert!(should_llm_judge(1, false, true));
        assert!(should_llm_judge(2, true, true));

        // Failed with work — judge
        assert!(should_llm_judge(1, false, false));
        assert!(should_llm_judge(3, false, false));

        // Simple success — don't judge
        assert!(!should_llm_judge(1, true, false));
        assert!(!should_llm_judge(2, true, false));

        // Complex success (3+ iterations) — judge
        assert!(should_llm_judge(3, true, false));
    }

    #[test]
    fn test_build_judge_prompt_includes_all_sections() {
        let input = LlmJudgeInput {
            task_run_id: "test-1".into(),
            task_prompt: "Fix the login bug".into(),
            execution_plan: Some("[{\"step\": \"read auth.rs\"}]".into()),
            outcome_summary: Some("Fixed authentication bypass".into()),
            goal_achieved: Some(true),
            status: "complete".into(),
            execution_trace_summary: Some("Step 1: read auth.rs, Step 2: modified login()".into()),
        };
        let prompt = build_judge_prompt(&input);
        assert!(prompt.contains("Fix the login bug"));
        assert!(prompt.contains("read auth.rs"));
        assert!(prompt.contains("Fixed authentication bypass"));
        assert!(prompt.contains("Goal was achieved"));
        assert!(prompt.contains("plan_adherence"));
        assert!(prompt.contains("goal_accuracy"));
        assert!(prompt.contains("plan_quality"));
    }
}
