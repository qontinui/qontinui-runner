//! Generic contextual bandit framework.
//!
//! Generalizes the proven Q-Router pattern into a reusable framework
//! supporting multiple context encoders and selection policies.
//!
//! The core abstraction: given a context (encoded to a discrete state key),
//! select an arm (action) using a policy, observe a reward, and update
//! the Q-table. The framework handles:
//! - Q-table persistence (load/save as rows)
//! - Arm statistics tracking (mean reward, visit count, variance)
//! - Manual overrides per context
//! - Fallback when insufficient data

use super::context::ContextEncoder;
use super::policies::SelectionPolicy;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

// =============================================================================
// Arm statistics
// =============================================================================

/// Statistics tracked for each (context, arm) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmStats {
    /// Estimated Q-value (running mean reward).
    pub q_value: f64,
    /// Number of times this arm was selected in this context.
    pub visit_count: u32,
    /// Running sum of squared rewards (for variance / UCB).
    pub sum_of_squares: f64,
    /// Last time this arm was updated.
    pub last_updated: chrono::DateTime<Utc>,
}

impl Default for ArmStats {
    fn default() -> Self {
        Self {
            q_value: 0.0,
            visit_count: 0,
            sum_of_squares: 0.0,
            last_updated: Utc::now(),
        }
    }
}

impl ArmStats {
    /// Variance of observed rewards.
    pub fn variance(&self) -> f64 {
        if self.visit_count < 2 {
            return f64::MAX; // Maximum uncertainty
        }
        let mean_sq = self.sum_of_squares / self.visit_count as f64;
        let sq_mean = self.q_value * self.q_value;
        (mean_sq - sq_mean).max(0.0) // Clamp for numerical stability
    }
}

// =============================================================================
// Selection result
// =============================================================================

/// Result of an arm selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    /// The selected arm identifier.
    pub arm: String,
    /// The context key used for selection.
    pub context_key: String,
    /// How the arm was selected.
    pub source: SelectionSource,
    /// Confidence in this selection (1.0 = fully exploiting, 0.0 = random).
    pub confidence: f64,
    /// Whether this was an exploration (vs exploitation) decision.
    pub is_exploration: bool,
}

/// How an arm was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    /// Selected by the bandit policy (epsilon-greedy, UCB, Thompson, etc.).
    Policy,
    /// Fallback due to insufficient data.
    Fallback,
    /// Manual override for this context.
    Override,
}

// =============================================================================
// Contextual Bandit
// =============================================================================

/// Generic contextual bandit over discrete context states and discrete arms.
///
/// Type parameters:
/// - `C`: Context encoder that maps rich input to a discrete state key.
/// - `P`: Selection policy (EpsilonGreedy, UCB1, ThompsonSampling, etc.).
pub struct ContextualBandit<C: ContextEncoder, P: SelectionPolicy> {
    /// Q-table: (context_key, arm_id) → ArmStats.
    q_table: HashMap<(String, String), ArmStats>,
    /// Manual overrides: context_key → forced arm.
    overrides: HashMap<String, String>,
    /// Context encoder.
    context_encoder: C,
    /// Selection policy.
    policy: P,
    /// Learning rate for Q-value updates.
    learning_rate: f64,
    /// Minimum visits per arm in a context before the bandit activates.
    min_visits_for_activation: u32,
}

impl<C: ContextEncoder, P: SelectionPolicy> ContextualBandit<C, P> {
    /// Create a new contextual bandit.
    pub fn new(context_encoder: C, policy: P, learning_rate: f64) -> Self {
        Self {
            q_table: HashMap::new(),
            overrides: HashMap::new(),
            context_encoder,
            policy,
            learning_rate,
            min_visits_for_activation: 3,
        }
    }

    /// Set the minimum visits required per arm before activation.
    pub fn with_min_visits(mut self, min_visits: u32) -> Self {
        self.min_visits_for_activation = min_visits;
        self
    }

    /// Select an arm for the given context input.
    ///
    /// Returns `None` if `available_arms` is empty.
    pub fn select(
        &mut self,
        input: &C::Input,
        available_arms: &[String],
    ) -> Option<SelectionResult> {
        if available_arms.is_empty() {
            return None;
        }

        let context_key = self.context_encoder.encode(input);

        // Check manual override
        if let Some(forced_arm) = self.overrides.get(&context_key) {
            if available_arms.contains(forced_arm) {
                return Some(SelectionResult {
                    arm: forced_arm.clone(),
                    context_key,
                    source: SelectionSource::Override,
                    confidence: 1.0,
                    is_exploration: false,
                });
            }
        }

        // Check if we have sufficient data for the bandit to activate
        let has_sufficient_data = self.has_sufficient_data(&context_key, available_arms);
        if !has_sufficient_data {
            return Some(SelectionResult {
                arm: available_arms[0].clone(), // Fallback to first arm (caller provides default)
                context_key,
                source: SelectionSource::Fallback,
                confidence: 0.0,
                is_exploration: true,
            });
        }

        // Collect arm stats for the policy
        let arm_stats: Vec<(String, ArmStats)> = available_arms
            .iter()
            .map(|arm| {
                let stats = self.get_stats(&context_key, arm);
                (arm.clone(), stats)
            })
            .collect();

        // Let the policy decide
        let (selected_arm, is_exploration) = self.policy.select(&arm_stats);
        let selected_stats = self.get_stats(&context_key, &selected_arm);

        debug!(
            "Bandit selected arm '{}' for context '{}' (q={:.4}, visits={}, explore={})",
            selected_arm, context_key, selected_stats.q_value, selected_stats.visit_count, is_exploration
        );

        Some(SelectionResult {
            arm: selected_arm,
            context_key,
            source: SelectionSource::Policy,
            confidence: if is_exploration {
                0.0
            } else {
                1.0 - self.policy.exploration_rate(&arm_stats)
            },
            is_exploration,
        })
    }

    /// Update the Q-value after observing a reward for (context, arm).
    ///
    /// Uses incremental mean update:
    ///   Q(s,a) ← Q(s,a) + α * (reward - Q(s,a))
    ///
    /// Returns the updated ArmStats.
    pub fn update(&mut self, context_key: &str, arm: &str, reward: f64) -> ArmStats {
        let key = (context_key.to_string(), arm.to_string());
        let entry = self.q_table.entry(key).or_default();

        entry.q_value += self.learning_rate * (reward - entry.q_value);
        entry.visit_count += 1;
        entry.sum_of_squares += reward * reward;
        entry.last_updated = Utc::now();

        entry.clone()
    }

    /// Update using the context encoder to derive the key.
    pub fn update_with_input(&mut self, input: &C::Input, arm: &str, reward: f64) -> ArmStats {
        let context_key = self.context_encoder.encode(input);
        self.update(&context_key, arm, reward)
    }

    /// Get stats for a (context, arm) pair.
    pub fn get_stats(&self, context_key: &str, arm: &str) -> ArmStats {
        let key = (context_key.to_string(), arm.to_string());
        self.q_table.get(&key).cloned().unwrap_or_default()
    }

    /// Check if the bandit has enough data to activate for a given context.
    fn has_sufficient_data(&self, context_key: &str, available_arms: &[String]) -> bool {
        // Every arm must have at least 1 visit, and total visits >= min_visits_for_activation
        let total_visits: u32 = available_arms
            .iter()
            .map(|arm| self.get_stats(context_key, arm).visit_count)
            .sum();

        if total_visits < self.min_visits_for_activation {
            return false;
        }

        available_arms
            .iter()
            .all(|arm| self.get_stats(context_key, arm).visit_count >= 1)
    }

    /// Set a manual override: lock a context to a specific arm.
    pub fn set_override(&mut self, context_key: &str, arm: &str) {
        self.overrides
            .insert(context_key.to_string(), arm.to_string());
    }

    /// Remove a manual override.
    pub fn remove_override(&mut self, context_key: &str) -> bool {
        self.overrides.remove(context_key).is_some()
    }

    /// Load Q-table entries from database rows.
    pub fn load_from_rows(&mut self, rows: Vec<(String, String, f64, u32, f64)>) {
        for (context_key, arm, q_value, visit_count, sum_of_squares) in rows {
            self.q_table.insert(
                (context_key, arm),
                ArmStats {
                    q_value,
                    visit_count,
                    sum_of_squares,
                    last_updated: Utc::now(),
                },
            );
        }
    }

    /// Load overrides from database rows.
    pub fn load_overrides(&mut self, overrides: Vec<(String, String)>) {
        for (context_key, arm) in overrides {
            self.overrides.insert(context_key, arm);
        }
    }

    /// Export the Q-table as serializable rows for persistence.
    pub fn table_snapshot(&self) -> Vec<QTableRow> {
        self.q_table
            .iter()
            .map(|((ctx, arm), stats)| QTableRow {
                context_key: ctx.clone(),
                arm: arm.clone(),
                q_value: stats.q_value,
                visit_count: stats.visit_count,
                sum_of_squares: stats.sum_of_squares,
            })
            .collect()
    }

    /// Get the greedy (best) arm for a context.
    pub fn greedy_arm(&self, context_key: &str, available_arms: &[String]) -> Option<String> {
        available_arms
            .iter()
            .max_by(|a, b| {
                let qa = self.get_stats(context_key, a).q_value;
                let qb = self.get_stats(context_key, b).q_value;
                qa.partial_cmp(&qb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
    }

    /// Get the greedy policy: best arm per context.
    pub fn policy_snapshot(&self, available_arms: &[String]) -> Vec<PolicyEntry> {
        let mut context_keys: Vec<String> = self
            .q_table
            .keys()
            .map(|(ctx, _)| ctx.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        context_keys.sort();

        context_keys
            .into_iter()
            .filter_map(|ctx| {
                let best = self.greedy_arm(&ctx, available_arms)?;
                let stats = self.get_stats(&ctx, &best);
                let total_visits: u32 = available_arms
                    .iter()
                    .map(|arm| self.get_stats(&ctx, arm).visit_count)
                    .sum();
                Some(PolicyEntry {
                    context_key: ctx,
                    recommended_arm: best,
                    q_value: stats.q_value,
                    total_visits,
                })
            })
            .collect()
    }

    /// Total number of (context, arm) entries in the Q-table.
    pub fn table_size(&self) -> usize {
        self.q_table.len()
    }

    /// Get a reference to the context encoder.
    pub fn encoder(&self) -> &C {
        &self.context_encoder
    }

    /// Increase exploration for a specific context (used by drift detection).
    ///
    /// Halves the visit counts for all arms in this context, which:
    /// - For UCB1: increases the confidence bound (more exploration)
    /// - For EpsilonGreedy: increases epsilon (via visit-based decay)
    /// - For ThompsonSampling: widens the posterior (more uncertainty)
    pub fn boost_exploration(&mut self, context_key: &str) {
        let keys_to_update: Vec<(String, String)> = self
            .q_table
            .keys()
            .filter(|(ctx, _)| ctx == context_key)
            .cloned()
            .collect();

        for key in keys_to_update {
            if let Some(entry) = self.q_table.get_mut(&key) {
                entry.visit_count = entry.visit_count / 2;
                entry.sum_of_squares /= 2.0;
            }
        }
    }
}

// =============================================================================
// Serializable rows
// =============================================================================

/// A single Q-table row for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QTableRow {
    pub context_key: String,
    pub arm: String,
    pub q_value: f64,
    pub visit_count: u32,
    pub sum_of_squares: f64,
}

/// A single policy entry for diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEntry {
    pub context_key: String,
    pub recommended_arm: String,
    pub q_value: f64,
    pub total_visits: u32,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online_learning::context::IdentityEncoder;
    use crate::online_learning::policies::EpsilonGreedy;

    #[test]
    fn test_bandit_fallback_on_insufficient_data() {
        let bandit_policy = EpsilonGreedy::new(0.3, 0.05, 20.0);
        let mut bandit = ContextualBandit::new(IdentityEncoder, bandit_policy, 0.1);

        let arms = vec!["model_a".to_string(), "model_b".to_string()];
        let result = bandit.select(&"test_context".to_string(), &arms).unwrap();

        assert_eq!(result.source, SelectionSource::Fallback);
        assert_eq!(result.arm, "model_a"); // First arm as fallback
    }

    #[test]
    fn test_bandit_override() {
        let policy = EpsilonGreedy::new(0.3, 0.05, 20.0);
        let mut bandit = ContextualBandit::new(IdentityEncoder, policy, 0.1);
        bandit.set_override("ctx", "model_b");

        let arms = vec!["model_a".to_string(), "model_b".to_string()];
        let result = bandit.select(&"ctx".to_string(), &arms).unwrap();

        assert_eq!(result.source, SelectionSource::Override);
        assert_eq!(result.arm, "model_b");
    }

    #[test]
    fn test_bandit_update_and_exploitation() {
        let policy = EpsilonGreedy::new(0.0, 0.0, 1.0); // No exploration
        let mut bandit = ContextualBandit::new(IdentityEncoder, policy, 0.1)
            .with_min_visits(2);

        let arms = vec!["arm_a".to_string(), "arm_b".to_string()];

        // Seed initial visits
        bandit.update("ctx", "arm_a", 0.3);
        bandit.update("ctx", "arm_b", 0.9);

        // With no exploration, should always pick arm_b (higher Q-value)
        let result = bandit.select(&"ctx".to_string(), &arms).unwrap();
        assert_eq!(result.arm, "arm_b");
        assert_eq!(result.source, SelectionSource::Policy);
    }

    #[test]
    fn test_bandit_table_snapshot() {
        let policy = EpsilonGreedy::new(0.3, 0.05, 20.0);
        let mut bandit = ContextualBandit::new(IdentityEncoder, policy, 0.1);

        bandit.update("ctx1", "arm_a", 0.5);
        bandit.update("ctx1", "arm_b", 0.8);
        bandit.update("ctx2", "arm_a", 0.6);

        let snapshot = bandit.table_snapshot();
        assert_eq!(snapshot.len(), 3);
    }

    #[test]
    fn test_bandit_load_from_rows() {
        let policy = EpsilonGreedy::new(0.0, 0.0, 1.0);
        let mut bandit = ContextualBandit::new(IdentityEncoder, policy, 0.1)
            .with_min_visits(1);

        bandit.load_from_rows(vec![
            ("ctx".to_string(), "arm_a".to_string(), 0.3, 5, 0.5),
            ("ctx".to_string(), "arm_b".to_string(), 0.9, 5, 4.0),
        ]);

        let arms = vec!["arm_a".to_string(), "arm_b".to_string()];
        let result = bandit.select(&"ctx".to_string(), &arms).unwrap();
        assert_eq!(result.arm, "arm_b");
    }
}
