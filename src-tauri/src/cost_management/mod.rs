//! Cost Management Module
//!
//! Provides token budget tracking, enforcement, and cost safety mechanisms
//! for AI-powered workflow execution.

pub mod anomaly;
pub mod budget;
pub mod circuit_breaker;

use std::sync::{Arc, Mutex};

use anomaly::CostAnomalyDetector;
use budget::{BudgetTracker, TokenBudget};
use circuit_breaker::{CostCircuitBreaker, CostCircuitBreakerConfig};

/// Per-run cost tracking components.
/// Created at task run start, destroyed on completion.
pub struct RunCostTrackers {
    pub budget: Arc<BudgetTracker>,
    pub circuit_breaker: Arc<CostCircuitBreaker>,
    pub anomaly_detector: Arc<Mutex<CostAnomalyDetector>>,
}

impl RunCostTrackers {
    /// Create a new set of cost trackers with default configuration.
    pub fn new() -> Self {
        Self::with_budget(TokenBudget::default())
    }

    /// Create with a custom budget configuration.
    pub fn with_budget(budget: TokenBudget) -> Self {
        Self {
            budget: Arc::new(BudgetTracker::new(budget)),
            circuit_breaker: Arc::new(CostCircuitBreaker::new(
                CostCircuitBreakerConfig::default(),
            )),
            anomaly_detector: Arc::new(Mutex::new(CostAnomalyDetector::new())),
        }
    }
}
