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
pub fn insert_rule(conn: &Connection, input: &InsertRuleInput) -> Result<GenerationRule, String> {
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
        format!("{}...", &first_sentence[..77])
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
}
