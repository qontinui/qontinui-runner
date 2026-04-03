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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityLevel {
    /// 0-1 criteria — trivial one-liner fix, config tweak, typo correction.
    Trivial,
    /// 1-4 criteria, 1-2 domains — straightforward single-pass generation.
    Simple,
    /// 5-7 criteria, 2-3 domains — still single-pass but more verification.
    Moderate,
    /// 8+ criteria, 3+ domains — benefits from decomposed generation.
    Complex,
}

/// Pipeline depth controls which generation phases are executed.
/// Derived from ComplexityLevel but can be overridden by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineDepth {
    /// One-liner fixes, typos, config changes.
    /// Pipeline: Discovery → Builder (template-based) → Autofix → Hardener → Validate
    /// Skips: Investigation, Specification, Verification↔Fixer loop
    Trivial,
    /// Small bug fixes, single-file changes.
    /// Pipeline: Discovery → Specification (lightweight) → Builder → Autofix → [Verify↔Fix max 1] → Hardener → Validate
    /// Skips: Investigation
    Simple,
    /// Standard features, multi-file changes. Full pipeline as-is.
    Standard,
    /// Large features, multi-subsystem. Full pipeline + decomposition.
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
        ComplexityLevel::Trivial | ComplexityLevel::Simple | ComplexityLevel::Moderate => {
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
    if criteria_count <= 1 {
        ComplexityLevel::Trivial
    } else if criteria_count <= SIMPLE_MAX_CRITERIA && domain_count <= SIMPLE_MAX_DOMAINS {
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
fn build_phases(domain_groups: &HashMap<VerificationDomain, Vec<String>>) -> Vec<Phase> {
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
// Pipeline Depth Classification
// ============================================================================

/// Classify pipeline depth from description text, BEFORE acceptance criteria exist.
/// This runs early in the pipeline (before Specification) to decide which phases to skip.
pub fn classify_pipeline_depth_from_description(description: &str) -> PipelineDepth {
    let word_count = description.split_whitespace().count();
    let desc_lower = description.to_lowercase();

    // Complex indicators — check FIRST because short descriptions can still
    // be complex ("refactor auth", "migrate DB to v2").
    let complex_keywords = [
        "refactor",
        "migrate",
        "redesign",
        "rewrite",
        "overhaul",
        "multi-service",
        "full-stack",
        "end-to-end",
        "architecture",
    ];
    let has_complex_keyword = complex_keywords.iter().any(|k| desc_lower.contains(k));

    let multi_domain_keywords = [
        ("frontend", "backend"),
        ("ui", "api"),
        ("database", "endpoint"),
        ("server", "client"),
        ("component", "route"),
    ];
    let has_multi_domain = multi_domain_keywords
        .iter()
        .any(|(a, b)| desc_lower.contains(a) && desc_lower.contains(b));

    if has_complex_keyword || has_multi_domain {
        return PipelineDepth::Complex;
    }

    // Trivial indicators: very short descriptions with single-action keywords
    let trivial_keywords = [
        "typo",
        "rename",
        "bump version",
        "update dependency",
        "fix import",
        "remove unused",
        "add comment",
        "fix whitespace",
    ];
    let is_trivial_keyword = trivial_keywords.iter().any(|k| desc_lower.contains(k));

    if word_count <= 15 && is_trivial_keyword {
        return PipelineDepth::Trivial;
    }
    if word_count <= 10 {
        return PipelineDepth::Trivial;
    }

    // Simple: short-ish descriptions without complex indicators
    if word_count <= 30 {
        return PipelineDepth::Simple;
    }

    PipelineDepth::Standard
}

/// Refine pipeline depth using criteria-based complexity assessment.
/// Called after Specification phase produces criteria, can upgrade (but not downgrade) depth.
pub fn refine_pipeline_depth(
    current: PipelineDepth,
    criteria_level: ComplexityLevel,
) -> PipelineDepth {
    match (current, criteria_level) {
        // If criteria-based assessment says Complex, upgrade regardless
        (_, ComplexityLevel::Complex) => PipelineDepth::Complex,
        // Never downgrade from what description-based assessment chose
        (PipelineDepth::Complex, _) => PipelineDepth::Complex,
        (PipelineDepth::Standard, _) => PipelineDepth::Standard,
        // Allow upgrading Simple→Standard if criteria suggest Moderate
        (PipelineDepth::Simple, ComplexityLevel::Moderate) => PipelineDepth::Standard,
        // Keep as-is
        (depth, _) => depth,
    }
}

impl std::fmt::Display for PipelineDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trivial => write!(f, "trivial"),
            Self::Simple => write!(f, "simple"),
            Self::Standard => write!(f, "standard"),
            Self::Complex => write!(f, "complex"),
        }
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
            ..Default::default()
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
        // Use neutral criterion IDs that don't accidentally match domain keywords.
        // "make_criterion" generates description "Test criterion {id}", so IDs must
        // avoid words like "api", "db", "migration", "status", "route", etc.
        let criteria = vec![
            make_criterion("check-one", "compilation"),
            make_criterion("check-two", "compilation"),
            make_criterion("verify-alpha", "behavior"),
            make_criterion("verify-beta", "behavior"),
            make_criterion("verify-gamma", "ui-content"),
        ];
        let assessment = assess_complexity(&criteria);

        // 5 criteria, 3 domains (compilation, general, ui-content) → Moderate
        assert!(
            matches!(
                assessment.level,
                ComplexityLevel::Moderate | ComplexityLevel::Simple
            ),
            "Expected Moderate or Simple, got {:?} (criteria={}, domains={})",
            assessment.level,
            assessment.criteria_count,
            assessment.domain_count,
        );
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
        assert_eq!(assessment.level, ComplexityLevel::Trivial);
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
        assert_eq!(determine_level(0, 0), ComplexityLevel::Trivial);
        assert_eq!(determine_level(1, 1), ComplexityLevel::Trivial);
        assert_eq!(determine_level(4, 2), ComplexityLevel::Simple);
        assert_eq!(determine_level(5, 2), ComplexityLevel::Moderate);
        assert_eq!(determine_level(5, 3), ComplexityLevel::Moderate);
        assert_eq!(determine_level(7, 3), ComplexityLevel::Moderate);
        assert_eq!(determine_level(8, 3), ComplexityLevel::Complex);
        assert_eq!(determine_level(5, 4), ComplexityLevel::Complex);
        assert_eq!(determine_level(8, 4), ComplexityLevel::Complex);
    }

    // ── Pipeline Depth Classification Tests ──────────────────────────────

    #[test]
    fn test_classify_trivial_short_description() {
        assert_eq!(
            classify_pipeline_depth_from_description("fix typo"),
            PipelineDepth::Trivial,
        );
        assert_eq!(
            classify_pipeline_depth_from_description("bump version"),
            PipelineDepth::Trivial,
        );
        // Very short description (<=10 words) is always Trivial
        assert_eq!(
            classify_pipeline_depth_from_description("add a new field to the config"),
            PipelineDepth::Trivial,
        );
    }

    #[test]
    fn test_classify_trivial_keyword_longer() {
        // Under 15 words with a trivial keyword
        assert_eq!(
            classify_pipeline_depth_from_description(
                "rename the function foo to bar in the utils module"
            ),
            PipelineDepth::Trivial,
        );
        assert_eq!(
            classify_pipeline_depth_from_description(
                "remove unused import of serde_json in the config module"
            ),
            PipelineDepth::Trivial,
        );
    }

    #[test]
    fn test_classify_simple_medium_description() {
        // 11-30 words, no complex keywords or multi-domain pairs
        assert_eq!(
            classify_pipeline_depth_from_description(
                "add a new handler that returns the list of users filtered by their active status"
            ),
            PipelineDepth::Simple,
        );
    }

    #[test]
    fn test_classify_standard_long_description() {
        // >30 words, no complex keywords
        let long_desc = "implement a caching layer for the user profile page that stores \
            results in memory with a configurable TTL and invalidation strategy so that \
            repeated requests do not hit the database every single time a page loads";
        assert_eq!(
            classify_pipeline_depth_from_description(long_desc),
            PipelineDepth::Standard,
        );
    }

    #[test]
    fn test_classify_complex_keywords() {
        assert_eq!(
            classify_pipeline_depth_from_description("refactor the authentication module"),
            PipelineDepth::Complex,
        );
        assert_eq!(
            classify_pipeline_depth_from_description("migrate the database schema to v2"),
            PipelineDepth::Complex,
        );
        assert_eq!(
            classify_pipeline_depth_from_description("redesign the settings page layout"),
            PipelineDepth::Complex,
        );
    }

    #[test]
    fn test_classify_complex_multi_domain() {
        assert_eq!(
            classify_pipeline_depth_from_description(
                "update the frontend component and the backend API to support filtering"
            ),
            PipelineDepth::Complex,
        );
        assert_eq!(
            classify_pipeline_depth_from_description(
                "add a new UI form that writes to the database endpoint"
            ),
            PipelineDepth::Complex,
        );
    }

    #[test]
    fn test_refine_pipeline_depth_upgrade() {
        // Complex criteria should upgrade any depth
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Trivial, ComplexityLevel::Complex),
            PipelineDepth::Complex,
        );
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Simple, ComplexityLevel::Complex),
            PipelineDepth::Complex,
        );
    }

    #[test]
    fn test_refine_pipeline_depth_no_downgrade() {
        // Never downgrade from Complex or Standard
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Complex, ComplexityLevel::Trivial),
            PipelineDepth::Complex,
        );
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Standard, ComplexityLevel::Simple),
            PipelineDepth::Standard,
        );
    }

    #[test]
    fn test_refine_pipeline_depth_simple_to_standard() {
        // Moderate criteria upgrade Simple to Standard
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Simple, ComplexityLevel::Moderate),
            PipelineDepth::Standard,
        );
    }

    #[test]
    fn test_refine_pipeline_depth_keep_trivial() {
        // Trivial stays Trivial when criteria agree
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Trivial, ComplexityLevel::Trivial),
            PipelineDepth::Trivial,
        );
        assert_eq!(
            refine_pipeline_depth(PipelineDepth::Trivial, ComplexityLevel::Simple),
            PipelineDepth::Trivial,
        );
    }

    #[test]
    fn test_pipeline_depth_display() {
        assert_eq!(PipelineDepth::Trivial.to_string(), "trivial");
        assert_eq!(PipelineDepth::Simple.to_string(), "simple");
        assert_eq!(PipelineDepth::Standard.to_string(), "standard");
        assert_eq!(PipelineDepth::Complex.to_string(), "complex");
    }
}
