//! Bandit-based model router.
//!
//! Replaces the static keyword/heuristic `TaskRouter` in `ai_router.rs` with a
//! contextual bandit that learns per-context model preferences from outcome data.
//!
//! Uses UCB1 policy: principled exploration that naturally favors under-tested arms
//! (models). When a new model is added, it automatically gets explored.

use super::bandit::{ContextualBandit, SelectionResult, SelectionSource};
use super::context::{ModelRoutingContext, ModelRoutingEncoder};
use super::policies::UCB1;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

// =============================================================================
// Configuration
// =============================================================================

/// Default models available for routing.
const DEFAULT_SIMPLE_MODEL: &str = "claude-haiku-4-5-20251001";
const DEFAULT_MEDIUM_MODEL: &str = "claude-sonnet-4-20250514";
const DEFAULT_COMPLEX_MODEL: &str = "claude-opus-4-20250514";

/// Minimum visits per context before bandit activates (falls back to static router).
const MIN_VISITS_FOR_ACTIVATION: u32 = 3;

/// Learning rate for Q-value updates.
const LEARNING_RATE: f64 = 0.1;

/// Cost penalty weight applied in reward calculation.
/// reward = composite_agentic_score - COST_PENALTY * total_cost_usd
const COST_PENALTY: f64 = 0.01;

// =============================================================================
// Model routing decision
// =============================================================================

/// Result of a model routing decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutingDecision {
    /// The selected model identifier.
    pub model: String,
    /// How the model was selected.
    pub source: RoutingSource,
    /// The context key used for the decision.
    pub context_key: String,
    /// Confidence in this selection.
    pub confidence: f64,
    /// Whether this was an exploration decision.
    pub exploration: bool,
}

/// How the model was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingSource {
    /// Selected by the contextual bandit.
    Bandit,
    /// Fallback to the static heuristic router.
    Fallback,
    /// Manual override for this context.
    Override,
}

// =============================================================================
// Model Router Bandit
// =============================================================================

/// Bandit-based model router that learns optimal model selection per task context.
pub struct ModelRouterBandit {
    /// The underlying contextual bandit.
    bandit: ContextualBandit<ModelRoutingEncoder, UCB1>,
    /// Available model identifiers.
    available_models: Vec<String>,
}

impl ModelRouterBandit {
    /// Create a new model router bandit with default settings.
    pub fn new() -> Self {
        let policy = UCB1::default_optimal();
        let bandit = ContextualBandit::new(ModelRoutingEncoder, policy, LEARNING_RATE)
            .with_min_visits(MIN_VISITS_FOR_ACTIVATION);

        Self {
            bandit,
            available_models: vec![
                DEFAULT_SIMPLE_MODEL.to_string(),
                DEFAULT_MEDIUM_MODEL.to_string(),
                DEFAULT_COMPLEX_MODEL.to_string(),
            ],
        }
    }

    /// Create with custom available models.
    pub fn with_models(models: Vec<String>) -> Self {
        let policy = UCB1::default_optimal();
        let bandit = ContextualBandit::new(ModelRoutingEncoder, policy, LEARNING_RATE)
            .with_min_visits(MIN_VISITS_FOR_ACTIVATION);

        Self {
            bandit,
            available_models: models,
        }
    }

    /// Route a task to the optimal model.
    ///
    /// Returns a routing decision. If the bandit lacks data for this context,
    /// falls back to the provided `fallback_model`.
    pub fn route(
        &mut self,
        context: &ModelRoutingContext,
        fallback_model: &str,
    ) -> ModelRoutingDecision {
        match self.bandit.select(context, &self.available_models) {
            Some(result) => {
                let source = match result.source {
                    SelectionSource::Policy => RoutingSource::Bandit,
                    SelectionSource::Fallback => RoutingSource::Fallback,
                    SelectionSource::Override => RoutingSource::Override,
                };

                // If fallback, use the caller's fallback model instead of bandit's default
                let model = if source == RoutingSource::Fallback {
                    fallback_model.to_string()
                } else {
                    result.arm
                };

                info!(
                    "Model router: {} → {} (source={:?}, context={}, explore={})",
                    context.complexity_tier,
                    model,
                    source,
                    result.context_key,
                    result.is_exploration
                );

                ModelRoutingDecision {
                    model,
                    source,
                    context_key: result.context_key,
                    confidence: result.confidence,
                    exploration: result.is_exploration,
                }
            }
            None => {
                debug!("Model router: no arms available, using fallback");
                ModelRoutingDecision {
                    model: fallback_model.to_string(),
                    source: RoutingSource::Fallback,
                    context_key: String::new(),
                    confidence: 0.0,
                    exploration: false,
                }
            }
        }
    }

    /// Update the bandit after observing a reward for a (context, model) pair.
    pub fn update(&mut self, context: &ModelRoutingContext, model: &str, reward: f64) {
        self.bandit.update_with_input(context, model, reward);
    }

    /// Update using a pre-computed context key.
    pub fn update_with_key(&mut self, context_key: &str, model: &str, reward: f64) {
        self.bandit.update(context_key, model, reward);
    }

    /// Compute reward from composite score and cost (same formula as Q-Router).
    pub fn compute_reward(composite_agentic_score: f64, total_cost_usd: Option<f64>) -> f64 {
        let cost_penalty = total_cost_usd.unwrap_or(0.0) * COST_PENALTY;
        (composite_agentic_score - cost_penalty).clamp(0.0, 1.0)
    }

    /// Increase exploration for a specific context (called by drift detection).
    pub fn boost_exploration(&mut self, context_key: &str) {
        self.bandit.boost_exploration(context_key);
    }

    /// Set a manual override for a context.
    pub fn set_override(&mut self, context_key: &str, model: &str) {
        self.bandit.set_override(context_key, model);
    }

    /// Remove a manual override.
    pub fn remove_override(&mut self, context_key: &str) -> bool {
        self.bandit.remove_override(context_key)
    }

    /// Load bandit state from database rows.
    pub fn load_from_rows(&mut self, rows: Vec<(String, String, f64, u32, f64)>) {
        self.bandit.load_from_rows(rows);
    }

    /// Load overrides from database rows.
    pub fn load_overrides(&mut self, overrides: Vec<(String, String)>) {
        self.bandit.load_overrides(overrides);
    }

    /// Export Q-table for persistence.
    pub fn table_snapshot(&self) -> Vec<super::bandit::QTableRow> {
        self.bandit.table_snapshot()
    }

    /// Get the greedy policy for diagnostics.
    pub fn policy_snapshot(&self) -> Vec<super::bandit::PolicyEntry> {
        self.bandit.policy_snapshot(&self.available_models)
    }

    /// Number of (context, model) entries in the Q-table.
    pub fn table_size(&self) -> usize {
        self.bandit.table_size()
    }

    /// The list of available models.
    pub fn available_models(&self) -> &[String] {
        &self.available_models
    }

    /// Update the list of available models (e.g., when a new model is released).
    pub fn set_available_models(&mut self, models: Vec<String>) {
        self.available_models = models;
    }
}

impl Default for ModelRouterBandit {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(complexity: &str, domain: &str) -> ModelRoutingContext {
        ModelRoutingContext {
            prompt_length: 1000,
            file_count: 5,
            complexity_tier: complexity.to_string(),
            domain: domain.to_string(),
            has_ui: false,
            step_type: None,
        }
    }

    #[test]
    fn test_fallback_on_empty_bandit() {
        let mut router = ModelRouterBandit::new();
        let ctx = make_context("moderate", "backend");
        let decision = router.route(&ctx, DEFAULT_MEDIUM_MODEL);

        assert_eq!(decision.source, RoutingSource::Fallback);
        assert_eq!(decision.model, DEFAULT_MEDIUM_MODEL);
    }

    #[test]
    fn test_learns_from_rewards() {
        let mut router = ModelRouterBandit::new();
        let ctx = make_context("complex", "backend");

        // Seed some rewards: opus consistently better
        for _ in 0..5 {
            router.update(&ctx, DEFAULT_COMPLEX_MODEL, 0.9);
            router.update(&ctx, DEFAULT_MEDIUM_MODEL, 0.5);
            router.update(&ctx, DEFAULT_SIMPLE_MODEL, 0.3);
        }

        // After learning, should prefer opus for complex tasks
        let decision = router.route(&ctx, DEFAULT_MEDIUM_MODEL);
        assert_ne!(decision.source, RoutingSource::Fallback);
        // UCB may still explore, but greedy arm should be opus
    }

    #[test]
    fn test_compute_reward() {
        let reward = ModelRouterBandit::compute_reward(0.85, Some(0.50));
        assert!((reward - 0.845).abs() < 0.01); // 0.85 - 0.01 * 0.50

        let reward_no_cost = ModelRouterBandit::compute_reward(0.85, None);
        assert!((reward_no_cost - 0.85).abs() < 0.01);
    }

    #[test]
    fn test_override() {
        let mut router = ModelRouterBandit::new();
        let ctx = make_context("simple", "frontend");

        // make_context uses prompt_length=1000 (medium) and file_count=5 (moderate)
        router.set_override(
            "medium:moderate:simple:frontend:no_ui",
            DEFAULT_COMPLEX_MODEL,
        );

        let decision = router.route(&ctx, DEFAULT_SIMPLE_MODEL);
        assert_eq!(decision.source, RoutingSource::Override);
        assert_eq!(decision.model, DEFAULT_COMPLEX_MODEL);
    }

    #[test]
    fn test_table_snapshot() {
        let mut router = ModelRouterBandit::new();
        let ctx = make_context("moderate", "backend");

        router.update(&ctx, DEFAULT_MEDIUM_MODEL, 0.7);
        router.update(&ctx, DEFAULT_SIMPLE_MODEL, 0.4);

        let snapshot = router.table_snapshot();
        assert_eq!(snapshot.len(), 2);
    }
}
