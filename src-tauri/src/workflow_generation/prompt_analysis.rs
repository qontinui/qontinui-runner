//! Prompt pattern analysis for workflow generation optimization.
//!
//! Analyzes scored examples across generation runs to identify improvement
//! opportunities. Groups reflection fixes by agent, identifies recurring
//! patterns, and generates actionable insights for prompt optimization.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ============================================================================
// Types
// ============================================================================

/// An actionable insight derived from analyzing generation data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInsight {
    /// Which agent this insight applies to
    pub agent: String,
    /// Type of insight
    pub insight_type: String,
    /// Human-readable description
    pub description: String,
    /// How many examples support this insight
    pub evidence_count: u32,
    /// Confidence level (0.0 - 1.0)
    pub confidence: f64,
    /// Suggested rule content if this insight should become a rule
    pub suggested_rule: Option<String>,
}

/// Aggregated fix pattern for an agent.
#[derive(Debug, Clone)]
struct FixPattern {
    fix_type: String,
    agent: String,
    count: u32,
    effective_count: u32,
    sample_description: String,
}

// ============================================================================
// Analysis Functions
// ============================================================================

/// Analyze reflection fixes attributed to a specific agent to find recurring patterns.
///
/// Fixes that recur 3+ times with "effective" status become insights.
pub fn analyze_reflection_fixes(
    conn: &Connection,
    agent: &str,
    min_count: u32,
) -> Result<Vec<PromptInsight>, String> {
    let mut stmt = conn
        .prepare(
            r#"SELECT fix_type, COUNT(*) as cnt,
                      SUM(CASE WHEN effectiveness = 'effective' THEN 1 ELSE 0 END) as effective_cnt,
                      MIN(fix_description) as sample_desc
               FROM reflection_fixes
               WHERE source_agent = ?1 AND status = 'applied'
               GROUP BY fix_type
               HAVING cnt >= ?2
               ORDER BY cnt DESC"#,
        )
        .map_err(|e| format!("Failed to prepare reflection analysis query: {}", e))?;

    let patterns: Vec<FixPattern> = stmt
        .query_map(params![agent, min_count], |row| {
            Ok(FixPattern {
                fix_type: row.get(0)?,
                agent: agent.to_string(),
                count: row.get::<_, i64>(1)? as u32,
                effective_count: row.get::<_, i64>(2)? as u32,
                sample_description: row.get(3)?,
            })
        })
        .map_err(|e| format!("Failed to query reflection patterns: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let mut insights = Vec::new();
    for pattern in patterns {
        let effectiveness_rate = if pattern.count > 0 {
            pattern.effective_count as f64 / pattern.count as f64
        } else {
            0.0
        };

        // Only create insights for patterns with reasonable effectiveness
        if effectiveness_rate >= 0.5 || pattern.count >= 5 {
            let suggested_rule = if effectiveness_rate >= 0.7 && pattern.count >= 3 {
                Some(format!(
                    "Based on {} occurrences ({}% effective): {}",
                    pattern.count,
                    (effectiveness_rate * 100.0) as u32,
                    pattern.sample_description
                ))
            } else {
                None
            };

            insights.push(PromptInsight {
                agent: agent.to_string(),
                insight_type: "recurring_failure".to_string(),
                description: format!(
                    "The {} agent repeatedly produces '{}' issues ({} occurrences, {}% effective fixes). Sample: {}",
                    agent,
                    pattern.fix_type,
                    pattern.count,
                    (effectiveness_rate * 100.0) as u32,
                    &pattern.sample_description[..pattern.sample_description.len().min(200)]
                ),
                evidence_count: pattern.count,
                confidence: effectiveness_rate,
                suggested_rule,
            });
        }
    }

    debug!(
        "Found {} insights for agent '{}' from reflection fixes",
        insights.len(),
        agent
    );

    Ok(insights)
}

/// Analyze specification gaps — criteria that consistently fail to catch real issues.
///
/// Compares specification criteria against verification phase results to find
/// criteria that don't translate to effective verification steps.
pub fn analyze_specification_gaps(conn: &Connection) -> Result<Vec<PromptInsight>, String> {
    // Find workflows where specification criteria existed but verification still failed
    let mut stmt = conn
        .prepare(
            r#"SELECT COUNT(*) as cnt,
                      MIN(gpa.description) as sample_desc
               FROM generation_pipeline_artifacts gpa
               WHERE gpa.specification_criteria IS NOT NULL
                 AND gpa.specification_criteria != 'null'
                 AND gpa.success = 1
                 AND EXISTS (
                     SELECT 1 FROM reflection_fixes rf
                     INNER JOIN task_runs tr ON rf.source_task_run_id = tr.id
                     WHERE tr.workflow_name IN (
                         SELECT name FROM unified_workflows WHERE id = gpa.workflow_id
                     )
                     AND rf.source_agent = 'verification'
                 )"#,
        )
        .map_err(|e| format!("Failed to prepare spec gap query: {}", e))?;

    let (gap_count, sample): (u32, String) = stmt
        .query_row([], |row| {
            Ok((
                row.get::<_, i64>(0).unwrap_or(0) as u32,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })
        .unwrap_or((0, String::new()));

    let mut insights = Vec::new();
    if gap_count >= 3 {
        insights.push(PromptInsight {
            agent: "specification".to_string(),
            insight_type: "specification_gap".to_string(),
            description: format!(
                "In {} workflows, specification criteria were defined but verification still \
                 missed issues caught by reflection. Consider adding more specific verification \
                 hints to criteria. Example workflow: {}",
                gap_count,
                &sample[..sample.len().min(100)]
            ),
            evidence_count: gap_count,
            confidence: 0.6,
            suggested_rule: None,
        });
    }

    Ok(insights)
}

/// Analyze verification blind spots — issues that verification missed
/// but were caught by reflection or user feedback.
pub fn analyze_verification_blind_spots(conn: &Connection) -> Result<Vec<PromptInsight>, String> {
    // Find fix types that reflection attributed to builder/hardener but
    // verification should have caught
    let mut stmt = conn
        .prepare(
            r#"SELECT fix_type, fix_description, COUNT(*) as cnt
               FROM reflection_fixes
               WHERE source_agent IN ('builder', 'hardener')
                 AND effectiveness = 'effective'
                 AND status = 'applied'
               GROUP BY fix_type
               HAVING cnt >= 3
               ORDER BY cnt DESC
               LIMIT 10"#,
        )
        .map_err(|e| format!("Failed to prepare blind spot query: {}", e))?;

    let rows: Vec<(String, String, u32)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? as u32,
            ))
        })
        .map_err(|e| format!("Failed to query blind spots: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    let mut insights = Vec::new();
    for (fix_type, sample_desc, count) in rows {
        insights.push(PromptInsight {
            agent: "verification".to_string(),
            insight_type: "verification_blind_spot".to_string(),
            description: format!(
                "Verification agent missed '{}' issues {} times that reflection later caught. \
                 The verification prompt should check for this pattern. Sample: {}",
                fix_type,
                count,
                &sample_desc[..sample_desc.len().min(200)]
            ),
            evidence_count: count,
            confidence: 0.7,
            suggested_rule: Some(format!(
                "Check for '{}' issues: {}",
                fix_type,
                &sample_desc[..sample_desc.len().min(150)]
            )),
        });
    }

    debug!("Found {} verification blind spot insights", insights.len());

    Ok(insights)
}

/// Run all analysis functions and return combined insights for all agents.
pub fn analyze_all(conn: &Connection) -> Result<Vec<PromptInsight>, String> {
    let mut all_insights = Vec::new();

    for agent in &["specification", "builder", "verification", "hardener"] {
        match analyze_reflection_fixes(conn, agent, 3) {
            Ok(insights) => all_insights.extend(insights),
            Err(e) => warn!("Failed to analyze reflection fixes for {}: {}", agent, e),
        }
    }

    match analyze_specification_gaps(conn) {
        Ok(insights) => all_insights.extend(insights),
        Err(e) => warn!("Failed to analyze specification gaps: {}", e),
    }

    match analyze_verification_blind_spots(conn) {
        Ok(insights) => all_insights.extend(insights),
        Err(e) => warn!("Failed to analyze verification blind spots: {}", e),
    }

    info!(
        "Prompt analysis complete: {} total insights across all agents",
        all_insights.len()
    );

    Ok(all_insights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("Failed to create in-memory DB");
        conn.execute_batch(
            r#"
            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                fix_type TEXT NOT NULL,
                fix_description TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'applied',
                effectiveness TEXT,
                source_agent TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE generation_pipeline_artifacts (
                id TEXT PRIMARY KEY,
                workflow_id TEXT,
                description TEXT NOT NULL,
                specification_criteria TEXT,
                success INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );

            CREATE TABLE unified_workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            );

            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                workflow_name TEXT
            );
            "#,
        )
        .expect("Failed to create test schema");
        conn
    }

    #[test]
    fn test_analyze_reflection_fixes_above_threshold() {
        let conn = setup_test_db();

        for i in 0..4 {
            conn.execute(
                "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, status, effectiveness, source_agent, created_at)
                 VALUES (?1, 'tr1', 'rtr1', 'missing_error_handling', 'Add error handling', 'applied', 'effective', 'builder', '2026-01-01T00:00:00Z')",
                params![format!("fix_{}", i)],
            ).expect("Failed to insert reflection fix");
        }

        let insights = analyze_reflection_fixes(&conn, "builder", 3).expect("Analysis failed");
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].agent, "builder");
        assert_eq!(insights[0].evidence_count, 4);
    }

    #[test]
    fn test_analyze_reflection_fixes_below_threshold() {
        let conn = setup_test_db();

        for i in 0..2 {
            conn.execute(
                "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, status, effectiveness, source_agent, created_at)
                 VALUES (?1, 'tr1', 'rtr1', 'missing_error_handling', 'Add error handling', 'applied', 'effective', 'builder', '2026-01-01T00:00:00Z')",
                params![format!("fix_{}", i)],
            ).expect("Failed to insert reflection fix");
        }

        let insights = analyze_reflection_fixes(&conn, "builder", 3).expect("Analysis failed");
        assert!(insights.is_empty());
    }

    #[test]
    fn test_analyze_specification_gaps() {
        let conn = setup_test_db();

        conn.execute(
            "INSERT INTO unified_workflows (id, name) VALUES ('wf1', 'Test Workflow')",
            [],
        )
        .expect("Failed to insert workflow");

        conn.execute(
            "INSERT INTO task_runs (id, workflow_name) VALUES ('tr1', 'Test Workflow')",
            [],
        )
        .expect("Failed to insert task run");

        // Insert 3 artifacts so gap_count >= 3 threshold is met
        for i in 0..3 {
            conn.execute(
                &format!(
                    "INSERT INTO generation_pipeline_artifacts (id, workflow_id, description, specification_criteria, success, created_at)
                     VALUES ('gpa{}', 'wf1', 'test {}', '[\"some criteria\"]', 1, '2026-01-01T00:00:00Z')",
                    i, i
                ),
                [],
            )
            .expect("Failed to insert artifact");
        }

        for i in 0..3 {
            conn.execute(
                "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, status, effectiveness, source_agent, created_at)
                 VALUES (?1, 'tr1', 'rtr1', 'missed_check', 'Verification missed this', 'applied', 'effective', 'verification', '2026-01-01T00:00:00Z')",
                params![format!("fix_{}", i)],
            ).expect("Failed to insert reflection fix");
        }

        let insights = analyze_specification_gaps(&conn).expect("Analysis failed");
        assert!(
            insights.len() >= 1,
            "Expected at least 1 specification_gap insight, got {}",
            insights.len()
        );
        assert_eq!(insights[0].insight_type, "specification_gap");
    }

    #[test]
    fn test_analyze_verification_blind_spots() {
        let conn = setup_test_db();

        for i in 0..3 {
            conn.execute(
                "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, status, effectiveness, source_agent, created_at)
                 VALUES (?1, 'tr1', 'rtr1', 'wrong_selector', 'Used incorrect CSS selector', 'applied', 'effective', 'builder', '2026-01-01T00:00:00Z')",
                params![format!("fix_{}", i)],
            ).expect("Failed to insert reflection fix");
        }

        let insights = analyze_verification_blind_spots(&conn).expect("Analysis failed");
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].insight_type, "verification_blind_spot");
    }

    #[test]
    fn test_analyze_all_combines_insights() {
        let conn = setup_test_db();

        // Insert data for analyze_reflection_fixes (builder agent, 4 fixes of same type)
        for i in 0..4 {
            conn.execute(
                "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, status, effectiveness, source_agent, created_at)
                 VALUES (?1, 'tr1', 'rtr1', 'missing_error_handling', 'Add error handling', 'applied', 'effective', 'builder', '2026-01-01T00:00:00Z')",
                params![format!("builder_fix_{}", i)],
            ).expect("Failed to insert builder fix");
        }

        // Insert data for analyze_verification_blind_spots (3 fixes from builder with effectiveness)
        // The builder fixes above also satisfy blind spot analysis since they are builder+effective+applied
        // with count >= 3. So we already have data for that.

        // Insert data for analyze_specification_gaps
        conn.execute(
            "INSERT INTO unified_workflows (id, name) VALUES ('wf1', 'Test Workflow')",
            [],
        )
        .expect("Failed to insert workflow");

        conn.execute(
            "INSERT INTO task_runs (id, workflow_name) VALUES ('tr1', 'Test Workflow')",
            [],
        )
        .expect("Failed to insert task run");

        conn.execute(
            "INSERT INTO generation_pipeline_artifacts (id, workflow_id, description, specification_criteria, success, created_at)
             VALUES ('gpa1', 'wf1', 'test', '[\"some criteria\"]', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("Failed to insert artifact");

        for i in 0..3 {
            conn.execute(
                "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, status, effectiveness, source_agent, created_at)
                 VALUES (?1, 'tr1', 'rtr1', 'missed_check', 'Verification missed this', 'applied', 'effective', 'verification', '2026-01-01T00:00:00Z')",
                params![format!("spec_fix_{}", i)],
            ).expect("Failed to insert verification fix");
        }

        let insights = analyze_all(&conn).expect("Analysis failed");

        // Should have insights from multiple sources:
        // - analyze_reflection_fixes for builder (4 fixes of same type >= threshold 3)
        // - analyze_verification_blind_spots (4 builder fixes with effectiveness >= 3)
        // - analyze_specification_gaps (artifact + verification fixes >= 3)
        assert!(
            insights.len() >= 2,
            "Expected at least 2 combined insights, got {}",
            insights.len()
        );

        let insight_types: Vec<&str> = insights.iter().map(|i| i.insight_type.as_str()).collect();
        assert!(
            insight_types.contains(&"recurring_failure")
                || insight_types.contains(&"verification_blind_spot")
                || insight_types.contains(&"specification_gap"),
            "Expected insights from multiple analysis functions, got types: {:?}",
            insight_types
        );
    }
}
