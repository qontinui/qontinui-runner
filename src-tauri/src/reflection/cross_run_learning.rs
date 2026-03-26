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
/// 5. Extract procedural skills from effective fix patterns (hermes-agent-inspired)
///
/// Returns (patterns_detected, rules_disabled, fixes_auto_applied).
/// Also returns extracted skills via the `extracted_skills` out-parameter so
/// the async caller can emit `skill.created` events on the workflow event bus.
pub fn post_run_analysis(
    conn: &Connection,
    workflow_name: &str,
    task_run_id: &str,
) -> Result<(u32, u32, u32), String> {
    let (_p, _r, _f, _skills) = post_run_analysis_with_skills(conn, workflow_name, task_run_id)?;
    Ok((_p, _r, _f))
}

/// Same as `post_run_analysis` but also returns extracted skills for event emission.
pub fn post_run_analysis_with_skills(
    conn: &Connection,
    workflow_name: &str,
    task_run_id: &str,
) -> Result<(u32, u32, u32, Vec<ExtractedSkill>), String> {
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

    // 5. Extract procedural skills from effective fix patterns (hermes-agent-inspired)
    let extracted_skills = extract_procedural_skills_detailed(conn, workflow_name, task_run_id, 3)?;
    if !extracted_skills.is_empty() {
        info!(
            "Extracted {} procedural skills from task run {}",
            extracted_skills.len(), task_run_id
        );
    }

    info!(
        "Cross-run analysis complete: {} patterns, {} oscillations, {} rules disabled, {} fixes auto-applied, {} skills extracted",
        patterns_detected + oscillation_count,
        oscillation_count,
        rules_disabled,
        fixes_applied,
        extracted_skills.len()
    );

    Ok((
        patterns_detected + oscillation_count,
        rules_disabled,
        fixes_applied,
        extracted_skills,
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

/// A skill that was auto-extracted during cross-run analysis.
/// Returned to the caller so they can emit async events on the workflow event bus.
#[derive(Debug, Clone)]
pub struct ExtractedSkill {
    pub skill_id: String,
    pub skill_slug: String,
    pub source_fix_id: String,
    pub source_task_run_id: String,
    pub workflow_name: String,
}

/// Extract procedural skills from successful complex task runs.
///
/// Inspired by hermes-agent's self-improving procedural memory: when a task run
/// succeeds after significant effort (multiple iterations, effective fixes), the
/// fix pattern is captured as a reusable skill with source="auto".
///
/// Skills are created with:
/// - A descriptive name derived from the fix type and target component
/// - The signature_hash embedded in the description for knowledge graph linking
/// - Approval_status="pending" (requires review before injection into prompts)
/// - A single-step template containing the fix as a command or prompt
///
/// Returns the number of skills created.
pub fn extract_procedural_skills(
    conn: &Connection,
    workflow_name: &str,
    task_run_id: &str,
    min_iterations: i64,
) -> Result<u32, String> {
    let skills = extract_procedural_skills_detailed(conn, workflow_name, task_run_id, min_iterations)?;
    Ok(skills.len() as u32)
}

/// Same as `extract_procedural_skills` but returns details of each created skill
/// so the caller can emit `skill.created` events on the workflow event bus.
pub fn extract_procedural_skills_detailed(
    conn: &Connection,
    workflow_name: &str,
    task_run_id: &str,
    min_iterations: i64,
) -> Result<Vec<ExtractedSkill>, String> {
    // Only extract from successful runs with significant iteration counts
    let run_info: Option<(String, i64)> = conn
        .query_row(
            r#"SELECT status, sessions_count
               FROM task_runs
               WHERE id = ?1"#,
            params![task_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(|v| Some(v))
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(format!("Failed to query task run: {}", e))
            }
        })?;

    let (_status, iterations) = match run_info {
        Some((s, i)) if s == "completed" && i >= min_iterations => (s, i),
        _ => return Ok(Vec::new()), // Skip non-qualifying runs
    };

    // Find effective fixes from this run that aren't already captured as skills
    let effective_fixes: Vec<(String, String, String, Option<String>, Option<String>, Option<String>)> = conn
        .prepare(
            r#"SELECT rf.id, rf.fix_type, rf.fix_description,
                      rf.file_changed, rf.old_value, rf.new_value
               FROM reflection_fixes rf
               WHERE rf.source_task_run_id = ?1
               AND rf.effectiveness = 'effective'
               AND rf.reuse_count >= 2
               AND rf.fix_type NOT IN ('no_change', 'manual_override')
               AND NOT EXISTS (
                   SELECT 1 FROM user_skills us
                   WHERE us.source = 'auto'
                   AND us.description LIKE '%' || rf.id || '%'
               )
               ORDER BY rf.reuse_count DESC
               LIMIT 5"#,
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![task_run_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .map_err(|e| format!("Failed to query effective fixes: {}", e))?;

    if effective_fixes.is_empty() {
        return Ok(Vec::new());
    }

    let mut extracted: Vec<ExtractedSkill> = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for (fix_id, fix_type, fix_desc, file_changed, _old_value, new_value) in &effective_fixes {
        // Generate a slug from the fix type and affected file.
        // Normalize Windows backslashes before extracting the filename component.
        let normalized = file_changed
            .as_deref()
            .map(|f| f.replace('\\', "/"));
        let component = normalized
            .as_deref()
            .and_then(|f| f.rsplit('/').next())
            .unwrap_or("general");
        let raw_slug = format!(
            "auto-{}-{}",
            fix_type.replace('_', "-"),
            component.replace('.', "-").to_lowercase()
        );
        // Truncate to 80 chars and strip non-alphanumeric/hyphen characters
        let slug: String = raw_slug
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .take(80)
            .collect();

        // Check for existing skill with same slug
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM user_skills WHERE slug = ?1",
                params![slug],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        if exists {
            continue;
        }

        let skill_id = format!("auto:{}", slug);
        let name = format!(
            "Auto: {} ({})",
            fix_desc.chars().take(60).collect::<String>(),
            fix_type
        );

        // Build description with provenance links
        let description = format!(
            "Procedurally extracted from workflow '{}' (task_run: {}, fix: {}). \
             Fix type: {}. Iterations: {}. {}",
            workflow_name,
            task_run_id,
            fix_id,
            fix_type,
            iterations,
            file_changed
                .as_deref()
                .map(|f| format!("File: {}", f))
                .unwrap_or_default()
        );

        // Create a single-step template based on the fix type
        let step = match fix_type.as_str() {
            "workflow_step_rewrite" | "selector_fix" | "tool_config_update" => {
                // For step rewrites, store the new value as a prompt step
                let content = new_value.as_deref().unwrap_or(fix_desc);
                serde_json::json!({
                    "kind": "single_step",
                    "step": {
                        "step_type": "prompt",
                        "name": format!("Apply {} fix", fix_type),
                        "prompt": format!("Apply the following fix: {}", content),
                        "description": fix_desc
                    }
                })
            }
            _ => {
                serde_json::json!({
                    "kind": "single_step",
                    "step": {
                        "step_type": "prompt",
                        "name": format!("Apply {} fix", fix_type),
                        "prompt": format!("Apply fix: {}", fix_desc),
                        "description": fix_desc
                    }
                })
            }
        };
        let template_json = serde_json::to_string(&step).unwrap_or_default();

        // Determine category from fix type
        let category = match fix_type.as_str() {
            "selector_fix" | "tool_config_update" => "testing",
            "workflow_step_rewrite" => "ai-task",
            _ => "custom",
        };

        // Look up matching cross_run_pattern via the fix's source finding signature_hash
        let source_pattern_id: Option<String> = conn
            .query_row(
                r#"SELECT crp.id FROM cross_run_patterns crp
                   INNER JOIN task_run_findings trf ON trf.signature_hash = crp.signature_hash
                   INNER JOIN reflection_fixes rf ON rf.source_finding_id = trf.id
                   WHERE rf.id = ?1
                   LIMIT 1"#,
                params![fix_id],
                |row| row.get(0),
            )
            .ok();

        // Insert the skill with source provenance FKs
        let result = conn.execute(
            r#"INSERT INTO user_skills (
                id, name, slug, description, category, tags, icon, color,
                allowed_phases, parameters, template, source,
                version, usage_count, approval_status,
                source_fix_id, source_pattern_id,
                created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"#,
            params![
                skill_id,
                name,
                slug,
                description,
                category,
                format!("[\"auto\",\"{}\"]", fix_type),
                "zap",          // icon
                "amber",        // color
                "[\"agentic\"]", // allowed_phases — skills are injected during agentic phase
                "[]",           // parameters — auto skills have no user-configurable params
                template_json,
                "auto",         // source
                "1.0.0",        // version
                0i64,           // usage_count
                "pending",      // approval_status — requires review
                fix_id,         // source_fix_id — FK to reflection_fixes
                source_pattern_id, // source_pattern_id — FK to cross_run_patterns
                now,
                now,
            ],
        );

        match result {
            Ok(_) => {
                info!(
                    "Extracted procedural skill '{}' from fix {} in workflow '{}'",
                    slug, fix_id, workflow_name
                );
                extracted.push(ExtractedSkill {
                    skill_id: skill_id.clone(),
                    skill_slug: slug.clone(),
                    source_fix_id: fix_id.clone(),
                    source_task_run_id: task_run_id.to_string(),
                    workflow_name: workflow_name.to_string(),
                });
            }
            Err(e) => {
                warn!(
                    "Failed to create procedural skill '{}': {}",
                    slug, e
                );
            }
        }
    }

    if !extracted.is_empty() {
        info!(
            "Extracted {} procedural skills from task run {} (workflow '{}')",
            extracted.len(), task_run_id, workflow_name
        );
    }

    Ok(extracted)
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

            CREATE TABLE user_skills (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                slug TEXT NOT NULL UNIQUE,
                description TEXT DEFAULT '',
                category TEXT DEFAULT 'custom',
                tags TEXT DEFAULT '[]',
                icon TEXT DEFAULT 'puzzle',
                color TEXT DEFAULT 'gray',
                allowed_phases TEXT DEFAULT '["setup"]',
                parameters TEXT DEFAULT '[]',
                template TEXT NOT NULL DEFAULT '{}',
                source TEXT DEFAULT 'user',
                version TEXT DEFAULT '1.0.0',
                author TEXT,
                checksum TEXT,
                depends_on TEXT,
                usage_count INTEGER DEFAULT 0,
                approval_status TEXT DEFAULT 'pending',
                forked_from TEXT,
                source_fix_id TEXT,
                source_pattern_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
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

    #[test]
    fn test_extract_procedural_skills() {
        let conn = setup_test_db();

        // Create a completed task run with enough iterations
        conn.execute(
            "INSERT INTO task_runs (id, task_name, status, sessions_count, workflow_name)
             VALUES ('tr-skill-1', 'Test task', 'completed', 5, 'test-workflow')",
            [],
        )
        .unwrap();

        // Create an effective fix
        conn.execute(
            r#"INSERT INTO reflection_fixes (id, source_task_run_id, reflection_task_run_id,
                fix_type, fix_description, file_changed, effectiveness, reuse_count)
               VALUES ('fix-skill-1', 'tr-skill-1', 'rtr-1', 'selector_fix',
                       'Use #email-input instead of .login-field',
                       'src/tests/login.spec.ts', 'effective', 2)"#,
            [],
        )
        .unwrap();

        // Extract skills
        let created = extract_procedural_skills(&conn, "test-workflow", "tr-skill-1", 3).unwrap();
        assert_eq!(created, 1, "Should have created 1 skill");

        // Verify skill was inserted
        let skill_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_skills WHERE source = 'auto'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(skill_count, 1);

        // Verify skill properties
        let (name, approval, category): (String, String, String) = conn
            .query_row(
                "SELECT name, approval_status, category FROM user_skills WHERE source = 'auto'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(name.contains("selector_fix"), "Name should reference fix type");
        assert_eq!(approval, "pending", "Auto skills should start as pending");
        assert_eq!(category, "testing", "Selector fixes should be categorized as testing");

        // Running extraction again should not create duplicates
        let created_again = extract_procedural_skills(&conn, "test-workflow", "tr-skill-1", 3).unwrap();
        assert_eq!(created_again, 0, "Should not create duplicate skills");
    }

    #[test]
    fn test_extract_procedural_skills_skips_non_qualifying_runs() {
        let conn = setup_test_db();

        // Create a failed task run
        conn.execute(
            "INSERT INTO task_runs (id, task_name, status, sessions_count, workflow_name)
             VALUES ('tr-fail-1', 'Failed task', 'failed', 5, 'test-workflow')",
            [],
        )
        .unwrap();

        let created = extract_procedural_skills(&conn, "test-workflow", "tr-fail-1", 3).unwrap();
        assert_eq!(created, 0, "Should not extract skills from failed runs");

        // Create a successful but low-iteration run
        conn.execute(
            "INSERT INTO task_runs (id, task_name, status, sessions_count, workflow_name)
             VALUES ('tr-easy-1', 'Easy task', 'completed', 1, 'test-workflow')",
            [],
        )
        .unwrap();

        let created = extract_procedural_skills(&conn, "test-workflow", "tr-easy-1", 3).unwrap();
        assert_eq!(created, 0, "Should not extract skills from low-iteration runs");
    }
}
