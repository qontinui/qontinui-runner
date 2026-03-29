//! Semantic Coverage Matrix
//!
//! Replaces name-matching coverage with entailment-scored mappings between
//! acceptance criteria and verification steps.

use serde::{Deserialize, Serialize};

use crate::workflow_generation::specification::{
    AcceptanceCriteria, CriterionPriority, VerificationMethod,
};

use super::StepEvaluation;

/// Semantic coverage matrix: criteria mapped to steps with entailment scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCoverageMatrix {
    pub entries: Vec<CoverageEntry>,
    pub uncovered_criteria: Vec<UncoveredCriterion>,
    pub unlinked_steps: Vec<String>,
    pub overall_coverage_score: f64,
}

/// A single criterion's coverage information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub criterion_id: String,
    pub criterion_description: String,
    pub criterion_priority: CriterionPriority,
    pub mapped_steps: Vec<MappedStep>,
    /// Highest entailment score among mapped steps
    pub best_entailment: f64,
    /// Whether coverage is adequate (based on priority threshold)
    pub coverage_adequate: bool,
}

/// A step mapped to a criterion with its entailment score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappedStep {
    pub step_id: String,
    pub step_name: String,
    pub entailment_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entailment_explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

/// A criterion with no verification step coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncoveredCriterion {
    pub criterion_id: String,
    pub criterion_description: String,
    pub priority: CriterionPriority,
    pub suggested_verification: String,
}

/// Build a semantic coverage matrix from step evaluations and acceptance criteria.
pub fn build_semantic_coverage_matrix(
    evaluations: &[StepEvaluation],
    criteria: Option<&AcceptanceCriteria>,
) -> SemanticCoverageMatrix {
    let criteria = match criteria {
        Some(c) => c,
        None => {
            return SemanticCoverageMatrix {
                entries: Vec::new(),
                uncovered_criteria: Vec::new(),
                unlinked_steps: evaluations
                    .iter()
                    .filter(|e| e.criterion_id.is_none())
                    .map(|e| e.step_id.clone())
                    .collect(),
                overall_coverage_score: 0.0,
            };
        }
    };

    let mut entries: Vec<CoverageEntry> = Vec::new();
    let mut covered_criterion_ids: Vec<String> = Vec::new();

    // Build coverage entries for each automatable criterion
    for criterion in &criteria.criteria {
        if criterion.method == VerificationMethod::Manual {
            continue;
        }

        let mapped_steps: Vec<MappedStep> = evaluations
            .iter()
            .filter(|e| e.criterion_id.as_ref() == Some(&criterion.id))
            .map(|e| {
                let entailment = e
                    .scores
                    .iter()
                    .find(|s| s.dimension == super::EvaluationDimension::Entailment);

                MappedStep {
                    step_id: e.step_id.clone(),
                    step_name: e.step_name.clone(),
                    entailment_score: entailment.map(|s| s.score).unwrap_or(0.0),
                    entailment_explanation: entailment.and_then(|s| s.explanation.clone()),
                    gaps: entailment.map(|s| s.evidence.clone()).unwrap_or_default(),
                }
            })
            .collect();

        let best_entailment = mapped_steps
            .iter()
            .map(|s| s.entailment_score)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0);

        let threshold = coverage_threshold(&criterion.priority);
        let coverage_adequate = best_entailment >= threshold;

        if !mapped_steps.is_empty() {
            covered_criterion_ids.push(criterion.id.clone());
        }

        entries.push(CoverageEntry {
            criterion_id: criterion.id.clone(),
            criterion_description: criterion.description.clone(),
            criterion_priority: criterion.priority.clone(),
            mapped_steps,
            best_entailment,
            coverage_adequate,
        });
    }

    // Uncovered criteria
    let uncovered_criteria: Vec<UncoveredCriterion> = criteria
        .criteria
        .iter()
        .filter(|c| c.method != VerificationMethod::Manual)
        .filter(|c| !covered_criterion_ids.contains(&c.id))
        .map(|c| UncoveredCriterion {
            criterion_id: c.id.clone(),
            criterion_description: c.description.clone(),
            priority: c.priority.clone(),
            suggested_verification: format!("{:?} check: {}", c.method, c.verification_hint),
        })
        .collect();

    // Unlinked steps (verification steps with no criterion mapping)
    let unlinked_steps: Vec<String> = evaluations
        .iter()
        .filter(|e| e.criterion_id.is_none())
        .map(|e| e.step_id.clone())
        .collect();

    // Overall coverage score: average of best entailment per criterion, weighted by priority
    let overall_coverage_score = if entries.is_empty() {
        0.0
    } else {
        let (weighted_sum, weight_total) = entries.iter().fold((0.0, 0.0), |(sum, wt), entry| {
            let weight = match entry.criterion_priority {
                CriterionPriority::Critical => 3.0,
                CriterionPriority::Important => 2.0,
                CriterionPriority::Optional => 1.0,
            };
            (sum + entry.best_entailment * weight, wt + weight)
        });
        if weight_total > 0.0 {
            weighted_sum / weight_total
        } else {
            0.0
        }
    };

    SemanticCoverageMatrix {
        entries,
        uncovered_criteria,
        unlinked_steps,
        overall_coverage_score,
    }
}

/// Priority-based thresholds for coverage adequacy.
fn coverage_threshold(priority: &CriterionPriority) -> f64 {
    match priority {
        CriterionPriority::Critical => 0.8,
        CriterionPriority::Important => 0.6,
        CriterionPriority::Optional => 0.3,
    }
}
