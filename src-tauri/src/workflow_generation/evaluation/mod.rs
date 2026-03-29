//! Step Quality Evaluation Engine
//!
//! Multi-signal scoring infrastructure that evaluates verification step quality
//! across 6 dimensions using 3 scoring tiers:
//!
//! - **Tier 1: Deterministic** — fast pattern-based checks (< 1ms per step)
//! - **Tier 2: LLM Judges** — composable judge pipeline (100-500ms per step)
//! - **Tier 3: PRM Scorer** — process reward model scores (10-50ms per step)
//!
//! Replaces name-matching coverage matrix with semantic entailment verification
//! and implements PURE min-form aggregation (trajectory reward = minimum step reward).

pub mod aggregation;
pub mod cache;
pub mod coverage;
pub mod deterministic;
pub mod judges;
pub mod prm_client;

use serde::{Deserialize, Serialize};

use crate::workflow_generation::specification::{AcceptanceCriteria, CriterionPriority};

// ============================================================================
// Core Types
// ============================================================================

/// The 6 dimensions along which every verification step is scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationDimension {
    /// Will this step produce the same result every time?
    Determinism,
    /// Does this step actually test its mapped criterion?
    Entailment,
    /// How precise is the check? Could it false-positive?
    Specificity,
    /// Can this step actually run as written?
    Executability,
    /// How much of the criterion does this step cover?
    Coverage,
    /// Will this step handle flaky environments?
    Robustness,
}

impl EvaluationDimension {
    pub fn all() -> &'static [EvaluationDimension] {
        &[
            EvaluationDimension::Determinism,
            EvaluationDimension::Entailment,
            EvaluationDimension::Specificity,
            EvaluationDimension::Executability,
            EvaluationDimension::Coverage,
            EvaluationDimension::Robustness,
        ]
    }
}

/// Which scoring tier produced a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringTier {
    /// Tier 1: fast deterministic checks (< 1ms)
    Deterministic,
    /// Tier 2: composable LLM judges (100-500ms)
    Judge,
    /// Tier 3: process reward model (10-50ms)
    Prm,
}

/// Score for a single evaluation dimension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionScore {
    pub dimension: EvaluationDimension,
    /// 0.0–1.0 quality score
    pub score: f64,
    /// 0.0–1.0 confidence in this score
    pub confidence: f64,
    /// Which tier produced this score
    pub tier: ScoringTier,
    /// Natural language explanation (from Tier 2/3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// Supporting evidence (matched patterns, judge reasoning)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

/// Flags raised during evaluation that require attention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationFlag {
    pub flag_type: EvaluationFlagType,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationFlagType {
    /// Step cannot execute as written
    Unexecutable,
    /// Step has placeholder values
    Placeholder,
    /// Step will frequently false-positive
    FalsePositive,
    /// Step has no entailment with any criterion
    NoEntailment,
    /// Step is non-deterministic (prompt-based)
    NonDeterministic,
}

/// Complete evaluation of a single verification step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepEvaluation {
    pub step_id: String,
    pub step_name: String,
    pub scores: Vec<DimensionScore>,
    /// Weighted combination of dimension scores
    pub composite_score: f64,
    /// PURE min-form: weakest dimension score
    pub min_score: f64,
    /// Critical issues requiring attention
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<EvaluationFlag>,
    /// Mapped criterion ID (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
    /// Mapped criterion priority (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion_priority: Option<CriterionPriority>,
}

impl StepEvaluation {
    /// Compute composite and min scores from dimension scores.
    pub fn finalize(&mut self) {
        if self.scores.is_empty() {
            self.composite_score = 0.0;
            self.min_score = 0.0;
            return;
        }

        // Weighted composite: entailment and executability matter most
        let weights: &[(EvaluationDimension, f64)] = &[
            (EvaluationDimension::Determinism, 0.15),
            (EvaluationDimension::Entailment, 0.25),
            (EvaluationDimension::Specificity, 0.15),
            (EvaluationDimension::Executability, 0.20),
            (EvaluationDimension::Coverage, 0.15),
            (EvaluationDimension::Robustness, 0.10),
        ];

        let mut weighted_sum = 0.0;
        let mut weight_total = 0.0;
        let mut min = f64::MAX;

        for ds in &self.scores {
            if let Some(&(_, w)) = weights.iter().find(|(d, _)| *d == ds.dimension) {
                weighted_sum += ds.score * w;
                weight_total += w;
            }
            if ds.score < min {
                min = ds.score;
            }
        }

        self.composite_score = if weight_total > 0.0 {
            weighted_sum / weight_total
        } else {
            0.0
        };
        self.min_score = if min == f64::MAX { 0.0 } else { min };
    }
}

/// Evaluation of an entire workflow's verification steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvaluation {
    pub step_evaluations: Vec<StepEvaluation>,
    /// Min of all step min_scores (PURE min-form)
    pub overall_score: f64,
    /// Semantic coverage matrix
    pub coverage_matrix: coverage::SemanticCoverageMatrix,
    /// Quality gate result
    pub quality_gate: QualityGateResult,
    /// Which scoring strategy was used
    pub scoring_strategy: ScoringStrategy,
    /// Total evaluation time in milliseconds
    pub evaluation_duration_ms: u64,
}

/// Determines which scoring tiers to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringStrategy {
    /// Tier 1 only — for real-time feedback during editing
    FastOnly,
    /// Tiers 1 + 2 — for post-generation quality gates
    Standard,
    /// Tiers 1 + 2 + 3 — for candidate selection during tree search
    Full,
}

/// Quality gate: should generation proceed or try again?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateResult {
    pub passed: bool,
    pub gate_level: QualityGateLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<QualityGateFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityGateLevel {
    /// All critical criteria have entailment > 0.7, no executability flags
    Minimum,
    /// All criteria have entailment > 0.5, composite > 0.6
    Standard,
    /// All dimensions > 0.7, min-form > 0.6
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateFailure {
    pub gate_level: QualityGateLevel,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion_id: Option<String>,
}

/// Specific repair instruction for the fixer agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairInstruction {
    pub step_id: String,
    pub step_name: String,
    pub weakest_dimension: EvaluationDimension,
    pub current_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub suggested_fix: String,
}

// ============================================================================
// Orchestrator
// ============================================================================

use crate::doctor::DoctorHandle;
use crate::unified_workflows::UnifiedWorkflow;
use serde_json::Value;
use std::time::Instant;
use tracing::info;

/// Run a full evaluation of a workflow's verification steps.
///
/// This is the main entry point. It orchestrates all scoring tiers based on
/// the chosen strategy and produces a complete `WorkflowEvaluation`.
pub fn evaluate_workflow(
    workflow: &UnifiedWorkflow,
    criteria: Option<&AcceptanceCriteria>,
    strategy: ScoringStrategy,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
    prm_base_url: Option<&str>,
) -> WorkflowEvaluation {
    let start = Instant::now();
    info!(
        "Starting workflow evaluation with strategy {:?}",
        strategy
    );

    // Collect all verification steps from top-level + stages
    let mut all_steps: Vec<&Value> = Vec::new();
    for step in &workflow.verification_steps {
        all_steps.push(step);
    }
    for stage in &workflow.stages {
        for step in &stage.verification_steps {
            all_steps.push(step);
        }
    }

    // Create PRM client once (reused across all steps)
    let prm_client = prm_base_url.map(prm_client::PrmClient::new);

    // Build step evaluations
    let mut step_evaluations: Vec<StepEvaluation> = Vec::new();

    for step_val in &all_steps {
        let step_id = step_val
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let step_name = step_val
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if step_id.is_empty() {
            info!("Skipping verification step with empty ID (name='{}')", step_name);
            continue;
        }

        // Resolve mapped criterion
        let (criterion_id, criterion_priority) = resolve_criterion(step_val, criteria);

        let mut scores: Vec<DimensionScore> = Vec::new();

        // --- Tier 1: Deterministic scoring (always runs) ---
        let tier1_scores = deterministic::score_step(step_val, criteria);
        scores.extend(tier1_scores);

        // --- Tier 2: LLM Judges (Standard and Full strategies) ---
        match strategy {
            ScoringStrategy::Standard | ScoringStrategy::Full => {
                let criterion = criterion_id.as_ref().and_then(|cid| {
                    criteria.and_then(|c| c.criteria.iter().find(|ac| ac.id == *cid))
                });

                let judge_context = judges::JudgeContext {
                    workflow_description: workflow.description.clone(),
                    all_criteria: criteria
                        .map(|c| c.criteria.clone())
                        .unwrap_or_default(),
                    project_context: None,
                };

                let judge_scores = judges::run_judge_pipeline(
                    step_val,
                    criterion,
                    &judge_context,
                    doctor_handle,
                    model,
                    provider,
                );

                // Merge: prefer judge scores over deterministic for overlapping dimensions
                for js in judge_scores {
                    if let Some(existing) = scores
                        .iter_mut()
                        .find(|s| s.dimension == js.dimension)
                    {
                        // Keep the judge score if it has higher confidence
                        if js.confidence > existing.confidence {
                            *existing = js;
                        }
                    } else {
                        scores.push(js);
                    }
                }
            }
            ScoringStrategy::FastOnly => {}
        }

        // --- Tier 3: PRM Scorer (Full strategy only) ---
        if matches!(strategy, ScoringStrategy::Full) {
            if let Some(ref client) = prm_client {
                if let Ok(prm_response) = client.score_step_sync(step_val, criteria) {
                    // PRM provides an overall step score; apply it to entailment dimension
                    if let Some(existing) = scores
                        .iter_mut()
                        .find(|s| s.dimension == EvaluationDimension::Entailment)
                    {
                        if prm_response.confidence > existing.confidence {
                            existing.score = prm_response.score;
                            existing.confidence = prm_response.confidence;
                            existing.tier = ScoringTier::Prm;
                            existing.explanation = Some(prm_response.critique);
                        }
                    }
                }
            }
        }

        // Generate flags from final scores (after all tiers merged)
        let flags = generate_flags_from_scores(&scores, &step_id, &criterion_id);

        let mut eval = StepEvaluation {
            step_id,
            step_name,
            scores,
            composite_score: 0.0,
            min_score: 0.0,
            flags,
            criterion_id,
            criterion_priority,
        };
        eval.finalize();
        step_evaluations.push(eval);
    }

    // Build semantic coverage matrix
    let coverage_matrix = coverage::build_semantic_coverage_matrix(
        &step_evaluations,
        criteria,
    );

    // Compute overall score via PURE min-form aggregation
    let overall_score = aggregation::aggregate_evaluation(&step_evaluations);

    // Run quality gate
    let quality_gate = run_quality_gate(&step_evaluations, &coverage_matrix, criteria);

    let duration_ms = start.elapsed().as_millis() as u64;
    info!(
        "Workflow evaluation complete: overall={:.3}, gate={}, steps={}, duration={}ms",
        overall_score,
        if quality_gate.passed { "PASS" } else { "FAIL" },
        step_evaluations.len(),
        duration_ms
    );

    WorkflowEvaluation {
        step_evaluations,
        overall_score,
        coverage_matrix,
        quality_gate,
        scoring_strategy: strategy,
        evaluation_duration_ms: duration_ms,
    }
}

/// Evaluate a single step (for API use).
pub fn evaluate_step(
    step: &Value,
    criteria: Option<&AcceptanceCriteria>,
    _doctor_handle: Option<&DoctorHandle>,
    _model: Option<&str>,
    _provider: Option<&str>,
) -> StepEvaluation {
    let step_id = step
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let step_name = step
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let (criterion_id, criterion_priority) = resolve_criterion(step, criteria);

    let scores = deterministic::score_step(step, criteria);
    let flags = generate_flags_from_scores(&scores, &step_id, &criterion_id);
    let mut eval = StepEvaluation {
        step_id,
        step_name,
        scores,
        composite_score: 0.0,
        min_score: 0.0,
        flags,
        criterion_id,
        criterion_priority,
    };
    eval.finalize();
    eval
}

/// Generate evaluation flags from dimension scores.
/// Shared by both `evaluate_workflow` and `evaluate_step`.
fn generate_flags_from_scores(
    scores: &[DimensionScore],
    step_id: &str,
    criterion_id: &Option<String>,
) -> Vec<EvaluationFlag> {
    let mut flags = Vec::new();

    if let Some(ds) = scores.iter().find(|s| s.dimension == EvaluationDimension::Executability) {
        if ds.score < 0.3 {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::Unexecutable,
                message: ds.explanation.clone().unwrap_or_else(|| "Step may not be executable".into()),
                step_id: Some(step_id.to_string()),
            });
        }
        if ds.score < 0.2 && ds.evidence.iter().any(|e| e.contains("placeholder")) {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::Placeholder,
                message: "Step contains placeholder values".into(),
                step_id: Some(step_id.to_string()),
            });
        }
    }

    if let Some(ds) = scores.iter().find(|s| s.dimension == EvaluationDimension::Determinism) {
        if ds.score == 0.0 {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::NonDeterministic,
                message: "Step is non-deterministic (prompt-based)".into(),
                step_id: Some(step_id.to_string()),
            });
        }
    }

    if let Some(ds) = scores.iter().find(|s| s.dimension == EvaluationDimension::Specificity) {
        if ds.score < 0.3 {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::FalsePositive,
                message: ds.explanation.clone().unwrap_or_else(|| "Step may frequently false-positive".into()),
                step_id: Some(step_id.to_string()),
            });
        }
    }

    // Only flag NoEntailment when the dimension was actually scored
    if let Some(ds) = scores.iter().find(|s| s.dimension == EvaluationDimension::Entailment) {
        if ds.score < 0.2 && criterion_id.is_some() {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::NoEntailment,
                message: format!("Step has very low entailment ({:.2}) with mapped criterion", ds.score),
                step_id: Some(step_id.to_string()),
            });
        }
    }

    flags
}

/// Generate repair instructions from a workflow evaluation.
pub fn generate_repair_guidance(evaluation: &WorkflowEvaluation) -> Vec<RepairInstruction> {
    evaluation
        .step_evaluations
        .iter()
        .filter(|e| e.min_score < 0.5)
        .filter_map(|e| {
            let weakest = e
                .scores
                .iter()
                .min_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))?;

            Some(RepairInstruction {
                step_id: e.step_id.clone(),
                step_name: e.step_name.clone(),
                weakest_dimension: weakest.dimension,
                current_score: weakest.score,
                explanation: weakest.explanation.clone(),
                suggested_fix: generate_fix_suggestion(weakest),
            })
        })
        .collect()
}

// ============================================================================
// Internal Helpers
// ============================================================================

/// Resolve a step's mapped criterion ID and priority.
fn resolve_criterion(
    step: &Value,
    criteria: Option<&AcceptanceCriteria>,
) -> (Option<String>, Option<CriterionPriority>) {
    let cid = step
        .get("criterion_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            step.get("criterion_ids")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let priority = cid.as_ref().and_then(|id| {
        criteria.and_then(|c| {
            c.criteria
                .iter()
                .find(|ac| ac.id == *id)
                .map(|ac| ac.priority.clone())
        })
    });

    (cid, priority)
}

/// Run quality gates at multiple levels, returning the strictest that passes.
fn run_quality_gate(
    evaluations: &[StepEvaluation],
    coverage: &coverage::SemanticCoverageMatrix,
    _criteria: Option<&AcceptanceCriteria>,
) -> QualityGateResult {
    let mut failures: Vec<QualityGateFailure> = Vec::new();

    // Check Minimum gate: all critical criteria have entailment > 0.7, no unexecutable flags
    // Note: entailment checks are skipped when no entailment scorer ran (FastOnly strategy)
    let mut minimum_passed = true;
    for eval in evaluations {
        if eval.criterion_priority == Some(CriterionPriority::Critical) {
            // Only check entailment if the dimension was actually scored
            if let Some(entailment_ds) = eval
                .scores
                .iter()
                .find(|s| s.dimension == EvaluationDimension::Entailment)
            {
                if entailment_ds.score < 0.7 {
                    minimum_passed = false;
                    failures.push(QualityGateFailure {
                        gate_level: QualityGateLevel::Minimum,
                        reason: format!(
                            "Critical criterion entailment too low: {:.2}",
                            entailment_ds.score
                        ),
                        step_id: Some(eval.step_id.clone()),
                        criterion_id: eval.criterion_id.clone(),
                    });
                }
            }
        }
        if eval.flags.iter().any(|f| f.flag_type == EvaluationFlagType::Unexecutable) {
            minimum_passed = false;
            failures.push(QualityGateFailure {
                gate_level: QualityGateLevel::Minimum,
                reason: "Step is unexecutable".into(),
                step_id: Some(eval.step_id.clone()),
                criterion_id: eval.criterion_id.clone(),
            });
        }
    }

    // Check for uncovered critical criteria
    for uc in &coverage.uncovered_criteria {
        if uc.priority == CriterionPriority::Critical {
            minimum_passed = false;
            failures.push(QualityGateFailure {
                gate_level: QualityGateLevel::Minimum,
                reason: format!("Critical criterion uncovered: {}", uc.criterion_description),
                step_id: None,
                criterion_id: Some(uc.criterion_id.clone()),
            });
        }
    }

    if !minimum_passed {
        return QualityGateResult {
            passed: false,
            gate_level: QualityGateLevel::Minimum,
            failures,
        };
    }

    // Check Standard gate: all criteria entailment > 0.5, composite > 0.6
    // Entailment checks are skipped when the dimension was not scored (FastOnly)
    let mut standard_passed = true;
    for eval in evaluations {
        if let Some(entailment_ds) = eval
            .scores
            .iter()
            .find(|s| s.dimension == EvaluationDimension::Entailment)
        {
            if entailment_ds.score < 0.5 && eval.criterion_id.is_some() {
                standard_passed = false;
                failures.push(QualityGateFailure {
                    gate_level: QualityGateLevel::Standard,
                    reason: format!("Criterion entailment below 0.5: {:.2}", entailment_ds.score),
                    step_id: Some(eval.step_id.clone()),
                    criterion_id: eval.criterion_id.clone(),
                });
            }
        }
        if eval.composite_score < 0.6 {
            standard_passed = false;
            failures.push(QualityGateFailure {
                gate_level: QualityGateLevel::Standard,
                reason: format!("Composite score below 0.6: {:.2}", eval.composite_score),
                step_id: Some(eval.step_id.clone()),
                criterion_id: eval.criterion_id.clone(),
            });
        }
    }

    if !standard_passed {
        return QualityGateResult {
            passed: true, // Minimum passed
            gate_level: QualityGateLevel::Minimum,
            failures,
        };
    }

    // Check Strict gate: all dimensions > 0.7, min-form > 0.6
    let mut strict_passed = true;
    for eval in evaluations {
        if eval.min_score < 0.6 {
            strict_passed = false;
            failures.push(QualityGateFailure {
                gate_level: QualityGateLevel::Strict,
                reason: format!("Min-form score below 0.6: {:.2}", eval.min_score),
                step_id: Some(eval.step_id.clone()),
                criterion_id: eval.criterion_id.clone(),
            });
        }
        for ds in &eval.scores {
            if ds.score < 0.7 {
                strict_passed = false;
                failures.push(QualityGateFailure {
                    gate_level: QualityGateLevel::Strict,
                    reason: format!(
                        "{:?} score below 0.7: {:.2}",
                        ds.dimension, ds.score
                    ),
                    step_id: Some(eval.step_id.clone()),
                    criterion_id: eval.criterion_id.clone(),
                });
            }
        }
    }

    QualityGateResult {
        passed: true,
        gate_level: if strict_passed {
            QualityGateLevel::Strict
        } else {
            QualityGateLevel::Standard
        },
        failures,
    }
}

/// Generate a fix suggestion based on the weakest dimension.
fn generate_fix_suggestion(score: &DimensionScore) -> String {
    match score.dimension {
        EvaluationDimension::Determinism => {
            "Convert prompt step to command with specific output check".into()
        }
        EvaluationDimension::Entailment => {
            let gaps = if score.evidence.is_empty() {
                "unknown gaps".to_string()
            } else {
                score.evidence.join(", ")
            };
            format!(
                "Step doesn't test criterion. Gaps: {}",
                gaps
            )
        }
        EvaluationDimension::Specificity => {
            "Add more specific pattern matching to avoid false positives".into()
        }
        EvaluationDimension::Executability => {
            "Fix placeholder paths/commands to use real project values".into()
        }
        EvaluationDimension::Coverage => {
            "Add additional checks to cover all aspects of the criterion".into()
        }
        EvaluationDimension::Robustness => {
            "Add retry_count: 3 and timeout_seconds: 30 for reliability".into()
        }
    }
}
