//! Generation Rules — Externalized workflow generation rules stored in SQLite.
//!
//! Rules that govern workflow generation (schema context, hardener, verification)
//! are stored in the `generation_rules` table. This allows the reflection system
//! to create/modify rules at runtime without Rust recompilation.

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};
use uuid::Uuid;

use crate::reflection::types::ReflectionFix;
use crate::str_utils::truncate_str;

/// Severity tiers for progressive rule loading.
///
/// Controls how many rules are loaded based on the generation phase:
/// - `Critical` — only critical rules (always-on, high-signal, ~5 rules)
/// - `Important` — critical + important rules (default for initial generation)
/// - `Full` — all rules including normal and hint (used in fixer iterations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleTier {
    Critical,
    Important,
    Full,
}

/// A single generation rule stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationRule {
    pub id: String,
    pub agent: String,
    pub section: String,
    pub rule_number: i32,
    pub title: String,
    pub content: String,
    pub condition: Option<String>,
    pub status: String,
    pub provenance: String,
    pub source_fix_id: Option<String>,
    pub severity: String,
    pub failure_count: i32,
    pub examples_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for inserting a new rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertRuleInput {
    pub agent: String,
    pub section: String,
    pub rule_number: i32,
    pub title: String,
    pub content: String,
    pub condition: Option<String>,
    pub provenance: String,
    pub source_fix_id: Option<String>,
    pub severity: Option<String>,
    pub examples_json: Option<String>,
}

/// Input for updating an existing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRuleInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub condition: Option<String>,
    pub status: Option<String>,
    pub rule_number: Option<i32>,
    /// Severity level: 'critical', 'important', 'normal', 'hint'
    pub severity: Option<String>,
    /// JSON array of positive/negative examples
    pub examples_json: Option<String>,
}

/// Query parameters for listing rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListRulesQuery {
    pub agent: Option<String>,
    pub section: Option<String>,
    pub status: Option<String>,
    pub provenance: Option<String>,
    /// Filter by severity level: 'critical', 'important', 'normal', 'hint'
    pub severity: Option<String>,
}

// ============================================================================
// Loading Functions
// ============================================================================

/// Load active rules for a specific agent and section, ordered by rule_number.
pub fn load_rules(conn: &Connection, agent: &str, section: &str) -> Vec<GenerationRule> {
    let mut stmt = match conn.prepare(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, failure_count, examples_json, created_at, updated_at
         FROM generation_rules
         WHERE agent = ?1 AND section = ?2 AND status = 'active'
         ORDER BY rule_number",
    ) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to prepare load_rules query: {}", e);
            return vec![];
        }
    };

    let rows = stmt.query_map(params![agent, section], |row| {
        Ok(GenerationRule {
            id: row.get(0)?,
            agent: row.get(1)?,
            section: row.get(2)?,
            rule_number: row.get(3)?,
            title: row.get(4)?,
            content: row.get(5)?,
            condition: row.get(6)?,
            status: row.get(7)?,
            provenance: row.get(8)?,
            source_fix_id: row.get(9)?,
            severity: row.get(10)?,
            failure_count: row.get(11)?,
            examples_json: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    });

    match rows {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            warn!("Failed to execute load_rules query: {}", e);
            vec![]
        }
    }
}

/// Load active rules filtered by severity tier for progressive loading.
///
/// - `Critical` loads only `severity = 'critical'` rules
/// - `Important` loads `'critical'` and `'important'` rules
/// - `Full` loads all severities (`'critical'`, `'important'`, `'normal'`, `'hint'`)
pub fn load_rules_progressive(
    conn: &Connection,
    agent: &str,
    section: &str,
    tier: RuleTier,
) -> Vec<GenerationRule> {
    let severity_clause = match tier {
        RuleTier::Critical => "AND severity = 'critical'",
        RuleTier::Important => "AND severity IN ('critical', 'important')",
        RuleTier::Full => "", // no filter — all severities
    };

    let sql = format!(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, failure_count, examples_json, created_at, updated_at
         FROM generation_rules
         WHERE agent = ?1 AND section = ?2 AND status = 'active' {}
         ORDER BY rule_number",
        severity_clause
    );

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to prepare load_rules_progressive query: {}", e);
            return vec![];
        }
    };

    let rows = stmt.query_map(params![agent, section], |row| {
        Ok(GenerationRule {
            id: row.get(0)?,
            agent: row.get(1)?,
            section: row.get(2)?,
            rule_number: row.get(3)?,
            title: row.get(4)?,
            content: row.get(5)?,
            condition: row.get(6)?,
            status: row.get(7)?,
            provenance: row.get(8)?,
            source_fix_id: row.get(9)?,
            severity: row.get(10)?,
            failure_count: row.get(11)?,
            examples_json: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    });

    match rows {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            warn!("Failed to execute load_rules_progressive query: {}", e);
            vec![]
        }
    }
}

/// Load all active rules for an agent, grouped by section.
pub fn load_rules_by_agent(conn: &Connection, agent: &str) -> HashMap<String, Vec<GenerationRule>> {
    let mut stmt = match conn.prepare(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, failure_count, examples_json, created_at, updated_at
         FROM generation_rules
         WHERE agent = ?1 AND status = 'active'
         ORDER BY section, rule_number",
    ) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to prepare load_rules_by_agent query: {}", e);
            return HashMap::new();
        }
    };

    let rows = stmt.query_map(params![agent], |row| {
        Ok(GenerationRule {
            id: row.get(0)?,
            agent: row.get(1)?,
            section: row.get(2)?,
            rule_number: row.get(3)?,
            title: row.get(4)?,
            content: row.get(5)?,
            condition: row.get(6)?,
            status: row.get(7)?,
            provenance: row.get(8)?,
            source_fix_id: row.get(9)?,
            severity: row.get(10)?,
            failure_count: row.get(11)?,
            examples_json: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    });

    let mut result: HashMap<String, Vec<GenerationRule>> = HashMap::new();
    if let Ok(mapped) = rows {
        for rule in mapped.flatten() {
            result.entry(rule.section.clone()).or_default().push(rule);
        }
    }
    result
}

/// List rules with optional filters.
pub fn list_rules(
    conn: &Connection,
    query: &ListRulesQuery,
) -> Result<Vec<GenerationRule>, String> {
    let mut sql = String::from(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, failure_count, examples_json, created_at, updated_at
         FROM generation_rules WHERE 1=1",
    );
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref agent) = query.agent {
        params_vec.push(Box::new(agent.clone()));
        sql.push_str(&format!(" AND agent = ?{}", params_vec.len()));
    }
    if let Some(ref section) = query.section {
        params_vec.push(Box::new(section.clone()));
        sql.push_str(&format!(" AND section = ?{}", params_vec.len()));
    }
    if let Some(ref status) = query.status {
        params_vec.push(Box::new(status.clone()));
        sql.push_str(&format!(" AND status = ?{}", params_vec.len()));
    }
    if let Some(ref provenance) = query.provenance {
        params_vec.push(Box::new(provenance.clone()));
        sql.push_str(&format!(" AND provenance = ?{}", params_vec.len()));
    }
    if let Some(ref severity) = query.severity {
        params_vec.push(Box::new(severity.clone()));
        sql.push_str(&format!(" AND severity = ?{}", params_vec.len()));
    }
    sql.push_str(" ORDER BY agent, section, rule_number");

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare list_rules query: {}", e))?;

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(GenerationRule {
                id: row.get(0)?,
                agent: row.get(1)?,
                section: row.get(2)?,
                rule_number: row.get(3)?,
                title: row.get(4)?,
                content: row.get(5)?,
                condition: row.get(6)?,
                status: row.get(7)?,
                provenance: row.get(8)?,
                source_fix_id: row.get(9)?,
                severity: row.get(10)?,
                failure_count: row.get(11)?,
                examples_json: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        })
        .map_err(|e| format!("Failed to execute list_rules query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Get a single rule by ID.
pub fn get_rule(conn: &Connection, id: &str) -> Result<Option<GenerationRule>, String> {
    let result = conn.query_row(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, failure_count, examples_json, created_at, updated_at
         FROM generation_rules WHERE id = ?1",
        params![id],
        |row| {
            Ok(GenerationRule {
                id: row.get(0)?,
                agent: row.get(1)?,
                section: row.get(2)?,
                rule_number: row.get(3)?,
                title: row.get(4)?,
                content: row.get(5)?,
                condition: row.get(6)?,
                status: row.get(7)?,
                provenance: row.get(8)?,
                source_fix_id: row.get(9)?,
                severity: row.get(10)?,
                failure_count: row.get(11)?,
                examples_json: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        },
    );

    match result {
        Ok(rule) => Ok(Some(rule)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("Failed to get rule: {}", e)),
    }
}

// ============================================================================
// Formatting Functions
// ============================================================================

/// A deserialized rule example from `examples_json`.
#[derive(Debug, Deserialize)]
struct RuleExample {
    #[serde(rename = "type")]
    example_type: String,
    output: String,
    #[serde(default)]
    explanation: String,
}

/// Format rules into numbered markdown for prompt injection.
///
/// When a rule has `examples_json`, up to 2 examples are appended as
/// GOOD/BAD pairs to provide concrete demonstrations (PromptWizard-inspired).
pub fn format_rules_as_markdown(rules: &[GenerationRule]) -> String {
    rules
        .iter()
        .map(|r| {
            let mut s = format!("{}. **{}**: {}", r.rule_number, r.title, r.content);

            // Append up to 2 synthetic examples if present
            if let Some(ref json) = r.examples_json {
                if let Ok(examples) = serde_json::from_str::<Vec<RuleExample>>(json) {
                    for ex in examples.iter().take(2) {
                        let label = if ex.example_type == "positive" {
                            "GOOD"
                        } else {
                            "BAD"
                        };
                        s.push_str(&format!("\n   - {}: `{}`", label, ex.output));
                        if !ex.explanation.is_empty() {
                            s.push_str(&format!(" — {}", ex.explanation));
                        }
                    }
                }
            }

            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Write Functions
// ============================================================================

/// Get the next rule_number for a given agent + section.
pub fn next_rule_number(conn: &Connection, agent: &str, section: &str) -> i32 {
    conn.query_row(
        "SELECT COALESCE(MAX(rule_number), 0) + 1 FROM generation_rules WHERE agent = ?1 AND section = ?2",
        params![agent, section],
        |row| row.get(0),
    )
    .unwrap_or(1)
}

/// Insert a new generation rule.
/// If `source_fix_id` is provided and a rule with that source already exists, returns the
/// existing rule instead of creating a duplicate.
pub fn insert_rule(conn: &Connection, input: &InsertRuleInput) -> Result<GenerationRule, String> {
    // Dedup guard: skip insert if a rule with the same source_fix_id already exists
    if let Some(ref fix_id) = input.source_fix_id {
        let existing: Option<GenerationRule> = conn
            .query_row(
                "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, failure_count, examples_json, created_at, updated_at
                 FROM generation_rules WHERE source_fix_id = ?1 LIMIT 1",
                params![fix_id],
                |row| {
                    Ok(GenerationRule {
                        id: row.get(0)?,
                        agent: row.get(1)?,
                        section: row.get(2)?,
                        rule_number: row.get(3)?,
                        title: row.get(4)?,
                        content: row.get(5)?,
                        condition: row.get(6)?,
                        status: row.get(7)?,
                        provenance: row.get(8)?,
                        source_fix_id: row.get(9)?,
                        severity: row.get(10)?,
                        failure_count: row.get(11)?,
                        examples_json: row.get(12)?,
                        created_at: row.get(13)?,
                        updated_at: row.get(14)?,
                    })
                },
            )
            .ok();

        if let Some(rule) = existing {
            warn!(
                "Skipping duplicate generation rule insert: source_fix_id={} already exists as rule={}",
                fix_id, rule.id
            );
            return Ok(rule);
        }
    }

    let id = format!("rule-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    let severity = input.severity.as_deref().unwrap_or("normal").to_string();

    conn.execute(
        "INSERT INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, severity, examples_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            input.agent,
            input.section,
            input.rule_number,
            input.title,
            input.content,
            input.condition,
            input.provenance,
            input.source_fix_id,
            severity,
            input.examples_json,
            now,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert generation rule: {}", e))?;

    Ok(GenerationRule {
        id,
        agent: input.agent.clone(),
        section: input.section.clone(),
        rule_number: input.rule_number,
        title: input.title.clone(),
        content: input.content.clone(),
        condition: input.condition.clone(),
        status: "active".to_string(),
        provenance: input.provenance.clone(),
        source_fix_id: input.source_fix_id.clone(),
        severity,
        failure_count: 0,
        examples_json: input.examples_json.clone(),
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Update an existing generation rule.
pub fn update_rule(
    conn: &Connection,
    id: &str,
    input: &UpdateRuleInput,
) -> Result<GenerationRule, String> {
    let now = Utc::now().to_rfc3339();
    let mut sets = vec!["updated_at = ?1".to_string()];
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now.clone())];

    if let Some(ref title) = input.title {
        params_vec.push(Box::new(title.clone()));
        sets.push(format!("title = ?{}", params_vec.len()));
    }
    if let Some(ref content) = input.content {
        params_vec.push(Box::new(content.clone()));
        sets.push(format!("content = ?{}", params_vec.len()));
    }
    if let Some(ref condition) = input.condition {
        params_vec.push(Box::new(condition.clone()));
        sets.push(format!("condition = ?{}", params_vec.len()));
    }
    if let Some(ref status) = input.status {
        params_vec.push(Box::new(status.clone()));
        sets.push(format!("status = ?{}", params_vec.len()));
    }
    if let Some(rule_number) = input.rule_number {
        params_vec.push(Box::new(rule_number));
        sets.push(format!("rule_number = ?{}", params_vec.len()));
    }
    if let Some(ref severity) = input.severity {
        params_vec.push(Box::new(severity.clone()));
        sets.push(format!("severity = ?{}", params_vec.len()));
    }
    if let Some(ref examples_json) = input.examples_json {
        params_vec.push(Box::new(examples_json.clone()));
        sets.push(format!("examples_json = ?{}", params_vec.len()));
    }

    params_vec.push(Box::new(id.to_string()));
    let id_param_idx = params_vec.len();

    let sql = format!(
        "UPDATE generation_rules SET {} WHERE id = ?{}",
        sets.join(", "),
        id_param_idx
    );

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, params_refs.as_slice())
        .map_err(|e| format!("Failed to update generation rule: {}", e))?;

    get_rule(conn, id)?.ok_or_else(|| format!("Rule {} not found after update", id))
}

/// Delete a generation rule (hard delete).
pub fn delete_rule(conn: &Connection, id: &str) -> Result<bool, String> {
    let rows = conn
        .execute("DELETE FROM generation_rules WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete generation rule: {}", e))?;
    Ok(rows > 0)
}

// ============================================================================
// Rule Application Tracking
// ============================================================================

/// Record which generation rules were applied during a workflow generation run.
/// Creates a `rule_applications` row for each active rule, linking it to the
/// generated workflow and (optionally) the task run that triggered generation.
pub fn record_rule_applications(
    conn: &Connection,
    rules: &[GenerationRule],
    workflow_id: Option<&str>,
    task_run_id: Option<&str>,
) {
    let now = Utc::now().to_rfc3339();
    for rule in rules {
        let id = format!("ruleapp-{}", Uuid::new_v4());
        if let Err(e) = conn.execute(
            r#"INSERT INTO rule_applications (id, rule_id, workflow_id, task_run_id, agent, section, applied_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
            params![id, rule.id, workflow_id, task_run_id, rule.agent, rule.section, now],
        ) {
            warn!("Failed to record rule application for {}: {}", rule.id, e);
        }
    }
}

// ============================================================================
// Helper Functions for Auto-Apply
// ============================================================================

/// Infer which agent a reflection fix targets based on keywords in the description.
pub fn infer_agent_from_fix(description: &str) -> String {
    let lower = description.to_lowercase();
    if lower.contains("hardener") || lower.contains("convert") || lower.contains("sdk replacement")
    {
        "hardener".to_string()
    } else if lower.contains("validation")
        || lower.contains("verify command")
        || lower.contains("check step")
        || lower.contains("url validation")
    {
        "verification".to_string()
    } else {
        // Default: schema_context handles generation rules
        "schema_context".to_string()
    }
}

/// Infer which section a reflection fix targets.
pub fn infer_section_from_fix(description: &str, agent: &str) -> String {
    match agent {
        "hardener" => {
            let lower = description.to_lowercase();
            if lower.contains("critical") || lower.contains("preserve") || lower.contains("do not")
            {
                "critical_rules".to_string()
            } else {
                "conversion_rules".to_string()
            }
        }
        "verification" => "check_rules".to_string(),
        _ => {
            let lower = description.to_lowercase();
            if lower.contains("uuid")
                || lower.contains("phase")
                || lower.contains("json")
                || lower.contains("timestamp")
            {
                "important_rules".to_string()
            } else {
                "verification_quality".to_string()
            }
        }
    }
}

/// Truncate a description to a short title (max ~80 chars).
pub fn truncate_to_title(description: &str) -> String {
    let first_sentence = description.split(". ").next().unwrap_or(description);
    if first_sentence.len() <= 80 {
        first_sentence.to_string()
    } else {
        format!("{}...", truncate_str(first_sentence, 77))
    }
}

// ============================================================================
// Direct Rule Creation from Reflection Fixes
// ============================================================================

/// Create generation rules directly from reflection fixes — no accumulation
/// threshold or effectiveness history required.
///
/// Every fix with a qualifying fix_type at high or medium confidence becomes a
/// generation rule immediately. This ensures that reflection insights improve
/// future workflow generation from the very first occurrence.
///
/// Deduplication is by content hash: if a rule with the same content already
/// exists, it is skipped.
///
/// Returns the number of newly created rules.
pub fn create_rules_from_reflection_fixes(
    conn: &Connection,
    fixes: &[ReflectionFix],
) -> Result<u32, String> {
    let qualifying_types = [
        "instruction_clarification",
        "workflow_step_rewrite",
        "context_addition",
    ];

    let mut created = 0u32;

    for fix in fixes {
        // Only promote qualifying fix types at high or medium confidence
        if !qualifying_types.contains(&fix.fix_type.as_str()) {
            continue;
        }
        match fix.confidence.as_str() {
            "high" | "medium" => {}
            _ => continue,
        }

        let rule_content = &fix.fix_description;
        if rule_content.is_empty() {
            continue;
        }

        // Dedup: check if a rule with the same content hash already exists
        let content_hash = simple_content_hash(rule_content);
        let existing_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE provenance = 'reflection' AND content LIKE ?1",
                rusqlite::params![format!("%{}%", &content_hash)],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if existing_count > 0 {
            continue;
        }

        // Also dedup by source_fix_id (insert_rule already handles this, but
        // checking early avoids unnecessary work)
        if let Some(ref fix_id) = Some(&fix.id) {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM generation_rules WHERE source_fix_id = ?1",
                    rusqlite::params![fix_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if exists > 0 {
                continue;
            }
        }

        // Determine agent and section from the fix's source_agent or by inference
        let (rule_agent, rule_section) = if let Some(ref agent) = fix.source_agent {
            map_insight_to_rule_location(agent, &fix.fix_type)
        } else {
            let inferred_agent = infer_agent_from_fix(rule_content);
            let inferred_section = infer_section_from_fix(rule_content, &inferred_agent);
            (inferred_agent, inferred_section)
        };

        let rule_number = next_rule_number(conn, &rule_agent, &rule_section);
        let title = truncate_to_title(rule_content);
        let tagged_content = format!("{}\n<!-- hash:{} -->", rule_content, content_hash);

        let input = InsertRuleInput {
            agent: rule_agent,
            section: rule_section,
            rule_number,
            title,
            content: tagged_content,
            condition: None,
            provenance: "reflection".to_string(),
            source_fix_id: Some(fix.id.clone()),
            severity: None,
            examples_json: None,
        };

        match insert_rule(conn, &input) {
            Ok(rule) => {
                created += 1;
                tracing::info!(
                    "Created generation rule {} from reflection fix {} (type: {}, confidence: {})",
                    rule.id,
                    fix.id,
                    fix.fix_type,
                    fix.confidence
                );
            }
            Err(e) => warn!(
                "Failed to create rule from reflection fix {}: {}",
                fix.id, e
            ),
        }
    }

    if created > 0 {
        tracing::info!(
            "Created {} generation rules directly from reflection fixes",
            created
        );
    }

    Ok(created)
}

// ============================================================================
// Auto-Rule Generation from Insights
// ============================================================================

/// Promote prompt insights into auto-generated rules.
///
/// An insight is promoted when:
/// - `confidence > 0.3`
/// - `evidence_count >= 1`
/// - No existing rule with similar content (content-hash dedup)
///
/// Returns the number of newly created rules.
pub fn promote_insights_to_rules(
    conn: &Connection,
    insights: &[super::prompt_analysis::PromptInsight],
) -> Result<u32, String> {
    let mut created = 0u32;

    for insight in insights {
        // Promote insights with reasonable confidence and any evidence.
        // Thresholds kept low so that projects with few runs still benefit.
        if insight.confidence <= 0.3 || insight.evidence_count < 1 {
            continue;
        }

        let rule_content = match &insight.suggested_rule {
            Some(content) => content.clone(),
            None => continue, // No suggested rule text → skip
        };

        // Dedup: check if a similar auto-generated rule already exists
        let content_hash = simple_content_hash(&rule_content);
        let existing_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE provenance = 'auto_insight' AND content LIKE ?1",
                rusqlite::params![format!("%{}%", &content_hash)],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if existing_count > 0 {
            continue;
        }

        // Map insight agent to rule agent + section
        let (rule_agent, rule_section) =
            map_insight_to_rule_location(&insight.agent, &insight.insight_type);

        let rule_number = next_rule_number(conn, &rule_agent, &rule_section);
        let title = truncate_to_title(&insight.description);

        // Tag the content with the hash for future dedup
        let tagged_content = format!("{}\n<!-- hash:{} -->", rule_content, content_hash);

        let input = InsertRuleInput {
            agent: rule_agent,
            section: rule_section,
            rule_number,
            title,
            content: tagged_content,
            condition: None,
            provenance: "auto_insight".to_string(),
            source_fix_id: None,
            severity: None,
            examples_json: None,
        };

        match insert_rule(conn, &input) {
            Ok(_) => created += 1,
            Err(e) => warn!("Failed to auto-create rule from insight: {}", e),
        }
    }

    if created > 0 {
        tracing::info!("Auto-generated {} rules from prompt insights", created);
    }

    Ok(created)
}

/// Simple content hash for dedup (first 16 chars of content, normalized).
fn simple_content_hash(content: &str) -> String {
    let normalized: String = content
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(32)
        .collect();
    normalized[..normalized.len().min(16)].to_string()
}

/// Map an insight's agent + type to the appropriate rule agent + section.
fn map_insight_to_rule_location(agent: &str, insight_type: &str) -> (String, String) {
    match (agent, insight_type) {
        ("specification", _) => ("specification".to_string(), "criteria_rules".to_string()),
        ("verification", "verification_blind_spot") => {
            ("verification".to_string(), "check_rules".to_string())
        }
        ("verification", _) => ("verification".to_string(), "check_rules".to_string()),
        ("hardener", _) => ("hardener".to_string(), "conversion_rules".to_string()),
        ("builder", _) => ("schema_context".to_string(), "important_rules".to_string()),
        _ => ("schema_context".to_string(), "important_rules".to_string()),
    }
}

// ============================================================================
// Failure Tracking & Auto-Promotion
// ============================================================================

/// Increment the failure_count of a rule and auto-promote to 'critical' if threshold reached.
pub fn increment_rule_failure_count(conn: &Connection, rule_id: &str) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE generation_rules SET failure_count = failure_count + 1, updated_at = ?1 WHERE id = ?2",
        params![now, rule_id],
    )
    .map_err(|e| format!("Failed to increment failure_count for rule {}: {}", rule_id, e))?;

    // Auto-promote to 'critical' if failure_count reaches threshold
    let failure_count: i32 = conn
        .query_row(
            "SELECT failure_count FROM generation_rules WHERE id = ?1",
            params![rule_id],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if failure_count >= 5 {
        conn.execute(
            "UPDATE generation_rules SET severity = 'critical', updated_at = ?1 WHERE id = ?2 AND severity != 'critical'",
            params![now, rule_id],
        )
        .map_err(|e| format!("Failed to auto-promote rule {} to critical: {}", rule_id, e))?;
        info!(
            "Auto-promoted rule {} to critical severity (failure_count={})",
            rule_id, failure_count
        );
    }

    Ok(())
}

/// Identify which rules were violated by a generation/verification error.
/// Returns IDs of rules whose content keywords appear in the error message.
pub fn identify_violated_rules(conn: &Connection, error_message: &str, agent: &str) -> Vec<String> {
    let rules = load_rules(conn, agent, "important_rules");
    let all_rules = {
        let mut r = rules;
        r.extend(load_rules(conn, agent, "verification_quality"));
        r
    };

    let error_lower = error_message.to_lowercase();
    let mut violated = Vec::new();

    for rule in &all_rules {
        // Extract key phrases from rule title for matching
        let title_lower = rule.title.to_lowercase();
        let keywords: Vec<&str> = title_lower.split_whitespace().collect();

        // Match if 2+ significant keywords from the title appear in the error
        let significant_matches = keywords
            .iter()
            .filter(|kw| kw.len() > 3) // skip short words
            .filter(|kw| error_lower.contains(*kw))
            .count();

        if significant_matches >= 2 {
            violated.push(rule.id.clone());
        }
    }

    violated
}

// ============================================================================
// Known Issue → Rule Sync
// ============================================================================

/// Create generation rules from active known issues that have verification templates.
///
/// Each known issue with a `verification_step_template` becomes an "important" rule
/// in the verification agent. Deduplicates by `source_fix_id` (using the known issue ID).
pub fn sync_rules_from_known_issues(conn: &Connection) -> Result<u32, String> {
    let active_issues =
        crate::known_issues::storage::find_relevant_issues_for_generation(conn, "regression")?;
    let mut created = 0u32;

    for issue in &active_issues {
        // Only sync issues that have a verification step template or hint
        if issue.verification_step_template.is_none() && issue.verification_hint.is_none() {
            continue;
        }

        let source_id = format!("known_issue:{}", issue.id);

        // Check if a rule already exists for this issue
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE source_fix_id = ?1",
                params![source_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if exists > 0 {
            continue;
        }

        // Build rule content from the issue
        let scope_info = issue.scope_value.as_deref().unwrap_or("the affected area");
        let hint = issue
            .verification_hint
            .as_deref()
            .unwrap_or(&issue.description);
        let rule_content = format!(
            "When generating workflows that touch {}, ensure verification includes a check for: {}",
            scope_info, hint
        );

        // Infer the agent from issue category
        let rule_agent = infer_agent_from_issue(&issue.category);
        let rule_section = "verification_quality".to_string();
        let rule_number = next_rule_number(conn, &rule_agent, &rule_section);

        let input = InsertRuleInput {
            agent: rule_agent,
            section: rule_section,
            rule_number,
            title: format!("Prevent: {}", truncate_str(&issue.title, 60)),
            content: rule_content,
            condition: issue.scope_value.clone(),
            provenance: "known_issue".to_string(),
            source_fix_id: Some(source_id),
            severity: Some("important".to_string()),
            examples_json: None,
        };

        match insert_rule(conn, &input) {
            Ok(rule) => {
                created += 1;
                info!(
                    "Created generation rule {} from known issue {} ({})",
                    rule.id, issue.id, issue.title
                );
            }
            Err(e) => warn!("Failed to create rule from known issue {}: {}", issue.id, e),
        }
    }

    if created > 0 {
        info!("Synced {} generation rules from known issues", created);
    }

    Ok(created)
}

/// Map a known issue category to the appropriate rule agent.
fn infer_agent_from_issue(category: &crate::known_issues::types::IssueCategory) -> String {
    use crate::known_issues::types::IssueCategory;
    match category {
        IssueCategory::Timing | IssueCategory::State | IssueCategory::DataIntegrity => {
            "verification".to_string()
        }
        _ => "schema_context".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_agent_from_fix() {
        assert_eq!(
            infer_agent_from_fix("hardener should convert prompts"),
            "hardener"
        );
        assert_eq!(
            infer_agent_from_fix("URL validation for check steps"),
            "verification"
        );
        assert_eq!(infer_agent_from_fix("gate step required"), "schema_context");
    }

    #[test]
    fn test_infer_section_from_fix() {
        assert_eq!(
            infer_section_from_fix("preserve step IDs", "hardener"),
            "critical_rules"
        );
        assert_eq!(
            infer_section_from_fix("convert prompts", "hardener"),
            "conversion_rules"
        );
        assert_eq!(
            infer_section_from_fix("anything", "verification"),
            "check_rules"
        );
        assert_eq!(
            infer_section_from_fix("UUID format rule", "schema_context"),
            "important_rules"
        );
        assert_eq!(
            infer_section_from_fix("gate step quality", "schema_context"),
            "verification_quality"
        );
    }

    #[test]
    fn test_truncate_to_title() {
        assert_eq!(truncate_to_title("Short title"), "Short title");
        let long = "This is a very long description that goes on and on and on and should be truncated at some point to keep it reasonable";
        assert!(truncate_to_title(long).len() <= 80);
    }

    #[test]
    fn test_format_rules_as_markdown() {
        let rules = vec![
            GenerationRule {
                id: "r1".into(),
                agent: "test".into(),
                section: "test".into(),
                rule_number: 1,
                title: "Rule One".into(),
                content: "Do this thing".into(),
                condition: None,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                severity: "normal".into(),
                failure_count: 0,
                examples_json: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
            GenerationRule {
                id: "r2".into(),
                agent: "test".into(),
                section: "test".into(),
                rule_number: 2,
                title: "Rule Two".into(),
                content: "Do that thing".into(),
                condition: None,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                severity: "normal".into(),
                failure_count: 0,
                examples_json: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
        ];
        let md = format_rules_as_markdown(&rules);
        assert!(md.contains("1. **Rule One**: Do this thing"));
        assert!(md.contains("2. **Rule Two**: Do that thing"));
    }

    // ========================================================================
    // Database-backed tests for promote_insights_to_rules and helpers
    // ========================================================================

    use crate::workflow_generation::prompt_analysis::PromptInsight;

    /// Create an in-memory SQLite database with the generation_rules table.
    fn setup_rules_db() -> Connection {
        let conn = Connection::open_in_memory().expect("Failed to create in-memory DB");
        conn.execute_batch(
            r#"
            CREATE TABLE generation_rules (
                id TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                section TEXT NOT NULL,
                rule_number INTEGER NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                condition TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                provenance TEXT NOT NULL DEFAULT 'seed',
                source_fix_id TEXT,
                severity TEXT NOT NULL DEFAULT 'normal',
                failure_count INTEGER NOT NULL DEFAULT 0,
                confidence REAL DEFAULT 1.0,
                auto_generated_at TEXT,
                evidence_count INTEGER DEFAULT 0,
                examples_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )
        .expect("Failed to create generation_rules table");
        conn
    }

    #[test]
    fn test_promote_high_confidence_insight() {
        let conn = setup_rules_db();
        let insights = vec![PromptInsight {
            agent: "builder".to_string(),
            insight_type: "recurring_failure".to_string(),
            description: "Builder produces missing error handling".to_string(),
            evidence_count: 6,
            confidence: 0.9,
            suggested_rule: Some("test rule".to_string()),
        }];

        let created = promote_insights_to_rules(&conn, &insights).expect("promote failed");
        assert_eq!(
            created, 1,
            "Expected 1 rule created for high-confidence insight"
        );

        // Verify the rule exists in the database
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE provenance = 'auto_insight'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_promote_low_confidence_skipped() {
        let conn = setup_rules_db();
        let insights = vec![PromptInsight {
            agent: "builder".to_string(),
            insight_type: "recurring_failure".to_string(),
            description: "Low confidence insight".to_string(),
            evidence_count: 6,
            confidence: 0.2,
            suggested_rule: Some("test rule".to_string()),
        }];

        let created = promote_insights_to_rules(&conn, &insights).expect("promote failed");
        assert_eq!(created, 0, "Expected 0 rules for low-confidence insight");
    }

    #[test]
    fn test_promote_low_evidence_skipped() {
        let conn = setup_rules_db();
        let insights = vec![PromptInsight {
            agent: "builder".to_string(),
            insight_type: "recurring_failure".to_string(),
            description: "Low evidence insight".to_string(),
            evidence_count: 0,
            confidence: 0.9,
            suggested_rule: Some("test rule".to_string()),
        }];

        let created = promote_insights_to_rules(&conn, &insights).expect("promote failed");
        assert_eq!(created, 0, "Expected 0 rules for low-evidence insight");
    }

    #[test]
    fn test_promote_no_suggested_rule_skipped() {
        let conn = setup_rules_db();
        let insights = vec![PromptInsight {
            agent: "builder".to_string(),
            insight_type: "recurring_failure".to_string(),
            description: "No suggested rule".to_string(),
            evidence_count: 6,
            confidence: 0.9,
            suggested_rule: None,
        }];

        let created = promote_insights_to_rules(&conn, &insights).expect("promote failed");
        assert_eq!(created, 0, "Expected 0 rules when suggested_rule is None");
    }

    #[test]
    fn test_promote_dedup() {
        let conn = setup_rules_db();
        let insights = vec![PromptInsight {
            agent: "builder".to_string(),
            insight_type: "recurring_failure".to_string(),
            description: "Dedup test insight".to_string(),
            evidence_count: 6,
            confidence: 0.9,
            suggested_rule: Some("test rule for dedup".to_string()),
        }];

        let first = promote_insights_to_rules(&conn, &insights).expect("first promote failed");
        assert_eq!(first, 1, "First promotion should create 1 rule");

        let second = promote_insights_to_rules(&conn, &insights).expect("second promote failed");
        assert_eq!(second, 0, "Second promotion should create 0 rules (dedup)");

        // Verify only 1 rule total in the database
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE provenance = 'auto_insight'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "Only 1 rule should exist after double promote");
    }

    #[test]
    fn test_simple_content_hash_empty() {
        let result = simple_content_hash("");
        assert_eq!(result, "", "Hash of empty string should be empty");
    }

    #[test]
    fn test_map_insight_to_rule_location() {
        assert_eq!(
            map_insight_to_rule_location("specification", "any"),
            ("specification".to_string(), "criteria_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("verification", "verification_blind_spot"),
            ("verification".to_string(), "check_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("hardener", "any"),
            ("hardener".to_string(), "conversion_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("builder", "any"),
            ("schema_context".to_string(), "important_rules".to_string()),
        );
        assert_eq!(
            map_insight_to_rule_location("unknown", "any"),
            ("schema_context".to_string(), "important_rules".to_string()),
        );
    }

    // ========================================================================
    // Tests for create_rules_from_reflection_fixes
    // ========================================================================

    fn make_test_fix(fix_type: &str, confidence: &str, description: &str) -> ReflectionFix {
        ReflectionFix {
            id: format!("fix-{}", uuid::Uuid::new_v4()),
            source_task_run_id: "src-1".into(),
            reflection_task_run_id: "ref-1".into(),
            source_finding_id: None,
            source_knowledge_id: None,
            fix_type: fix_type.into(),
            fix_description: description.into(),
            file_changed: None,
            old_value: None,
            new_value: None,
            confidence: confidence.into(),
            content_hash: None,
            status: "applied".into(),
            effectiveness: None,
            effectiveness_evidence: None,
            applied_at: "2026-01-01T00:00:00Z".into(),
            evaluated_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            source_agent: Some("builder".into()),
            reasoning: None,
            alternatives_considered: None,
            reflection_scope: None,
            project_path: None,
            applicability_context: None,
        }
    }

    #[test]
    fn test_create_rules_from_reflection_fixes_qualifying() {
        let conn = setup_rules_db();
        let fixes = vec![
            make_test_fix(
                "instruction_clarification",
                "high",
                "Always include error handling in shell commands",
            ),
            make_test_fix(
                "context_addition",
                "medium",
                "Add workspace path to context for file operations",
            ),
        ];

        let created =
            create_rules_from_reflection_fixes(&conn, &fixes).expect("create rules failed");
        assert_eq!(created, 2, "Expected 2 rules from qualifying fixes");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE provenance = 'reflection'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_create_rules_skips_low_confidence() {
        let conn = setup_rules_db();
        let fixes = vec![make_test_fix(
            "instruction_clarification",
            "low",
            "Uncertain fix",
        )];

        let created =
            create_rules_from_reflection_fixes(&conn, &fixes).expect("create rules failed");
        assert_eq!(created, 0, "Expected 0 rules for low-confidence fix");
    }

    #[test]
    fn test_create_rules_skips_non_qualifying_types() {
        let conn = setup_rules_db();
        let fixes = vec![make_test_fix("selector_fix", "high", "Fixed CSS selector")];

        let created =
            create_rules_from_reflection_fixes(&conn, &fixes).expect("create rules failed");
        assert_eq!(created, 0, "Expected 0 rules for non-qualifying fix type");
    }

    #[test]
    fn test_create_rules_dedup() {
        let conn = setup_rules_db();
        let fixes = vec![make_test_fix(
            "instruction_clarification",
            "high",
            "Always include error handling in shell commands",
        )];

        let first = create_rules_from_reflection_fixes(&conn, &fixes).expect("first create failed");
        assert_eq!(first, 1);

        // Second call with same content should be deduped
        let fixes2 = vec![make_test_fix(
            "instruction_clarification",
            "high",
            "Always include error handling in shell commands",
        )];
        let second =
            create_rules_from_reflection_fixes(&conn, &fixes2).expect("second create failed");
        assert_eq!(second, 0, "Expected 0 rules on duplicate");
    }
}

// ============================================================================
// Seed Rules
// ============================================================================

/// Ensure seed rules are present in the database.
/// These are hardcoded quality rules that prevent known anti-patterns.
/// Deduplicates by matching on provenance='seed' and title — safe to call repeatedly.
pub fn ensure_seed_rules(conn: &Connection) {
    let seed_rules = [
        // ── Critical severity: structural correctness rules ────────────
        // These prevent immediate generation failures (invalid JSON, wrong step types, etc.)
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 1,
            title: "Generate valid UUIDs".to_string(),
            content: "Generate valid UUIDs for all `id` fields (format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx).".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 2,
            title: "Phase field must match array".to_string(),
            content: "`phase` field MUST match the array the step is in (setup_steps -> \"setup\", verification_steps -> \"verification\", agentic_steps -> \"agentic\", completion_steps -> \"completion\").".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 3,
            title: "Agentic steps are prompt only".to_string(),
            content: "`agentic_steps` can ONLY contain `prompt` type steps.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 5,
            title: "JSON only output".to_string(),
            content: "Return ONLY the JSON object, no markdown formatting, no code fences, no commentary outside the JSON structure.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 16,
            title: "Only 3 step types exist".to_string(),
            content: "The only valid step types are `command`, `ui_bridge`, and `prompt`. Do NOT use `shell_command`, `api_request`, `mcp_call`, `check`, `check_group`, `test`, `gate`, or `spec` — these are not valid types. Tests are run via `command` with `test_type` set. Checks are run via `command` with `check_type` set.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 17,
            title: "Every command step MUST include a mode field".to_string(),
            content: "Valid modes are `shell`, `check`, `check_group`, `test`. The mode must match the fields present: `check` requires `check_type`, `check_group` requires `check_group_id`, `test` requires `test_type` or `test_id`, `shell` is for plain commands.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 6,
            title: "Deterministic verification step required".to_string(),
            content: "verification_steps MUST include at least one deterministic, automated step — one of: `command` (with check_type or a command), `test`, or `ui_bridge` (assert action). Do NOT use only `prompt` type steps for verification.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("critical".to_string()),
            examples_json: None,
        },
        // ── Important severity: correctness-adjacent rules ───────────
        // These prevent common errors but aren't structurally fatal
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 4,
            title: "ISO 8601 timestamps".to_string(),
            content: "Use ISO 8601 format for timestamps (e.g., \"2024-01-15T10:30:00Z\").".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 7,
            title: "Code modification requires typecheck".to_string(),
            content: "When the workflow creates or modifies source code files, verification MUST include a `command` step with `check_type` set to the appropriate type checker.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 8,
            title: "Web app verification requires SDK or Playwright".to_string(),
            content: "When the workflow verifies interactive UI behavior of a web application, verification MUST include `ui_bridge` steps or Playwright-based checks. For simple health checks or content verification (page loads, contains text), `command` steps with `curl` are sufficient.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 9,
            title: "Data flow references must be valid".to_string(),
            content: "All step IDs referenced in `inputs`, `depends_on`, and `extract` must correspond to existing step IDs within the same workflow. Circular dependencies in `depends_on` are invalid.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 10,
            title: "Prompts are supplementary".to_string(),
            content: "`prompt` type steps in verification must NEVER be the sole verification mechanism.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 14,
            title: "Feature-specific verification required".to_string(),
            content: "Must include feature-specific checks, not just compilation/lint checks.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 15,
            title: "Agentic-verification correspondence".to_string(),
            content: "Each agentic step must have a corresponding deterministic verification step.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 18,
            title: "No Python f-strings or jq in shell commands".to_string(),
            content: "When piping curl output to `python -c`, NEVER use f-strings (`f'...'`). Use string concatenation instead. Do NOT use `jq` — it is not available on Windows. For JSON assertions, pipe to `python -c \"import sys,json; d=json.load(sys.stdin); ...\"`. For checking fields exist, use `grep`.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 19,
            title: "Retry format".to_string(),
            content: "Use flat `retry_count` and `retry_delay_ms` fields directly on step objects. Do NOT use a nested `retry` object (e.g., `\"retry\": {\"count\": 5}` is WRONG, use `\"retry_count\": 5`).".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 20,
            title: "No bash negation prefix in shell commands".to_string(),
            content: "NEVER use `!` as a command prefix to invert exit codes (e.g., `! grep -qE 'pattern' file`). The `!` operator is bash-specific and may not work on Windows. Instead, use explicit exit code handling: `grep -qE 'pattern' file && exit 1 || exit 0`.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 23,
            title: "No echo EXIT pattern".to_string(),
            content: "NEVER use `echo EXIT:$?`, `echo $?`, or similar patterns to capture exit codes. Let the command's natural exit code propagate. Use `command && echo PASS || echo FAIL` only when explicit text output is needed for parsing.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 24,
            title: "Safe grep pipeline".to_string(),
            content: "When using grep to count occurrences, always use `grep -c PATTERN || true` to handle the zero-match case (grep returns exit code 1 when no matches are found). For distinguishing 'not found' from 'error', use `command 2>&1 | grep PATTERN; test $? -ne 2`.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("important".to_string()),
            examples_json: None,
        },
        // ── Normal severity: situational rules ───────────────────────
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 11,
            title: "Test steps with inline commands use repository".to_string(),
            content: "When a `test` step runs a shell command, set `test_type: \"repository\"`.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("normal".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 12,
            title: "Next.js App Router path conventions".to_string(),
            content: "Use correct paths like `src/app/(app)/`.".to_string(),
            condition: Some("targets_nextjs".to_string()),
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("normal".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 13,
            title: "Verification step caching".to_string(),
            content: "Running tasks cache verification steps at startup; API updates don't affect current task.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("hint".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 21,
            title: "Removal tasks require runtime verification".to_string(),
            content: "When a workflow involves removing pages/routes/components from a web app, verification MUST include runtime checks (UI Bridge navigate + assertion, or HTTP request to the removed URL), not just static file-existence checks (`test ! -f`). Build caches and routing config may still serve removed content after source file deletion.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("normal".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "verification_quality".to_string(),
            rule_number: 22,
            title: "Per-stage UI verification in multi-stage workflows".to_string(),
            content: "In multi-stage workflows, each stage that modifies UI must have its own UI Bridge verification steps. A UI Bridge check in one stage does NOT cover UI changes made in another stage.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("normal".to_string()),
            examples_json: None,
        },
        InsertRuleInput {
            agent: "schema_context".to_string(),
            section: "important_rules".to_string(),
            rule_number: 25,
            title: "Declare Python dependency".to_string(),
            content: "If ANY step uses `python` or `python3` commands, a prior setup step MUST verify Python availability (`python3 --version || python --version`). Do not assume Python is installed.".to_string(),
            condition: None,
            provenance: "seed".to_string(),
            source_fix_id: None,
            severity: Some("normal".to_string()),
            examples_json: None,
        },
    ];

    for input in &seed_rules {
        // Dedup: skip if a seed rule with the same title already exists
        let already_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM generation_rules WHERE provenance = 'seed' AND title = ?1",
                params![input.title],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .unwrap_or(false);

        if already_exists {
            tracing::debug!("Seed rule already exists, skipping: {}", input.title);
            continue;
        }

        match insert_rule(conn, input) {
            Ok(rule) => {
                tracing::debug!("Seed rule ensured: {} ({})", rule.title, rule.id);
            }
            Err(e) => {
                tracing::warn!("Failed to ensure seed rule '{}': {}", input.title, e);
            }
        }
    }
}
