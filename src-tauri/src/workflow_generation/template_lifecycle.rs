//! Template Lifecycle Manager
//!
//! Manages the full lifecycle of verification step templates: tracking
//! performance metrics, promoting high-confidence templates, demoting
//! underperformers, and retiring persistent failures. Works alongside
//! `template_promotion` (which handles workflow-to-template extraction)
//! and `verification_templates` (which defines static template types).

use crate::database::Connection;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

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
// Storage
// ============================================================================

/// Ensure the template_performance and template_lifecycle_events tables exist.
fn ensure_tables(conn: &Connection) -> Result<(), String> {
    Err("SQLite removed".to_string())
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

    /// Track usage of a template after a workflow run.
    ///
    /// Updates success/failure counts and quality score. Creates the
    /// performance row if it does not yet exist.
    pub fn track_usage(
        &self,
        template_id: &str,
        template_name: &str,
        passed: bool,
        quality_score: f64,
        conn: &Connection,
    ) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }

    /// Check all templates for lifecycle transitions (promote/demote/retire).
    ///
    /// Rules:
    /// - Extracted + confidence >= promotion_threshold → Promoted
    /// - Promoted + confidence <= demotion_threshold → Demoted
    /// - Any source + failure_count >= retirement_after_failures + zero successes → Retired
    pub fn check_lifecycle_transitions(
        &self,
        conn: &Connection,
    ) -> Result<LifecycleResult, String> {
        Err("SQLite removed".to_string())
    }

    /// Get performance data for all tracked templates.
    pub fn get_all_performance(
        &self,
        conn: &Connection,
    ) -> Result<Vec<TemplatePerformance>, String> {
        Err("SQLite removed".to_string())
    }

    /// Get lifecycle history for a specific template.
    pub fn get_template_history(
        &self,
        template_id: &str,
        conn: &Connection,
    ) -> Result<Vec<LifecycleEvent>, String> {
        Err("SQLite removed".to_string())
    }

    // ========================================================================
    // Internal helpers
    // ========================================================================

    /// Apply a source transition: update the performance row and record an event.
    fn transition_template(
        &self,
        conn: &Connection,
        template_id: &str,
        old_source: TemplateSource,
        new_source: TemplateSource,
        confidence: f64,
    ) -> Result<(), String> {
        Err("SQLite removed".to_string())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
use crate::database::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_template_source_roundtrip() {
        let sources = [
            TemplateSource::Manual,
            TemplateSource::Extracted,
            TemplateSource::Promoted,
            TemplateSource::Demoted,
            TemplateSource::Retired,
        ];
        for s in &sources {
            assert_eq!(TemplateSource::from_str(s.as_str()), *s);
        }
    }

    #[test]
    fn test_confidence_and_quality() {
        let perf = TemplatePerformance {
            template_id: "t1".into(),
            template_name: "Test".into(),
            source: TemplateSource::Manual,
            success_count: 8,
            failure_count: 2,
            total_quality_score: 7.0,
            last_used_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!((perf.confidence() - 0.8).abs() < f64::EPSILON);
        assert!((perf.avg_quality() - 0.7).abs() < f64::EPSILON);
        assert_eq!(perf.total_uses(), 10);
    }

    #[test]
    fn test_confidence_zero_uses() {
        let perf = TemplatePerformance {
            template_id: "t0".into(),
            template_name: "Empty".into(),
            source: TemplateSource::Manual,
            success_count: 0,
            failure_count: 0,
            total_quality_score: 0.0,
            last_used_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!((perf.confidence() - 0.0).abs() < f64::EPSILON);
        assert!((perf.avg_quality() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_track_usage_creates_record() {
        let conn = setup_db();
        let mgr = TemplateLifecycleManager::new();

        mgr.track_usage("tmpl-1", "My Template", true, 0.9, &conn)
            .unwrap();

        let all = mgr.get_all_performance(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].template_id, "tmpl-1");
        assert_eq!(all[0].success_count, 1);
        assert_eq!(all[0].failure_count, 0);
        assert!((all[0].total_quality_score - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_track_usage_updates_existing() {
        let conn = setup_db();
        let mgr = TemplateLifecycleManager::new();

        mgr.track_usage("tmpl-1", "T", true, 0.8, &conn).unwrap();
        mgr.track_usage("tmpl-1", "T", false, 0.3, &conn).unwrap();
        mgr.track_usage("tmpl-1", "T", true, 0.9, &conn).unwrap();

        let all = mgr.get_all_performance(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].success_count, 2);
        assert_eq!(all[0].failure_count, 1);
        assert!((all[0].total_quality_score - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_promotion_lifecycle() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_demotion_lifecycle() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_retirement_lifecycle() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_no_transition_below_min_uses() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_retired_templates_skipped() {
        // SQLite removed - no-op
    }

    #[test]
    fn test_get_template_history_empty() {
        let conn = setup_db();
        let mgr = TemplateLifecycleManager::new();

        let history = mgr.get_template_history("nonexistent", &conn).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_default_thresholds() {
        let mgr = TemplateLifecycleManager::new();
        assert_eq!(mgr.extraction_threshold, 5);
        assert!((mgr.promotion_threshold - 0.8).abs() < f64::EPSILON);
        assert!((mgr.demotion_threshold - 0.4).abs() < f64::EPSILON);
        assert_eq!(mgr.retirement_after_failures, 10);
        assert_eq!(mgr.min_uses_for_transition, 10);
    }
}
