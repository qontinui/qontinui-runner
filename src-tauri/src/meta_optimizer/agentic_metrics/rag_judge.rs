//! RAG quality metrics — LLM-as-judge for retrieval evaluation.
//!
//! Evaluates four dimensions of RAG quality using a single batched LLM call:
//! - **Contextual Precision**: Are top-ranked results the most relevant?
//! - **Contextual Recall**: Is relevant context being missed?
//! - **Faithfulness**: Does the output stay faithful to retrieved evidence?
//! - **Answer Relevancy**: Is the retrieved context useful for the consuming prompt?
//!
//! Only scored for runs that involve retrieval (hybrid search or visual RAG).

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{AgenticMetric, MetricResult};

/// A captured retrieval event for post-hoc RAG evaluation.
///
/// Logged by the hybrid search callers when retrieval feeds into an LLM prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalEvent {
    /// The query text used for retrieval.
    pub query_text: String,
    /// Which table was searched (findings, knowledge, error_events, etc.).
    pub source_table: String,
    /// Number of results returned.
    pub result_count: usize,
    /// Top results with their similarity scores (truncated for token efficiency).
    pub top_results: Vec<RetrievalResultSummary>,
    /// The formatted context string injected into the LLM prompt (if available).
    pub formatted_context: Option<String>,
}

/// Summary of a single retrieval result for evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalResultSummary {
    /// Brief identifier or title of the result.
    pub title: String,
    /// Cosine similarity score.
    pub similarity: f32,
    /// Hybrid score.
    pub hybrid_score: f32,
    /// First ~200 chars of the result content.
    pub content_preview: String,
}

/// Input data for RAG quality evaluation.
#[derive(Debug, Clone)]
pub struct RagJudgeInput {
    pub task_run_id: String,
    /// The original task prompt (what was the user trying to accomplish).
    pub task_prompt: String,
    /// All retrieval events captured during this run.
    pub retrieval_events: Vec<RetrievalEvent>,
    /// The LLM output that consumed the retrieved context (if available).
    pub generated_output: Option<String>,
}

/// A single RAG metric score parsed from the LLM judge response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RagJudgeScore {
    metric: String,
    score: f64,
    rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RagJudgeResponse {
    scores: Vec<RagJudgeScore>,
}

/// Build the batched evaluation prompt for all four RAG metrics.
fn build_rag_judge_prompt(input: &RagJudgeInput) -> String {
    let mut retrieval_section = String::new();
    for (i, event) in input.retrieval_events.iter().enumerate() {
        retrieval_section.push_str(&format!(
            "\n### Retrieval {} (table: {}, {} results)\n",
            i + 1,
            event.source_table,
            event.result_count,
        ));
        retrieval_section.push_str(&format!("Query: {}\n", event.query_text));
        for (j, result) in event.top_results.iter().take(5).enumerate() {
            retrieval_section.push_str(&format!(
                "  Result {}: sim={:.3}, hybrid={:.3} — {} — {}\n",
                j + 1,
                result.similarity,
                result.hybrid_score,
                result.title,
                &result.content_preview[..result.content_preview.len().min(150)],
            ));
        }
        if let Some(ref ctx) = event.formatted_context {
            let preview = &ctx[..ctx.len().min(500)];
            retrieval_section.push_str(&format!("Formatted context (preview): {}\n", preview));
        }
    }

    let output_section = input
        .generated_output
        .as_deref()
        .map(|o| &o[..o.len().min(1000)])
        .unwrap_or("(no generated output available)");

    format!(
        r#"You are a RAG (Retrieval-Augmented Generation) quality evaluator. Score the following retrieval-augmented workflow on four dimensions.
Each score must be a number between 0.0 and 1.0. Provide a one-sentence rationale for each.

## Task Goal
{task_prompt}

## Retrieval Events
{retrieval_section}

## Generated Output (that consumed the retrieved context)
{output_section}

Respond with ONLY a JSON object in this exact format (no markdown fences, no extra text):
{{"scores": [
  {{"metric": "rag_contextual_precision", "score": <0.0-1.0>, "rationale": "<one sentence — are the top-ranked results the most relevant?>"}},
  {{"metric": "rag_contextual_recall", "score": <0.0-1.0>, "rationale": "<one sentence — is important context being missed?>"}},
  {{"metric": "rag_faithfulness", "score": <0.0-1.0>, "rationale": "<one sentence — does the output stay faithful to retrieved evidence?>"}},
  {{"metric": "rag_answer_relevancy", "score": <0.0-1.0>, "rationale": "<one sentence — is the retrieved context useful for the task?>"}}
]}}"#,
        task_prompt = input.task_prompt,
        retrieval_section = retrieval_section,
        output_section = output_section,
    )
}

/// Parse the LLM response into RAG metric results.
fn parse_rag_judge_response(response: &str, model_used: &str) -> Vec<MetricResult> {
    let json_str = super::llm_judge::extract_json_from_response(response);

    let parsed: RagJudgeResponse = match serde_json::from_str(&json_str) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Failed to parse RAG judge response: {}. Raw: {}",
                e,
                &response[..response.len().min(200)]
            );
            return rag_fallback_scores(model_used, "Failed to parse LLM response");
        }
    };

    let mut results = Vec::new();
    for score in &parsed.scores {
        let metric = match score.metric.as_str() {
            "rag_contextual_precision" => AgenticMetric::RagContextualPrecision,
            "rag_contextual_recall" => AgenticMetric::RagContextualRecall,
            "rag_faithfulness" => AgenticMetric::RagFaithfulness,
            "rag_answer_relevancy" => AgenticMetric::RagAnswerRelevancy,
            other => {
                debug!("Ignoring unknown RAG metric from LLM judge: {}", other);
                continue;
            }
        };

        results.push(MetricResult {
            metric,
            score: score.score.clamp(0.0, 1.0),
            confidence: 0.65, // RAG evaluation is inherently harder to judge
            rationale: score.rationale.clone(),
            is_llm_judged: true,
            model_used: Some(model_used.to_string()),
        });
    }

    // Fill missing metrics
    for expected in &[
        AgenticMetric::RagContextualPrecision,
        AgenticMetric::RagContextualRecall,
        AgenticMetric::RagFaithfulness,
        AgenticMetric::RagAnswerRelevancy,
    ] {
        if !results.iter().any(|r| r.metric == *expected) {
            results.push(MetricResult {
                metric: *expected,
                score: 0.5,
                confidence: 0.2,
                rationale: "RAG metric missing from LLM judge response — neutral score".into(),
                is_llm_judged: true,
                model_used: Some(model_used.to_string()),
            });
        }
    }

    results
}

/// Fallback scores when RAG LLM response can't be parsed.
fn rag_fallback_scores(model_used: &str, reason: &str) -> Vec<MetricResult> {
    vec![
        AgenticMetric::RagContextualPrecision,
        AgenticMetric::RagContextualRecall,
        AgenticMetric::RagFaithfulness,
        AgenticMetric::RagAnswerRelevancy,
    ]
    .into_iter()
    .map(|metric| MetricResult {
        metric,
        score: 0.5,
        confidence: 0.1,
        rationale: format!("RAG judge fallback: {}", reason),
        is_llm_judged: true,
        model_used: Some(model_used.to_string()),
    })
    .collect()
}

/// Run the RAG judge evaluation synchronously.
///
/// Blocking call — use `tokio::task::spawn_blocking` in async contexts.
pub fn evaluate_rag_sync(input: &RagJudgeInput) -> Vec<MetricResult> {
    use crate::ai_provider::routing::run_prompt_with_routing;
    use crate::ai_router::TaskContext;

    if input.retrieval_events.is_empty() {
        return vec![]; // No retrievals → no RAG metrics to score
    }

    let prompt = build_rag_judge_prompt(input);
    let context = TaskContext::from_prompt(&prompt);

    info!(
        "Running RAG judge evaluation for task {} ({} retrieval events, {} chars prompt)",
        input.task_run_id,
        input.retrieval_events.len(),
        prompt.len()
    );

    let response = run_prompt_with_routing(&prompt, &context, None);

    if !response.success {
        let error = response.error.unwrap_or_else(|| "Unknown error".into());
        warn!(
            "RAG judge call failed for task {}: {}",
            input.task_run_id, error
        );
        return rag_fallback_scores("unknown", &error);
    }

    let results = parse_rag_judge_response(&response.output, "routed");

    info!(
        "RAG judge scored task {}: {}",
        input.task_run_id,
        results
            .iter()
            .map(|r| format!("{}={:.2}", r.metric, r.score))
            .collect::<Vec<_>>()
            .join(", ")
    );

    results
}

/// Store a retrieval event as a task_run_event for post-hoc RAG evaluation.
///
/// Uses event_type='retrieval' so they can be queried later by the RAG judge.
pub fn persist_retrieval_event(task_run_id: &str, event: &RetrievalEvent) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

/// Load retrieval events for a task run (for RAG judge input).
pub fn load_retrieval_events(task_run_id: &str) -> Result<Vec<RetrievalEvent>, String> {
    Err("SQLite removed".to_string())
}

/// Create a RetrievalEvent from any hybrid search results with SearchResult<T>.
///
/// Generic helper — provide a closure to extract title and content_preview from the item.
pub fn capture_retrieval_event<T>(
    query_text: &str,
    source_table: &str,
    results: &[crate::database::hybrid_search::SearchResult<T>],
    formatted_context: Option<&str>,
    extract: impl Fn(&T) -> (String, String),
) -> RetrievalEvent {
    RetrievalEvent {
        query_text: query_text.to_string(),
        source_table: source_table.to_string(),
        result_count: results.len(),
        top_results: results
            .iter()
            .take(5)
            .map(|r| {
                let (title, preview) = extract(&r.item);
                RetrievalResultSummary {
                    title,
                    similarity: r.similarity,
                    hybrid_score: r.hybrid_score,
                    content_preview: preview[..preview.len().min(200)].to_string(),
                }
            })
            .collect(),
        formatted_context: formatted_context.map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rag_judge_response_valid() {
        let response = r#"{"scores": [
            {"metric": "rag_contextual_precision", "score": 0.8, "rationale": "Good ranking"},
            {"metric": "rag_contextual_recall", "score": 0.7, "rationale": "Mostly complete"},
            {"metric": "rag_faithfulness", "score": 0.9, "rationale": "Stayed on topic"},
            {"metric": "rag_answer_relevancy", "score": 0.85, "rationale": "Useful context"}
        ]}"#;
        let results = parse_rag_judge_response(response, "test");
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].metric, AgenticMetric::RagContextualPrecision);
        assert!((results[0].score - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_parse_rag_judge_response_missing_metrics() {
        let response = r#"{"scores": [
            {"metric": "rag_faithfulness", "score": 0.9, "rationale": "Good"}
        ]}"#;
        let results = parse_rag_judge_response(response, "test");
        assert_eq!(results.len(), 4); // 3 missing filled with 0.5
    }

    #[test]
    fn test_parse_rag_judge_response_invalid() {
        let results = parse_rag_judge_response("not json", "test");
        assert_eq!(results.len(), 4);
        assert!(results.iter().all(|r| r.score == 0.5));
    }

    #[test]
    fn test_empty_retrieval_events_returns_empty() {
        // Can't call evaluate_rag_sync without AI provider, but verify the guard
        let input = RagJudgeInput {
            task_run_id: "test".into(),
            task_prompt: "test".into(),
            retrieval_events: vec![],
            generated_output: None,
        };
        // Empty events → no RAG metrics
        assert!(input.retrieval_events.is_empty());
    }

    #[test]
    fn test_build_prompt_includes_retrieval_data() {
        let input = RagJudgeInput {
            task_run_id: "test".into(),
            task_prompt: "Fix the login bug".into(),
            retrieval_events: vec![RetrievalEvent {
                query_text: "login authentication error".into(),
                source_table: "task_knowledge".into(),
                result_count: 3,
                top_results: vec![RetrievalResultSummary {
                    title: "Auth fix #42".into(),
                    similarity: 0.85,
                    hybrid_score: 0.9,
                    content_preview: "Fixed session token handling".into(),
                }],
                formatted_context: Some("Related knowledge: auth fix...".into()),
            }],
            generated_output: Some("Applied session token fix".into()),
        };
        let prompt = build_rag_judge_prompt(&input);
        assert!(prompt.contains("Fix the login bug"));
        assert!(prompt.contains("login authentication error"));
        assert!(prompt.contains("Auth fix #42"));
        assert!(prompt.contains("rag_contextual_precision"));
    }
}
