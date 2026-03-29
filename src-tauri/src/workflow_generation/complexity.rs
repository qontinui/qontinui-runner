//! Complexity Assessment
//!
//! Analyzes acceptance criteria to determine whether a workflow requires
//! single-pass or decomposed (multi-domain) generation. This gates the
//! exploration pipeline: simple workflows skip decomposition overhead.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::debug;

use super::domain_routing::{classify_criteria_domains, VerificationDomain};
use super::specification::AcceptanceCriterion;

// ============================================================================
// Types
// ============================================================================

/// Complexity level of the generation task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityLevel {
    /// 1-4 criteria, 1-2 domains — straightforward single-pass generation.
    Simple,
    /// 5-7 criteria, 2-3 domains — still single-pass but more verification.
    Moderate,
    /// 8+ criteria, 3+ domains — benefits from decomposed generation.
    Complex,
}

/// Recommended generation strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum GenerationStrategy {
    /// Generate the entire workflow in one pass.
    SinglePass,
    /// Decompose into ordered phases, each targeting one domain.
    Decomposed { phases: Vec<Phase> },
}

/// A generation phase — a group of criteria for one domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    /// The verification domain this phase targets.
    pub domain: VerificationDomain,
    /// IDs of acceptance criteria addressed in this phase.
    pub criteria_ids: Vec<String>,
    /// Brief description of what this phase focuses on.
    pub context_focus: String,
    /// Domains that must be completed before this phase.
    pub dependencies: Vec<VerificationDomain>,
}

/// Full complexity assessment result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAssessment {
    /// Determined complexity level.
    pub level: ComplexityLevel,
    /// Total number of criteria analyzed.
    pub criteria_count: usize,
    /// Number of distinct verification domains.
    pub domain_count: usize,
    /// List of distinct domains found.
    pub domains: Vec<VerificationDomain>,
    /// Recommended generation strategy.
    pub recommendation: GenerationStrategy,
}

// ============================================================================
// Constants
// ============================================================================

/// Criteria count thresholds for complexity levels.
const SIMPLE_MAX_CRITERIA: usize = 4;
const MODERATE_MAX_CRITERIA: usize = 7;

/// Domain count thresholds for complexity levels.
const SIMPLE_MAX_DOMAINS: usize = 2;
const MODERATE_MAX_DOMAINS: usize = 3;

// ============================================================================
// Public API
// ============================================================================

/// Assess the complexity of a set of acceptance criteria and recommend a
/// generation strategy.
///
/// For simple and moderate workloads the recommendation is `SinglePass`.
/// For complex workloads (8+ criteria spanning 3+ domains) the recommendation
/// is `Decomposed` with phases ordered by domain dependency.
pub fn assess_complexity(criteria: &[AcceptanceCriterion]) -> ComplexityAssessment {
    let domain_groups = classify_criteria_domains(criteria);
    let criteria_count = criteria.len();
    let domain_count = domain_groups.len();

    let domains: Vec<VerificationDomain> = domain_groups.keys().copied().collect();

    let level = determine_level(criteria_count, domain_count);

    debug!(
        criteria_count,
        domain_count,
        ?level,
        "Complexity assessment complete"
    );

    let recommendation = match level {
        ComplexityLevel::Simple | ComplexityLevel::Moderate => {
            debug!("Recommending single-pass generation");
            GenerationStrategy::SinglePass
        }
        ComplexityLevel::Complex => {
            let phases = build_phases(&domain_groups);
            debug!(
                phase_count = phases.len(),
                "Recommending decomposed generation"
            );
            GenerationStrategy::Decomposed { phases }
        }
    };

    ComplexityAssessment {
        level,
        criteria_count,
        domain_count,
        domains,
        recommendation,
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Determine the complexity level from criteria and domain counts.
fn determine_level(criteria_count: usize, domain_count: usize) -> ComplexityLevel {
    if criteria_count <= SIMPLE_MAX_CRITERIA && domain_count <= SIMPLE_MAX_DOMAINS {
        ComplexityLevel::Simple
    } else if criteria_count <= MODERATE_MAX_CRITERIA && domain_count <= MODERATE_MAX_DOMAINS {
        ComplexityLevel::Moderate
    } else {
        ComplexityLevel::Complex
    }
}

/// Returns the canonical dependency order for verification domains.
///
/// Earlier domains are prerequisites for later ones:
///   Compilation → DatabaseState → ApiEndpoint → Security → UiContent → Performance → Integration → CiCd
fn domain_dependency_order() -> Vec<VerificationDomain> {
    vec![
        VerificationDomain::Compilation,
        VerificationDomain::DatabaseState,
        VerificationDomain::ApiEndpoint,
        VerificationDomain::Security,
        VerificationDomain::UiContent,
        VerificationDomain::Performance,
        VerificationDomain::Integration,
        VerificationDomain::CiCd,
    ]
}

/// Convert domain groups into ordered phases with dependency information.
///
/// Phases are ordered according to `domain_dependency_order()`. Each phase
/// lists the domains that precede it in the canonical order as dependencies.
fn build_phases(
    domain_groups: &HashMap<VerificationDomain, Vec<String>>,
) -> Vec<Phase> {
    let canonical_order = domain_dependency_order();

    // Collect only the domains that actually appear in the criteria, preserving
    // the canonical dependency order.
    let active_domains: Vec<VerificationDomain> = canonical_order
        .iter()
        .filter(|d| domain_groups.contains_key(d))
        .copied()
        .collect();

    // Any domains in the groups that aren't in the canonical list get appended
    // at the end (future-proofing).
    let extra_domains: Vec<VerificationDomain> = domain_groups
        .keys()
        .filter(|d| !canonical_order.contains(d))
        .copied()
        .collect();

    let ordered: Vec<VerificationDomain> = active_domains
        .iter()
        .chain(extra_domains.iter())
        .copied()
        .collect();

    let mut phases = Vec::with_capacity(ordered.len());
    let mut prior_domains: Vec<VerificationDomain> = Vec::new();

    for domain in &ordered {
        let criteria = &domain_groups[domain];
        let criteria_ids: Vec<String> = criteria.clone();

        let context_focus = format!(
            "{} verification ({} {})",
            domain_display_name(*domain),
            criteria_ids.len(),
            if criteria_ids.len() == 1 {
                "criterion"
            } else {
                "criteria"
            }
        );

        // Dependencies are all active domains that precede this one in the
        // canonical ordering and are present in the current assessment.
        let dependencies: Vec<VerificationDomain> = prior_domains
            .iter()
            .filter(|d| ordered.contains(d))
            .copied()
            .collect();

        debug!(
            ?domain,
            criteria_count = criteria_ids.len(),
            dep_count = dependencies.len(),
            "Built phase"
        );

        phases.push(Phase {
            domain: *domain,
            criteria_ids,
            context_focus,
            dependencies,
        });

        prior_domains.push(*domain);
    }

    phases
}

/// Human-readable display name for a verification domain.
fn domain_display_name(domain: VerificationDomain) -> &'static str {
    match domain {
        VerificationDomain::Compilation => "Compilation",
        VerificationDomain::DatabaseState => "Database state",
        VerificationDomain::ApiEndpoint => "API endpoint",
        VerificationDomain::Security => "Security",
        VerificationDomain::UiContent => "UI content",
        VerificationDomain::Performance => "Performance",
        VerificationDomain::Integration => "Integration",
        VerificationDomain::CiCd => "CI/CD",
        VerificationDomain::General => "General",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::specification::{CriterionPriority, VerificationMethod};

    fn make_criterion(id: &str, category: &str) -> AcceptanceCriterion {
        AcceptanceCriterion {
            id: id.to_string(),
            description: format!("Test criterion {id}"),
            method: VerificationMethod::Command,
            priority: CriterionPriority::Critical,
            verification_hint: String::new(),
            category: category.to_string(),
        }
    }

    #[test]
    fn test_simple_assessment() {
        let criteria = vec![
            make_criterion("typecheck-passes", "compilation"),
            make_criterion("build-succeeds", "compilation"),
        ];
        let assessment = assess_complexity(&criteria);

        assert_eq!(assessment.level, ComplexityLevel::Simple);
        assert_eq!(assessment.criteria_count, 2);
        assert!(matches!(
            assessment.recommendation,
            GenerationStrategy::SinglePass
        ));
    }

    #[test]
    fn test_moderate_assessment() {
        let criteria = vec![
            make_criterion("typecheck-passes", "compilation"),
            make_criterion("api-returns-200", "behavior"),
            make_criterion("api-returns-json", "behavior"),
            make_criterion("db-migration-runs", "behavior"),
            make_criterion("ui-shows-title", "ui-content"),
        ];
        let assessment = assess_complexity(&criteria);

        // 5 criteria, likely 2-3 domains → Moderate
        assert!(matches!(
            assessment.level,
            ComplexityLevel::Moderate | ComplexityLevel::Simple
        ));
        assert!(matches!(
            assessment.recommendation,
            GenerationStrategy::SinglePass
        ));
    }

    #[test]
    fn test_complex_assessment_produces_phases() {
        // Create 8+ criteria across 4+ domains to trigger Complex.
        let criteria = vec![
            make_criterion("typecheck-passes", "compilation"),
            make_criterion("build-succeeds", "compilation"),
            make_criterion("db-migration-runs", "database"),
            make_criterion("db-seed-works", "database"),
            make_criterion("api-health", "api"),
            make_criterion("api-auth", "security"),
            make_criterion("ui-title", "ui-content"),
            make_criterion("ci-passes", "ci-cd"),
            make_criterion("perf-under-200ms", "performance"),
            make_criterion("integration-e2e", "integration"),
        ];
        let assessment = assess_complexity(&criteria);

        assert_eq!(assessment.level, ComplexityLevel::Complex);
        assert_eq!(assessment.criteria_count, 10);

        match &assessment.recommendation {
            GenerationStrategy::Decomposed { phases } => {
                assert!(!phases.is_empty());
                // First phase should have no dependencies.
                assert!(phases[0].dependencies.is_empty());
                // Later phases should list prior domains as dependencies.
                if phases.len() > 1 {
                    assert!(!phases.last().unwrap().dependencies.is_empty());
                }
            }
            GenerationStrategy::SinglePass => {
                panic!("Expected Decomposed strategy for complex assessment");
            }
        }
    }

    #[test]
    fn test_empty_criteria() {
        let assessment = assess_complexity(&[]);
        assert_eq!(assessment.level, ComplexityLevel::Simple);
        assert_eq!(assessment.criteria_count, 0);
        assert_eq!(assessment.domain_count, 0);
    }

    #[test]
    fn test_domain_dependency_order_is_complete() {
        let order = domain_dependency_order();
        assert_eq!(order.len(), 8, "All 8 canonical domains should be listed");
    }

    #[test]
    fn test_determine_level_boundaries() {
        assert_eq!(determine_level(4, 2), ComplexityLevel::Simple);
        assert_eq!(determine_level(5, 2), ComplexityLevel::Moderate);
        assert_eq!(determine_level(5, 3), ComplexityLevel::Moderate);
        assert_eq!(determine_level(7, 3), ComplexityLevel::Moderate);
        assert_eq!(determine_level(8, 3), ComplexityLevel::Complex);
        assert_eq!(determine_level(5, 4), ComplexityLevel::Complex);
        assert_eq!(determine_level(8, 4), ComplexityLevel::Complex);
    }
}
