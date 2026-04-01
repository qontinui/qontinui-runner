//! Cost Circuit Breaker
//!
//! Halts execution when cost anomalies are detected:
//! - Single AI call costs > configurable threshold
//! - Cumulative run cost exceeds budget by > 50%
//! - Cache hit rate drops below 10% for N consecutive calls

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Result of a circuit breaker check.
#[derive(Debug, Clone, PartialEq)]
pub enum CircuitBreakerResult {
    /// Cost is within acceptable limits.
    Ok,
    /// Cost is elevated but not yet tripped — caller should log/alert.
    Warning(String),
    /// Circuit breaker has tripped — execution should halt.
    Tripped(String),
}

/// Configuration thresholds for the cost circuit breaker.
#[derive(Debug, Clone)]
pub struct CostCircuitBreakerConfig {
    /// Maximum cost (USD) for a single AI call before tripping.
    pub max_single_call_usd: f64,
    /// Maximum budget overage as a fraction (0.5 = 50% over budget).
    pub budget_overage_pct: f64,
    /// Minimum acceptable cache hit rate (0.1 = 10%).
    pub min_cache_hit_rate: f64,
    /// Number of consecutive cache misses before tripping.
    pub consecutive_miss_limit: u32,
}

impl Default for CostCircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_single_call_usd: 2.0,
            budget_overage_pct: 0.5,
            min_cache_hit_rate: 0.1,
            consecutive_miss_limit: 5,
        }
    }
}

/// Thread-safe cost circuit breaker.
///
/// Monitors AI call costs and cache efficiency in real time. When anomalous
/// conditions are detected, the breaker "trips" and signals callers to halt
/// execution.
pub struct CostCircuitBreaker {
    config: CostCircuitBreakerConfig,
    consecutive_cache_misses: AtomicU32,
    tripped: AtomicBool,
}

impl CostCircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CostCircuitBreakerConfig) -> Self {
        Self {
            config,
            consecutive_cache_misses: AtomicU32::new(0),
            tripped: AtomicBool::new(false),
        }
    }

    /// Check whether a single AI call cost exceeds the threshold.
    ///
    /// Returns `Tripped` if the cost exceeds `max_single_call_usd`,
    /// `Warning` if it exceeds 80% of the threshold, or `Ok` otherwise.
    pub fn check_single_call(&self, cost_usd: f64) -> CircuitBreakerResult {
        if self.tripped.load(Ordering::Relaxed) {
            return CircuitBreakerResult::Tripped(
                "Circuit breaker already tripped".to_string(),
            );
        }

        if cost_usd > self.config.max_single_call_usd {
            self.tripped.store(true, Ordering::Relaxed);
            return CircuitBreakerResult::Tripped(format!(
                "Single AI call cost ${:.4} exceeds maximum ${:.2}",
                cost_usd, self.config.max_single_call_usd,
            ));
        }

        let warning_threshold = self.config.max_single_call_usd * 0.8;
        if cost_usd > warning_threshold {
            return CircuitBreakerResult::Warning(format!(
                "Single AI call cost ${:.4} approaching maximum ${:.2}",
                cost_usd, self.config.max_single_call_usd,
            ));
        }

        CircuitBreakerResult::Ok
    }

    /// Check whether cumulative run cost exceeds the budget by more than
    /// the allowed overage percentage.
    ///
    /// Returns `Tripped` if `current_cost > budget_limit * (1 + budget_overage_pct)`.
    pub fn check_budget(&self, current_cost: f64, budget_limit: f64) -> CircuitBreakerResult {
        if self.tripped.load(Ordering::Relaxed) {
            return CircuitBreakerResult::Tripped(
                "Circuit breaker already tripped".to_string(),
            );
        }

        if budget_limit <= 0.0 {
            return CircuitBreakerResult::Ok;
        }

        let max_allowed = budget_limit * (1.0 + self.config.budget_overage_pct);
        if current_cost > max_allowed {
            self.tripped.store(true, Ordering::Relaxed);
            return CircuitBreakerResult::Tripped(format!(
                "Run cost ${:.4} exceeds budget ${:.2} by more than {:.0}%",
                current_cost,
                budget_limit,
                self.config.budget_overage_pct * 100.0,
            ));
        }

        if current_cost > budget_limit {
            return CircuitBreakerResult::Warning(format!(
                "Run cost ${:.4} has exceeded budget ${:.2}",
                current_cost, budget_limit,
            ));
        }

        CircuitBreakerResult::Ok
    }

    /// Record cache metrics for a single AI call and check cache health.
    ///
    /// If `cache_read_tokens == 0` while `cache_creation_tokens > 0`, this
    /// counts as a cache miss. After `consecutive_miss_limit` consecutive
    /// misses, the breaker trips.
    pub fn record_cache_metrics(
        &self,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
    ) -> CircuitBreakerResult {
        if self.tripped.load(Ordering::Relaxed) {
            return CircuitBreakerResult::Tripped(
                "Circuit breaker already tripped".to_string(),
            );
        }

        let total = cache_creation_tokens + cache_read_tokens;
        if total == 0 {
            // No cache activity — not a miss, just no caching involved.
            return CircuitBreakerResult::Ok;
        }

        let hit_rate = cache_read_tokens as f64 / total as f64;

        if hit_rate < self.config.min_cache_hit_rate {
            let misses = self.consecutive_cache_misses.fetch_add(1, Ordering::Relaxed) + 1;
            if misses >= self.config.consecutive_miss_limit {
                self.tripped.store(true, Ordering::Relaxed);
                return CircuitBreakerResult::Tripped(format!(
                    "Cache hit rate {:.1}% below {:.0}% for {} consecutive calls",
                    hit_rate * 100.0,
                    self.config.min_cache_hit_rate * 100.0,
                    misses,
                ));
            }
            return CircuitBreakerResult::Warning(format!(
                "Low cache hit rate {:.1}% ({} consecutive misses)",
                hit_rate * 100.0,
                misses,
            ));
        }

        // Good cache hit — reset consecutive miss counter
        self.consecutive_cache_misses.store(0, Ordering::Relaxed);
        CircuitBreakerResult::Ok
    }

    /// Check whether the circuit breaker is currently tripped.
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Relaxed)
    }

    /// Reset the circuit breaker, clearing the tripped state and miss counter.
    pub fn reset(&self) {
        self.tripped.store(false, Ordering::Relaxed);
        self.consecutive_cache_misses.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_breaker() -> CostCircuitBreaker {
        CostCircuitBreaker::new(CostCircuitBreakerConfig::default())
    }

    #[test]
    fn test_single_call_ok() {
        let cb = default_breaker();
        assert_eq!(cb.check_single_call(0.50), CircuitBreakerResult::Ok);
        assert!(!cb.is_tripped());
    }

    #[test]
    fn test_single_call_warning() {
        let cb = default_breaker();
        // 80% of 2.0 = 1.6
        let result = cb.check_single_call(1.70);
        assert!(matches!(result, CircuitBreakerResult::Warning(_)));
        assert!(!cb.is_tripped());
    }

    #[test]
    fn test_single_call_tripped() {
        let cb = default_breaker();
        let result = cb.check_single_call(2.50);
        assert!(matches!(result, CircuitBreakerResult::Tripped(_)));
        assert!(cb.is_tripped());
    }

    #[test]
    fn test_budget_ok() {
        let cb = default_breaker();
        assert_eq!(cb.check_budget(3.0, 5.0), CircuitBreakerResult::Ok);
    }

    #[test]
    fn test_budget_warning() {
        let cb = default_breaker();
        // Over budget but within 50% overage
        let result = cb.check_budget(5.50, 5.0);
        assert!(matches!(result, CircuitBreakerResult::Warning(_)));
    }

    #[test]
    fn test_budget_tripped() {
        let cb = default_breaker();
        // 5.0 * 1.5 = 7.5 — exceeding this trips
        let result = cb.check_budget(8.0, 5.0);
        assert!(matches!(result, CircuitBreakerResult::Tripped(_)));
        assert!(cb.is_tripped());
    }

    #[test]
    fn test_budget_zero_limit_always_ok() {
        let cb = default_breaker();
        assert_eq!(cb.check_budget(100.0, 0.0), CircuitBreakerResult::Ok);
    }

    #[test]
    fn test_cache_good_hit_rate() {
        let cb = default_breaker();
        let result = cb.record_cache_metrics(100, 900); // 90% hit rate
        assert_eq!(result, CircuitBreakerResult::Ok);
    }

    #[test]
    fn test_cache_low_hit_rate_warning() {
        let cb = default_breaker();
        let result = cb.record_cache_metrics(950, 50); // ~5% hit rate
        assert!(matches!(result, CircuitBreakerResult::Warning(_)));
    }

    #[test]
    fn test_cache_consecutive_misses_trip() {
        let cb = default_breaker();
        // 5 consecutive low-cache calls should trip (consecutive_miss_limit = 5)
        for i in 0..4 {
            let result = cb.record_cache_metrics(1000, 0); // 0% hit rate
            assert!(
                matches!(result, CircuitBreakerResult::Warning(_)),
                "iteration {} should be warning",
                i
            );
        }
        let result = cb.record_cache_metrics(1000, 0);
        assert!(matches!(result, CircuitBreakerResult::Tripped(_)));
        assert!(cb.is_tripped());
    }

    #[test]
    fn test_cache_reset_on_good_hit() {
        let cb = default_breaker();
        // Build up some misses
        for _ in 0..3 {
            cb.record_cache_metrics(1000, 0);
        }
        // Good hit resets the counter
        cb.record_cache_metrics(100, 900);
        assert_eq!(cb.consecutive_cache_misses.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_no_cache_activity_is_ok() {
        let cb = default_breaker();
        assert_eq!(cb.record_cache_metrics(0, 0), CircuitBreakerResult::Ok);
    }

    #[test]
    fn test_reset() {
        let cb = default_breaker();
        cb.check_single_call(3.0); // trip it
        assert!(cb.is_tripped());
        cb.reset();
        assert!(!cb.is_tripped());
        assert_eq!(cb.consecutive_cache_misses.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_already_tripped_returns_tripped() {
        let cb = default_breaker();
        cb.check_single_call(3.0); // trip it
        // All subsequent checks return Tripped
        assert!(matches!(
            cb.check_single_call(0.01),
            CircuitBreakerResult::Tripped(_)
        ));
        assert!(matches!(
            cb.check_budget(0.01, 100.0),
            CircuitBreakerResult::Tripped(_)
        ));
        assert!(matches!(
            cb.record_cache_metrics(0, 1000),
            CircuitBreakerResult::Tripped(_)
        ));
    }
}
