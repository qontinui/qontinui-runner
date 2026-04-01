//! Per-Phase Model Q-Router
//!
//! Learns which ModelTier (Simple/Medium/Complex → Haiku/Sonnet/Opus) works best
//! for each workflow phase, given a TaskState. Extends the architecture Q-router
//! with model-level routing decisions.
//!
//! # State-Action Space
//! State: TaskState (50 cells from q_router.rs) x Phase (5 phases)
//! Action: ModelTier (Simple, Medium, Complex)
//! Total: 50 x 5 x 3 = 750 state-action pairs

use super::q_router::{QEntry, TaskState};
use crate::orchestrator::agent_roles::ModelTier;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

// =============================================================================
// Constants
// =============================================================================

/// Phases that have distinct model routing preferences.
pub const ROUTABLE_PHASES: [&str; 5] = [
    "setup",
    "verification",
    "agentic",
    "completion",
    "generation",
];

/// Learning rate for Q-value updates.
const ALPHA: f64 = 0.1;
/// Base exploration rate (decays per state-phase as visits accumulate).
const EPSILON_BASE: f64 = 0.3;
/// Minimum exploration rate.
const EPSILON_MIN: f64 = 0.05;
/// Decay rate for epsilon per visit.
const EPSILON_DECAY: f64 = 20.0;
/// Cache savings bonus weight in reward function.
const CACHE_SAVINGS_WEIGHT: f64 = 0.005;
/// Cost penalty weight in reward function.
const COST_PENALTY: f64 = 0.01;

// =============================================================================
// Helper: ModelTier ↔ string conversion
// =============================================================================

/// Convert a ModelTier to its stable string key for storage.
fn tier_to_str(tier: &ModelTier) -> &'static str {
    match tier {
        ModelTier::Simple => "simple",
        ModelTier::Medium => "medium",
        ModelTier::Complex => "complex",
    }
}

/// Parse a ModelTier from its string key.
fn tier_from_str(s: &str) -> Option<ModelTier> {
    match s {
        "simple" => Some(ModelTier::Simple),
        "medium" => Some(ModelTier::Medium),
        "complex" => Some(ModelTier::Complex),
        _ => None,
    }
}

/// All model tier variants for iteration.
const ALL_TIERS: [ModelTier; 3] = [ModelTier::Simple, ModelTier::Medium, ModelTier::Complex];

// =============================================================================
// Composite key type (for documentation / future use)
// =============================================================================

/// A composite key identifying one cell in the phase-model Q-table.
///
/// Conceptually: (TaskState key, workflow phase, model tier).
/// Stored as a `(String, String, String)` tuple in the HashMap for simplicity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhaseModelKey {
    pub state_key: String,
    pub phase: String,
    pub model_tier: String,
}

impl PhaseModelKey {
    pub fn new(state: &TaskState, phase: &str, tier: &ModelTier) -> Self {
        Self {
            state_key: state.to_key(),
            phase: phase.to_string(),
            model_tier: tier_to_str(tier).to_string(),
        }
    }

    /// Convert to the tuple form used as HashMap key.
    pub fn to_tuple(&self) -> (String, String, String) {
        (
            self.state_key.clone(),
            self.phase.clone(),
            self.model_tier.clone(),
        )
    }
}

// =============================================================================
// PhaseModelRouter
// =============================================================================

/// Per-phase Q-learning router for model tier selection.
///
/// Maintains a Q-table mapping (TaskState, Phase, ModelTier) → QEntry.
/// Supports epsilon-greedy selection, incremental Q-value updates,
/// and serialization for API/frontend display.
pub struct PhaseModelRouter {
    /// In-memory Q-table: (state_key, phase, model_tier_str) → QEntry.
    table: HashMap<(String, String, String), QEntry>,
    /// Learning rate.
    alpha: f64,
    /// RNG for exploration.
    rng: StdRng,
}

impl PhaseModelRouter {
    /// Create an empty router with default hyperparameters.
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            alpha: ALPHA,
            rng: StdRng::from_os_rng(),
        }
    }

    /// Create a router with a specific learning rate (useful for tests).
    #[cfg(test)]
    fn with_alpha(alpha: f64) -> Self {
        Self {
            table: HashMap::new(),
            alpha,
            rng: StdRng::from_os_rng(),
        }
    }

    /// Load Q-table entries from database rows.
    ///
    /// Each row: (state_key, phase, model_tier, q_value, visit_count).
    pub fn load_from_rows(&mut self, rows: Vec<(String, String, String, f64, u32)>) {
        for (state_key, phase, model_tier, q_value, visit_count) in rows {
            self.table.insert(
                (state_key, phase, model_tier),
                QEntry {
                    q_value,
                    visit_count,
                },
            );
        }
    }

    /// Get the Q-entry for a (state, phase, tier) triple.
    pub fn get_q(&self, state: &TaskState, phase: &str, tier: &ModelTier) -> QEntry {
        let key = (
            state.to_key(),
            phase.to_string(),
            tier_to_str(tier).to_string(),
        );
        self.table.get(&key).cloned().unwrap_or_default()
    }

    /// Update Q-value after observing a reward for (state, phase, tier).
    ///
    /// Contextual bandit update (no next-state):
    ///   Q(s,a) ← Q(s,a) + α * (reward - Q(s,a))
    ///
    /// Returns the updated QEntry for database persistence.
    pub fn update(
        &mut self,
        state: &TaskState,
        phase: &str,
        tier: &ModelTier,
        reward: f64,
    ) -> QEntry {
        let key = (
            state.to_key(),
            phase.to_string(),
            tier_to_str(tier).to_string(),
        );
        let entry = self.table.entry(key).or_default();

        entry.q_value += self.alpha * (reward - entry.q_value);
        entry.visit_count += 1;

        debug!(
            state = %state.to_key(),
            phase = phase,
            tier = tier_to_str(tier),
            new_q = entry.q_value,
            visits = entry.visit_count,
            "model Q-table update"
        );

        entry.clone()
    }

    /// Compute reward from composite score, cost, and cache savings.
    ///
    /// reward = composite_score - COST_PENALTY * cost + CACHE_SAVINGS_WEIGHT * cache_savings
    /// Clamped to [0, 1].
    pub fn compute_reward(
        composite_score: f64,
        cost_usd: Option<f64>,
        cache_savings_usd: Option<f64>,
    ) -> f64 {
        let cost_penalty = cost_usd.unwrap_or(0.0) * COST_PENALTY;
        let cache_bonus = cache_savings_usd.unwrap_or(0.0) * CACHE_SAVINGS_WEIGHT;
        (composite_score - cost_penalty + cache_bonus).clamp(0.0, 1.0)
    }

    /// Select a model tier using epsilon-greedy policy.
    ///
    /// With probability ε, pick randomly among the 3 tiers.
    /// Otherwise, pick the tier with the highest Q-value (greedy).
    pub fn select_model_tier(&mut self, state: &TaskState, phase: &str) -> ModelTier {
        let epsilon = self.effective_epsilon(state, phase);

        if self.rng.random_bool(epsilon.min(1.0)) {
            // Explore: random tier
            let idx = self.rng.random_range(0..ALL_TIERS.len());
            let tier = ALL_TIERS[idx];
            debug!(
                state = %state.to_key(),
                phase = phase,
                epsilon = epsilon,
                selected = tier_to_str(&tier),
                "model Q-router: explore"
            );
            tier
        } else {
            // Exploit: greedy
            let tier = self.greedy_tier(state, phase);
            debug!(
                state = %state.to_key(),
                phase = phase,
                epsilon = epsilon,
                selected = tier_to_str(&tier),
                "model Q-router: exploit"
            );
            tier
        }
    }

    /// Get the greedy (best) model tier for a (state, phase) pair.
    ///
    /// Returns the tier with the highest Q-value. Ties broken in favor of
    /// cheaper tiers (Simple preferred over Medium over Complex).
    pub fn greedy_tier(&self, state: &TaskState, phase: &str) -> ModelTier {
        ALL_TIERS
            .iter()
            .min_by(|a, b| {
                let qa = self.get_q(state, phase, a).q_value;
                let qb = self.get_q(state, phase, b).q_value;
                // Reverse: higher Q sorts first; on tie, earlier (cheaper) tier wins
                qb.partial_cmp(&qa).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
            .unwrap_or(ModelTier::Simple)
    }

    /// Compute effective epsilon for a (state, phase) pair.
    ///
    /// Auto-decays: ε = max(EPSILON_MIN, EPSILON_BASE * exp(-total_visits / EPSILON_DECAY))
    pub fn effective_epsilon(&self, state: &TaskState, phase: &str) -> f64 {
        let total_visits: u32 = ALL_TIERS
            .iter()
            .map(|t| self.get_q(state, phase, t).visit_count)
            .sum();

        let epsilon = EPSILON_BASE * (-(total_visits as f64) / EPSILON_DECAY).exp();
        epsilon.max(EPSILON_MIN)
    }

    /// Check if we have sufficient data for a (state, phase) to route confidently.
    ///
    /// Requires all 3 tiers to have been visited at least once.
    pub fn has_sufficient_data(&self, state: &TaskState, phase: &str) -> bool {
        ALL_TIERS
            .iter()
            .all(|t| self.get_q(state, phase, t).visit_count >= 1)
    }

    /// Get the full Q-table as serializable rows for API/frontend.
    pub fn table_snapshot(&self) -> Vec<PhaseModelRow> {
        self.table
            .iter()
            .map(|((state_key, phase, model_tier), entry)| PhaseModelRow {
                state_key: state_key.clone(),
                phase: phase.clone(),
                model_tier: model_tier.clone(),
                q_value: entry.q_value,
                visit_count: entry.visit_count,
            })
            .collect()
    }

    /// Get aggregate stats about the phase-model Q-routing system.
    pub fn stats(&self) -> PhaseModelStats {
        let total_entries = self.table.len();
        let total_visits: u64 = self.table.values().map(|e| e.visit_count as u64).sum();

        // Count unique (state, phase) pairs that have data
        let mut phase_state_keys: std::collections::HashSet<(&String, &String)> =
            std::collections::HashSet::new();
        for (sk, phase, _) in self.table.keys() {
            phase_state_keys.insert((sk, phase));
        }
        let phases_with_data = phase_state_keys.len();

        // Count (state, phase) pairs where all 3 tiers have been visited
        let routable_phase_states = phase_state_keys
            .iter()
            .filter(|(sk, phase)| {
                TaskState::from_key(sk)
                    .map(|ts| self.has_sufficient_data(&ts, phase))
                    .unwrap_or(false)
            })
            .count();

        let avg_q_value = if total_entries > 0 {
            self.table.values().map(|e| e.q_value).sum::<f64>() / total_entries as f64
        } else {
            0.0
        };

        info!(
            total_entries = total_entries,
            phases_with_data = phases_with_data,
            routable = routable_phase_states,
            total_visits = total_visits,
            avg_q = format!("{:.4}", avg_q_value),
            "model Q-router stats"
        );

        PhaseModelStats {
            total_entries,
            phases_with_data,
            routable_phase_states,
            total_visits,
            avg_q_value,
        }
    }
}

// =============================================================================
// Serialization types for API responses
// =============================================================================

/// One row of the phase-model Q-table (for API/frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseModelRow {
    pub state_key: String,
    pub phase: String,
    pub model_tier: String,
    pub q_value: f64,
    pub visit_count: u32,
}

/// Aggregate stats about the phase-model Q-routing system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseModelStats {
    pub total_entries: usize,
    pub phases_with_data: usize,
    pub routable_phase_states: usize,
    pub total_visits: u64,
    pub avg_q_value: f64,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoresearch::q_router::{ComplexityTier, Domain};

    fn make_state(domain: Domain, complexity: ComplexityTier, has_ui: bool) -> TaskState {
        TaskState {
            primary_domain: domain,
            complexity,
            has_ui_component: has_ui,
        }
    }

    #[test]
    fn test_basic_update() {
        let mut router = PhaseModelRouter::with_alpha(0.1);
        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);

        // Repeatedly observe reward of 0.8 — Q-value should converge toward 0.8
        for _ in 0..100 {
            router.update(&state, "verification", &ModelTier::Medium, 0.8);
        }

        let q = router.get_q(&state, "verification", &ModelTier::Medium);
        assert!(
            (q.q_value - 0.8).abs() < 0.01,
            "Q-value {} should converge to ~0.8",
            q.q_value
        );
        assert_eq!(q.visit_count, 100);

        // Different phase should be independent
        let q_other = router.get_q(&state, "setup", &ModelTier::Medium);
        assert_eq!(q_other.q_value, 0.0);
        assert_eq!(q_other.visit_count, 0);
    }

    #[test]
    fn test_epsilon_greedy_explores() {
        let mut router = PhaseModelRouter::with_alpha(0.1);
        let state = make_state(Domain::Frontend, ComplexityTier::Simple, true);

        // With no visits, epsilon should be at EPSILON_BASE (0.3)
        let eps = router.effective_epsilon(&state, "agentic");
        assert!(
            (eps - EPSILON_BASE).abs() < 0.001,
            "Initial epsilon should be {}, got {}",
            EPSILON_BASE,
            eps
        );

        // After many visits, epsilon should decay toward minimum
        for _ in 0..50 {
            router.update(&state, "agentic", &ModelTier::Simple, 0.5);
        }
        for _ in 0..50 {
            router.update(&state, "agentic", &ModelTier::Medium, 0.6);
        }

        let eps_after = router.effective_epsilon(&state, "agentic");
        assert!(
            eps_after < eps,
            "Epsilon should decay: {} < {}",
            eps_after,
            eps
        );
        assert!(
            eps_after < 0.06,
            "Epsilon should approach minimum after 100 visits: {}",
            eps_after
        );
    }

    #[test]
    fn test_greedy_selects_best() {
        let mut router = PhaseModelRouter::with_alpha(1.0);
        let state = make_state(Domain::Testing, ComplexityTier::Complex, false);

        router.update(&state, "verification", &ModelTier::Simple, 0.3);
        router.update(&state, "verification", &ModelTier::Medium, 0.5);
        router.update(&state, "verification", &ModelTier::Complex, 0.9);

        let best = router.greedy_tier(&state, "verification");
        assert_eq!(best, ModelTier::Complex);

        // Different phase, different best
        router.update(&state, "setup", &ModelTier::Simple, 0.9);
        router.update(&state, "setup", &ModelTier::Medium, 0.4);
        router.update(&state, "setup", &ModelTier::Complex, 0.5);

        let best_setup = router.greedy_tier(&state, "setup");
        assert_eq!(best_setup, ModelTier::Simple);
    }

    #[test]
    fn test_reward_computation() {
        // Basic: no cost, no cache savings
        assert_eq!(PhaseModelRouter::compute_reward(0.8, None, None), 0.8);
        assert_eq!(
            PhaseModelRouter::compute_reward(0.8, Some(0.0), None),
            0.8
        );

        // With cost penalty: 0.8 - 0.01 * 5.0 = 0.75
        let reward = PhaseModelRouter::compute_reward(0.8, Some(5.0), None);
        assert!((reward - 0.75).abs() < 0.001);

        // Clamped to 0 with very high cost
        let reward = PhaseModelRouter::compute_reward(0.1, Some(100.0), None);
        assert_eq!(reward, 0.0);

        // Clamped to 1 with very high score + cache bonus
        let reward = PhaseModelRouter::compute_reward(0.99, None, Some(100.0));
        assert_eq!(reward, 1.0);
    }

    #[test]
    fn test_cache_savings_bonus() {
        // Without cache savings
        let r1 = PhaseModelRouter::compute_reward(0.7, Some(2.0), None);
        // With cache savings: should be higher
        let r2 = PhaseModelRouter::compute_reward(0.7, Some(2.0), Some(10.0));

        // r2 = 0.7 - 0.01*2.0 + 0.005*10.0 = 0.7 - 0.02 + 0.05 = 0.73
        // r1 = 0.7 - 0.01*2.0             = 0.7 - 0.02         = 0.68
        assert!(
            r2 > r1,
            "Cache savings should increase reward: {} > {}",
            r2,
            r1
        );
        assert!((r1 - 0.68).abs() < 0.001, "r1 should be 0.68, got {}", r1);
        assert!((r2 - 0.73).abs() < 0.001, "r2 should be 0.73, got {}", r2);
    }

    #[test]
    fn test_sufficient_data_check() {
        let mut router = PhaseModelRouter::with_alpha(0.1);
        let state = make_state(Domain::Database, ComplexityTier::Moderate, false);

        assert!(!router.has_sufficient_data(&state, "generation"));

        router.update(&state, "generation", &ModelTier::Simple, 0.5);
        assert!(!router.has_sufficient_data(&state, "generation")); // 1 of 3

        router.update(&state, "generation", &ModelTier::Medium, 0.6);
        assert!(!router.has_sufficient_data(&state, "generation")); // 2 of 3

        router.update(&state, "generation", &ModelTier::Complex, 0.7);
        assert!(router.has_sufficient_data(&state, "generation")); // all 3

        // Different phase still insufficient
        assert!(!router.has_sufficient_data(&state, "setup"));
    }

    #[test]
    fn test_load_from_rows() {
        let mut router = PhaseModelRouter::new();
        router.load_from_rows(vec![
            (
                "backend:moderate:no_ui".into(),
                "verification".into(),
                "simple".into(),
                0.65,
                8,
            ),
            (
                "backend:moderate:no_ui".into(),
                "verification".into(),
                "complex".into(),
                0.82,
                12,
            ),
        ]);

        let state = make_state(Domain::Backend, ComplexityTier::Moderate, false);
        let q_simple = router.get_q(&state, "verification", &ModelTier::Simple);
        assert_eq!(q_simple.q_value, 0.65);
        assert_eq!(q_simple.visit_count, 8);

        let q_complex = router.get_q(&state, "verification", &ModelTier::Complex);
        assert_eq!(q_complex.q_value, 0.82);
        assert_eq!(q_complex.visit_count, 12);
    }

    #[test]
    fn test_table_snapshot() {
        let mut router = PhaseModelRouter::with_alpha(1.0);
        let state = make_state(Domain::Frontend, ComplexityTier::Simple, true);

        router.update(&state, "setup", &ModelTier::Simple, 0.7);
        router.update(&state, "verification", &ModelTier::Complex, 0.9);

        let snapshot = router.table_snapshot();
        assert_eq!(snapshot.len(), 2);

        for row in &snapshot {
            assert!(
                TaskState::from_key(&row.state_key).is_some(),
                "Invalid state_key: {}",
                row.state_key
            );
            assert!(
                ROUTABLE_PHASES.contains(&row.phase.as_str()),
                "Invalid phase: {}",
                row.phase
            );
            assert!(
                tier_from_str(&row.model_tier).is_some(),
                "Invalid tier: {}",
                row.model_tier
            );
        }
    }

    #[test]
    fn test_stats() {
        let mut router = PhaseModelRouter::with_alpha(0.5);

        // Empty
        let stats = router.stats();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.phases_with_data, 0);
        assert_eq!(stats.routable_phase_states, 0);

        // Add data for one fully-explored (state, phase)
        let state = make_state(Domain::Backend, ComplexityTier::Simple, false);
        router.update(&state, "verification", &ModelTier::Simple, 0.5);
        router.update(&state, "verification", &ModelTier::Medium, 0.6);
        router.update(&state, "verification", &ModelTier::Complex, 0.4);

        let stats = router.stats();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.phases_with_data, 1);
        assert_eq!(stats.routable_phase_states, 1);
        assert_eq!(stats.total_visits, 3);
        assert!(stats.avg_q_value > 0.0);

        // Add partial data for another phase — not routable
        router.update(&state, "setup", &ModelTier::Medium, 0.7);
        let stats = router.stats();
        assert_eq!(stats.total_entries, 4);
        assert_eq!(stats.phases_with_data, 2);
        assert_eq!(stats.routable_phase_states, 1); // setup still incomplete
    }

    #[test]
    fn test_cold_state_returns_simple() {
        // With no data, all Q-values are 0.0 — greedy picks first in iteration
        // order, which is Simple (cheapest tier wins ties).
        let router = PhaseModelRouter::new();
        let state = make_state(Domain::Infra, ComplexityTier::Trivial, false);
        let best = router.greedy_tier(&state, "completion");
        assert_eq!(best, ModelTier::Simple);
    }

    #[test]
    fn test_phase_independence() {
        let mut router = PhaseModelRouter::with_alpha(1.0);
        let state = make_state(Domain::Backend, ComplexityTier::Complex, false);

        // Complex is best for verification
        router.update(&state, "verification", &ModelTier::Simple, 0.3);
        router.update(&state, "verification", &ModelTier::Medium, 0.5);
        router.update(&state, "verification", &ModelTier::Complex, 0.9);

        // Simple is best for setup
        router.update(&state, "setup", &ModelTier::Simple, 0.8);
        router.update(&state, "setup", &ModelTier::Medium, 0.4);
        router.update(&state, "setup", &ModelTier::Complex, 0.3);

        assert_eq!(router.greedy_tier(&state, "verification"), ModelTier::Complex);
        assert_eq!(router.greedy_tier(&state, "setup"), ModelTier::Simple);
    }
}
