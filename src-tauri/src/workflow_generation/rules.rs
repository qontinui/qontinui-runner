//! Generation Rules — Externalized workflow generation rules stored in SQLite.
//!
//! Rules that govern workflow generation (schema context, hardener, verification)
//! are stored in the `generation_rules` table. This allows the reflection system
//! to create/modify rules at runtime without Rust recompilation.

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;
use uuid::Uuid;

use crate::str_utils::truncate_str;

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
}

/// Input for updating an existing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRuleInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub condition: Option<String>,
    pub status: Option<String>,
    pub rule_number: Option<i32>,
}

/// Query parameters for listing rules.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListRulesQuery {
    pub agent: Option<String>,
    pub section: Option<String>,
    pub status: Option<String>,
    pub provenance: Option<String>,
}

// ============================================================================
// Loading Functions
// ============================================================================

/// Load active rules for a specific agent and section, ordered by rule_number.
pub fn load_rules(conn: &Connection, agent: &str, section: &str) -> Vec<GenerationRule> {
    let mut stmt = match conn.prepare(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, created_at, updated_at
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
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
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

/// Load all active rules for an agent, grouped by section.
pub fn load_rules_by_agent(conn: &Connection, agent: &str) -> HashMap<String, Vec<GenerationRule>> {
    let mut stmt = match conn.prepare(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, created_at, updated_at
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
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
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
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, created_at, updated_at
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
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })
        .map_err(|e| format!("Failed to execute list_rules query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Get a single rule by ID.
pub fn get_rule(conn: &Connection, id: &str) -> Result<Option<GenerationRule>, String> {
    let result = conn.query_row(
        "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, created_at, updated_at
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
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
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

/// Format rules into numbered markdown for prompt injection.
pub fn format_rules_as_markdown(rules: &[GenerationRule]) -> String {
    rules
        .iter()
        .map(|r| format!("{}. **{}**: {}", r.rule_number, r.title, r.content))
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
                "SELECT id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, created_at, updated_at
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
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
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

    conn.execute(
        "INSERT INTO generation_rules (id, agent, section, rule_number, title, content, condition, status, provenance, source_fix_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11)",
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
// Auto-Rule Generation from Insights
// ============================================================================

/// Promote high-confidence prompt insights into auto-generated rules.
///
/// An insight is promoted when:
/// - `confidence > 0.8`
/// - `evidence_count >= 5`
/// - No existing rule with similar content (content-hash dedup)
///
/// Returns the number of newly created rules.
pub fn promote_insights_to_rules(
    conn: &Connection,
    insights: &[super::prompt_analysis::PromptInsight],
) -> Result<u32, String> {
    let mut created = 0u32;

    for insight in insights {
        // Only promote high-confidence insights with sufficient evidence
        if insight.confidence <= 0.8 || insight.evidence_count < 5 {
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
                confidence REAL DEFAULT 1.0,
                auto_generated_at TEXT,
                evidence_count INTEGER DEFAULT 0,
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
            confidence: 0.5,
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
            evidence_count: 3,
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
}
