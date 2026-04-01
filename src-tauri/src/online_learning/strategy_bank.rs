//! Evolvable strategy bank.
//!
//! Stores learned optimization strategies as evolvable "skills" that can be
//! selected via Thompson Sampling, composed, and refined through failure analysis.
//!
//! A strategy encodes a *policy* for approaching a class of tasks — it composes
//! model choice, architecture preference, skill chain, and parameter overrides.
//!
//! Inspired by MemSkill's meta-memory skill bank (ViktorAxelsen/MemSkill).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// Strategy definition
// =============================================================================

/// A learned strategy: a reusable pattern for approaching a class of tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    /// Unique identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this strategy does.
    pub description: String,
    /// What contexts this strategy applies to.
    pub applicability: StrategyApplicability,
    /// Components of the strategy (what to do).
    pub components: StrategyComponents,
    /// Performance statistics (how well it works).
    pub stats: StrategyStats,
    /// How this strategy was created.
    pub provenance: StrategyProvenance,
    /// Current lifecycle status.
    pub status: StrategyStatus,
    /// ID of the parent strategy (if evolved).
    pub parent_strategy_id: Option<String>,
    /// Creation timestamp.
    pub created_at: String,
    /// Last update timestamp.
    pub updated_at: String,
}

/// Defines what contexts a strategy applies to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyApplicability {
    /// Domain tags this strategy works for (empty = all domains).
    pub domains: Vec<String>,
    /// Complexity tiers this strategy works for (empty = all).
    pub complexity_tiers: Vec<String>,
    /// Technology patterns (empty = all).
    pub technology_tags: Vec<String>,
    /// Minimum confidence to select this strategy.
    pub min_confidence: f64,
}

impl Default for StrategyApplicability {
    fn default() -> Self {
        Self {
            domains: Vec::new(),
            complexity_tiers: Vec::new(),
            technology_tags: Vec::new(),
            min_confidence: 0.0,
        }
    }
}

/// What the strategy configures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyComponents {
    /// Preferred workflow architecture (Traditional, AgenticVerification, MultiAgentPipeline).
    pub preferred_architecture: Option<String>,
    /// Preferred model for primary execution.
    pub preferred_model: Option<String>,
    /// Ordered list of skill/step IDs to apply.
    pub skill_chain: Vec<String>,
    /// Step-type-specific approach overrides.
    pub step_type_overrides: HashMap<String, String>,
    /// Agent-specific prompt variant preferences.
    pub prompt_variant_preferences: HashMap<String, String>,
    /// Arbitrary parameter overrides.
    pub parameter_overrides: HashMap<String, serde_json::Value>,
}

impl Default for StrategyComponents {
    fn default() -> Self {
        Self {
            preferred_architecture: None,
            preferred_model: None,
            skill_chain: Vec::new(),
            step_type_overrides: HashMap::new(),
            prompt_variant_preferences: HashMap::new(),
            parameter_overrides: HashMap::new(),
        }
    }
}

/// Performance statistics for a strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyStats {
    /// Number of times this strategy was selected.
    pub times_selected: u32,
    /// Number of times it led to a successful outcome.
    pub times_succeeded: u32,
    /// Average composite agentic score when this strategy was used.
    pub avg_composite_score: f64,
    /// Average cost in USD.
    pub avg_cost_usd: f64,
    /// Average duration in seconds.
    pub avg_duration_secs: f64,
    /// Last time this strategy was used.
    pub last_used: Option<String>,
}

impl Default for StrategyStats {
    fn default() -> Self {
        Self {
            times_selected: 0,
            times_succeeded: 0,
            avg_composite_score: 0.0,
            avg_cost_usd: 0.0,
            avg_duration_secs: 0.0,
            last_used: None,
        }
    }
}

impl StrategyStats {
    /// Success rate (0.0-1.0).
    pub fn success_rate(&self) -> f64 {
        if self.times_selected == 0 {
            0.0
        } else {
            self.times_succeeded as f64 / self.times_selected as f64
        }
    }

    /// Update stats after a run using this strategy.
    pub fn record_outcome(
        &mut self,
        success: bool,
        composite_score: f64,
        cost_usd: f64,
        duration_secs: f64,
    ) {
        let n = self.times_selected as f64;
        self.times_selected += 1;
        if success {
            self.times_succeeded += 1;
        }

        // Running average update
        let new_n = self.times_selected as f64;
        self.avg_composite_score = (self.avg_composite_score * n + composite_score) / new_n;
        self.avg_cost_usd = (self.avg_cost_usd * n + cost_usd) / new_n;
        self.avg_duration_secs = (self.avg_duration_secs * n + duration_secs) / new_n;
        self.last_used = Some(chrono::Utc::now().to_rfc3339());
    }
}

/// How a strategy was created.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StrategyProvenance {
    /// Extracted from a high-performing run.
    ExtractedFromRun { source_run_id: String },
    /// Created by analyzing failures and inverting the anti-pattern.
    FailureMining { failure_pattern_id: String },
    /// Evolved from a parent strategy via mutation.
    Evolved {
        parent_id: String,
        mutation_type: String,
    },
    /// Created from cross-run pattern detection.
    CrossRunPattern { pattern_ids: Vec<String> },
    /// Manually created by a user.
    Manual,
}

/// Lifecycle status of a strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyStatus {
    /// Newly created, not yet validated through canary testing.
    Candidate,
    /// Validated and available for selection.
    Active,
    /// Drift detected; reduced selection priority.
    Degraded,
    /// No longer effective; kept for history.
    Retired,
}

// =============================================================================
// Strategy matching
// =============================================================================

impl Strategy {
    /// Check if this strategy is applicable to the given context.
    pub fn matches_context(&self, domain: &str, complexity: &str, tech_tags: &[String]) -> bool {
        // Check domain match (empty = matches all)
        if !self.applicability.domains.is_empty()
            && !self.applicability.domains.iter().any(|d| d == domain)
        {
            return false;
        }

        // Check complexity match
        if !self.applicability.complexity_tiers.is_empty()
            && !self
                .applicability
                .complexity_tiers
                .iter()
                .any(|c| c == complexity)
        {
            return false;
        }

        // Check technology tag overlap
        if !self.applicability.technology_tags.is_empty()
            && !self
                .applicability
                .technology_tags
                .iter()
                .any(|t| tech_tags.contains(t))
        {
            return false;
        }

        true
    }

    /// Check if this strategy is selectable (active and meets confidence threshold).
    pub fn is_selectable(&self, min_confidence: f64) -> bool {
        self.status == StrategyStatus::Active
            && self.stats.times_selected >= 3 // Minimum data
            && self.stats.success_rate() >= min_confidence
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_strategy(id: &str, domains: Vec<&str>, status: StrategyStatus) -> Strategy {
        Strategy {
            id: id.to_string(),
            name: format!("Strategy {}", id),
            description: "Test strategy".to_string(),
            applicability: StrategyApplicability {
                domains: domains.into_iter().map(String::from).collect(),
                complexity_tiers: Vec::new(),
                technology_tags: Vec::new(),
                min_confidence: 0.0,
            },
            components: StrategyComponents::default(),
            stats: StrategyStats {
                times_selected: 10,
                times_succeeded: 8,
                avg_composite_score: 0.85,
                avg_cost_usd: 0.50,
                avg_duration_secs: 120.0,
                last_used: Some("2026-03-30T00:00:00Z".to_string()),
            },
            provenance: StrategyProvenance::Manual,
            status,
            parent_strategy_id: None,
            created_at: "2026-03-30T00:00:00Z".to_string(),
            updated_at: "2026-03-30T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_matches_all_domains() {
        let strategy = make_strategy("s1", vec![], StrategyStatus::Active);
        assert!(strategy.matches_context("backend", "moderate", &[]));
        assert!(strategy.matches_context("frontend", "complex", &[]));
    }

    #[test]
    fn test_matches_specific_domain() {
        let strategy = make_strategy("s1", vec!["backend"], StrategyStatus::Active);
        assert!(strategy.matches_context("backend", "moderate", &[]));
        assert!(!strategy.matches_context("frontend", "moderate", &[]));
    }

    #[test]
    fn test_is_selectable() {
        let active = make_strategy("s1", vec![], StrategyStatus::Active);
        assert!(active.is_selectable(0.5));

        let candidate = make_strategy("s2", vec![], StrategyStatus::Candidate);
        assert!(!candidate.is_selectable(0.5));

        let retired = make_strategy("s3", vec![], StrategyStatus::Retired);
        assert!(!retired.is_selectable(0.5));
    }

    #[test]
    fn test_stats_record_outcome() {
        let mut stats = StrategyStats::default();
        stats.record_outcome(true, 0.9, 0.50, 120.0);
        stats.record_outcome(false, 0.3, 0.80, 200.0);

        assert_eq!(stats.times_selected, 2);
        assert_eq!(stats.times_succeeded, 1);
        assert!((stats.success_rate() - 0.5).abs() < 0.01);
        assert!((stats.avg_composite_score - 0.6).abs() < 0.01);
    }
}
