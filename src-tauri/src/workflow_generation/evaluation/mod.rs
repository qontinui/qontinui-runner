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
///
/// `target_family` / `runner_port` are used by the quality-gate URL invariant
/// check (Phase D). Pass `None` to auto-detect `target_family` from the
/// workflow + description and fall back to the env-var port
/// (`crate::mcp::types::get_mcp_api_port()`).
pub fn evaluate_workflow(
    workflow: &UnifiedWorkflow,
    criteria: Option<&AcceptanceCriteria>,
    strategy: ScoringStrategy,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
    prm_base_url: Option<&str>,
    target_family: Option<crate::mcp::types::UiBridgeFamily>,
    runner_port: Option<u16>,
) -> WorkflowEvaluation {
    let start = Instant::now();
    info!("Starting workflow evaluation with strategy {:?}", strategy);

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
            info!(
                "Skipping verification step with empty ID (name='{}')",
                step_name
            );
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
                    all_criteria: criteria.map(|c| c.criteria.clone()).unwrap_or_default(),
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
                    if let Some(existing) = scores.iter_mut().find(|s| s.dimension == js.dimension)
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
    let coverage_matrix = coverage::build_semantic_coverage_matrix(&step_evaluations, criteria);

    // Compute overall score via PURE min-form aggregation
    let overall_score = aggregation::aggregate_evaluation(&step_evaluations);

    // Resolve URL invariant parameters (Phase D defense-in-depth check).
    let resolved_family =
        target_family.unwrap_or_else(|| detect_target_family(workflow, &workflow.description));
    let resolved_port = runner_port.unwrap_or_else(crate::mcp::types::get_mcp_api_port);

    // Run quality gate
    let quality_gate = run_quality_gate(
        &step_evaluations,
        &coverage_matrix,
        criteria,
        workflow,
        resolved_family,
        resolved_port,
    );

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

    if let Some(ds) = scores
        .iter()
        .find(|s| s.dimension == EvaluationDimension::Executability)
    {
        if ds.score < 0.3 {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::Unexecutable,
                message: ds
                    .explanation
                    .clone()
                    .unwrap_or_else(|| "Step may not be executable".into()),
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

    if let Some(ds) = scores
        .iter()
        .find(|s| s.dimension == EvaluationDimension::Determinism)
    {
        if ds.score == 0.0 {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::NonDeterministic,
                message: "Step is non-deterministic (prompt-based)".into(),
                step_id: Some(step_id.to_string()),
            });
        }
    }

    if let Some(ds) = scores
        .iter()
        .find(|s| s.dimension == EvaluationDimension::Specificity)
    {
        if ds.score < 0.3 {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::FalsePositive,
                message: ds
                    .explanation
                    .clone()
                    .unwrap_or_else(|| "Step may frequently false-positive".into()),
                step_id: Some(step_id.to_string()),
            });
        }
    }

    // Only flag NoEntailment when the dimension was actually scored
    if let Some(ds) = scores
        .iter()
        .find(|s| s.dimension == EvaluationDimension::Entailment)
    {
        if ds.score < 0.2 && criterion_id.is_some() {
            flags.push(EvaluationFlag {
                flag_type: EvaluationFlagType::NoEntailment,
                message: format!(
                    "Step has very low entailment ({:.2}) with mapped criterion",
                    ds.score
                ),
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
            let weakest = e.scores.iter().min_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?;

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

/// Detect which UI Bridge family a workflow targets.
///
/// This mirrors the Phase B hardener helper. Inlined here because Phase B
/// may not export it as `pub`; a follow-up can dedupe.
fn detect_target_family(
    workflow: &UnifiedWorkflow,
    description: &str,
) -> crate::mcp::types::UiBridgeFamily {
    use crate::mcp::types::UiBridgeFamily;
    if description.contains("Spec Generation Brief") {
        return UiBridgeFamily::Control;
    }
    let json = serde_json::to_string(workflow).unwrap_or_default();
    if json.contains("localhost:1420") || json.contains("localhost:3001") {
        return UiBridgeFamily::Sdk;
    }
    UiBridgeFamily::Control
}

/// Scan a workflow's `command` fields for UI Bridge URL invariant violations.
///
/// Two invariants:
///
/// 1. If `target_family` is `Control`, no step may reference the
///    `/ui-bridge/sdk/` path — that endpoint drives external SDK apps, not
///    the runner's own UI.
/// 2. Any `localhost:<digits>/ui-bridge/...` occurrence must use
///    `<digits> == runner_port` — otherwise the workflow talks to a
///    different runner instance (or the default 9876 while bound elsewhere).
///
/// Returns a list of human-readable violation messages. Empty list == pass.
fn check_ui_bridge_url_invariants(
    workflow: &UnifiedWorkflow,
    target_family: crate::mcp::types::UiBridgeFamily,
    runner_port: u16,
) -> Vec<String> {
    use crate::mcp::types::UiBridgeFamily;

    let mut violations: Vec<String> = Vec::new();

    // Regex-free scanner for `localhost:<digits>/ui-bridge/...` so we don't
    // add a dependency just for this check.
    fn find_wrong_port_violations(cmd: &str, runner_port: u16) -> Vec<u16> {
        let mut wrong: Vec<u16> = Vec::new();
        let needle = "localhost:";
        let bytes = cmd.as_bytes();
        let mut i = 0;
        while i + needle.len() < bytes.len() {
            if &bytes[i..i + needle.len()] == needle.as_bytes() {
                let start = i + needle.len();
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    // Must be followed by `/ui-bridge/` to be an invariant
                    // we care about.
                    let rest_start = end;
                    let expected_suffix = "/ui-bridge/";
                    if rest_start + expected_suffix.len() <= bytes.len()
                        && &bytes[rest_start..rest_start + expected_suffix.len()]
                            == expected_suffix.as_bytes()
                    {
                        if let Ok(port_str) = std::str::from_utf8(&bytes[start..end]) {
                            if let Ok(port) = port_str.parse::<u16>() {
                                if port != runner_port {
                                    wrong.push(port);
                                }
                            }
                        }
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
        wrong
    }

    let scan_step = |step: &Value, phase: &str, stage_name: Option<&str>, out: &mut Vec<String>| {
        let cmd = match step.get("command").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return,
        };

        let origin = match stage_name {
            Some(name) => format!("stage '{}' {}", name, phase),
            None => format!("top-level {}", phase),
        };
        let step_id = step.get("id").and_then(|v| v.as_str()).unwrap_or("<no-id>");

        // Invariant 1: Control family must not use `/ui-bridge/sdk/`.
        if target_family == UiBridgeFamily::Control && cmd.contains("/ui-bridge/sdk/") {
            out.push(format!(
                "UI Bridge family mismatch in {} step '{}': workflow targets runner-self (Control), \
                 but step command contains '/ui-bridge/sdk/' (external SDK family). \
                 Rewrite to '/ui-bridge/control/'.",
                origin, step_id
            ));
        }

        // Invariant 2: wrong port in `localhost:<n>/ui-bridge/...`.
        for wrong in find_wrong_port_violations(cmd, runner_port) {
            out.push(format!(
                "UI Bridge wrong-port URL in {} step '{}': found 'localhost:{}/ui-bridge/...' \
                 but this runner is bound to port {}. Phase B's URL rewriter should have \
                 replaced this.",
                origin, step_id, wrong, runner_port
            ));
        }
    };

    // Top-level steps: setup, verification, completion.
    for step in &workflow.setup_steps {
        scan_step(step, "setup_steps", None, &mut violations);
    }
    for step in &workflow.verification_steps {
        scan_step(step, "verification_steps", None, &mut violations);
    }
    for step in &workflow.completion_steps {
        scan_step(step, "completion_steps", None, &mut violations);
    }

    // Per-stage steps: same three phases inside each stage.
    for stage in &workflow.stages {
        let stage_name = stage.name.as_str();
        for step in &stage.setup_steps {
            scan_step(step, "setup_steps", Some(stage_name), &mut violations);
        }
        for step in &stage.verification_steps {
            scan_step(
                step,
                "verification_steps",
                Some(stage_name),
                &mut violations,
            );
        }
        for step in &stage.completion_steps {
            scan_step(step, "completion_steps", Some(stage_name), &mut violations);
        }
    }

    violations
}

/// Run quality gates at multiple levels, returning the strictest that passes.
fn run_quality_gate(
    evaluations: &[StepEvaluation],
    coverage: &coverage::SemanticCoverageMatrix,
    _criteria: Option<&AcceptanceCriteria>,
    workflow: &UnifiedWorkflow,
    target_family: crate::mcp::types::UiBridgeFamily,
    runner_port: u16,
) -> QualityGateResult {
    let mut failures: Vec<QualityGateFailure> = Vec::new();

    // ── Phase D: UI Bridge URL invariants ────────────────────────────────────
    // Defense-in-depth: if generation / sanitization emits a workflow that
    // targets the wrong UI Bridge family or the wrong port for this runner,
    // fail the gate here so the regression surfaces at generation time rather
    // than silently at execution time.
    let url_violations = check_ui_bridge_url_invariants(workflow, target_family, runner_port);
    if !url_violations.is_empty() {
        for msg in &url_violations {
            failures.push(QualityGateFailure {
                gate_level: QualityGateLevel::Minimum,
                reason: msg.clone(),
                step_id: None,
                criterion_id: None,
            });
        }
        // Both sdk-path (Control mode) and wrong-port violations are treated
        // as correctness failures — the workflow will target the wrong runner
        // or wrong endpoint family at execution time.
        return QualityGateResult {
            passed: false,
            gate_level: QualityGateLevel::Minimum,
            failures,
        };
    }

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
        if eval
            .flags
            .iter()
            .any(|f| f.flag_type == EvaluationFlagType::Unexecutable)
        {
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
                    reason: format!("{:?} score below 0.7: {:.2}", ds.dimension, ds.score),
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
            format!("Step doesn't test criterion. Gaps: {}", gaps)
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

#[cfg(test)]
mod url_invariant_tests {
    use super::*;
    use crate::mcp::types::UiBridgeFamily;
    use crate::unified_workflows::UnifiedWorkflow;
    use serde_json::json;
    use std::collections::HashMap;

    fn make_workflow_with_verification_steps(steps: Vec<Value>) -> UnifiedWorkflow {
        UnifiedWorkflow {
            id: "test-wf-url-inv".to_string(),
            name: "Test URL Invariant Workflow".to_string(),
            description: "A test workflow for URL invariants".to_string(),
            category: "test".to_string(),
            tags: vec![],
            setup_steps: vec![],
            verification_steps: steps,
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
            constraint_overrides: HashMap::new(),
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
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn control_mode_flags_sdk_path() {
        let step = json!({
            "id": "step-1",
            "name": "Check runner UI",
            "command": "curl http://localhost:9877/ui-bridge/sdk/snapshot",
        });
        let wf = make_workflow_with_verification_steps(vec![step]);

        let violations = check_ui_bridge_url_invariants(&wf, UiBridgeFamily::Control, 9877);
        assert_eq!(
            violations.len(),
            1,
            "expected exactly one sdk-path violation, got {:?}",
            violations
        );
        assert!(
            violations[0].contains("/ui-bridge/sdk/"),
            "violation message should mention the sdk path: {}",
            violations[0]
        );
    }

    #[test]
    fn control_mode_with_control_paths_passes() {
        let step1 = json!({
            "id": "step-1",
            "name": "Snapshot",
            "command": "curl http://localhost:9877/ui-bridge/control/snapshot",
        });
        let step2 = json!({
            "id": "step-2",
            "name": "Discover",
            "command": "curl http://localhost:9877/ui-bridge/control/elements/discover",
        });
        let wf = make_workflow_with_verification_steps(vec![step1, step2]);

        let violations = check_ui_bridge_url_invariants(&wf, UiBridgeFamily::Control, 9877);
        assert!(
            violations.is_empty(),
            "expected no violations, got {:?}",
            violations
        );
    }

    #[test]
    fn wrong_port_is_flagged() {
        // Runner bound to 9877, but the workflow references default 9876.
        let step = json!({
            "id": "step-1",
            "name": "Snapshot wrong port",
            "command": "curl http://localhost:9876/ui-bridge/control/snapshot",
        });
        let wf = make_workflow_with_verification_steps(vec![step]);

        let violations = check_ui_bridge_url_invariants(&wf, UiBridgeFamily::Control, 9877);
        assert!(
            !violations.is_empty(),
            "expected at least one wrong-port violation, got none"
        );
        assert!(
            violations.iter().any(|v| v.contains("9876")
                && v.contains("9877")
                && v.to_lowercase().contains("wrong-port")),
            "expected wrong-port violation mentioning both ports: {:?}",
            violations
        );
    }

    #[test]
    fn workflow_without_ui_bridge_urls_passes() {
        let step1 = json!({
            "id": "step-1",
            "name": "Run tests",
            "command": "cargo test --lib",
        });
        let step2 = json!({
            "id": "step-2",
            "name": "Hit unrelated endpoint",
            "command": "curl http://example.com/api/status",
        });
        let wf = make_workflow_with_verification_steps(vec![step1, step2]);

        // Neither Control nor Sdk should flag anything when no /ui-bridge/
        // URLs are present.
        let v_ctl = check_ui_bridge_url_invariants(&wf, UiBridgeFamily::Control, 9877);
        let v_sdk = check_ui_bridge_url_invariants(&wf, UiBridgeFamily::Sdk, 9877);
        assert!(
            v_ctl.is_empty(),
            "Control mode should have no violations: {:?}",
            v_ctl
        );
        assert!(
            v_sdk.is_empty(),
            "Sdk mode should have no violations: {:?}",
            v_sdk
        );
    }
}
