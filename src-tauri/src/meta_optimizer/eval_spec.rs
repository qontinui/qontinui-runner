//! Declarative evaluation specification types.
//!
//! Defines `EvalSpec` — a structured test suite for validating meta-optimizer
//! recommendations before human review. Inspired by promptfoo's declarative
//! YAML-based eval configs.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::info;

use crate::database::pg::PgDb;

// =============================================================================
// Core types
// =============================================================================

/// A declarative evaluation specification — defines test cases and thresholds
/// for validating a recommendation or prompt variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSpec {
    pub id: String,
    pub name: String,
    /// Which pipeline agent this spec targets (e.g., "spec_analyst", "implementer").
    pub target_agent: Option<String>,
    /// Test cases to run.
    pub test_cases: Vec<EvalTestCase>,
    /// Thresholds for pass/fail.
    pub thresholds: EvalThresholds,
}

/// A single test case within an eval spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTestCase {
    pub id: String,
    pub description: String,
    /// Input specification: which workflows to run against.
    pub input: EvalInput,
    /// Assertions that must hold for this test case to pass.
    pub assertions: Vec<EvalAssertion>,
}

/// What to run the eval against.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalInput {
    /// Run a single workflow by ID.
    Workflow { workflow_id: String },
    /// Run multiple workflows.
    WorkflowIds { ids: Vec<String> },
}

/// An assertion that must hold for a test case to pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvalAssertion {
    /// Minimum success rate (0.0–1.0).
    SuccessRate { min: f64 },
    /// Maximum duration per run (milliseconds).
    MaxDuration { ms: u64 },
    /// Maximum cost per run (USD cents).
    MaxCost { cents: f64 },
    /// Maximum iterations per run.
    MaxIterations { count: u32 },
    /// No regression vs baseline: metric must not drop more than `tolerance_pp`
    /// percentage points.
    NoRegression { metric: String, tolerance_pp: f64 },
    /// LLM-as-judge hallucination check: output hallucination rate must be below threshold.
    HallucinationCheck { max_hallucination_rate: f64 },
    /// LLM-as-judge answer relevance: output must meet minimum relevance score.
    AnswerRelevance { min_relevance: f64 },
    /// LLM-as-judge factuality: output must meet minimum factuality score.
    Factuality { min_factuality: f64 },
    /// Content safety: output must not contain blocked content categories.
    ContentSafety { blocked_categories: Vec<String> },
}

/// Thresholds for declaring an eval as passed, failed, or inconclusive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalThresholds {
    /// Minimum number of trials to run per group (baseline and candidate).
    #[serde(default = "default_min_trials")]
    pub min_trials: u32,
    /// p-value threshold for statistical significance.
    #[serde(default = "default_significance")]
    pub significance_threshold: f64,
    /// Minimum fraction of test cases that must pass (0.0–1.0).
    #[serde(default = "default_required_pass_rate")]
    pub required_pass_rate: f64,
}

fn default_min_trials() -> u32 {
    3
}
fn default_significance() -> f64 {
    0.05
}
fn default_required_pass_rate() -> f64 {
    1.0
}

impl Default for EvalThresholds {
    fn default() -> Self {
        Self {
            min_trials: default_min_trials(),
            significance_threshold: default_significance(),
            required_pass_rate: default_required_pass_rate(),
        }
    }
}

// =============================================================================
// Eval results
// =============================================================================

/// Result of running an eval spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    pub id: String,
    pub spec_id: String,
    pub recommendation_id: Option<String>,
    /// Overall status: "passed", "failed", "inconclusive", "running".
    pub status: String,
    /// Per-test-case results.
    pub test_case_results: Vec<TestCaseResult>,
    /// Aggregate baseline metrics.
    pub baseline_metrics: Option<EvalAggregateMetrics>,
    /// Aggregate candidate metrics.
    pub candidate_metrics: Option<EvalAggregateMetrics>,
    /// Statistical comparison.
    pub comparison: Option<EvalComparison>,
    pub trials_run: u32,
    pub created_at: String,
}

/// Result for a single test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseResult {
    pub test_case_id: String,
    pub passed: bool,
    pub assertion_results: Vec<AssertionResult>,
}

/// Result for a single assertion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertionResult {
    pub assertion_type: String,
    pub passed: bool,
    pub actual_value: f64,
    pub threshold_value: f64,
    pub message: String,
}

/// Aggregate metrics from eval trials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalAggregateMetrics {
    pub success_rate: f64,
    pub mean_duration_ms: f64,
    pub mean_iterations: f64,
    pub mean_cost_cents: f64,
    pub trial_count: u32,
}

/// Statistical comparison between baseline and candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalComparison {
    pub success_rate_delta_pp: f64,
    pub p_value: Option<f64>,
    pub confidence_interval: Option<(f64, f64)>,
    pub effect_size: Option<f64>,
    pub verdict: String,
}

// =============================================================================
// Database CRUD
// =============================================================================

/// Create or update an eval spec.
pub fn save_eval_spec(pg_db: &Arc<PgDb>, spec: &EvalSpec) -> Result<(), String> {
    let id = spec.id.clone();
    let name = spec.name.clone();
    let target_agent = spec.target_agent.clone();
    let spec_json =
        serde_json::to_string(spec).map_err(|e| format!("Failed to serialize spec: {}", e))?;

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.save_eval_spec(
            &id,
            &name,
            &target_agent.clone().unwrap_or_default(),
            &spec_json,
        ))
    })?;
    info!("Saved eval spec {} ({})", id, name);
    Ok(())
}

/// List all eval specs, optionally filtered by target agent.
pub fn list_eval_specs(
    pg_db: &Arc<PgDb>,
    target_agent: Option<&str>,
) -> Result<Vec<EvalSpec>, String> {
    let jsons = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_eval_specs(target_agent))
    })?;
    let specs: Vec<EvalSpec> = jsons
        .iter()
        .filter_map(|j| serde_json::from_str(j).ok())
        .collect();
    Ok(specs)
}

/// Get a single eval spec by ID.
pub fn get_eval_spec(pg_db: &Arc<PgDb>, spec_id: &str) -> Result<Option<EvalSpec>, String> {
    let result =
        tokio::task::block_in_place(|| Handle::current().block_on(pg_db.get_eval_spec(spec_id)))?;
    match result {
        Some(json) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("Failed to deserialize eval spec: {}", e)),
        None => Ok(None),
    }
}

/// Delete an eval spec.
pub fn delete_eval_spec(pg_db: &Arc<PgDb>, spec_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| Handle::current().block_on(pg_db.delete_eval_spec(spec_id)))
}

/// Save an eval result.
pub fn save_eval_result(pg_db: &Arc<PgDb>, result: &EvalResult) -> Result<(), String> {
    let id = result.id.clone();
    let spec_id = result.spec_id.clone();
    let recommendation_id = result.recommendation_id.clone();
    let status = result.status.clone();
    let result_json = serde_json::to_string(result)
        .map_err(|e| format!("Failed to serialize eval result: {}", e))?;
    let p_value = result.comparison.as_ref().and_then(|c| c.p_value);
    let trials_run = result.trials_run as i64;

    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.save_eval_result(
            &id,
            &spec_id,
            recommendation_id.as_deref(),
            &status,
            &result_json,
            p_value,
            trials_run,
        ))
    })?;
    info!("Saved eval result {} (status={})", id, status);
    Ok(())
}

/// List eval results, optionally filtered by spec or recommendation.
pub fn list_eval_results(
    pg_db: &Arc<PgDb>,
    spec_id: Option<&str>,
    recommendation_id: Option<&str>,
) -> Result<Vec<EvalResult>, String> {
    let jsons = tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_eval_results(spec_id, recommendation_id))
    })?;
    let results: Vec<EvalResult> = jsons
        .iter()
        .filter_map(|j| serde_json::from_str(j).ok())
        .collect();
    Ok(results)
}

/// Update the eval_status and eval_result_id on a recommendation.
pub fn attach_eval_result(
    pg_db: &Arc<PgDb>,
    recommendation_id: &str,
    eval_result_id: &str,
    eval_status: &str,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.attach_eval_result(
            recommendation_id,
            eval_result_id,
            eval_status,
        ))
    })?;
    info!(
        "Attached eval result {} to recommendation {} (status={})",
        eval_result_id, recommendation_id, eval_status
    );
    Ok(())
}

// =============================================================================
// Auto-generation of default eval specs
// =============================================================================

/// Generate a default eval spec for a given target agent based on historical performance.
///
/// Uses recent learning outcomes to derive sensible assertion thresholds
/// (e.g., "success rate must be at least 80% of historical average").
pub fn generate_default_spec(target_agent: &str) -> Result<EvalSpec, String> {
    let agent = target_agent.to_string();

    // Get historical performance baseline
    let baseline = crate::database::pipeline_traces::get_agent_aggregates_for_period(
        &agent,
        &(chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339(),
        &chrono::Utc::now().to_rfc3339(),
    )?;

    let id = format!("es-auto-{}", uuid::Uuid::new_v4());

    let assertions = if let Some(ref agg) = baseline {
        let sr = if agg.run_count > 0 {
            agg.success_count as f64 / agg.run_count as f64
        } else {
            0.0
        };

        let mut a = vec![
            // Must maintain at least 80% of current success rate
            EvalAssertion::SuccessRate {
                min: (sr * 0.8).max(0.0),
            },
            // No regression beyond 5pp
            EvalAssertion::NoRegression {
                metric: "success_rate".to_string(),
                tolerance_pp: 5.0,
            },
        ];

        // If we have duration data, add a duration ceiling (2x average)
        if agg.avg_duration_ms > 0.0 {
            a.push(EvalAssertion::MaxDuration {
                ms: (agg.avg_duration_ms * 2.0) as u64,
            });
        }

        a
    } else {
        // No historical data — use generous defaults
        vec![
            EvalAssertion::SuccessRate { min: 0.5 },
            EvalAssertion::NoRegression {
                metric: "success_rate".to_string(),
                tolerance_pp: 10.0,
            },
        ]
    };

    Ok(EvalSpec {
        id,
        name: format!("Auto-generated spec for {}", target_agent),
        target_agent: Some(target_agent.to_string()),
        test_cases: vec![EvalTestCase {
            id: format!("tc-{}", uuid::Uuid::new_v4()),
            description: format!("Baseline validation for {} agent", target_agent),
            input: EvalInput::WorkflowIds { ids: vec![] }, // populated at eval time from recent runs
            assertions,
        }],
        thresholds: EvalThresholds::default(),
    })
}

// Deprecated wrappers removed — all functions are now PG-primary.
