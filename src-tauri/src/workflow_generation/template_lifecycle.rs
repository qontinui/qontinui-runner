//! Template Lifecycle Manager
//!
//! Manages the full lifecycle of verification step templates: tracking
//! performance metrics, promoting high-confidence templates, demoting
//! underperformers, and retiring persistent failures. Works alongside
//! `template_promotion` (which handles workflow-to-template extraction)
//! and `verification_templates` (which defines static template types).

use chrono::Utc;
use rusqlite::{params, Connection};
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
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS template_performance (
            template_id TEXT PRIMARY KEY,
            template_name TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'manual',
            success_count INTEGER NOT NULL DEFAULT 0,
            failure_count INTEGER NOT NULL DEFAULT 0,
            total_quality_score REAL NOT NULL DEFAULT 0.0,
            last_used_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS template_lifecycle_events (
            id TEXT PRIMARY KEY,
            template_id TEXT NOT NULL,
            action TEXT NOT NULL,
            old_source TEXT NOT NULL,
            new_source TEXT NOT NULL,
            confidence_at_transition REAL NOT NULL DEFAULT 0.0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_lifecycle_events_template
            ON template_lifecycle_events(template_id);",
    )
    .map_err(|e| format!("Failed to create template lifecycle tables: {}", e))
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
        ensure_tables(conn)?;

        let now = Utc::now().to_rfc3339();

        // Upsert: try update first, then insert if no row was touched
        let updated = conn
            .execute(
                "UPDATE template_performance
                 SET success_count = success_count + ?1,
                     failure_count = failure_count + ?2,
                     total_quality_score = total_quality_score + ?3,
                     last_used_at = ?4,
                     updated_at = ?4
                 WHERE template_id = ?5",
                params![
                    if passed { 1 } else { 0 },
                    if passed { 0 } else { 1 },
                    quality_score,
                    now,
                    template_id,
                ],
            )
            .map_err(|e| format!("Failed to update template_performance: {}", e))?;

        if updated == 0 {
            let source = TemplateSource::Manual;
            conn.execute(
                "INSERT INTO template_performance
                 (template_id, template_name, source, success_count, failure_count,
                  total_quality_score, last_used_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    template_id,
                    template_name,
                    source.as_str(),
                    if passed { 1u32 } else { 0u32 },
                    if passed { 0u32 } else { 1u32 },
                    quality_score,
                    now,
                    now,
                ],
            )
            .map_err(|e| format!("Failed to insert template_performance: {}", e))?;

            debug!(
                "Created performance record for template '{}' ({})",
                template_name, template_id,
            );
        } else {
            debug!(
                "Updated performance for template '{}': passed={}, quality={:.2}",
                template_id, passed, quality_score,
            );
        }

        Ok(())
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
        ensure_tables(conn)?;

        let templates = self.get_all_performance(conn)?;
        let templates_tracked = templates.len();
        let mut actions: Vec<LifecycleAction> = Vec::new();

        for t in &templates {
            // Skip already-retired templates
            if t.source == TemplateSource::Retired {
                continue;
            }

            // Retirement check: many failures, zero successes — regardless of min_uses
            if t.failure_count as usize >= self.retirement_after_failures && t.success_count == 0 {
                let old_source = t.source;
                let new_source = TemplateSource::Retired;

                self.transition_template(conn, &t.template_id, old_source, new_source, t.confidence())?;

                info!(
                    "Retired template '{}' ({}) — {} failures with 0 successes",
                    t.template_name, t.template_id, t.failure_count,
                );
                actions.push(LifecycleAction::Retired(t.template_id.clone()));
                continue;
            }

            // Need enough uses before considering promote/demote
            if t.total_uses() < self.min_uses_for_transition as u32 {
                continue;
            }

            let confidence = t.confidence();

            match t.source {
                TemplateSource::Extracted if confidence >= self.promotion_threshold => {
                    self.transition_template(
                        conn,
                        &t.template_id,
                        TemplateSource::Extracted,
                        TemplateSource::Promoted,
                        confidence,
                    )?;

                    info!(
                        "Promoted template '{}' ({}) — confidence {:.2} >= {:.2}",
                        t.template_name, t.template_id, confidence, self.promotion_threshold,
                    );
                    actions.push(LifecycleAction::Promoted(t.template_id.clone()));
                }
                TemplateSource::Promoted if confidence <= self.demotion_threshold => {
                    self.transition_template(
                        conn,
                        &t.template_id,
                        TemplateSource::Promoted,
                        TemplateSource::Demoted,
                        confidence,
                    )?;

                    warn!(
                        "Demoted template '{}' ({}) — confidence {:.2} <= {:.2}",
                        t.template_name, t.template_id, confidence, self.demotion_threshold,
                    );
                    actions.push(LifecycleAction::Demoted(t.template_id.clone()));
                }
                _ => {}
            }
        }

        if !actions.is_empty() {
            info!(
                "Lifecycle check complete: {} actions across {} tracked templates",
                actions.len(),
                templates_tracked,
            );
        } else {
            debug!(
                "Lifecycle check complete: no transitions needed ({} templates tracked)",
                templates_tracked,
            );
        }

        Ok(LifecycleResult {
            actions,
            templates_tracked,
        })
    }

    /// Get performance data for all tracked templates.
    pub fn get_all_performance(
        &self,
        conn: &Connection,
    ) -> Result<Vec<TemplatePerformance>, String> {
        ensure_tables(conn)?;

        let mut stmt = conn
            .prepare(
                "SELECT template_id, template_name, source, success_count, failure_count,
                        total_quality_score, last_used_at, created_at, updated_at
                 FROM template_performance
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| format!("Failed to prepare template_performance query: {}", e))?;

        let rows: Vec<TemplatePerformance> = stmt
            .query_map([], |row| {
                let source_str: String = row.get(2)?;
                Ok(TemplatePerformance {
                    template_id: row.get(0)?,
                    template_name: row.get(1)?,
                    source: TemplateSource::from_str(&source_str),
                    success_count: row.get(3)?,
                    failure_count: row.get(4)?,
                    total_quality_score: row.get(5)?,
                    last_used_at: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })
            .map_err(|e| format!("Failed to query template_performance: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    /// Get lifecycle history for a specific template.
    pub fn get_template_history(
        &self,
        template_id: &str,
        conn: &Connection,
    ) -> Result<Vec<LifecycleEvent>, String> {
        ensure_tables(conn)?;

        let mut stmt = conn
            .prepare(
                "SELECT id, template_id, action, old_source, new_source,
                        confidence_at_transition, created_at
                 FROM template_lifecycle_events
                 WHERE template_id = ?1
                 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("Failed to prepare lifecycle_events query: {}", e))?;

        let rows: Vec<LifecycleEvent> = stmt
            .query_map(params![template_id], |row| {
                Ok(LifecycleEvent {
                    id: row.get(0)?,
                    template_id: row.get(1)?,
                    action: row.get(2)?,
                    old_source: row.get(3)?,
                    new_source: row.get(4)?,
                    confidence_at_transition: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|e| format!("Failed to query lifecycle_events: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
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
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "UPDATE template_performance SET source = ?1, updated_at = ?2 WHERE template_id = ?3",
            params![new_source.as_str(), now, template_id],
        )
        .map_err(|e| format!("Failed to update template source: {}", e))?;

        let action_str = match new_source {
            TemplateSource::Promoted => "promoted",
            TemplateSource::Demoted => "demoted",
            TemplateSource::Retired => "retired",
            TemplateSource::Extracted => "extracted",
            TemplateSource::Manual => "manual",
        };

        let event_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO template_lifecycle_events
             (id, template_id, action, old_source, new_source, confidence_at_transition, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event_id,
                template_id,
                action_str,
                old_source.as_str(),
                new_source.as_str(),
                confidence,
                now,
            ],
        )
        .map_err(|e| format!("Failed to insert lifecycle event: {}", e))?;

        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

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
        let conn = setup_db();
        let mut mgr = TemplateLifecycleManager::new();
        mgr.min_uses_for_transition = 5;
        mgr.promotion_threshold = 0.8;

        // Seed an extracted template with high confidence
        conn.execute(
            "INSERT INTO template_performance
             (template_id, template_name, source, success_count, failure_count,
              total_quality_score, created_at, updated_at)
             VALUES ('tmpl-x', 'Extracted T', 'extracted', 5, 0, 4.5,
                     datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = mgr.check_lifecycle_transitions(&conn).unwrap();
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(&result.actions[0], LifecycleAction::Promoted(id) if id == "tmpl-x"));

        // Verify source changed in DB
        let all = mgr.get_all_performance(&conn).unwrap();
        let t = all.iter().find(|t| t.template_id == "tmpl-x").unwrap();
        assert_eq!(t.source, TemplateSource::Promoted);

        // Verify event was recorded
        let history = mgr.get_template_history("tmpl-x", &conn).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].action, "promoted");
        assert_eq!(history[0].old_source, "extracted");
        assert_eq!(history[0].new_source, "promoted");
    }

    #[test]
    fn test_demotion_lifecycle() {
        let conn = setup_db();
        let mut mgr = TemplateLifecycleManager::new();
        mgr.min_uses_for_transition = 5;
        mgr.demotion_threshold = 0.4;

        // Seed a promoted template with low confidence
        conn.execute(
            "INSERT INTO template_performance
             (template_id, template_name, source, success_count, failure_count,
              total_quality_score, created_at, updated_at)
             VALUES ('tmpl-d', 'Demoted T', 'promoted', 1, 5, 1.0,
                     datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = mgr.check_lifecycle_transitions(&conn).unwrap();
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(&result.actions[0], LifecycleAction::Demoted(id) if id == "tmpl-d"));

        let all = mgr.get_all_performance(&conn).unwrap();
        let t = all.iter().find(|t| t.template_id == "tmpl-d").unwrap();
        assert_eq!(t.source, TemplateSource::Demoted);
    }

    #[test]
    fn test_retirement_lifecycle() {
        let conn = setup_db();
        let mut mgr = TemplateLifecycleManager::new();
        mgr.retirement_after_failures = 3;

        // Seed a template with only failures
        conn.execute(
            "INSERT INTO template_performance
             (template_id, template_name, source, success_count, failure_count,
              total_quality_score, created_at, updated_at)
             VALUES ('tmpl-r', 'Retired T', 'extracted', 0, 3, 0.0,
                     datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = mgr.check_lifecycle_transitions(&conn).unwrap();
        assert_eq!(result.actions.len(), 1);
        assert!(matches!(&result.actions[0], LifecycleAction::Retired(id) if id == "tmpl-r"));

        let all = mgr.get_all_performance(&conn).unwrap();
        let t = all.iter().find(|t| t.template_id == "tmpl-r").unwrap();
        assert_eq!(t.source, TemplateSource::Retired);
    }

    #[test]
    fn test_no_transition_below_min_uses() {
        let conn = setup_db();
        let mut mgr = TemplateLifecycleManager::new();
        mgr.min_uses_for_transition = 10;

        // Extracted with high confidence but only 3 uses — should not promote
        conn.execute(
            "INSERT INTO template_performance
             (template_id, template_name, source, success_count, failure_count,
              total_quality_score, created_at, updated_at)
             VALUES ('tmpl-low', 'Low Uses', 'extracted', 3, 0, 2.7,
                     datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = mgr.check_lifecycle_transitions(&conn).unwrap();
        assert!(result.actions.is_empty());
    }

    #[test]
    fn test_retired_templates_skipped() {
        let conn = setup_db();
        let mgr = TemplateLifecycleManager::new();

        conn.execute(
            "INSERT INTO template_performance
             (template_id, template_name, source, success_count, failure_count,
              total_quality_score, created_at, updated_at)
             VALUES ('tmpl-dead', 'Dead', 'retired', 0, 20, 0.0,
                     datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        let result = mgr.check_lifecycle_transitions(&conn).unwrap();
        assert!(result.actions.is_empty());
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
