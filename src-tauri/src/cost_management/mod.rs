//! Cost Management Module
//!
//! Provides token budget tracking, enforcement, and cost safety mechanisms
//! for AI-powered workflow execution.

pub mod anomaly;
pub mod budget;
pub mod circuit_breaker;

use std::sync::{Arc, Mutex};

use anomaly::CostAnomalyDetector;
use budget::{BudgetTracker, PriorConsumption, TokenBudget};
use circuit_breaker::{CostCircuitBreaker, CostCircuitBreakerConfig};

/// Per-run cost tracking components.
/// Created at task run start, destroyed on completion.
pub struct RunCostTrackers {
    pub budget: Arc<BudgetTracker>,
    pub circuit_breaker: Arc<CostCircuitBreaker>,
    pub anomaly_detector: Arc<Mutex<CostAnomalyDetector>>,
}

impl RunCostTrackers {
    /// Create with an explicit budget and consumption already billed to this
    /// execution.
    ///
    /// `prior` comes from the durable `phase_token_usage` ledger, so a resumed
    /// run continues against its remaining budget instead of restarting at
    /// $0.00 consumed. A fresh execution passes
    /// [`PriorConsumption::default`] — which is what an empty ledger read
    /// yields — so there is no resume-specific branch.
    ///
    /// Only the budget tracker is seeded. The circuit breaker and anomaly
    /// detector are deliberately fresh on each attempt: both reason about the
    /// *rate* and *shape* of spend within one attempt (consecutive expensive
    /// calls, per-call outliers), not a cumulative total, so replaying a prior
    /// attempt's call sequence into them would trip them on history rather
    /// than on the current attempt's behaviour.
    pub fn seeded(budget: TokenBudget, prior: PriorConsumption) -> Self {
        Self {
            budget: Arc::new(BudgetTracker::with_consumed(budget, prior)),
            circuit_breaker: Arc::new(CostCircuitBreaker::new(CostCircuitBreakerConfig::default())),
            anomaly_detector: Arc::new(Mutex::new(CostAnomalyDetector::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_dollar_budget() -> TokenBudget {
        TokenBudget {
            max_cost_per_run_usd: 1.0,
            max_tokens_per_run: 100_000,
            phase_budgets: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn a_fresh_run_starts_at_zero_consumed() {
        let trackers = RunCostTrackers::seeded(one_dollar_budget(), PriorConsumption::default());
        let snap = trackers.budget.snapshot();
        assert_eq!(snap.total_tokens, 0);
        assert_eq!(snap.total_cost_usd, 0.0);
        assert_eq!(snap.max_cost_usd, 1.0);
        assert!(!trackers.budget.is_exhausted());
    }

    #[test]
    fn seeded_trackers_carry_prior_spend_into_the_budget() {
        let prior = PriorConsumption::from_phases(vec![("agentic".to_string(), 40_000, 0.60)]);
        let trackers = RunCostTrackers::seeded(one_dollar_budget(), prior);

        let snap = trackers.budget.snapshot();
        assert_eq!(snap.total_tokens, 40_000);
        assert!((snap.total_cost_usd - 0.60).abs() < 1e-9);
        assert!(
            !trackers.budget.is_exhausted(),
            "$0.60 of $1.00 is not exhausted"
        );
    }

    #[test]
    fn seeded_trackers_are_over_budget_when_prior_spend_exceeds_the_cap() {
        // The whole point of Phase 5: a crash-looping run must not get a fresh
        // budget on every resume.
        let prior = PriorConsumption::from_phases(vec![("agentic".to_string(), 10_000, 4.20)]);
        let trackers = RunCostTrackers::seeded(one_dollar_budget(), prior);
        assert!(trackers.budget.is_exhausted());
        assert_eq!(trackers.budget.remaining_fraction(), 0.0);
    }

    #[test]
    fn seeding_leaves_the_circuit_breaker_and_anomaly_detector_fresh() {
        // Deliberate: both reason about the rate/shape of spend WITHIN one
        // attempt, so replaying prior history into them would trip them on the
        // past rather than on this attempt's behaviour.
        let prior = PriorConsumption::from_phases(vec![("agentic".to_string(), 10_000, 4.20)]);
        let trackers = RunCostTrackers::seeded(one_dollar_budget(), prior);
        assert!(!trackers.circuit_breaker.is_tripped());
        assert_eq!(
            trackers
                .anomaly_detector
                .lock()
                .expect("anomaly detector mutex")
                .sample_count(),
            0
        );
    }
}
