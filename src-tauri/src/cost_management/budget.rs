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
use crate::str_utils::truncate_str;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

// =============================================================================
// Configuration
// =============================================================================

/// Token budget configuration for a single task run.
///
/// **This is also the operator-facing settings section** — it is the
/// `cost_budget` key of `settings.json` (see
/// [`crate::settings::Settings::cost_budget`] and the
/// [`crate::config_facade::SettingsField`] impl). Before this was wired
/// through, `$5.00 / 500,000 tokens` was a hardcoded constant reachable only
/// via a `RunCostTrackers::with_budget` constructor that no production code
/// called — a configurable-looking seam that nothing configured. That
/// constructor is gone; the settings key replaces it.
///
/// Every field carries its own serde default reproducing the old constant
/// exactly, so a `settings.json` written before the key existed — or one that
/// sets only `max_cost_per_run_usd` — loads and behaves as before.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum total cost (USD) for a single task run.
    #[serde(default = "default_max_cost_per_run_usd")]
    pub max_cost_per_run_usd: f64,
    /// Maximum total tokens (input + output) per task run.
    #[serde(default = "default_max_tokens_per_run")]
    pub max_tokens_per_run: u64,
    /// Per-phase budgets as fraction of total (must sum to ~1.0).
    /// Keys: "setup", "verification", "agentic", "completion", "generation"
    #[serde(default = "default_phase_budgets")]
    pub phase_budgets: HashMap<String, f64>,
}

fn default_max_cost_per_run_usd() -> f64 {
    5.0
}

fn default_max_tokens_per_run() -> u64 {
    500_000
}

fn default_phase_budgets() -> HashMap<String, f64> {
    let mut phase_budgets = HashMap::new();
    phase_budgets.insert("setup".into(), 0.05);
    phase_budgets.insert("verification".into(), 0.15);
    phase_budgets.insert("agentic".into(), 0.65);
    phase_budgets.insert("completion".into(), 0.05);
    phase_budgets.insert("generation".into(), 0.10);
    phase_budgets
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_cost_per_run_usd: default_max_cost_per_run_usd(),
            max_tokens_per_run: default_max_tokens_per_run(),
            phase_budgets: default_phase_budgets(),
        }
    }
}

impl TokenBudget {
    /// Replace values that cannot express a usable budget with the built-in
    /// defaults, logging each substitution.
    ///
    /// A hand-edited `settings.json` carrying `0`, a negative number, or a
    /// NaN would otherwise make **every** run start already over budget —
    /// an operator typo that silently disables all AI work. Per-phase
    /// fractions outside `(0.0, 1.0]` are dropped individually: an unknown or
    /// nonsensical phase entry must not invalidate the whole section.
    pub fn sanitized(mut self) -> Self {
        if !self.max_cost_per_run_usd.is_finite() || self.max_cost_per_run_usd <= 0.0 {
            warn!(
                "settings.cost_budget.max_cost_per_run_usd = {} is not a usable limit — using {}",
                self.max_cost_per_run_usd,
                default_max_cost_per_run_usd()
            );
            self.max_cost_per_run_usd = default_max_cost_per_run_usd();
        }
        if self.max_tokens_per_run == 0 {
            warn!(
                "settings.cost_budget.max_tokens_per_run = 0 is not a usable limit — using {}",
                default_max_tokens_per_run()
            );
            self.max_tokens_per_run = default_max_tokens_per_run();
        }
        self.phase_budgets.retain(|phase, fraction| {
            let ok = fraction.is_finite() && *fraction > 0.0 && *fraction <= 1.0;
            if !ok {
                warn!(
                    "settings.cost_budget.phase_budgets[\"{}\"] = {} is outside \
                     (0.0, 1.0] — dropping the per-phase cap for that phase",
                    phase, fraction
                );
            }
            ok
        });
        self
    }

    /// The operator-configured budget from `settings.json`, sanitized.
    ///
    /// Read once per task run at tracker registration, so a settings change is
    /// live for the next run with no restart.
    pub fn from_settings() -> Self {
        crate::settings::load_settings().cost_budget.sanitized()
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

/// Consumption a run had **already** incurred before this tracker existed.
///
/// Seeded from the durable `phase_token_usage` ledger at tracker registration
/// (see `AppState::register_cost_trackers`). Without it, a resumed run built a
/// brand-new `BudgetTracker` with a zeroed state and therefore restarted its
/// budget at $0.00 consumed — so a crash-looping run re-executed, re-billed,
/// reset its budget, and repeated.
///
/// A fresh execution has no ledger rows and yields
/// [`PriorConsumption::default`] (all zeros) naturally, so there is no
/// "is this a resume" branch anywhere.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PriorConsumption {
    /// Total input+output tokens already billed to this execution.
    pub total_tokens: u64,
    /// Total cost (USD) already billed to this execution.
    pub total_cost_usd: f64,
    /// Per-phase `(tokens, cost_usd)` already billed, so the per-phase caps
    /// survive a resume too — not just the run total.
    pub per_phase: HashMap<String, (u64, f64)>,
}

impl PriorConsumption {
    /// Build from per-phase `(phase, tokens, cost_usd)` triples, deriving the
    /// run totals by summation so the two can never disagree.
    ///
    /// Deliberately DB-agnostic: the caller converts ledger rows (which store
    /// integer cents) into USD, keeping `cost_management` free of any database
    /// dependency and unit-testable without Postgres.
    pub fn from_phases(phases: impl IntoIterator<Item = (String, u64, f64)>) -> Self {
        let mut out = Self::default();
        for (phase, tokens, cost_usd) in phases {
            out.total_tokens += tokens;
            out.total_cost_usd += cost_usd;
            let entry = out.per_phase.entry(phase).or_default();
            entry.0 += tokens;
            entry.1 += cost_usd;
        }
        out
    }

    /// True when nothing was previously billed — i.e. this is a first attempt.
    pub fn is_empty(&self) -> bool {
        self.total_tokens == 0 && self.total_cost_usd == 0.0 && self.per_phase.is_empty()
    }
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
    /// Create a new tracker with the given budget configuration and no prior
    /// consumption.
    pub fn new(budget: TokenBudget) -> Self {
        Self::with_consumed(budget, PriorConsumption::default())
    }

    /// Create a tracker that already counts `prior` against `budget`.
    ///
    /// This is how a resumed run continues against its **remaining** budget.
    /// `BudgetState` stays private to this module — seeding goes through this
    /// explicit constructor rather than by exposing the state.
    ///
    /// The tracker is over budget from the outset when `prior` already exceeds
    /// the cap: [`BudgetTracker::is_exhausted`] reads the seeded totals, so the
    /// enforcement middleware injects its early-stop instruction on the very
    /// first AI call of the resumed attempt.
    pub fn with_consumed(budget: TokenBudget, prior: PriorConsumption) -> Self {
        Self {
            budget,
            state: Mutex::new(BudgetState {
                total_tokens: prior.total_tokens,
                total_cost_usd: prior.total_cost_usd,
                per_phase: prior.per_phase,
            }),
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
                truncate_str(prompt, 2000)
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

    // -------------------------------------------------------------------
    // Resume seeding (plan 2026-08-20-workflow-resume-reexecutes-and-rebills,
    // Phase 5). A resumed run used to build a brand-new BudgetTracker with a
    // zeroed state, restarting its cost budget at $0.00 consumed.
    // -------------------------------------------------------------------

    /// The cap used by the seeding tests: $1.00 total, with a 50% agentic
    /// sub-cap so per-phase seeding is observable too.
    fn seeding_budget() -> TokenBudget {
        let mut phase_budgets = HashMap::new();
        phase_budgets.insert("agentic".into(), 0.5);
        TokenBudget {
            max_cost_per_run_usd: 1.0,
            max_tokens_per_run: 100_000,
            phase_budgets,
        }
    }

    #[test]
    fn fresh_execution_seeds_zero_consumption() {
        // An execution with no ledger rows yields an EMPTY iterator, which is
        // exactly what a first attempt reads — hence no "is this a resume"
        // branch anywhere in the production path.
        let prior = PriorConsumption::from_phases(Vec::<(String, u64, f64)>::new());
        assert!(prior.is_empty());
        assert_eq!(prior, PriorConsumption::default());

        let tracker = BudgetTracker::with_consumed(seeding_budget(), prior);
        let snap = tracker.snapshot();
        assert_eq!(snap.total_tokens, 0);
        assert_eq!(snap.total_cost_usd, 0.0);
        assert!(snap.per_phase.is_empty());
        assert!(!tracker.is_exhausted());
        assert!((tracker.remaining_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn prior_ledger_spend_seeds_the_tracker() {
        let prior = PriorConsumption::from_phases(vec![
            ("setup".to_string(), 1_000, 0.01),
            ("agentic".to_string(), 20_000, 0.30),
        ]);
        assert_eq!(prior.total_tokens, 21_000);
        assert!((prior.total_cost_usd - 0.31).abs() < 1e-9);

        let tracker = BudgetTracker::with_consumed(seeding_budget(), prior);
        let snap = tracker.snapshot();
        assert_eq!(snap.total_tokens, 21_000);
        assert!((snap.total_cost_usd - 0.31).abs() < 1e-9);
        assert_eq!(snap.per_phase.get("agentic"), Some(&(20_000u64, 0.30f64)));
        assert!(!tracker.is_exhausted());

        // The resumed attempt continues against the REMAINING budget, not a
        // fresh $1.00: $0.31 of $1.00 is already gone.
        assert!((tracker.remaining_fraction() - 0.69).abs() < 1e-9);
    }

    #[test]
    fn prior_consumption_from_phases_folds_repeated_phases() {
        // The ledger read groups by phase, but folding is done by summation
        // here so run totals can never disagree with the per-phase rows even
        // if a caller hands over ungrouped rows.
        let prior = PriorConsumption::from_phases(vec![
            ("agentic".to_string(), 100, 0.01),
            ("agentic".to_string(), 250, 0.02),
        ]);
        assert_eq!(prior.per_phase.len(), 1);
        assert_eq!(prior.per_phase.get("agentic"), Some(&(350u64, 0.03f64)));
        assert_eq!(prior.total_tokens, 350);
        assert!((prior.total_cost_usd - 0.03).abs() < 1e-9);
    }

    #[test]
    fn resumed_run_over_cap_is_exhausted_immediately() {
        // THE DEFECT: this run already burned $1.50 of a $1.00 cap across
        // earlier attempts. It must be over budget before it makes a single
        // AI call — not handed a fresh budget to burn again.
        let prior = PriorConsumption::from_phases(vec![("agentic".to_string(), 60_000, 1.50)]);
        let tracker = BudgetTracker::with_consumed(seeding_budget(), prior);

        assert!(
            tracker.is_exhausted(),
            "a resumed run whose prior spend exceeds the cap must be exhausted at construction"
        );
        assert_eq!(tracker.remaining_fraction(), 0.0);

        // And the very first recorded call reports Exceeded rather than Ok.
        let result = tracker.record("agentic", 1, 0.0);
        assert!(
            matches!(result, BudgetResult::Exceeded { .. }),
            "expected Exceeded on the first call of an over-cap resume; got {:?}",
            result
        );
    }

    #[test]
    fn resumed_run_over_cap_gets_the_early_stop_prompt_on_its_first_call() {
        // End-to-end through the enforcement seam: an over-cap resume must be
        // told to wrap up on its very first prompt, rather than being handed a
        // fresh budget and re-billing the whole run again.
        let prior = PriorConsumption::from_phases(vec![("agentic".to_string(), 60_000, 1.50)]);
        let tracker = Arc::new(BudgetTracker::with_consumed(seeding_budget(), prior));
        let mw = BudgetEnforcementMiddleware::new(tracker);
        let ctx = MiddlewareContext::new("agentic");

        let rewritten = mw
            .pre_call("do the work", &ctx)
            .expect("an exhausted budget must rewrite the prompt");
        assert!(
            rewritten.contains("token budget for this task run is exhausted"),
            "expected the early-stop instruction; got: {}",
            rewritten
        );
    }

    #[test]
    fn seeded_per_phase_caps_survive_a_resume() {
        // Not just the run total: the per-phase sub-cap has to carry over too,
        // or a resumed run gets a fresh allowance for each phase.
        let prior = PriorConsumption::from_phases(vec![("agentic".to_string(), 30_000, 0.40)]);
        let tracker = BudgetTracker::with_consumed(seeding_budget(), prior);

        // agentic cap = 50% of $1.00 / 100_000 tokens.
        let (remaining_tokens, remaining_cost) = tracker.remaining_for_phase("agentic");
        assert_eq!(remaining_tokens, 20_000);
        assert!((remaining_cost - 0.10).abs() < 1e-9);

        // $0.11 more blows the $0.50 agentic sub-cap even though the run total
        // ($0.51) is still under $1.00.
        let result = tracker.record("agentic", 1_000, 0.11);
        match result {
            BudgetResult::Exceeded { phase, .. } => assert_eq!(phase, "agentic"),
            other => panic!("expected the agentic phase cap to trip; got {:?}", other),
        }
    }

    // -------------------------------------------------------------------
    // Settings wiring: the cap is genuinely configurable, and every default
    // reproduces the constant it replaced.
    // -------------------------------------------------------------------

    #[test]
    fn token_budget_from_empty_json_reproduces_the_old_hardcoded_cap() {
        let from_json: TokenBudget =
            serde_json::from_str("{}").expect("every TokenBudget field needs a serde default");
        assert_eq!(from_json, TokenBudget::default());
        assert_eq!(from_json.max_cost_per_run_usd, 5.0);
        assert_eq!(from_json.max_tokens_per_run, 500_000);
        assert_eq!(from_json.phase_budgets.get("agentic"), Some(&0.65));
        assert_eq!(from_json.phase_budgets.len(), 5);
    }

    #[test]
    fn settings_without_a_cost_budget_key_loads_the_old_hardcoded_cap() {
        // A settings.json written before this key existed must behave exactly
        // as it did.
        let settings: crate::settings::Settings =
            serde_json::from_str("{}").expect("Settings must load from an empty object");
        assert_eq!(settings.cost_budget, TokenBudget::default());
    }

    #[test]
    fn token_budget_partial_json_keeps_the_untouched_defaults() {
        let partial: TokenBudget = serde_json::from_str(r#"{"max_cost_per_run_usd": 25.0}"#)
            .expect("a partial cost_budget must load");
        assert_eq!(partial.max_cost_per_run_usd, 25.0);
        assert_eq!(partial.max_tokens_per_run, 500_000);
        assert_eq!(partial.phase_budgets, default_phase_budgets());
    }

    #[test]
    fn sanitized_replaces_unusable_run_limits() {
        // An operator typo of 0 would otherwise make EVERY run start already
        // over budget, silently disabling all AI work.
        let zeroed = TokenBudget {
            max_cost_per_run_usd: 0.0,
            max_tokens_per_run: 0,
            phase_budgets: HashMap::new(),
        }
        .sanitized();
        assert_eq!(zeroed.max_cost_per_run_usd, 5.0);
        assert_eq!(zeroed.max_tokens_per_run, 500_000);

        let negative = TokenBudget {
            max_cost_per_run_usd: -3.0,
            max_tokens_per_run: 10,
            phase_budgets: HashMap::new(),
        }
        .sanitized();
        assert_eq!(negative.max_cost_per_run_usd, 5.0);
        assert_eq!(
            negative.max_tokens_per_run, 10,
            "a small but usable cap is kept as-is"
        );
    }

    #[test]
    fn sanitized_drops_only_the_out_of_range_phase_fractions() {
        let mut phase_budgets = HashMap::new();
        phase_budgets.insert("agentic".to_string(), 0.65);
        phase_budgets.insert("setup".to_string(), 0.0);
        phase_budgets.insert("verification".to_string(), -0.2);
        phase_budgets.insert("completion".to_string(), 1.5);
        let sanitized = TokenBudget {
            max_cost_per_run_usd: 5.0,
            max_tokens_per_run: 500_000,
            phase_budgets,
        }
        .sanitized();

        assert_eq!(sanitized.phase_budgets.len(), 1);
        assert_eq!(sanitized.phase_budgets.get("agentic"), Some(&0.65));
    }

    #[test]
    fn a_configured_cap_actually_binds_the_tracker() {
        // The point of the settings wiring: a smaller configured cap must
        // exhaust sooner than the built-in $5.00 would.
        let configured: TokenBudget = serde_json::from_str(
            r#"{"max_cost_per_run_usd": 0.25, "max_tokens_per_run": 1000, "phase_budgets": {}}"#,
        )
        .expect("configured cost_budget must load");
        let tracker = BudgetTracker::new(configured.sanitized());

        assert!(matches!(
            tracker.record("agentic", 100, 0.30),
            BudgetResult::Exceeded { .. }
        ));
        assert!(tracker.is_exhausted());
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
