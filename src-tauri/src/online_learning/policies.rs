//! Bandit selection policies.
//!
//! Three policies for different use cases:
//! - **EpsilonGreedy** — Simple, proven (used by Q-Router). Good default.
//! - **UCB1** — Principled exploration via confidence bounds. Best for model routing.
//! - **ThompsonSampling** — Bayesian exploration via posterior sampling. Best for strategy selection.

use super::bandit::ArmStats;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

/// Trait for arm selection policies.
pub trait SelectionPolicy: Send + Sync {
    /// Select an arm given stats for all available arms.
    ///
    /// Returns `(selected_arm_id, is_exploration)`.
    fn select(&mut self, arms: &[(String, ArmStats)]) -> (String, bool);

    /// Current exploration rate (for confidence reporting).
    fn exploration_rate(&self, arms: &[(String, ArmStats)]) -> f64;

    /// Policy name for observability.
    fn name(&self) -> &'static str;
}

// =============================================================================
// Epsilon-Greedy
// =============================================================================

/// Epsilon-greedy policy with adaptive decay.
///
/// With probability ε, pick a random arm (explore).
/// Otherwise, pick the arm with highest Q-value (exploit).
/// ε decays as total visits increase: ε = max(ε_min, ε_base * exp(-visits / decay_rate))
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpsilonGreedy {
    /// Base exploration rate (before decay).
    pub epsilon_base: f64,
    /// Minimum exploration rate.
    pub epsilon_min: f64,
    /// Decay rate: higher = slower decay.
    pub decay_rate: f64,
    /// Random number generator.
    #[serde(skip)]
    rng: Option<StdRng>,
}

impl EpsilonGreedy {
    /// Create a new epsilon-greedy policy.
    pub fn new(epsilon_base: f64, epsilon_min: f64, decay_rate: f64) -> Self {
        Self {
            epsilon_base,
            epsilon_min,
            decay_rate,
            rng: Some(StdRng::from_os_rng()),
        }
    }

    /// Compute effective epsilon given total visit count.
    fn effective_epsilon(&self, total_visits: u32) -> f64 {
        let eps = self.epsilon_base * (-(total_visits as f64) / self.decay_rate).exp();
        eps.max(self.epsilon_min)
    }

    fn rng(&mut self) -> &mut StdRng {
        self.rng.get_or_insert_with(StdRng::from_os_rng)
    }
}

impl SelectionPolicy for EpsilonGreedy {
    fn select(&mut self, arms: &[(String, ArmStats)]) -> (String, bool) {
        if arms.is_empty() {
            panic!("Cannot select from empty arm set");
        }

        let total_visits: u32 = arms.iter().map(|(_, s)| s.visit_count).sum();
        let epsilon = self.effective_epsilon(total_visits);
        let rng = self.rng();

        if rng.random_bool(epsilon.min(1.0)) {
            // Explore: random arm
            let idx = rng.random_range(0..arms.len());
            (arms[idx].0.clone(), true)
        } else {
            // Exploit: argmax Q-value
            let best = arms
                .iter()
                .max_by(|(_, a), (_, b)| {
                    a.q_value.partial_cmp(&b.q_value).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            (best.0.clone(), false)
        }
    }

    fn exploration_rate(&self, arms: &[(String, ArmStats)]) -> f64 {
        let total_visits: u32 = arms.iter().map(|(_, s)| s.visit_count).sum();
        self.effective_epsilon(total_visits)
    }

    fn name(&self) -> &'static str {
        "epsilon_greedy"
    }
}

// =============================================================================
// UCB1 — Upper Confidence Bound
// =============================================================================

/// UCB1 (Upper Confidence Bound) policy.
///
/// Selects the arm that maximizes: Q(a) + c * sqrt(ln(N) / n_a)
///
/// where N = total visits across all arms, n_a = visits for arm a,
/// and c = exploration_weight (default: sqrt(2) ≈ 1.414).
///
/// This provides principled exploration that naturally favors under-tested arms
/// without requiring a random exploration parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UCB1 {
    /// Exploration weight (c). Higher = more exploration.
    /// Default: sqrt(2) ≈ 1.414 (theoretical optimal for [0,1] rewards).
    pub exploration_weight: f64,
}

impl UCB1 {
    /// Create a new UCB1 policy with the given exploration weight.
    pub fn new(exploration_weight: f64) -> Self {
        Self { exploration_weight }
    }

    /// Create with the theoretically optimal exploration weight for [0,1] rewards.
    pub fn default_optimal() -> Self {
        Self::new(std::f64::consts::SQRT_2)
    }

    /// Compute UCB score for an arm.
    fn ucb_score(&self, q_value: f64, arm_visits: u32, total_visits: u32) -> f64 {
        if arm_visits == 0 {
            return f64::MAX; // Unvisited arms get infinite UCB → guaranteed exploration
        }
        let exploration_bonus =
            self.exploration_weight * ((total_visits as f64).ln() / arm_visits as f64).sqrt();
        q_value + exploration_bonus
    }
}

impl SelectionPolicy for UCB1 {
    fn select(&mut self, arms: &[(String, ArmStats)]) -> (String, bool) {
        if arms.is_empty() {
            panic!("Cannot select from empty arm set");
        }

        let total_visits: u32 = arms.iter().map(|(_, s)| s.visit_count).sum();

        // Find the arm with the highest UCB score
        let best = arms
            .iter()
            .max_by(|(_, a), (_, b)| {
                let ucb_a = self.ucb_score(a.q_value, a.visit_count, total_visits);
                let ucb_b = self.ucb_score(b.q_value, b.visit_count, total_visits);
                ucb_a.partial_cmp(&ucb_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();

        // Determine if this is exploration: if the selected arm is NOT the greedy choice
        let greedy = arms
            .iter()
            .max_by(|(_, a), (_, b)| {
                a.q_value.partial_cmp(&b.q_value).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap();

        let is_exploration = best.0 != greedy.0;
        (best.0.clone(), is_exploration)
    }

    fn exploration_rate(&self, arms: &[(String, ArmStats)]) -> f64 {
        // UCB doesn't have a fixed exploration rate; approximate as
        // fraction of arms with visit_count below the mean
        let total: u32 = arms.iter().map(|(_, s)| s.visit_count).sum();
        if total == 0 {
            return 1.0;
        }
        let mean_visits = total as f64 / arms.len() as f64;
        let under_explored = arms
            .iter()
            .filter(|(_, s)| (s.visit_count as f64) < mean_visits * 0.5)
            .count();
        under_explored as f64 / arms.len() as f64
    }

    fn name(&self) -> &'static str {
        "ucb1"
    }
}

// =============================================================================
// Thompson Sampling — Beta-Bernoulli
// =============================================================================

/// Thompson Sampling with Beta-Bernoulli model.
///
/// Maintains alpha (successes + 1) and beta (failures + 1) parameters for each arm.
/// At selection time, draws a sample from each arm's Beta distribution and picks
/// the arm with the highest sample.
///
/// This is optimal for binary reward signals (success/failure) and provides
/// natural Bayesian exploration that accounts for uncertainty.
///
/// For continuous rewards in [0,1], Beta parameters are derived from the
/// Q-value (mean reward) and visit count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThompsonSampling {
    /// Random number generator.
    #[serde(skip)]
    rng: Option<StdRng>,
}

impl ThompsonSampling {
    /// Create a new Thompson Sampling policy.
    pub fn new() -> Self {
        Self {
            rng: Some(StdRng::from_os_rng()),
        }
    }

    fn rng(&mut self) -> &mut StdRng {
        self.rng.get_or_insert_with(StdRng::from_os_rng)
    }

    /// Draw a sample from Beta(alpha, beta) using the Jöhnk algorithm.
    /// Simple implementation suitable for the parameter ranges we'll see.
    fn sample_beta(&mut self, alpha: f64, beta: f64) -> f64 {
        if alpha <= 0.0 || beta <= 0.0 {
            return 0.5; // Fallback for degenerate cases
        }

        let rng = self.rng();

        // Use the ratio-of-uniforms method for small alpha, beta
        // For alpha=1, beta=1 (uniform prior), just return uniform
        if (alpha - 1.0).abs() < 1e-10 && (beta - 1.0).abs() < 1e-10 {
            return rng.random::<f64>();
        }

        // Gamma-based approach: X ~ Gamma(alpha, 1), Y ~ Gamma(beta, 1),
        // then X/(X+Y) ~ Beta(alpha, beta)
        let x = self.sample_gamma(alpha);
        let y = self.sample_gamma(beta);

        if x + y == 0.0 {
            0.5
        } else {
            x / (x + y)
        }
    }

    /// Simple gamma sampling using Marsaglia and Tsang's method.
    fn sample_gamma(&mut self, shape: f64) -> f64 {
        if shape < 1.0 {
            // For shape < 1, use the relation:
            // If X ~ Gamma(shape+1, 1), U ~ Uniform(0,1),
            // then X * U^(1/shape) ~ Gamma(shape, 1)
            let x = self.sample_gamma(shape + 1.0);
            let u: f64 = self.rng().random();
            return x * u.powf(1.0 / shape);
        }

        let rng = self.rng();

        // Marsaglia and Tsang's method for shape >= 1
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();

        loop {
            // Box-Muller transform for standard normal sample
            let u1: f64 = rng.random::<f64>().max(1e-10);
            let u2: f64 = rng.random::<f64>();
            let x: f64 = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            let v = 1.0 + c * x;
            if v <= 0.0 {
                continue;
            }
            let v = v * v * v;
            let u: f64 = rng.random();

            // Squeeze test
            if u < 1.0 - 0.0331 * (x * x) * (x * x) {
                return d * v;
            }
            if u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln()) {
                return d * v;
            }
        }
    }
}

impl SelectionPolicy for ThompsonSampling {
    fn select(&mut self, arms: &[(String, ArmStats)]) -> (String, bool) {
        if arms.is_empty() {
            panic!("Cannot select from empty arm set");
        }

        // For each arm, derive Beta parameters from Q-value and visit count.
        // Approximate: successes = q_value * visit_count, failures = (1 - q_value) * visit_count
        // Use prior Beta(1, 1) = uniform.
        let mut best_arm = &arms[0].0;
        let mut best_sample = f64::NEG_INFINITY;
        let mut greedy_arm = &arms[0].0;
        let mut best_q = f64::NEG_INFINITY;

        for (arm_id, stats) in arms {
            // Track greedy arm
            if stats.q_value > best_q {
                best_q = stats.q_value;
                greedy_arm = arm_id;
            }

            // Beta parameters from historical performance
            let alpha = if stats.visit_count > 0 {
                1.0 + stats.q_value.max(0.0) * stats.visit_count as f64
            } else {
                1.0 // Uniform prior
            };
            let beta = if stats.visit_count > 0 {
                1.0 + (1.0 - stats.q_value.max(0.0).min(1.0)) * stats.visit_count as f64
            } else {
                1.0 // Uniform prior
            };

            let sample = self.sample_beta(alpha, beta);
            if sample > best_sample {
                best_sample = sample;
                best_arm = arm_id;
            }
        }

        let is_exploration = best_arm != greedy_arm;
        (best_arm.clone(), is_exploration)
    }

    fn exploration_rate(&self, arms: &[(String, ArmStats)]) -> f64 {
        // Thompson Sampling doesn't have a fixed exploration rate.
        // Approximate as the probability that the greedy arm is NOT selected.
        let total_visits: u32 = arms.iter().map(|(_, s)| s.visit_count).sum();
        if total_visits == 0 {
            return 1.0;
        }
        // Very rough approximation: exploration decreases with data
        1.0 / (1.0 + (total_visits as f64 / 10.0))
    }

    fn name(&self) -> &'static str {
        "thompson_sampling"
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epsilon_greedy_always_exploit() {
        let mut policy = EpsilonGreedy::new(0.0, 0.0, 1.0); // No exploration
        let arms = vec![
            ("a".to_string(), ArmStats { q_value: 0.3, visit_count: 10, sum_of_squares: 1.0, last_updated: chrono::Utc::now() }),
            ("b".to_string(), ArmStats { q_value: 0.9, visit_count: 10, sum_of_squares: 8.0, last_updated: chrono::Utc::now() }),
        ];
        let (selected, is_explore) = policy.select(&arms);
        assert_eq!(selected, "b");
        assert!(!is_explore);
    }

    #[test]
    fn test_ucb1_favors_unvisited() {
        let mut policy = UCB1::default_optimal();
        let arms = vec![
            ("a".to_string(), ArmStats { q_value: 0.9, visit_count: 100, sum_of_squares: 80.0, last_updated: chrono::Utc::now() }),
            ("b".to_string(), ArmStats { q_value: 0.1, visit_count: 0, sum_of_squares: 0.0, last_updated: chrono::Utc::now() }),
        ];
        let (selected, _) = policy.select(&arms);
        assert_eq!(selected, "b"); // Unvisited arm gets infinite UCB
    }

    #[test]
    fn test_ucb1_converges_to_best() {
        let mut policy = UCB1::default_optimal();
        let arms = vec![
            ("a".to_string(), ArmStats { q_value: 0.9, visit_count: 100, sum_of_squares: 80.0, last_updated: chrono::Utc::now() }),
            ("b".to_string(), ArmStats { q_value: 0.3, visit_count: 100, sum_of_squares: 10.0, last_updated: chrono::Utc::now() }),
        ];
        let (selected, _) = policy.select(&arms);
        assert_eq!(selected, "a"); // With equal visits, best Q-value wins
    }

    #[test]
    fn test_thompson_sampling_explores_uncertain() {
        let mut policy = ThompsonSampling::new();
        // With very few visits, Thompson should be highly exploratory
        let arms = vec![
            ("a".to_string(), ArmStats { q_value: 0.5, visit_count: 1, sum_of_squares: 0.25, last_updated: chrono::Utc::now() }),
            ("b".to_string(), ArmStats { q_value: 0.5, visit_count: 1, sum_of_squares: 0.25, last_updated: chrono::Utc::now() }),
        ];
        // With equal stats and low data, both arms should get selected roughly equally
        let mut a_count = 0;
        for _ in 0..100 {
            let (selected, _) = policy.select(&arms);
            if selected == "a" {
                a_count += 1;
            }
        }
        // Should be roughly 50/50 (allow wide range due to randomness)
        assert!(a_count > 20 && a_count < 80, "Expected ~50/50 split, got a={}", a_count);
    }

    #[test]
    fn test_thompson_sampling_converges() {
        let mut policy = ThompsonSampling::new();
        // With lots of data, should converge to the better arm
        let arms = vec![
            ("a".to_string(), ArmStats { q_value: 0.9, visit_count: 1000, sum_of_squares: 800.0, last_updated: chrono::Utc::now() }),
            ("b".to_string(), ArmStats { q_value: 0.3, visit_count: 1000, sum_of_squares: 100.0, last_updated: chrono::Utc::now() }),
        ];
        let mut a_count = 0;
        for _ in 0..100 {
            let (selected, _) = policy.select(&arms);
            if selected == "a" {
                a_count += 1;
            }
        }
        assert!(a_count > 90, "Expected strong preference for a, got a={}", a_count);
    }
}
