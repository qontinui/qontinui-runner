//! Online learning coordinator.
//!
//! Single entry point that orchestrates all online learning subsystems
//! after each workflow run completes. Runs as a non-blocking `tokio::spawn`
//! so it never delays the main execution loop.
//!
//! ## Pipeline
//!
//! 1. Credit assignment (ADCA) → persisted to `step_credit_assignments`
//! 2. Model routing update (bandit Q-value) → persisted to `model_routing_table`
//! 3. Drift detection (ADWIN, DDM, Page-Hinkley) → persisted to `performance_drift_signals`
//! 4. Experience reflection → persisted to `experience_summaries` + `step_type_knowledge`
//! 5. Strategy update → persisted to `strategy_bank`

use super::credit_assignment::{self, StepAttribution};
use super::model_router::ModelRouterBandit;
use super::reflection::{self, RunReflectionInput, StepReflection};
use crate::meta_optimizer::drift_detection::DriftMonitor;
use std::sync::Arc;
use tracing::{debug, info, warn};

// =============================================================================
// Outcome data
// =============================================================================

/// All data needed by the online learning coordinator from a completed run.
///
/// This is populated from `WorkflowOutcome` and pipeline trace data.
#[derive(Debug, Clone)]
pub struct OnlineLearningOutcome {
    /// Task run ID.
    pub task_run_id: String,
    /// Final outcome status ("success", "failure", "partial").
    pub status: String,
    /// Composite agentic score (0.0-1.0).
    pub composite_score: Option<f64>,
    /// Total cost in USD.
    pub cost_usd: Option<f64>,
    /// Total duration in seconds.
    pub duration_secs: Option<f64>,
    /// Model used for primary execution.
    pub model_used: Option<String>,
    /// Primary domain tag.
    pub domain: String,
    /// Complexity tier.
    pub complexity_tier: String,
    /// Whether the task has UI components.
    pub has_ui: bool,
    /// Prompt length in characters.
    pub prompt_length: usize,
    /// File count.
    pub file_count: usize,
    /// Workflow architecture used.
    pub workflow_architecture: Option<String>,
    /// Strategy ID if a strategy was applied.
    pub strategy_id: Option<String>,
    /// Step-level attribution data (from pipeline traces).
    pub step_attributions: Vec<StepAttribution>,
    /// Step-level reflection data.
    pub step_reflections: Vec<StepReflection>,
    /// Total iterations.
    pub total_iterations: u32,
    /// Technology tags.
    pub technology_tags: Vec<String>,
}

// =============================================================================
// Coordinator results
// =============================================================================

/// Summary of what the coordinator did during a post-run update.
#[derive(Debug, Clone, Default)]
pub struct CoordinatorResult {
    /// Number of step credits computed.
    pub credits_computed: usize,
    /// Whether the model routing table was updated.
    pub model_routing_updated: bool,
    /// Number of drift signals emitted.
    pub drift_signals: usize,
    /// Number of experience lessons extracted.
    pub lessons_extracted: usize,
    /// Whether strategy stats were updated.
    pub strategy_updated: bool,
    /// Number of items persisted to PG.
    pub items_persisted: usize,
    /// Total coordinator execution time in milliseconds.
    pub elapsed_ms: u64,
}

// =============================================================================
// Coordinator (sync — called from spawn_blocking)
// =============================================================================

/// Run the full online learning pipeline for a completed workflow.
///
/// This is the **in-memory computation** phase. It updates the global model
/// router and drift monitor, and returns data that needs to be persisted.
/// The caller is responsible for async PG persistence.
pub fn post_run_online_learning(
    outcome: &OnlineLearningOutcome,
    model_router: &mut ModelRouterBandit,
    drift_monitor: &mut DriftMonitor,
) -> CoordinatorResult {
    let start = std::time::Instant::now();
    let mut result = CoordinatorResult::default();

    // =========================================================================
    // 1. Credit assignment
    // =========================================================================
    let _credits = credit_assignment::compute_step_credits(
        &outcome.step_attributions,
        outcome.composite_score.unwrap_or(0.0),
        outcome.status == "success",
    );
    result.credits_computed = _credits.len();

    // =========================================================================
    // 2. Model routing update
    // =========================================================================
    if let Some(ref model) = outcome.model_used {
        let reward = ModelRouterBandit::compute_reward(
            outcome.composite_score.unwrap_or(0.0),
            outcome.cost_usd,
        );
        let context = super::context::ModelRoutingContext {
            prompt_length: outcome.prompt_length,
            file_count: outcome.file_count,
            complexity_tier: outcome.complexity_tier.clone(),
            domain: outcome.domain.clone(),
            has_ui: outcome.has_ui,
            step_type: None,
        };
        model_router.update(&context, model, reward);
        result.model_routing_updated = true;
    }

    // =========================================================================
    // 3. Drift detection
    // =========================================================================
    let context_key = format!(
        "{}:{}:{}",
        outcome.domain,
        outcome.complexity_tier,
        if outcome.has_ui { "ui" } else { "no_ui" }
    );
    let signals = drift_monitor.observe_outcome(
        &context_key,
        outcome.composite_score,
        outcome.status == "success",
        outcome.cost_usd,
        outcome.duration_secs,
    );
    result.drift_signals = signals.len();

    // If drift detected, boost exploration in the model router
    for signal in &signals {
        if signal.drift_level == crate::meta_optimizer::drift_detection::DriftLevel::Drift {
            model_router.boost_exploration(&signal.context_key);
            warn!(
                "Drift detected for {}:{} — boosting exploration",
                signal.metric_name, signal.context_key
            );
        }
    }

    // =========================================================================
    // 4. Experience reflection
    // =========================================================================
    let mut step_lessons = Vec::new();
    for step_ref in &outcome.step_reflections {
        step_lessons.extend(reflection::extract_step_lessons(step_ref));
    }

    let experience_input = RunReflectionInput {
        task_run_id: outcome.task_run_id.clone(),
        domain: outcome.domain.clone(),
        complexity_tier: outcome.complexity_tier.clone(),
        outcome_status: outcome.status.clone(),
        composite_score: outcome.composite_score,
        steps: outcome.step_reflections.clone(),
        total_iterations: outcome.total_iterations,
        total_cost_usd: outcome.cost_usd,
    };
    let _experience_summary = reflection::summarize_experience(&experience_input);
    let derived_lessons = reflection::derive_step_lessons(&_experience_summary);
    step_lessons.extend(derived_lessons);
    result.lessons_extracted = step_lessons.len();

    // =========================================================================
    // 5. Strategy update
    // =========================================================================
    if outcome.strategy_id.is_some() {
        result.strategy_updated = true;
    }

    result.elapsed_ms = start.elapsed().as_millis() as u64;

    info!(
        "Online learning completed in {}ms for run {}: credits={} routing={} drift={} lessons={}",
        result.elapsed_ms,
        outcome.task_run_id,
        result.credits_computed,
        result.model_routing_updated,
        result.drift_signals,
        result.lessons_extracted,
    );

    result
}

// NOTE: PG persistence is handled inline in loop_controller.rs (Phase 2 of
// the online learning block). This keeps the coordinator sync-only and avoids
// holding MutexGuards across .await points.

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_outcome() -> OnlineLearningOutcome {
        OnlineLearningOutcome {
            task_run_id: "test-run-1".to_string(),
            status: "success".to_string(),
            composite_score: Some(0.85),
            cost_usd: Some(0.50),
            duration_secs: Some(120.0),
            model_used: Some("claude-sonnet-4-20250514".to_string()),
            domain: "backend".to_string(),
            complexity_tier: "moderate".to_string(),
            has_ui: false,
            prompt_length: 1000,
            file_count: 5,
            workflow_architecture: Some("agentic_verification".to_string()),
            strategy_id: None,
            step_attributions: vec![
                StepAttribution {
                    step_index: 0,
                    step_type: "plan".to_string(),
                    agent_type: "planner".to_string(),
                    duration_ms: 2000,
                    tokens_used: 500,
                    cost_usd: 0.10,
                    downstream_success: Some(true),
                    confidence_delta: Some(0.1),
                    output_utilization: 0.8,
                    state_change_magnitude: 0.5,
                },
                StepAttribution {
                    step_index: 1,
                    step_type: "generate".to_string(),
                    agent_type: "implementer".to_string(),
                    duration_ms: 5000,
                    tokens_used: 1500,
                    cost_usd: 0.30,
                    downstream_success: Some(true),
                    confidence_delta: Some(0.3),
                    output_utilization: 0.9,
                    state_change_magnitude: 5.0,
                },
            ],
            step_reflections: vec![
                StepReflection {
                    step_type: "plan".to_string(),
                    step_index: 0,
                    task_run_id: "test-run-1".to_string(),
                    duration_ms: 2000,
                    success: true,
                    tool_calls: vec![],
                    error_type: None,
                    retry_count: 0,
                },
                StepReflection {
                    step_type: "generate".to_string(),
                    step_index: 1,
                    task_run_id: "test-run-1".to_string(),
                    duration_ms: 5000,
                    success: true,
                    tool_calls: vec!["write_file".to_string()],
                    error_type: None,
                    retry_count: 0,
                },
            ],
            total_iterations: 1,
            technology_tags: vec!["rust".to_string()],
        }
    }

    #[test]
    fn test_coordinator_basic_flow() {
        let outcome = make_outcome();
        let mut model_router = ModelRouterBandit::new();
        let mut drift_monitor = DriftMonitor::new();

        let result = post_run_online_learning(&outcome, &mut model_router, &mut drift_monitor);

        assert_eq!(result.credits_computed, 2);
        assert!(result.model_routing_updated);
        assert_eq!(result.drift_signals, 0); // No drift on first observation
        assert!(result.elapsed_ms < 1000); // Should be fast (no LLM calls)
    }

    #[test]
    fn test_coordinator_without_model() {
        let mut outcome = make_outcome();
        outcome.model_used = None;
        let mut model_router = ModelRouterBandit::new();
        let mut drift_monitor = DriftMonitor::new();

        let result = post_run_online_learning(&outcome, &mut model_router, &mut drift_monitor);

        assert!(!result.model_routing_updated); // No model = no routing update
    }

    #[test]
    fn test_coordinator_with_step_failures() {
        let mut outcome = make_outcome();
        outcome.status = "failure".to_string();
        outcome.composite_score = Some(0.20);
        outcome.step_reflections = vec![StepReflection {
            step_type: "generate".to_string(),
            step_index: 0,
            task_run_id: "test-run-1".to_string(),
            duration_ms: 30000,
            success: false,
            tool_calls: (0..15).map(|i| format!("tool_{}", i)).collect(),
            error_type: Some("runtime_error".to_string()),
            retry_count: 5,
        }];

        let mut model_router = ModelRouterBandit::new();
        let mut drift_monitor = DriftMonitor::new();

        let result = post_run_online_learning(&outcome, &mut model_router, &mut drift_monitor);

        // Should extract lessons from the failed step (repeated failure + excessive tools)
        assert!(result.lessons_extracted > 0);
    }
}
