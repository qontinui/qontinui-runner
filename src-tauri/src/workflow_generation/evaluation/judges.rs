//! Tier 2: Composable LLM Judges
//!
//! Implements composable judge units that evaluate verification steps using
//! AI reasoning. Inspired by haizelabs/verdict. Each judge evaluates one
//! dimension and returns a score with natural language reasoning.

use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::ai_provider;
use crate::ai_router::TaskContext;
use crate::doctor::DoctorHandle;
use crate::workflow_generation::generator::extract_json_from_response;
use crate::workflow_generation::specification::AcceptanceCriterion;
use super::cache::EntailmentCache;
use super::{DimensionScore, EvaluationDimension, ScoringTier};

fn entailment_cache() -> &'static Mutex<EntailmentCache> {
    static CACHE: std::sync::OnceLock<Mutex<EntailmentCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(EntailmentCache::default_cache()))
}

// ============================================================================
// Types
// ============================================================================

/// Judge verdict from a single evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    pub score: f64,
    pub reasoning: String,
    pub evidence: Vec<String>,
    pub confidence: f64,
}

/// Context available to all judges.
pub struct JudgeContext {
    pub workflow_description: String,
    pub all_criteria: Vec<AcceptanceCriterion>,
    pub project_context: Option<String>,
}

// ============================================================================
// Main Entry Point
// ============================================================================

/// Run the judge pipeline on a single step.
///
/// Runs Entailment, Determinism, and Specificity judges. Returns dimension
/// scores from each judge. Judges that fail gracefully return no score.
pub fn run_judge_pipeline(
    step: &Value,
    criterion: Option<&AcceptanceCriterion>,
    context: &JudgeContext,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> Vec<DimensionScore> {
    let step_summary = build_step_summary(step);
    let step_name = step.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    info!("Running judge pipeline on step '{}'", step_name);

    let mut scores: Vec<DimensionScore> = Vec::new();

    // Judge 1: Entailment (most critical)
    match run_entailment_judge(&step_summary, criterion, context, doctor_handle, model, provider) {
        Some(score) => {
            debug!(
                "Entailment judge for '{}': score={:.2}, confidence={:.2}",
                step_name, score.score, score.confidence
            );
            scores.push(score);
        }
        None => {
            warn!("Entailment judge failed for step '{}'", step_name);
        }
    }

    // Judge 2: Determinism
    match run_determinism_judge(&step_summary, doctor_handle, model, provider) {
        Some(score) => {
            debug!(
                "Determinism judge for '{}': score={:.2}, confidence={:.2}",
                step_name, score.score, score.confidence
            );
            scores.push(score);
        }
        None => {
            warn!("Determinism judge failed for step '{}'", step_name);
        }
    }

    // Judge 3: Specificity
    match run_specificity_judge(&step_summary, criterion, doctor_handle, model, provider) {
        Some(score) => {
            debug!(
                "Specificity judge for '{}': score={:.2}, confidence={:.2}",
                step_name, score.score, score.confidence
            );
            scores.push(score);
        }
        None => {
            warn!("Specificity judge failed for step '{}'", step_name);
        }
    }

    info!(
        "Judge pipeline complete for '{}': {} scores collected",
        step_name,
        scores.len()
    );
    scores
}

/// Get entailment cache statistics.
pub fn get_cache_stats() -> super::cache::CacheStats {
    entailment_cache()
        .lock()
        .map(|c| c.stats())
        .unwrap_or(super::cache::CacheStats {
            total_entries: 0,
            valid_entries: 0,
            max_size: 0,
            ttl_seconds: 0,
        })
}

// ============================================================================
// Individual Judges
// ============================================================================

/// Entailment judge: does the step actually prove its acceptance criterion?
fn run_entailment_judge(
    step_summary: &str,
    criterion: Option<&AcceptanceCriterion>,
    _context: &JudgeContext,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> Option<DimensionScore> {
    let criterion = match criterion {
        Some(c) => c,
        None => {
            // No criterion mapped — return a low score with explanation
            return Some(DimensionScore {
                dimension: EvaluationDimension::Entailment,
                score: 0.3,
                confidence: 0.85,
                tier: ScoringTier::Judge,
                explanation: Some("No criterion mapped to evaluate entailment against".to_string()),
                evidence: Vec::new(),
            });
        }
    };

    let step_type = step_summary.lines().next().unwrap_or("unknown");

    // Check entailment cache first
    if let Ok(cache) = entailment_cache().lock() {
        if let Some(cached) = cache.get(&criterion.description, step_summary, step_type) {
            debug!(
                "Entailment cache hit for criterion '{}': score={:.2}",
                criterion.id, cached.score
            );
            return Some(DimensionScore {
                dimension: EvaluationDimension::Entailment,
                score: cached.score,
                confidence: 0.85,
                tier: cached.tier,
                explanation: Some(cached.explanation.clone()),
                evidence: Vec::new(),
            });
        }
    }

    let prompt = format!(
        r#"You are evaluating whether a verification step actually PROVES its acceptance criterion is satisfied.

## Acceptance Criterion
ID: {}
Description: {}
Verification Hint: {}
Priority: {:?}

## Verification Step
{}

## Task
Analyze whether this step, if it passes, proves the criterion is satisfied.

Score on a 0.0-1.0 scale:
- 1.0 = Complete entailment: step passing PROVES criterion is satisfied
- 0.7 = Strong entailment: step covers most of the criterion
- 0.5 = Partial entailment: step covers some aspects but misses key parts
- 0.3 = Weak entailment: step is tangentially related
- 0.0 = No entailment: step doesn't test this criterion at all

Respond with ONLY a JSON object:
{{"score": 0.0, "reasoning": "...", "evidence": ["gap1", "gap2"]}}"#,
        criterion.id,
        criterion.description,
        criterion.verification_hint,
        criterion.priority,
        step_summary,
    );

    let task_context = TaskContext::from_prompt("evaluation_judge: entailment");

    let response = ai_provider::run_prompt_with_model_override(
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

    if !response.success {
        warn!(
            "Entailment judge AI call failed: {:?}",
            response.error
        );
        return None;
    }

    let verdict = parse_judge_response(&response.output)?;
    let score = verdict.score.clamp(0.0, 1.0);

    // Store in entailment cache
    if let Ok(mut cache) = entailment_cache().lock() {
        cache.put(
            &criterion.description,
            step_summary,
            step_type,
            score,
            verdict.reasoning.clone(),
            ScoringTier::Judge,
        );
    }

    Some(DimensionScore {
        dimension: EvaluationDimension::Entailment,
        score,
        confidence: 0.85,
        tier: ScoringTier::Judge,
        explanation: Some(verdict.reasoning),
        evidence: verdict.evidence,
    })
}

/// Determinism judge: will the step produce identical results across environments?
fn run_determinism_judge(
    step_summary: &str,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> Option<DimensionScore> {
    let prompt = format!(
        r#"You are evaluating whether a verification step will produce identical pass/fail results across different environments and runs.

## Verification Step
{}

## Check for these issues:
- Hardcoded paths that differ across machines
- Time-dependent checks (timestamps, dates, "today")
- State leakage from previous runs (assumes prior state exists)
- Network-dependent without timeout/retry
- Random values (UUIDs, temp file names)
- Order-dependent checks (assumes specific execution order)

Score 0.0-1.0 where 1.0 = perfectly deterministic, 0.0 = will produce different results each run.

Respond with ONLY a JSON object:
{{"score": 0.0, "reasoning": "...", "evidence": ["issue1", "issue2"]}}"#,
        step_summary,
    );

    let task_context = TaskContext::from_prompt("evaluation_judge: determinism");

    let response = ai_provider::run_prompt_with_model_override(
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

    if !response.success {
        warn!(
            "Determinism judge AI call failed: {:?}",
            response.error
        );
        return None;
    }

    let verdict = parse_judge_response(&response.output)?;

    Some(DimensionScore {
        dimension: EvaluationDimension::Determinism,
        score: verdict.score.clamp(0.0, 1.0),
        confidence: 0.80,
        tier: ScoringTier::Judge,
        explanation: Some(verdict.reasoning),
        evidence: verdict.evidence,
    })
}

/// Specificity judge: could the step false-positive (pass when requirement is NOT met)?
fn run_specificity_judge(
    step_summary: &str,
    criterion: Option<&AcceptanceCriterion>,
    doctor_handle: Option<&DoctorHandle>,
    model: Option<&str>,
    provider: Option<&str>,
) -> Option<DimensionScore> {
    let criterion_section = match criterion {
        Some(c) => format!("\n## Target Criterion\n{}", c.description),
        None => String::new(),
    };

    let prompt = format!(
        r#"You are evaluating whether a verification step could PASS when the requirement is NOT actually satisfied (false positive risk).

## Verification Step
{}{}

## Check for these false-positive scenarios:
- Pattern matching that would match negations ("no error" matches "error")
- HTTP checks that don't verify response body (200 OK with wrong content)
- File existence checks without content verification
- Substring matches on common words
- Checks that verify format but not semantics

Score 1.0 = no false positives possible, 0.0 = will frequently false-positive.

Respond with ONLY a JSON object:
{{"score": 0.0, "reasoning": "...", "evidence": ["false_positive_scenario1"]}}"#,
        step_summary,
        criterion_section,
    );

    let task_context = TaskContext::from_prompt("evaluation_judge: specificity");

    let response = ai_provider::run_prompt_with_model_override(
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

    if !response.success {
        warn!(
            "Specificity judge AI call failed: {:?}",
            response.error
        );
        return None;
    }

    let verdict = parse_judge_response(&response.output)?;

    Some(DimensionScore {
        dimension: EvaluationDimension::Specificity,
        score: verdict.score.clamp(0.0, 1.0),
        confidence: 0.80,
        tier: ScoringTier::Judge,
        explanation: Some(verdict.reasoning),
        evidence: verdict.evidence,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Build a human-readable summary of a step for judge prompts.
fn build_step_summary(step: &Value) -> String {
    let step_type = step.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
    let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    let command = step.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = step.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let expected = step.get("expected_output").and_then(|v| v.as_str()).unwrap_or("");
    let action = step.get("action").and_then(|v| v.as_str()).unwrap_or("");

    let mut summary = format!("Type: {}\nName: {}", step_type, name);
    if !command.is_empty() {
        summary.push_str(&format!("\nCommand: {}", command));
    }
    if !prompt.is_empty() {
        summary.push_str(&format!("\nPrompt: {}", prompt));
    }
    if !expected.is_empty() {
        summary.push_str(&format!("\nExpected Output: {}", expected));
    }
    if !action.is_empty() {
        summary.push_str(&format!("\nAction: {}", action));
    }
    summary
}

/// Parse a judge response into a JudgeVerdict.
fn parse_judge_response(response: &str) -> Option<JudgeVerdict> {
    let json_str = extract_json_from_response(response);

    let parsed: Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse judge response as JSON: {}", e);
            debug!("Raw judge response: {}", response);
            return None;
        }
    };

    let score = match parsed.get("score").and_then(|v| v.as_f64()) {
        Some(s) => s,
        None => {
            warn!("Judge response missing 'score' field");
            return None;
        }
    };
    let reasoning = parsed
        .get("reasoning")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let evidence = parsed
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Some(JudgeVerdict {
        score,
        reasoning,
        evidence,
        confidence: 0.85,
    })
}
