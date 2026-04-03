//! Strategy evolution mechanisms.
//!
//! Three ways strategies are created and refined:
//! 1. **Extraction** — P90+ performing runs generate candidate strategies
//! 2. **Failure mining** — Analyze failures to invert anti-patterns
//! 3. **Mutation** — Create variants of top strategies with small modifications
//!
//! All new strategies enter as `Candidate` and must pass canary validation
//! before becoming `Active`.

use super::strategy_bank::*;
use chrono::Utc;
use tracing::info;
use uuid::Uuid;

// =============================================================================
// Strategy extraction from successful runs
// =============================================================================

/// Data from a high-performing run, used to extract a strategy.
#[derive(Debug, Clone)]
pub struct HighPerformingRun {
    pub task_run_id: String,
    pub domain: String,
    pub complexity_tier: String,
    pub technology_tags: Vec<String>,
    pub workflow_architecture: String,
    pub model_used: Option<String>,
    pub composite_score: f64,
    pub cost_usd: f64,
    pub duration_secs: f64,
}

/// Extract a candidate strategy from a high-performing run.
///
/// Called when a run achieves `composite_score > P90` for its context.
pub fn extract_strategy_from_run(run: &HighPerformingRun) -> Strategy {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    info!(
        "Extracting strategy from high-performing run {} (score={:.3})",
        run.task_run_id, run.composite_score
    );

    Strategy {
        id,
        name: format!(
            "Extracted: {} {} (score {:.0}%)",
            run.domain,
            run.complexity_tier,
            run.composite_score * 100.0
        ),
        description: format!(
            "Strategy extracted from run {} which achieved {:.1}% composite score \
             on a {} {} task.",
            run.task_run_id,
            run.composite_score * 100.0,
            run.complexity_tier,
            run.domain
        ),
        applicability: StrategyApplicability {
            domains: vec![run.domain.clone()],
            complexity_tiers: vec![run.complexity_tier.clone()],
            technology_tags: run.technology_tags.clone(),
            min_confidence: 0.0,
        },
        components: StrategyComponents {
            preferred_architecture: Some(run.workflow_architecture.clone()),
            preferred_model: run.model_used.clone(),
            skill_chain: Vec::new(),
            step_type_overrides: std::collections::HashMap::new(),
            prompt_variant_preferences: std::collections::HashMap::new(),
            parameter_overrides: std::collections::HashMap::new(),
        },
        stats: StrategyStats {
            times_selected: 1,
            times_succeeded: 1,
            avg_composite_score: run.composite_score,
            avg_cost_usd: run.cost_usd,
            avg_duration_secs: run.duration_secs,
            last_used: Some(now.clone()),
        },
        provenance: StrategyProvenance::ExtractedFromRun {
            source_run_id: run.task_run_id.clone(),
        },
        status: StrategyStatus::Candidate,
        parent_strategy_id: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

// =============================================================================
// Failure mining
// =============================================================================

/// Data for a failure-success contrast pair.
#[derive(Debug, Clone)]
pub struct FailureContrastPair {
    /// The failing run's configuration.
    pub failure_run_id: String,
    pub failure_architecture: String,
    pub failure_model: Option<String>,
    /// The succeeding run's configuration.
    pub success_run_id: String,
    pub success_architecture: String,
    pub success_model: Option<String>,
    /// Shared context.
    pub domain: String,
    pub complexity_tier: String,
}

/// Create a strategy from a failure-success contrast pair.
///
/// When the same context has both failing and succeeding runs with different
/// configurations, extract the successful configuration as a strategy.
pub fn strategy_from_failure_mining(pair: &FailureContrastPair) -> Strategy {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    info!(
        "Failure mining: contrasting {} (fail) with {} (success)",
        pair.failure_run_id, pair.success_run_id
    );

    Strategy {
        id,
        name: format!(
            "Failure-mined: {} {} (avoid {})",
            pair.domain, pair.complexity_tier, pair.failure_architecture
        ),
        description: format!(
            "Strategy mined from failure contrast: run {} failed with {} architecture \
             while run {} succeeded with {}. Prefer the successful configuration.",
            pair.failure_run_id,
            pair.failure_architecture,
            pair.success_run_id,
            pair.success_architecture
        ),
        applicability: StrategyApplicability {
            domains: vec![pair.domain.clone()],
            complexity_tiers: vec![pair.complexity_tier.clone()],
            technology_tags: Vec::new(),
            min_confidence: 0.0,
        },
        components: StrategyComponents {
            preferred_architecture: Some(pair.success_architecture.clone()),
            preferred_model: pair.success_model.clone(),
            skill_chain: Vec::new(),
            step_type_overrides: std::collections::HashMap::new(),
            prompt_variant_preferences: std::collections::HashMap::new(),
            parameter_overrides: std::collections::HashMap::new(),
        },
        stats: StrategyStats::default(),
        provenance: StrategyProvenance::FailureMining {
            failure_pattern_id: pair.failure_run_id.clone(),
        },
        status: StrategyStatus::Candidate,
        parent_strategy_id: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

// =============================================================================
// Mutation
// =============================================================================

/// Types of mutations that can be applied to a strategy.
#[derive(Debug, Clone)]
pub enum MutationType {
    /// Swap the preferred model.
    SwapModel(String),
    /// Swap the preferred architecture.
    SwapArchitecture(String),
    /// Add a step type override.
    AddStepOverride(String, String),
    /// Remove a step type override.
    RemoveStepOverride(String),
}

impl MutationType {
    pub fn description(&self) -> String {
        match self {
            Self::SwapModel(m) => format!("swap model to {}", m),
            Self::SwapArchitecture(a) => format!("swap architecture to {}", a),
            Self::AddStepOverride(s, o) => format!("add step override: {}={}", s, o),
            Self::RemoveStepOverride(s) => format!("remove step override: {}", s),
        }
    }
}

/// Create a mutated variant of an existing strategy.
///
/// The variant enters as `Candidate` and competes with the parent
/// through the canary system.
pub fn mutate_strategy(parent: &Strategy, mutation: MutationType) -> Strategy {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut components = parent.components.clone();
    let mutation_desc = mutation.description();

    match mutation {
        MutationType::SwapModel(model) => {
            components.preferred_model = Some(model);
        }
        MutationType::SwapArchitecture(arch) => {
            components.preferred_architecture = Some(arch);
        }
        MutationType::AddStepOverride(step_type, override_val) => {
            components
                .step_type_overrides
                .insert(step_type, override_val);
        }
        MutationType::RemoveStepOverride(step_type) => {
            components.step_type_overrides.remove(&step_type);
        }
    }

    info!(
        "Mutating strategy '{}' ({}): {}",
        parent.name, parent.id, mutation_desc
    );

    Strategy {
        id,
        name: format!("{} (mutated: {})", parent.name, mutation_desc),
        description: format!(
            "Variant of '{}' with mutation: {}",
            parent.name, mutation_desc
        ),
        applicability: parent.applicability.clone(),
        components,
        stats: StrategyStats::default(),
        provenance: StrategyProvenance::Evolved {
            parent_id: parent.id.clone(),
            mutation_type: mutation_desc,
        },
        status: StrategyStatus::Candidate,
        parent_strategy_id: Some(parent.id.clone()),
        created_at: now.clone(),
        updated_at: now,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_from_run() {
        let run = HighPerformingRun {
            task_run_id: "run-123".to_string(),
            domain: "backend".to_string(),
            complexity_tier: "complex".to_string(),
            technology_tags: vec!["rust".to_string()],
            workflow_architecture: "agentic_verification".to_string(),
            model_used: Some("claude-sonnet-4-20250514".to_string()),
            composite_score: 0.95,
            cost_usd: 0.30,
            duration_secs: 90.0,
        };

        let strategy = extract_strategy_from_run(&run);
        assert_eq!(strategy.status, StrategyStatus::Candidate);
        assert!(strategy.name.contains("backend"));
        assert!(strategy.name.contains("complex"));
        assert_eq!(
            strategy.components.preferred_architecture.as_deref(),
            Some("agentic_verification")
        );
        assert!(matches!(
            strategy.provenance,
            StrategyProvenance::ExtractedFromRun { .. }
        ));
    }

    #[test]
    fn test_failure_mining() {
        let pair = FailureContrastPair {
            failure_run_id: "fail-1".to_string(),
            failure_architecture: "traditional".to_string(),
            failure_model: Some("claude-haiku-4-5-20251001".to_string()),
            success_run_id: "success-1".to_string(),
            success_architecture: "agentic_verification".to_string(),
            success_model: Some("claude-sonnet-4-20250514".to_string()),
            domain: "database".to_string(),
            complexity_tier: "complex".to_string(),
        };

        let strategy = strategy_from_failure_mining(&pair);
        assert_eq!(strategy.status, StrategyStatus::Candidate);
        assert_eq!(
            strategy.components.preferred_architecture.as_deref(),
            Some("agentic_verification")
        );
    }

    #[test]
    fn test_mutation_swap_model() {
        let parent = extract_strategy_from_run(&HighPerformingRun {
            task_run_id: "run-1".to_string(),
            domain: "backend".to_string(),
            complexity_tier: "moderate".to_string(),
            technology_tags: vec![],
            workflow_architecture: "traditional".to_string(),
            model_used: Some("claude-sonnet-4-20250514".to_string()),
            composite_score: 0.9,
            cost_usd: 0.5,
            duration_secs: 100.0,
        });

        let mutant = mutate_strategy(
            &parent,
            MutationType::SwapModel("claude-opus-4-20250514".to_string()),
        );
        assert_eq!(mutant.status, StrategyStatus::Candidate);
        assert_eq!(
            mutant.components.preferred_model.as_deref(),
            Some("claude-opus-4-20250514")
        );
        assert_eq!(
            mutant.parent_strategy_id.as_deref(),
            Some(parent.id.as_str())
        );
    }
}
