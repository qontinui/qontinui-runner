//! Token Budget Management
//!
//! Tracks and enforces per-run and per-phase token budgets to prevent
//! runaway costs during autonomous workflow execution.
//!
//! # Architecture
//!
//! - `TokenBudget`: Configuration specifying limits.
//! - `BudgetTracker`: Thread-safe runtime tracker that records consumption
//!   and returns `BudgetResult` after each AI call.
//! - `BudgetEnforcementMiddleware`: Implements `AiMiddleware` to inject
//!   conciseness hints at 80% and early-stop instructions at 100%.

use crate::ai_provider::middleware::{AiMiddleware, MiddlewareContext};
use crate::ai_provider::AiResponse;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

// =============================================================================
// Configuration
// =============================================================================

/// Token budget configuration for a single task run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum total cost (USD) for a single task run.
    pub max_cost_per_run_usd: f64,
    /// Maximum total tokens (input + output) per task run.
    pub max_tokens_per_run: u64,
    /// Per-phase budgets as fraction of total (must sum to ~1.0).
    /// Keys: "setup", "verification", "agentic", "completion", "generation"
    pub phase_budgets: HashMap<String, f64>,
}

impl Default for TokenBudget {
    fn default() -> Self {
        let mut phase_budgets = HashMap::new();
        phase_budgets.insert("setup".into(), 0.05);
        phase_budgets.insert("verification".into(), 0.15);
        phase_budgets.insert("agentic".into(), 0.65);
        phase_budgets.insert("completion".into(), 0.05);
        phase_budgets.insert("generation".into(), 0.10);

        Self {
            max_cost_per_run_usd: 5.0,
            max_tokens_per_run: 500_000,
            phase_budgets,
        }
    }
}

// =============================================================================
// Budget State
// =============================================================================

/// Internal tracking state (behind Mutex).
#[derive(Debug, Default)]
struct BudgetState {
    total_tokens: u64,
    total_cost_usd: f64,
    per_phase: HashMap<String, (u64, f64)>, // (tokens, cost)
}

/// Result of recording consumption against a budget.
#[derive(Debug, Clone)]
pub enum BudgetResult {
    /// Within budget. `remaining_fraction` is 0.0-1.0 of total remaining.
    Ok { remaining_fraction: f64 },
    /// Approaching limit (>80% consumed).
    Warning {
        remaining_fraction: f64,
        message: String,
    },
    /// Budget exhausted.
    Exceeded { phase: String, overage_usd: f64 },
}

// =============================================================================
// Budget Tracker
// =============================================================================

/// Thread-safe token budget tracker for a single task run.
///
/// Create one at task start, pass an `Arc<BudgetTracker>` to the middleware
/// chain and the orchestrator loop.
pub struct BudgetTracker {
    budget: TokenBudget,
    state: Mutex<BudgetState>,
}

impl BudgetTracker {
    /// Create a new tracker with the given budget configuration.
    pub fn new(budget: TokenBudget) -> Self {
        Self {
            budget,
            state: Mutex::new(BudgetState::default()),
        }
    }

    /// Create with default budget.
    pub fn with_defaults() -> Self {
        Self::new(TokenBudget::default())
    }

    /// Record token consumption after an AI call.
    pub fn record(&self, phase: &str, tokens: u64, cost_usd: f64) -> BudgetResult {
        let mut state = self.state.lock().unwrap();
        state.total_tokens += tokens;
        state.total_cost_usd += cost_usd;

        let entry = state.per_phase.entry(phase.to_string()).or_default();
        entry.0 += tokens;
        entry.1 += cost_usd;

        // Check phase budget
        if let Some(&phase_fraction) = self.budget.phase_budgets.get(phase) {
            let phase_limit_usd = self.budget.max_cost_per_run_usd * phase_fraction;
            if entry.1 > phase_limit_usd {
                return BudgetResult::Exceeded {
                    phase: phase.to_string(),
                    overage_usd: entry.1 - phase_limit_usd,
                };
            }
        }

        // Check total budget
        if state.total_cost_usd > self.budget.max_cost_per_run_usd {
            return BudgetResult::Exceeded {
                phase: "total".to_string(),
                overage_usd: state.total_cost_usd - self.budget.max_cost_per_run_usd,
            };
        }

        if state.total_tokens > self.budget.max_tokens_per_run {
            return BudgetResult::Exceeded {
                phase: "total".to_string(),
                overage_usd: 0.0,
            };
        }

        // Calculate remaining fraction
        let cost_remaining =
            1.0 - (state.total_cost_usd / self.budget.max_cost_per_run_usd).min(1.0);
        let token_remaining =
            1.0 - (state.total_tokens as f64 / self.budget.max_tokens_per_run as f64).min(1.0);
        let remaining_fraction = cost_remaining.min(token_remaining);

        if remaining_fraction < 0.2 {
            BudgetResult::Warning {
                remaining_fraction,
                message: format!(
                    "Budget {:.0}% consumed (${:.3}/{:.2} USD, {}/{} tokens)",
                    (1.0 - remaining_fraction) * 100.0,
                    state.total_cost_usd,
                    self.budget.max_cost_per_run_usd,
                    state.total_tokens,
                    self.budget.max_tokens_per_run,
                ),
            }
        } else {
            BudgetResult::Ok { remaining_fraction }
        }
    }

    /// Check if the overall budget is exhausted.
    pub fn is_exhausted(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.total_cost_usd > self.budget.max_cost_per_run_usd
            || state.total_tokens > self.budget.max_tokens_per_run
    }

    /// Get remaining budget as a fraction (0.0 = exhausted, 1.0 = full).
    pub fn remaining_fraction(&self) -> f64 {
        let state = self.state.lock().unwrap();
        let cost_remaining =
            1.0 - (state.total_cost_usd / self.budget.max_cost_per_run_usd).min(1.0);
        let token_remaining =
            1.0 - (state.total_tokens as f64 / self.budget.max_tokens_per_run as f64).min(1.0);
        cost_remaining.min(token_remaining).max(0.0)
    }

    /// Get remaining budget for a specific phase.
    pub fn remaining_for_phase(&self, phase: &str) -> (u64, f64) {
        let state = self.state.lock().unwrap();
        let consumed = state.per_phase.get(phase).copied().unwrap_or((0, 0.0));

        let phase_fraction = self.budget.phase_budgets.get(phase).copied().unwrap_or(1.0);

        let phase_token_limit = (self.budget.max_tokens_per_run as f64 * phase_fraction) as u64;
        let phase_cost_limit = self.budget.max_cost_per_run_usd * phase_fraction;

        let remaining_tokens = phase_token_limit.saturating_sub(consumed.0);
        let remaining_cost = (phase_cost_limit - consumed.1).max(0.0);

        (remaining_tokens, remaining_cost)
    }

    /// Get a snapshot of current consumption.
    pub fn snapshot(&self) -> BudgetSnapshot {
        let state = self.state.lock().unwrap();
        BudgetSnapshot {
            total_tokens: state.total_tokens,
            total_cost_usd: state.total_cost_usd,
            max_tokens: self.budget.max_tokens_per_run,
            max_cost_usd: self.budget.max_cost_per_run_usd,
            per_phase: state.per_phase.clone(),
        }
    }
}

/// A point-in-time snapshot of budget consumption.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetSnapshot {
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub max_tokens: u64,
    pub max_cost_usd: f64,
    pub per_phase: HashMap<String, (u64, f64)>,
}

// =============================================================================
// Budget Enforcement Middleware
// =============================================================================

/// Middleware that enforces token budgets by modifying prompts.
///
/// - At >80% consumed: Injects a conciseness hint into the prompt.
/// - At 100% consumed: Replaces the prompt with a "summarize and finish" instruction.
pub struct BudgetEnforcementMiddleware {
    tracker: Arc<BudgetTracker>,
}

impl BudgetEnforcementMiddleware {
    pub fn new(tracker: Arc<BudgetTracker>) -> Self {
        Self { tracker }
    }
}

impl AiMiddleware for BudgetEnforcementMiddleware {
    fn name(&self) -> &'static str {
        "budget_enforcement"
    }

    fn pre_call(&self, prompt: &str, ctx: &MiddlewareContext) -> Option<String> {
        if self.tracker.is_exhausted() {
            warn!(
                "Budget exhausted in phase '{}' — injecting early-stop instruction",
                ctx.phase
            );
            return Some(format!(
                "IMPORTANT: The token budget for this task run is exhausted. \
                 Summarize your progress so far and produce your final output immediately. \
                 Do not start new work.\n\n{}",
                &prompt[..prompt.len().min(2000)]
            ));
        }

        let remaining = self.tracker.remaining_fraction();
        if remaining < 0.2 {
            debug!(
                "Budget low ({:.0}% remaining) — injecting conciseness hint",
                remaining * 100.0
            );
            return Some(format!(
                "NOTE: Token budget is running low ({:.0}% remaining). \
                 Be concise and focused. Prioritize completing the most critical work.\n\n{}",
                remaining * 100.0,
                prompt
            ));
        }

        None
    }

    fn post_call(&self, response: &mut AiResponse, ctx: &MiddlewareContext) {
        // Record actual consumption from the response
        if let (Some(input), Some(output)) = (response.input_tokens, response.output_tokens) {
            let total_tokens = input + output;
            // Estimate cost (simple approximation — callers can provide exact cost)
            let estimated_cost = total_tokens as f64 * 0.000003; // ~$3/M tokens average

            let result = self
                .tracker
                .record(&ctx.phase, total_tokens, estimated_cost);
            match result {
                BudgetResult::Warning { message, .. } => {
                    info!("Budget warning: {}", message);
                }
                BudgetResult::Exceeded { phase, overage_usd } => {
                    warn!(
                        "Budget exceeded in phase '{}' by ${:.4}",
                        phase, overage_usd
                    );
                }
                _ => {}
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_tracker_basic() {
        let budget = TokenBudget {
            max_cost_per_run_usd: 1.0,
            max_tokens_per_run: 100_000,
            phase_budgets: HashMap::new(),
        };
        let tracker = BudgetTracker::new(budget);

        let result = tracker.record("agentic", 10_000, 0.1);
        assert!(matches!(result, BudgetResult::Ok { .. }));
        assert!(!tracker.is_exhausted());
    }

    #[test]
    fn test_budget_tracker_exhaustion() {
        let budget = TokenBudget {
            max_cost_per_run_usd: 0.5,
            max_tokens_per_run: 100_000,
            phase_budgets: HashMap::new(),
        };
        let tracker = BudgetTracker::new(budget);

        // First call — under budget
        let result = tracker.record("agentic", 50_000, 0.3);
        assert!(!tracker.is_exhausted());

        // Second call — exceeds cost
        let result = tracker.record("agentic", 30_000, 0.3);
        assert!(matches!(result, BudgetResult::Exceeded { .. }));
        assert!(tracker.is_exhausted());
    }

    #[test]
    fn test_budget_tracker_warning_at_80pct() {
        let budget = TokenBudget {
            max_cost_per_run_usd: 1.0,
            max_tokens_per_run: 100_000,
            phase_budgets: HashMap::new(),
        };
        let tracker = BudgetTracker::new(budget);

        // Consume 85% of cost budget
        let result = tracker.record("agentic", 50_000, 0.85);
        assert!(matches!(result, BudgetResult::Warning { .. }));
    }

    #[test]
    fn test_budget_tracker_phase_limit() {
        let mut phase_budgets = HashMap::new();
        phase_budgets.insert("setup".into(), 0.05); // 5% of total = $0.05

        let budget = TokenBudget {
            max_cost_per_run_usd: 1.0,
            max_tokens_per_run: 100_000,
            phase_budgets,
        };
        let tracker = BudgetTracker::new(budget);

        // Exceed setup phase limit
        let result = tracker.record("setup", 10_000, 0.06);
        assert!(matches!(result, BudgetResult::Exceeded { .. }));
        if let BudgetResult::Exceeded { phase, .. } = result {
            assert_eq!(phase, "setup");
        }
    }

    #[test]
    fn test_budget_snapshot() {
        let tracker = BudgetTracker::with_defaults();
        tracker.record("agentic", 5000, 0.05);
        tracker.record("verification", 2000, 0.02);

        let snap = tracker.snapshot();
        assert_eq!(snap.total_tokens, 7000);
        assert!((snap.total_cost_usd - 0.07).abs() < 0.001);
        assert!(snap.per_phase.contains_key("agentic"));
        assert!(snap.per_phase.contains_key("verification"));
    }

    #[test]
    fn test_remaining_for_phase() {
        let mut phase_budgets = HashMap::new();
        phase_budgets.insert("agentic".into(), 0.65);

        let budget = TokenBudget {
            max_cost_per_run_usd: 10.0,
            max_tokens_per_run: 1_000_000,
            phase_budgets,
        };
        let tracker = BudgetTracker::new(budget);

        tracker.record("agentic", 100_000, 1.0);
        let (remaining_tokens, remaining_cost) = tracker.remaining_for_phase("agentic");
        assert_eq!(remaining_tokens, 550_000); // 650K - 100K
        assert!((remaining_cost - 5.5).abs() < 0.01); // $6.50 - $1.00
    }
}
