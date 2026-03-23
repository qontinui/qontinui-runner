//! Agentic Metrics — DeepEval-inspired evaluation for workflow runs.
//!
//! Adopts the agentic metric taxonomy from deepeval (task completion, step
//! efficiency, tool correctness, plan adherence, goal accuracy, plan quality,
//! argument correctness) as 0.0–1.0 scored metrics for the meta-optimizer.
//!
//! Phase 1 (this module): deterministic metrics computed without LLM calls.
//! Phase 2b (future): RAG metrics for hybrid search + visual RAG quality.
//! Phase 3 (future): LLM-as-judge metrics for qualitative evaluation.
//!
//! ## Module structure
//!
//! - `mod` — Core types, composite scoring, `compute_deterministic()`
//! - `task_completion` — Score from outcome status + verification results
//! - `step_efficiency` — Score from iterations vs baseline, zero-iter → 0.0
//! - `tool_correctness` — Tools used vs expected tool baselines
//! - `argument_correctness` — Pipeline trace input/output schema validation
//! - `llm_judge` — Batched LLM-as-judge for plan adherence, goal accuracy, plan quality
//! - `rag_*` (future) — RAG retrieval quality metrics

pub mod argument_correctness;
pub mod llm_judge;
pub mod rag_judge;
pub mod scoring;
pub mod step_efficiency;
pub mod task_completion;
pub mod tool_correctness;

use serde::{Deserialize, Serialize};

/// Which agentic metric this score represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgenticMetric {
    // --- Agentic metrics ---
    TaskCompletion,
    StepEfficiency,
    ToolCorrectness,
    ArgumentCorrectness,
    PlanAdherence,
    GoalAccuracy,
    PlanQuality,
    // --- RAG metrics (scored only for retrieval-involving runs) ---
    RagContextualPrecision,
    RagContextualRecall,
    RagFaithfulness,
    RagAnswerRelevancy,
}

impl AgenticMetric {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TaskCompletion => "task_completion",
            Self::StepEfficiency => "step_efficiency",
            Self::ToolCorrectness => "tool_correctness",
            Self::ArgumentCorrectness => "argument_correctness",
            Self::PlanAdherence => "plan_adherence",
            Self::GoalAccuracy => "goal_accuracy",
            Self::PlanQuality => "plan_quality",
            Self::RagContextualPrecision => "rag_contextual_precision",
            Self::RagContextualRecall => "rag_contextual_recall",
            Self::RagFaithfulness => "rag_faithfulness",
            Self::RagAnswerRelevancy => "rag_answer_relevancy",
        }
    }

    /// Default weight for composite score calculation.
    ///
    /// RAG metrics only contribute when scored (retrieval-involving runs).
    /// Weights are renormalized to sum to 1.0 across scored metrics.
    pub fn default_weight(&self) -> f64 {
        match self {
            Self::TaskCompletion => 0.25,
            Self::GoalAccuracy => 0.20,
            Self::StepEfficiency => 0.12,
            Self::ToolCorrectness => 0.08,
            Self::PlanAdherence => 0.08,
            Self::PlanQuality => 0.04,
            Self::ArgumentCorrectness => 0.03,
            Self::RagContextualPrecision => 0.05,
            Self::RagContextualRecall => 0.05,
            Self::RagFaithfulness => 0.05,
            Self::RagAnswerRelevancy => 0.05,
        }
    }

    /// Whether this is a RAG metric (only scored for retrieval-involving runs).
    pub fn is_rag_metric(&self) -> bool {
        matches!(
            self,
            Self::RagContextualPrecision
                | Self::RagContextualRecall
                | Self::RagFaithfulness
                | Self::RagAnswerRelevancy
        )
    }
}

impl std::fmt::Display for AgenticMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A single metric evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricResult {
    pub metric: AgenticMetric,
    /// Score from 0.0 (worst) to 1.0 (best).
    pub score: f64,
    /// How reliable this score is, 0.0–1.0.
    pub confidence: f64,
    /// Human-readable explanation of why this score was given.
    pub rationale: String,
    /// Whether an LLM was used to compute this score.
    pub is_llm_judged: bool,
    /// Model ID used for LLM-judged scores, None for deterministic.
    pub model_used: Option<String>,
}

/// All metric scores for one task run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticMetricScorecard {
    pub task_run_id: String,
    pub scores: Vec<MetricResult>,
    /// Weighted average of all scores using default weights.
    pub composite_score: f64,
    pub scored_at: String,
}

/// Input data for deterministic metric computation.
///
/// Extracted from `WorkflowOutcome` and `learning_outcomes` to decouple
/// the metrics module from the orchestrator types.
#[derive(Debug, Clone)]
pub struct DeterministicInput {
    pub task_run_id: String,
    pub status: String,
    pub verification_passed: bool,
    pub was_stopped: bool,
    pub iterations: u32,
    pub step_count: Option<i64>,
    pub verification_step_count: Option<i64>,
    pub agentic_step_count: Option<i64>,
    pub max_iterations_reached: bool,
    pub duration_secs: f64,
    pub error_type: Option<String>,
    /// Tools used during the workflow run (from learning_outcomes.tools_used JSON).
    pub tools_used: Vec<String>,
    /// Pipeline agent trace snapshots for argument correctness validation.
    /// Empty for traditional architecture runs (no traces).
    pub agent_traces: Vec<argument_correctness::TraceSnapshot>,
}

/// Optional learned baselines for deterministic scoring.
#[derive(Debug, Clone, Default)]
pub struct LearnedBaselines {
    pub step_baseline: Option<step_efficiency::StepBaseline>,
    pub tool_baseline: Option<tool_correctness::ToolBaseline>,
}

/// Compute all deterministic (zero LLM cost) agentic metrics for a run.
///
/// Scores: TaskCompletion, StepEfficiency, ToolCorrectness, ArgumentCorrectness.
/// Pass `baselines` to use learned baselines instead of defaults.
pub fn compute_deterministic(
    input: &DeterministicInput,
    baselines: &LearnedBaselines,
) -> Vec<MetricResult> {
    let tool_input = tool_correctness::ToolCorrectnessInput {
        tools_used: input.tools_used.clone(),
        zero_iterations: input.iterations == 0,
    };

    let arg_input = argument_correctness::ArgumentCorrectnessInput {
        traces: input.agent_traces.clone(),
        zero_iterations: input.iterations == 0,
    };

    vec![
        task_completion::score(input),
        step_efficiency::score(input, baselines.step_baseline.as_ref()),
        tool_correctness::score(&tool_input, baselines.tool_baseline.as_ref()),
        argument_correctness::score(&arg_input),
    ]
}

/// Compute a weighted composite score from a set of metric results.
///
/// Only scored metrics contribute; weights are renormalized to sum to 1.0.
/// Returns 0.0 if no scores are provided.
pub fn composite_score(scores: &[MetricResult]) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }

    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;

    for result in scores {
        let weight = result.metric.default_weight();
        weighted_sum += result.score * weight;
        weight_sum += weight;
    }

    if weight_sum > 0.0 {
        weighted_sum / weight_sum
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(status: &str, verification_passed: bool, iterations: u32) -> DeterministicInput {
        DeterministicInput {
            task_run_id: "test-run-1".to_string(),
            status: status.to_string(),
            verification_passed,
            was_stopped: false,
            iterations,
            step_count: Some(5),
            verification_step_count: Some(2),
            agentic_step_count: Some(3),
            max_iterations_reached: false,
            duration_secs: 100.0,
            error_type: None,
            tools_used: vec!["read_file".to_string()],
            agent_traces: vec![],
        }
    }

    #[test]
    fn test_compute_deterministic_returns_four_scores() {
        let input = make_input("success", true, 2);
        let scores = compute_deterministic(&input, &LearnedBaselines::default());
        assert_eq!(scores.len(), 4);
        assert_eq!(scores[0].metric, AgenticMetric::TaskCompletion);
        assert_eq!(scores[1].metric, AgenticMetric::StepEfficiency);
        assert_eq!(scores[2].metric, AgenticMetric::ToolCorrectness);
        assert_eq!(scores[3].metric, AgenticMetric::ArgumentCorrectness);
    }

    #[test]
    fn test_composite_score_empty() {
        assert_eq!(composite_score(&[]), 0.0);
    }

    #[test]
    fn test_composite_score_renormalizes() {
        // Two metrics with known weights: TaskCompletion(0.25), StepEfficiency(0.12)
        let scores = vec![
            MetricResult {
                metric: AgenticMetric::TaskCompletion,
                score: 1.0,
                confidence: 1.0,
                rationale: String::new(),
                is_llm_judged: false,
                model_used: None,
            },
            MetricResult {
                metric: AgenticMetric::StepEfficiency,
                score: 0.5,
                confidence: 1.0,
                rationale: String::new(),
                is_llm_judged: false,
                model_used: None,
            },
        ];
        let composite = composite_score(&scores);
        // (1.0 * 0.25 + 0.5 * 0.12) / (0.25 + 0.12) = 0.31 / 0.37 ≈ 0.8378
        assert!((composite - 0.8378).abs() < 0.01);
    }
}
