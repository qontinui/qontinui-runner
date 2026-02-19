//! Step Type Knowledge — Per-step-type best practices and pitfalls.
//!
//! Two-layer knowledge system:
//! - **Universal** — best practices and pitfalls per step type (ships as seed data)
//! - **System-specific** — environment-specific patterns learned over time
//!
//! Knowledge is stored in SQLite, injected into the Builder Agent's prompt
//! filtered to only the relevant step types.

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::warn;
use uuid::Uuid;

/// A single step type knowledge entry stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepTypeKnowledge {
    pub id: String,
    pub step_type: String,
    pub layer: String,
    pub title: String,
    pub content: String,
    pub priority: i32,
    pub status: String,
    pub provenance: String,
    pub source_fix_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for inserting a new knowledge entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertKnowledgeInput {
    pub step_type: String,
    #[serde(default = "default_layer")]
    pub layer: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_provenance")]
    pub provenance: String,
    pub source_fix_id: Option<String>,
}

fn default_layer() -> String {
    "universal".to_string()
}

fn default_provenance() -> String {
    "manual".to_string()
}

/// Input for updating an existing knowledge entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateKnowledgeInput {
    pub title: Option<String>,
    pub content: Option<String>,
    pub priority: Option<i32>,
    pub status: Option<String>,
    pub layer: Option<String>,
}

/// Query parameters for listing knowledge entries.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListKnowledgeQuery {
    pub step_type: Option<String>,
    pub layer: Option<String>,
    pub status: Option<String>,
}

// ============================================================================
// Loading Functions (for prompt injection)
// ============================================================================

/// Load active knowledge entries for the given step types, ordered by priority DESC.
pub fn load_knowledge_for_step_types(
    conn: &Connection,
    step_types: &[&str],
) -> Vec<StepTypeKnowledge> {
    if step_types.is_empty() {
        return vec![];
    }

    // Build IN clause with positional parameters
    let placeholders: Vec<String> = (1..=step_types.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
         FROM step_type_knowledge
         WHERE step_type IN ({}) AND status = 'active'
         ORDER BY step_type, priority DESC",
        placeholders.join(", ")
    );

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to prepare load_knowledge query: {}", e);
            return vec![];
        }
    };

    let params_vec: Vec<&dyn rusqlite::types::ToSql> = step_types
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();

    let rows = stmt.query_map(params_vec.as_slice(), |row| {
        Ok(StepTypeKnowledge {
            id: row.get(0)?,
            step_type: row.get(1)?,
            layer: row.get(2)?,
            title: row.get(3)?,
            content: row.get(4)?,
            priority: row.get(5)?,
            status: row.get(6)?,
            provenance: row.get(7)?,
            source_fix_id: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    });

    match rows {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            warn!("Failed to execute load_knowledge query: {}", e);
            vec![]
        }
    }
}

/// Format knowledge entries as markdown for prompt injection.
///
/// Groups by step_type, outputs:
/// ```text
/// ## Step Type Best Practices
///
/// ### shell_command
/// - **Always set working_directory**: Shell commands must specify...
/// ```
pub fn format_knowledge_as_markdown(entries: &[StepTypeKnowledge]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut grouped: HashMap<&str, Vec<&StepTypeKnowledge>> = HashMap::new();
    for entry in entries {
        grouped
            .entry(entry.step_type.as_str())
            .or_default()
            .push(entry);
    }

    // Sort step types for deterministic output
    let mut step_types: Vec<&&str> = grouped.keys().collect();
    step_types.sort();

    let mut output = String::from("## Step Type Best Practices\n\n");

    for step_type in step_types {
        output.push_str(&format!("### {}\n", step_type));
        if let Some(entries) = grouped.get(*step_type) {
            for entry in entries {
                output.push_str(&format!("- **{}**: {}\n", entry.title, entry.content));
            }
        }
        output.push('\n');
    }

    output
}

// ============================================================================
// CRUD Functions
// ============================================================================

/// List knowledge entries with optional filters.
pub fn list_knowledge(
    conn: &Connection,
    query: &ListKnowledgeQuery,
) -> Result<Vec<StepTypeKnowledge>, String> {
    let mut sql = String::from(
        "SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
         FROM step_type_knowledge WHERE 1=1",
    );
    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(ref step_type) = query.step_type {
        params_vec.push(Box::new(step_type.clone()));
        sql.push_str(&format!(" AND step_type = ?{}", params_vec.len()));
    }
    if let Some(ref layer) = query.layer {
        params_vec.push(Box::new(layer.clone()));
        sql.push_str(&format!(" AND layer = ?{}", params_vec.len()));
    }
    if let Some(ref status) = query.status {
        params_vec.push(Box::new(status.clone()));
        sql.push_str(&format!(" AND status = ?{}", params_vec.len()));
    }
    sql.push_str(" ORDER BY step_type, priority DESC");

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to prepare list_knowledge query: {}", e))?;

    let rows = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(StepTypeKnowledge {
                id: row.get(0)?,
                step_type: row.get(1)?,
                layer: row.get(2)?,
                title: row.get(3)?,
                content: row.get(4)?,
                priority: row.get(5)?,
                status: row.get(6)?,
                provenance: row.get(7)?,
                source_fix_id: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })
        .map_err(|e| format!("Failed to execute list_knowledge query: {}", e))?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Get a single knowledge entry by ID.
pub fn get_knowledge(conn: &Connection, id: &str) -> Result<Option<StepTypeKnowledge>, String> {
    let result = conn.query_row(
        "SELECT id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at
         FROM step_type_knowledge WHERE id = ?1",
        params![id],
        |row| {
            Ok(StepTypeKnowledge {
                id: row.get(0)?,
                step_type: row.get(1)?,
                layer: row.get(2)?,
                title: row.get(3)?,
                content: row.get(4)?,
                priority: row.get(5)?,
                status: row.get(6)?,
                provenance: row.get(7)?,
                source_fix_id: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        },
    );

    match result {
        Ok(entry) => Ok(Some(entry)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("Failed to get knowledge entry: {}", e)),
    }
}

/// Insert a new knowledge entry.
pub fn insert_knowledge(
    conn: &Connection,
    input: &InsertKnowledgeInput,
) -> Result<StepTypeKnowledge, String> {
    let id = format!("stk-{}", Uuid::new_v4());
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO step_type_knowledge (id, step_type, layer, title, content, priority, status, provenance, source_fix_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?10)",
        params![
            id,
            input.step_type,
            input.layer,
            input.title,
            input.content,
            input.priority,
            input.provenance,
            input.source_fix_id,
            now,
            now,
        ],
    )
    .map_err(|e| format!("Failed to insert knowledge entry: {}", e))?;

    Ok(StepTypeKnowledge {
        id,
        step_type: input.step_type.clone(),
        layer: input.layer.clone(),
        title: input.title.clone(),
        content: input.content.clone(),
        priority: input.priority,
        status: "active".to_string(),
        provenance: input.provenance.clone(),
        source_fix_id: input.source_fix_id.clone(),
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Update an existing knowledge entry.
pub fn update_knowledge(
    conn: &Connection,
    id: &str,
    input: &UpdateKnowledgeInput,
) -> Result<StepTypeKnowledge, String> {
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
    if let Some(priority) = input.priority {
        params_vec.push(Box::new(priority));
        sets.push(format!("priority = ?{}", params_vec.len()));
    }
    if let Some(ref status) = input.status {
        params_vec.push(Box::new(status.clone()));
        sets.push(format!("status = ?{}", params_vec.len()));
    }
    if let Some(ref layer) = input.layer {
        params_vec.push(Box::new(layer.clone()));
        sets.push(format!("layer = ?{}", params_vec.len()));
    }

    params_vec.push(Box::new(id.to_string()));
    let id_param_idx = params_vec.len();

    let sql = format!(
        "UPDATE step_type_knowledge SET {} WHERE id = ?{}",
        sets.join(", "),
        id_param_idx
    );

    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();
    conn.execute(&sql, params_refs.as_slice())
        .map_err(|e| format!("Failed to update knowledge entry: {}", e))?;

    get_knowledge(conn, id)?.ok_or_else(|| format!("Knowledge entry {} not found after update", id))
}

/// Delete a knowledge entry (hard delete).
pub fn delete_knowledge(conn: &Connection, id: &str) -> Result<bool, String> {
    let rows = conn
        .execute("DELETE FROM step_type_knowledge WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete knowledge entry: {}", e))?;
    Ok(rows > 0)
}

// ============================================================================
// Reflection Helper
// ============================================================================

/// Infer step type from a reflection fix description based on keywords.
/// Uses compound/specific keywords first, then falls back to generic ones.
/// Scores each step type by keyword match count and returns the best match.
pub fn infer_step_type_from_fix(description: &str) -> Option<String> {
    let lower = description.to_lowercase();

    // Each entry: (step_type, keywords) -- more specific keywords listed first
    let mappings: &[(&str, &[&str])] = &[
        (
            "shell_command",
            &[
                "shell_command",
                "shell command",
                "working_directory",
                "fail_on_error",
            ],
        ),
        (
            "api_request",
            &[
                "api_request",
                "api request",
                "status_code",
                "body_contains",
                "json_path",
                "http_status",
            ],
        ),
        (
            "prompt",
            &[
                "prompt step",
                "agentic prompt",
                "agent instruction",
                "base_prompt",
            ],
        ),
        (
            "check",
            &[
                "check_type",
                "check step",
                "typecheck",
                "lint check",
                "format check",
            ],
        ),
        (
            "test",
            &[
                "test_type",
                "test step",
                "pytest",
                "jest",
                "cargo test",
                "test runner",
            ],
        ),
        ("gate", &["gate step", "gate ", "required_steps"]),
        (
            "spec",
            &["spec step", "spec ", "specification", "behavioral spec"],
        ),
    ];

    let mut best_type: Option<&str> = None;
    let mut best_count = 0;

    for (step_type, keywords) in mappings {
        let count = keywords.iter().filter(|kw| lower.contains(**kw)).count();
        if count > best_count {
            best_count = count;
            best_type = Some(step_type);
        }
    }

    best_type.map(|s| s.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reflection_fixes (
                id TEXT PRIMARY KEY
            );
            CREATE TABLE IF NOT EXISTS step_type_knowledge (
                id TEXT PRIMARY KEY,
                step_type TEXT NOT NULL,
                layer TEXT NOT NULL DEFAULT 'universal',
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'active',
                provenance TEXT NOT NULL DEFAULT 'seed',
                source_fix_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (source_fix_id) REFERENCES reflection_fixes(id) ON DELETE SET NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stk_step_type ON step_type_knowledge(step_type);
            CREATE INDEX IF NOT EXISTS idx_stk_layer ON step_type_knowledge(layer);
            CREATE INDEX IF NOT EXISTS idx_stk_composite ON step_type_knowledge(step_type, layer, status);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_step_type_knowledge_insert_and_get() {
        let conn = create_test_db();
        let input = InsertKnowledgeInput {
            step_type: "shell_command".to_string(),
            layer: "universal".to_string(),
            title: "Set working_directory".to_string(),
            content: "Always set working_directory for shell commands.".to_string(),
            priority: 10,
            provenance: "seed".to_string(),
            source_fix_id: None,
        };

        let entry = insert_knowledge(&conn, &input).unwrap();
        assert_eq!(entry.step_type, "shell_command");
        assert_eq!(entry.title, "Set working_directory");
        assert_eq!(entry.priority, 10);

        let fetched = get_knowledge(&conn, &entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
        assert_eq!(fetched.content, input.content);
    }

    #[test]
    fn test_step_type_knowledge_update() {
        let conn = create_test_db();
        let input = InsertKnowledgeInput {
            step_type: "api_request".to_string(),
            layer: "universal".to_string(),
            title: "Old title".to_string(),
            content: "Old content".to_string(),
            priority: 5,
            provenance: "seed".to_string(),
            source_fix_id: None,
        };
        let entry = insert_knowledge(&conn, &input).unwrap();

        let update = UpdateKnowledgeInput {
            title: Some("New title".to_string()),
            content: None,
            priority: Some(20),
            status: None,
            layer: None,
        };
        let updated = update_knowledge(&conn, &entry.id, &update).unwrap();
        assert_eq!(updated.title, "New title");
        assert_eq!(updated.priority, 20);
        assert_eq!(updated.content, "Old content");
    }

    #[test]
    fn test_step_type_knowledge_delete() {
        let conn = create_test_db();
        let input = InsertKnowledgeInput {
            step_type: "check".to_string(),
            layer: "universal".to_string(),
            title: "Test".to_string(),
            content: "Test content".to_string(),
            priority: 0,
            provenance: "manual".to_string(),
            source_fix_id: None,
        };
        let entry = insert_knowledge(&conn, &input).unwrap();
        assert!(delete_knowledge(&conn, &entry.id).unwrap());
        assert!(get_knowledge(&conn, &entry.id).unwrap().is_none());
    }

    #[test]
    fn test_step_type_knowledge_list_with_filters() {
        let conn = create_test_db();

        for (st, layer) in &[
            ("shell_command", "universal"),
            ("api_request", "universal"),
            ("shell_command", "system_specific"),
        ] {
            let input = InsertKnowledgeInput {
                step_type: st.to_string(),
                layer: layer.to_string(),
                title: format!("{} {}", st, layer),
                content: "content".to_string(),
                priority: 0,
                provenance: "seed".to_string(),
                source_fix_id: None,
            };
            insert_knowledge(&conn, &input).unwrap();
        }

        // Filter by step_type
        let query = ListKnowledgeQuery {
            step_type: Some("shell_command".to_string()),
            ..Default::default()
        };
        let results = list_knowledge(&conn, &query).unwrap();
        assert_eq!(results.len(), 2);

        // Filter by layer
        let query = ListKnowledgeQuery {
            layer: Some("universal".to_string()),
            ..Default::default()
        };
        let results = list_knowledge(&conn, &query).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_load_knowledge_for_step_types() {
        let conn = create_test_db();

        for (st, priority) in &[
            ("shell_command", 10),
            ("shell_command", 5),
            ("api_request", 8),
            ("check", 3),
        ] {
            let input = InsertKnowledgeInput {
                step_type: st.to_string(),
                layer: "universal".to_string(),
                title: format!("{} p{}", st, priority),
                content: format!("content for {} priority {}", st, priority),
                priority: *priority,
                provenance: "seed".to_string(),
                source_fix_id: None,
            };
            insert_knowledge(&conn, &input).unwrap();
        }

        // Load for shell_command and api_request only
        let entries = load_knowledge_for_step_types(&conn, &["shell_command", "api_request"]);
        assert_eq!(entries.len(), 3); // 2 shell_command + 1 api_request
                                      // Verify ordering: within each step_type, higher priority first
        let shell_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.step_type == "shell_command")
            .collect();
        assert_eq!(shell_entries[0].priority, 10);
        assert_eq!(shell_entries[1].priority, 5);
    }

    #[test]
    fn test_load_knowledge_excludes_disabled() {
        let conn = create_test_db();

        let input = InsertKnowledgeInput {
            step_type: "gate".to_string(),
            layer: "universal".to_string(),
            title: "Active entry".to_string(),
            content: "Active".to_string(),
            priority: 0,
            provenance: "seed".to_string(),
            source_fix_id: None,
        };
        insert_knowledge(&conn, &input).unwrap();

        let input2 = InsertKnowledgeInput {
            step_type: "gate".to_string(),
            layer: "universal".to_string(),
            title: "Disabled entry".to_string(),
            content: "Disabled".to_string(),
            priority: 0,
            provenance: "seed".to_string(),
            source_fix_id: None,
        };
        let disabled = insert_knowledge(&conn, &input2).unwrap();
        update_knowledge(
            &conn,
            &disabled.id,
            &UpdateKnowledgeInput {
                status: Some("disabled".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let entries = load_knowledge_for_step_types(&conn, &["gate"]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Active entry");
    }

    #[test]
    fn test_format_knowledge_as_markdown() {
        let entries = vec![
            StepTypeKnowledge {
                id: "1".into(),
                step_type: "shell_command".into(),
                layer: "universal".into(),
                title: "Set working_directory".into(),
                content: "Always set working_directory for shell commands.".into(),
                priority: 10,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
            StepTypeKnowledge {
                id: "2".into(),
                step_type: "api_request".into(),
                layer: "universal".into(),
                title: "Content assertions".into(),
                content: "Always include content-specific assertions.".into(),
                priority: 10,
                status: "active".into(),
                provenance: "seed".into(),
                source_fix_id: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
        ];

        let md = format_knowledge_as_markdown(&entries);
        assert!(md.contains("## Step Type Best Practices"));
        assert!(md.contains("### api_request"));
        assert!(md.contains("### shell_command"));
        assert!(md.contains("- **Set working_directory**: Always set working_directory"));
        assert!(
            md.contains("- **Content assertions**: Always include content-specific assertions.")
        );
    }

    #[test]
    fn test_format_knowledge_empty() {
        assert_eq!(format_knowledge_as_markdown(&[]), "");
    }

    #[test]
    fn test_infer_step_type_from_fix() {
        assert_eq!(
            infer_step_type_from_fix("shell_command missing working_directory"),
            Some("shell_command".to_string())
        );
        assert_eq!(
            infer_step_type_from_fix("api_request needs body_contains assertion"),
            Some("api_request".to_string())
        );
        assert_eq!(
            infer_step_type_from_fix("gate step missing required_steps"),
            Some("gate".to_string())
        );
        assert_eq!(
            infer_step_type_from_fix("prompt instructions unclear"),
            Some("prompt".to_string())
        );
        assert_eq!(infer_step_type_from_fix("unrelated description"), None);
    }

    #[test]
    fn test_load_knowledge_empty_step_types() {
        let conn = create_test_db();
        let entries = load_knowledge_for_step_types(&conn, &[]);
        assert!(entries.is_empty());
    }
}
