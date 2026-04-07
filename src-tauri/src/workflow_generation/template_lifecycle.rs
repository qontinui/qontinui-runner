//! Template Lifecycle Manager
//!
//! Manages the full lifecycle of verification step templates: tracking
//! performance metrics, promoting high-confidence templates, demoting
//! underperformers, and retiring persistent failures. Works alongside
//! `template_promotion` (which handles workflow-to-template extraction)
//! and `verification_templates` (which defines static template types).

use serde::{Deserialize, Serialize};

// ============================================================================
// Types
// ============================================================================

/// Source of a template in the lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateSource {
    /// Manually defined (static templates).
    Manual,
    /// Extracted automatically from successful run patterns.
    Extracted,
    /// Promoted from Extracted based on performance.
    Promoted,
    /// Demoted from Promoted due to declining performance.
    Demoted,
    /// Retired — no longer used.
    Retired,
}

impl TemplateSource {
    fn as_str(&self) -> &'static str {
        match self {
            TemplateSource::Manual => "manual",
            TemplateSource::Extracted => "extracted",
            TemplateSource::Promoted => "promoted",
            TemplateSource::Demoted => "demoted",
            TemplateSource::Retired => "retired",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "manual" => TemplateSource::Manual,
            "extracted" => TemplateSource::Extracted,
            "promoted" => TemplateSource::Promoted,
            "demoted" => TemplateSource::Demoted,
            "retired" => TemplateSource::Retired,
            _ => TemplateSource::Manual,
        }
    }
}

/// Template performance record for lifecycle management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplatePerformance {
    pub template_id: String,
    pub template_name: String,
    pub source: TemplateSource,
    pub success_count: u32,
    pub failure_count: u32,
    /// Sum of quality scores for averaging.
    pub total_quality_score: f64,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl TemplatePerformance {
    /// Confidence as success rate (0.0–1.0).
    pub fn confidence(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 0.0;
        }
        self.success_count as f64 / total as f64
    }

    /// Average quality score across all uses.
    pub fn avg_quality(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 0.0;
        }
        self.total_quality_score / total as f64
    }

    /// Total number of uses (successes + failures).
    pub fn total_uses(&self) -> u32 {
        self.success_count + self.failure_count
    }
}

/// Actions taken during a lifecycle check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LifecycleAction {
    /// Template was promoted (contains template_id).
    Promoted(String),
    /// Template was demoted (contains template_id).
    Demoted(String),
    /// Template was retired (contains template_id).
    Retired(String),
    /// Template was extracted from patterns (contains template_id).
    Extracted(String),
}

/// Result of a lifecycle check.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleResult {
    pub actions: Vec<LifecycleAction>,
    pub templates_tracked: usize,
}

/// A recorded lifecycle transition event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub id: String,
    pub template_id: String,
    pub action: String,
    pub old_source: String,
    pub new_source: String,
    pub confidence_at_transition: f64,
    pub created_at: String,
}

// ============================================================================
// TemplateLifecycleManager
// ============================================================================

/// Manages the full lifecycle of verification step templates.
pub struct TemplateLifecycleManager {
    /// Minimum successful instances before a pattern can be extracted as a template.
    pub extraction_threshold: usize,
    /// Confidence (success rate) required to promote from Extracted to Promoted.
    pub promotion_threshold: f64,
    /// Confidence (success rate) below which a Promoted template is demoted.
    pub demotion_threshold: f64,
    /// Number of failures (with zero successes) after which a template is retired.
    pub retirement_after_failures: usize,
    /// Minimum total uses before any promote/demote transition is considered.
    pub min_uses_for_transition: usize,
}

impl Default for TemplateLifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateLifecycleManager {
    pub fn new() -> Self {
        Self {
            extraction_threshold: 5,
            promotion_threshold: 0.8,
            demotion_threshold: 0.4,
            retirement_after_failures: 10,
            min_uses_for_transition: 10,
        }
    }

}

// ============================================================================
// Tests
// ============================================================================

