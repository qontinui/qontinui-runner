//! Cross-run learning orchestrator.
//!
//! Called after reflection runs complete. Analyzes patterns across multiple runs,
//! detects recurring issues, auto-disables ineffective rules, and auto-applies
//! known-good fixes for recurring findings.

use crate::database::{cross_run_ops, graph_ops};
use rusqlite::{params, Connection};
use tracing::{info, warn};

/// Run cross-run analysis after a reflection run completes.
/// This is the main entry point called from reflection/trigger.rs.
///
/// Performs:
/// 1. Detect recurring findings (break point #5: cross-run patterns)
/// 2. Detect fix oscillations (fixes that work temporarily then regress)
/// 3. Auto-disable ineffective generation rules (break point #3)
/// 4. Auto-apply known-good fixes for recurring findings (break point #7)
///
/// Returns (patterns_detected, rules_disabled, fixes_auto_applied).
pub fn post_run_analysis(
    conn: &Connection,
    workflow_name: &str,
    task_run_id: &str,
) -> Result<(u32, u32, u32), String> {
    info!(
        "Starting cross-run analysis for workflow '{}' after task run {}",
        workflow_name, task_run_id
    );

    // 1. Detect recurring findings
    let recurring = cross_run_ops::detect_recurring_findings(conn, workflow_name, 3)?;
    let patterns_detected = recurring.len() as u32;
    if patterns_detected > 0 {
        info!(
            "Detected {} recurring finding patterns for '{}'",
            patterns_detected, workflow_name
        );
    }

    // 2. Detect fix oscillations
    let oscillations = cross_run_ops::detect_fix_oscillations(conn, workflow_name)?;
    let oscillation_count = oscillations.len() as u32;
    if oscillation_count > 0 {
        warn!(
            "Detected {} fix oscillation patterns for '{}' — fixes marked effective but findings recurred",
            oscillation_count, workflow_name
        );
    }

    // 3. Auto-disable ineffective rules
    let rules_disabled = auto_disable_ineffective_rules(conn, 3)?;

    // 4. Auto-apply known-good fixes for recurring findings
    let fixes_applied = auto_apply_recurring_fixes(conn, workflow_name, &recurring)?;

    info!(
        "Cross-run analysis complete: {} patterns, {} oscillations, {} rules disabled, {} fixes auto-applied",
        patterns_detected + oscillation_count,
        oscillation_count,
        rules_disabled,
        fixes_applied
    );

    Ok((
        patterns_detected + oscillation_count,
        rules_disabled,
        fixes_applied,
    ))
}

/// Auto-disable generation rules that have been loaded multiple times
/// with no measurable positive effect.
///
/// A rule is disabled when:
/// - It has >= threshold 'no_effect' entries in rule_influence_log
/// - It has 0 'prevented_error' entries
/// - Its source reflection_fix (if any) was evaluated as 'ineffective'
///
/// This closes feedback loop break point #3: "ineffective rules stay active forever"
pub fn auto_disable_ineffective_rules(
    conn: &Connection,
    threshold: u32,
) -> Result<u32, String> {
    let ineffective = graph_ops::get_ineffective_rules(conn, threshold as i64)?;
    let mut disabled_count = 0u32;

    for rule in &ineffective {
        // Double-check: also verify source fix effectiveness if linked
        let should_disable =
            if let Some(source_fix) = get_rule_source_fix_effectiveness(conn, &rule.rule_id)? {
                // If the source fix was evaluated as ineffective or regression, disable
                source_fix == "ineffective" || source_fix == "caused_regression"
            } else {
                // No source fix — rely on the influence log data alone
                rule.no_effect_count >= threshold as i64 && rule.prevented_error_count == 0
            };

        if should_disable {
            conn.execute(
                "UPDATE generation_rules SET status = 'disabled', updated_at = datetime('now') WHERE id = ?1 AND status = 'active'",
                params![rule.rule_id],
            )
            .map_err(|e| format!("Failed to disable rule {}: {}", rule.rule_id, e))?;

            info!(
                "Auto-disabled ineffective generation rule: {} (no_effect={}, prevented={})",
                rule.rule_id, rule.no_effect_count, rule.prevented_error_count
            );
            disabled_count += 1;
        }
    }

    if disabled_count > 0 {
        info!(
            "Disabled {} ineffective generation rules (threshold={})",
            disabled_count, threshold
        );
    }

    Ok(disabled_count)
}

/// For recurring findings, check if there's a known-effective fix that could be reused.
/// Specifically targets selector_fix and tool_config_update fixes that were effective
/// but aren't being auto-applied by the existing system.
///
/// This closes feedback loop break point #7: "selector/config fixes not auto-applied"
fn auto_apply_recurring_fixes(
    conn: &Connection,
    workflow_name: &str,
    recurring_patterns: &[cross_run_ops::CrossRunPattern],
) -> Result<u32, String> {
    let mut applied_count = 0u32;

    for pattern in recurring_patterns {
        // Look for effective fixes for this signature hash
        let effective_fixes: Vec<(String, String, String)> = conn
            .prepare(
                r#"SELECT rf.id, rf.fix_type, rf.fix_description
                   FROM reflection_fixes rf
                   JOIN task_run_findings trf ON rf.source_finding_id = trf.id
                   WHERE trf.signature_hash = ?1
                   AND rf.effectiveness = 'effective'
                   AND rf.fix_type IN ('selector_fix', 'tool_config_update')
                   ORDER BY rf.reuse_count DESC
                   LIMIT 1"#,
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![pattern.signature_hash], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
            })
            .map_err(|e| format!("Failed to find effective fixes: {}", e))?;

        if let Some((fix_id, fix_type, fix_desc)) = effective_fixes.first() {
            // Record the fix application
            let app_id = uuid::Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                r#"INSERT INTO fix_applications (id, fix_id, task_run_id, error_signature_hash, outcome, applied_at)
                   VALUES (?1, ?2, ?3, ?4, 'pending', ?5)"#,
                params![
                    format!("fa-{}", app_id),
                    fix_id,
                    pattern.last_seen_task_run_id,
                    pattern.signature_hash,
                    now
                ],
            )
            .map_err(|e| format!("Failed to insert fix application: {}", e))?;

            // Increment reuse count
            conn.execute(
                "UPDATE reflection_fixes SET reuse_count = reuse_count + 1 WHERE id = ?1",
                params![fix_id],
            )
            .map_err(|e| format!("Failed to increment reuse count: {}", e))?;

            info!(
                "Auto-applied {} fix '{}' for recurring finding in '{}' (signature={}, occurrences={})",
                fix_type, fix_desc, workflow_name, pattern.signature_hash, pattern.occurrence_count
            );
            applied_count += 1;
        }
    }

    Ok(applied_count)
}

/// Get the effectiveness of the reflection fix that created a generation rule.
fn get_rule_source_fix_effectiveness(
    conn: &Connection,
    rule_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        r#"SELECT rf.effectiveness
           FROM generation_rules gr
           JOIN reflection_fixes rf ON gr.source_fix_id = rf.id
           WHERE gr.id = ?1"#,
        params![rule_id],
        |row| row.get(0),
    )
    .map(|v: Option<String>| v)
    .or_else(|e| {
        if e == rusqlite::Error::QueryReturnedNoRows {
            Ok(None)
        } else {
            Err(format!("Failed to get rule source fix: {}", e))
        }
    })
}

/// Route a reflection fix to the correct generation agent based on step provenance
/// rather than step-type heuristic.
///
/// This closes feedback loop break point #1: "fixes dropped if no step type match"
/// When the standard infer_step_type_from_fix() fails, this function queries
/// step_provenance to find which agent created the problematic step and routes
/// the fix as a generation rule for that agent.
///
/// Returns `Some((generating_agent, phase))` if a provenance match is found.
pub fn provenance_based_fix_routing(
    conn: &Connection,
    fix_description: &str,
    file_changed: Option<&str>,
    workflow_id: Option<&str>,
) -> Option<(String, String)> {
    // Need a workflow_id to look up provenance
    let workflow_id = workflow_id?;

    let provenances = graph_ops::get_provenance_for_workflow(conn, workflow_id).ok()?;

    if provenances.is_empty() {
        return None;
    }

    // Find the most recent provenance entry that might relate to this fix
    // by checking if the fix description mentions a step name or phase
    let fix_lower = fix_description.to_lowercase();
    for prov in &provenances {
        if fix_lower.contains(&prov.step_name.to_lowercase())
            || fix_lower.contains(&prov.phase.to_lowercase())
        {
            // Found a match — route to the agent that created this step
            return Some((prov.generating_agent.clone(), prov.phase.clone()));
        }
    }

    // Fallback: if file_changed matches content in a step's JSON, find the agent
    if let Some(file) = file_changed {
        let file_lower = file.to_lowercase();
        for prov in &provenances {
            if let Some(ref json) = prov.final_step_json {
                if json.to_lowercase().contains(&file_lower) {
                    return Some((prov.generating_agent.clone(), prov.phase.clone()));
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create an in-memory SQLite database with all tables needed by cross_run_learning.
    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE task_runs (
                id TEXT PRIMARY KEY,
                task_name TEXT NOT NULL DEFAULT '',
                task_type TEXT NOT NULL DEFAULT 'task',
                status TEXT NOT NULL DEFAULT 'running',
                sessions_count INTEGER NOT NULL DEFAULT 0,
                auto_continue BOOLEAN NOT NULL DEFAULT 1,
                output_log TEXT DEFAULT '',
                workflow_name TEXT,
                workflow_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE unified_workflows (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT DEFAULT '',
                category TEXT DEFAULT 'general',
                tags TEXT DEFAULT '[]',
                setup_steps TEXT DEFAULT '[]',
                verification_steps TEXT DEFAULT '[]',
                agentic_steps TEXT DEFAULT '[]',
                completion_steps TEXT DEFAULT '[]',
                max_iterations INTEGER DEFAULT 10,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE task_run_findings (
                id TEXT PRIMARY KEY,
                task_run_id TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'code_bug',
                severity TEXT NOT NULL DEFAULT 'medium',
                signature_hash TEXT,
                title TEXT NOT NULL DEFAULT '',
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'detected',
                action_type TEXT NOT NULL DEFAULT 'auto_fix',
                detected_in_session INTEGER NOT NULL DEFAULT 0,
                detected_at TEXT NOT NULL DEFAULT (datetime('now')),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE reflection_fixes (
                id TEXT PRIMARY KEY,
                source_task_run_id TEXT NOT NULL,
                reflection_task_run_id TEXT NOT NULL,
                source_finding_id TEXT,
                fix_type TEXT NOT NULL,
                fix_description TEXT NOT NULL,
                file_changed TEXT,
                old_value TEXT,
                new_value TEXT,
                confidence TEXT NOT NULL DEFAULT 'medium',
                status TEXT NOT NULL DEFAULT 'applied',
                effectiveness TEXT,
                applied_at TEXT NOT NULL DEFAULT (datetime('now')),
                evaluated_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                reuse_count INTEGER DEFAULT 0
            );

            CREATE TABLE generation_rules (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                section TEXT NOT NULL,
                rule_number INTEGER NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                provenance TEXT NOT NULL DEFAULT 'seed',
                source_fix_id TEXT,
                confidence REAL DEFAULT 1.0,
                severity TEXT NOT NULL DEFAULT 'normal',
                failure_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE fix_applications (
                id TEXT PRIMARY KEY,
                fix_id TEXT NOT NULL,
                task_run_id TEXT NOT NULL,
                error_signature_hash TEXT,
                outcome TEXT DEFAULT 'pending',
                applied_at TEXT NOT NULL,
                evaluated_at TEXT
            );

            CREATE TABLE cross_run_patterns (
                id TEXT PRIMARY KEY,
                pattern_type TEXT NOT NULL,
                signature_hash TEXT NOT NULL,
                workflow_name TEXT,
                occurrence_count INTEGER NOT NULL DEFAULT 1,
                first_seen_task_run_id TEXT,
                last_seen_task_run_id TEXT,
                first_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_seen_at TEXT NOT NULL DEFAULT (datetime('now')),
                affected_components TEXT,
                pattern_data TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                resolved_by_fix_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(pattern_type, signature_hash)
            );

            CREATE TABLE step_provenance (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL,
                workflow_version_id TEXT,
                step_name TEXT NOT NULL,
                step_index INTEGER NOT NULL,
                phase TEXT NOT NULL,
                generating_agent TEXT NOT NULL,
                generation_iteration INTEGER,
                original_step_json TEXT,
                final_step_json TEXT,
                ui_bridge_event_ids TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE rule_influence_log (
                id TEXT PRIMARY KEY,
                rule_id TEXT NOT NULL,
                task_run_id TEXT NOT NULL,
                workflow_id TEXT,
                influence_type TEXT NOT NULL DEFAULT 'loaded',
                evidence TEXT,
                phase TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_auto_disable_ineffective_rules() {
        let conn = setup_test_db();

        // Create a reflection fix marked 'ineffective'
        conn.execute(
            "INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id, fix_type, fix_description, effectiveness)
             VALUES ('fix-1', 'tr-1', 'rtr-1', 'workflow_step_rewrite', 'Fix login step', 'ineffective')",
            [],
        )
        .unwrap();

        // Create an active generation rule that points to the ineffective fix
        conn.execute(
            "INSERT INTO generation_rules (id, agent, section, rule_number, title, content, status, source_fix_id, created_at, updated_at)
             VALUES ('rule-1', 'builder', 'important_rules', 1, 'Login rule', 'Always check login', 'active', 'fix-1', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        // Insert 3 'no_effect' entries in rule_influence_log (meets threshold of 3)
        for i in 0..3 {
            conn.execute(
                "INSERT INTO rule_influence_log (id, rule_id, task_run_id, influence_type, created_at)
                 VALUES (?1, 'rule-1', ?2, 'no_effect', datetime('now'))",
                params![format!("ri-{}", i), format!("tr-run-{}", i)],
            )
            .unwrap();
        }

        // Act
        let disabled = auto_disable_ineffective_rules(&conn, 3).unwrap();

        // Assert: rule should be disabled
        assert_eq!(disabled, 1);

        let status: String = conn
            .query_row(
                "SELECT status FROM generation_rules WHERE id = 'rule-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "disabled");
    }

    #[test]
    fn test_auto_disable_skips_effective_rules() {
        let conn = setup_test_db();

        // Create a generation rule with NO source fix (will rely on influence log alone)
        conn.execute(
            "INSERT INTO generation_rules (id, agent, section, rule_number, title, content, status, created_at, updated_at)
             VALUES ('rule-eff', 'builder', 'important_rules', 1, 'Good rule', 'Always verify', 'active', datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();

        // Insert 3 'no_effect' entries
        for i in 0..3 {
            conn.execute(
                "INSERT INTO rule_influence_log (id, rule_id, task_run_id, influence_type, created_at)
                 VALUES (?1, 'rule-eff', ?2, 'no_effect', datetime('now'))",
                params![format!("ri-ne-{}", i), format!("tr-ne-{}", i)],
            )
            .unwrap();
        }

        // Also insert 1 'prevented_error' entry — this should protect the rule
        conn.execute(
            "INSERT INTO rule_influence_log (id, rule_id, task_run_id, influence_type, created_at)
             VALUES ('ri-pe-0', 'rule-eff', 'tr-pe-0', 'prevented_error', datetime('now'))",
            [],
        )
        .unwrap();

        // Act
        let disabled = auto_disable_ineffective_rules(&conn, 3).unwrap();

        // Assert: rule should NOT be disabled because it has a 'prevented_error' entry
        assert_eq!(disabled, 0);

        let status: String = conn
            .query_row(
                "SELECT status FROM generation_rules WHERE id = 'rule-eff'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "active");
    }

    #[test]
    fn test_provenance_based_fix_routing() {
        let conn = setup_test_db();

        let workflow_id = "wf-test-1";

        // Use graph_ops::insert_step_provenance so the data goes through the same
        // code path used by the production read query.
        graph_ops::insert_step_provenance(
            &conn,
            workflow_id,
            None,
            "login_step",
            0,
            "setup",
            "builder",
            None,
            None,
            None,
        )
        .unwrap();
        graph_ops::insert_step_provenance(
            &conn,
            workflow_id,
            None,
            "verify_dashboard",
            1,
            "verification",
            "hardener",
            None,
            None,
            None,
        )
        .unwrap();

        // Verify provenance entries exist
        let provs = graph_ops::get_provenance_for_workflow(&conn, workflow_id).unwrap();
        assert_eq!(provs.len(), 2, "Expected 2 provenance entries, got {}", provs.len());

        // Fix description mentions 'login_step' — should match the builder agent
        let result = provenance_based_fix_routing(
            &conn,
            "Fix the login_step selector to use #email-input",
            None,
            Some(workflow_id),
        );

        assert!(result.is_some(), "Expected Some routing result but got None");
        let (agent, phase) = result.unwrap();
        assert_eq!(agent, "builder");
        assert_eq!(phase, "setup");
    }

    #[test]
    fn test_provenance_based_fix_routing_no_match() {
        let conn = setup_test_db();

        // No provenance entries exist for this workflow
        let result = provenance_based_fix_routing(
            &conn,
            "Fix something unrelated",
            None,
            Some("wf-nonexistent"),
        );

        assert!(result.is_none());
    }
}
