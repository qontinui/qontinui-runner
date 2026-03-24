//! Unified search across all qontinui data stores.
//!
//! Combines SQL text search + vector similarity for findings, fixes,
//! knowledge, errors, rules, and components into a single ranked result list.

use rusqlite::{params, Connection};
use serde::Serialize;
use tracing::warn;

#[derive(Debug, Clone, Serialize)]
pub struct UnifiedSearchResult {
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    pub snippet: String,
    pub relevance_score: f64,
    pub source_table: String,
}

/// Search across all data stores with a text query.
/// Results are ranked by text match relevance.
pub fn unified_search(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<UnifiedSearchResult>, String> {
    let mut results = Vec::new();
    let query_pattern = format!("%{}%", query);

    // 1. Search task_run_findings
    search_findings(conn, &query_pattern, &mut results);

    // 2. Search reflection_fixes
    search_fixes(conn, &query_pattern, &mut results);

    // 3. Search task_knowledge
    search_knowledge(conn, &query_pattern, &mut results);

    // 4. Search error_events
    search_errors(conn, &query_pattern, &mut results);

    // 5. Search generation_rules
    search_rules(conn, &query_pattern, &mut results);

    // 6. Search unified_workflows
    search_workflows(conn, &query_pattern, &mut results);

    // Sort by relevance DESC, truncate
    results.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit);

    Ok(results)
}

fn search_findings(conn: &Connection, pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    let query = r#"SELECT id, title, description, category, severity
                   FROM task_run_findings
                   WHERE title LIKE ?1 OR description LIKE ?1
                   ORDER BY detected_at DESC
                   LIMIT 20"#;

    let outcome = conn.prepare(query).and_then(|mut stmt| {
        let rows = stmt.query_map(params![pattern], |row| {
            let id: String = row.get(0)?;
            let title: String = row.get(1)?;
            let description: String = row.get(2)?;
            let category: String = row.get(3)?;
            let severity: String = row.get(4)?;
            Ok((id, title, description, category, severity))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            if let Ok((id, title, description, category, severity)) = row {
                let title_matches = title.to_lowercase().contains(
                    &pattern
                        .trim_matches('%')
                        .to_lowercase(),
                );
                let score = if title_matches { 1.0 } else { 0.7 };
                let snippet = format!("[{}][{}] {}", category, severity, truncate_str(&description, 120));
                collected.push(UnifiedSearchResult {
                    entity_type: "finding".to_string(),
                    entity_id: id,
                    title,
                    snippet,
                    relevance_score: score,
                    source_table: "task_run_findings".to_string(),
                });
            }
        }
        Ok(collected)
    });

    match outcome {
        Ok(items) => results.extend(items),
        Err(e) => warn!("Unified search: findings query failed: {}", e),
    }
}

fn search_fixes(conn: &Connection, pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    let query = r#"SELECT id, fix_type, fix_description, reflection_scope, status
                   FROM reflection_fixes
                   WHERE fix_description LIKE ?1 OR fix_type LIKE ?1
                   ORDER BY created_at DESC
                   LIMIT 20"#;

    let outcome = conn.prepare(query).and_then(|mut stmt| {
        let rows = stmt.query_map(params![pattern], |row| {
            let id: String = row.get(0)?;
            let fix_type: String = row.get(1)?;
            let fix_description: String = row.get(2)?;
            let scope: String = row.get(3)?;
            let status: String = row.get(4)?;
            Ok((id, fix_type, fix_description, scope, status))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            if let Ok((id, fix_type, fix_description, scope, status)) = row {
                let type_matches = fix_type.to_lowercase().contains(
                    &pattern
                        .trim_matches('%')
                        .to_lowercase(),
                );
                let score = if type_matches { 1.0 } else { 0.7 };
                let snippet = format!("[{}][{}] {}", scope, status, truncate_str(&fix_description, 120));
                collected.push(UnifiedSearchResult {
                    entity_type: "fix".to_string(),
                    entity_id: id,
                    title: fix_type,
                    snippet,
                    relevance_score: score,
                    source_table: "reflection_fixes".to_string(),
                });
            }
        }
        Ok(collected)
    });

    match outcome {
        Ok(items) => results.extend(items),
        Err(e) => warn!("Unified search: fixes query failed: {}", e),
    }
}

fn search_knowledge(conn: &Connection, pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    let query = r#"SELECT id, category, content, confidence
                   FROM task_knowledge
                   WHERE content LIKE ?1 OR category LIKE ?1
                   ORDER BY created_at DESC
                   LIMIT 20"#;

    let outcome = conn.prepare(query).and_then(|mut stmt| {
        let rows = stmt.query_map(params![pattern], |row| {
            let id: String = row.get(0)?;
            let category: String = row.get(1)?;
            let content: String = row.get(2)?;
            let confidence: String = row.get(3)?;
            Ok((id, category, content, confidence))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            if let Ok((id, category, content, confidence)) = row {
                let cat_matches = category.to_lowercase().contains(
                    &pattern
                        .trim_matches('%')
                        .to_lowercase(),
                );
                let score = if cat_matches { 1.0 } else { 0.7 };
                let snippet = format!("[{}][conf:{}] {}", category, confidence, truncate_str(&content, 120));
                collected.push(UnifiedSearchResult {
                    entity_type: "knowledge".to_string(),
                    entity_id: id,
                    title: format!("{} knowledge", category),
                    snippet,
                    relevance_score: score,
                    source_table: "task_knowledge".to_string(),
                });
            }
        }
        Ok(collected)
    });

    match outcome {
        Ok(items) => results.extend(items),
        Err(e) => warn!("Unified search: knowledge query failed: {}", e),
    }
}

fn search_errors(conn: &Connection, pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    let query = r#"SELECT id, log_source_name, severity, message, error_type
                   FROM error_events
                   WHERE message LIKE ?1 OR error_type LIKE ?1
                   ORDER BY last_seen_at DESC
                   LIMIT 20"#;

    let outcome = conn.prepare(query).and_then(|mut stmt| {
        let rows = stmt.query_map(params![pattern], |row| {
            let id: i64 = row.get(0)?;
            let source: String = row.get(1)?;
            let severity: String = row.get(2)?;
            let message: String = row.get(3)?;
            let error_type: Option<String> = row.get(4)?;
            Ok((id, source, severity, message, error_type))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            if let Ok((id, source, severity, message, error_type)) = row {
                let type_matches = error_type
                    .as_deref()
                    .map(|t| {
                        t.to_lowercase().contains(
                            &pattern
                                .trim_matches('%')
                                .to_lowercase(),
                        )
                    })
                    .unwrap_or(false);
                let score = if type_matches { 1.0 } else { 0.7 };
                let title = error_type.unwrap_or_else(|| format!("{} error", source));
                let snippet = format!("[{}][{}] {}", source, severity, truncate_str(&message, 120));
                collected.push(UnifiedSearchResult {
                    entity_type: "error".to_string(),
                    entity_id: id.to_string(),
                    title,
                    snippet,
                    relevance_score: score,
                    source_table: "error_events".to_string(),
                });
            }
        }
        Ok(collected)
    });

    match outcome {
        Ok(items) => results.extend(items),
        Err(e) => warn!("Unified search: errors query failed: {}", e),
    }
}

fn search_rules(conn: &Connection, pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    let query = r#"SELECT id, agent, title, content, severity, status
                   FROM generation_rules
                   WHERE title LIKE ?1 OR content LIKE ?1
                   ORDER BY created_at DESC
                   LIMIT 20"#;

    let outcome = conn.prepare(query).and_then(|mut stmt| {
        let rows = stmt.query_map(params![pattern], |row| {
            let id: String = row.get(0)?;
            let agent: String = row.get(1)?;
            let title: String = row.get(2)?;
            let content: String = row.get(3)?;
            let severity: String = row.get(4)?;
            let status: String = row.get(5)?;
            Ok((id, agent, title, content, severity, status))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            if let Ok((id, agent, title, content, severity, status)) = row {
                let title_matches = title.to_lowercase().contains(
                    &pattern
                        .trim_matches('%')
                        .to_lowercase(),
                );
                let score = if title_matches { 1.0 } else { 0.7 };
                let snippet = format!("[{}][{}][{}] {}", agent, severity, status, truncate_str(&content, 120));
                collected.push(UnifiedSearchResult {
                    entity_type: "rule".to_string(),
                    entity_id: id,
                    title: format!("{} ({})", title, agent),
                    snippet,
                    relevance_score: score,
                    source_table: "generation_rules".to_string(),
                });
            }
        }
        Ok(collected)
    });

    match outcome {
        Ok(items) => results.extend(items),
        Err(e) => warn!("Unified search: rules query failed: {}", e),
    }
}

fn search_workflows(conn: &Connection, pattern: &str, results: &mut Vec<UnifiedSearchResult>) {
    let query = r#"SELECT id, name, description, category
                   FROM unified_workflows
                   WHERE name LIKE ?1 OR description LIKE ?1
                   ORDER BY updated_at DESC
                   LIMIT 20"#;

    let outcome = conn.prepare(query).and_then(|mut stmt| {
        let rows = stmt.query_map(params![pattern], |row| {
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let description: String = row.get(2)?;
            let category: String = row.get(3)?;
            Ok((id, name, description, category))
        })?;

        let mut collected = Vec::new();
        for row in rows {
            if let Ok((id, name, description, category)) = row {
                let name_matches = name.to_lowercase().contains(
                    &pattern
                        .trim_matches('%')
                        .to_lowercase(),
                );
                let score = if name_matches { 1.0 } else { 0.7 };
                let snippet = format!("[{}] {}", category, truncate_str(&description, 120));
                collected.push(UnifiedSearchResult {
                    entity_type: "workflow".to_string(),
                    entity_id: id,
                    title: name,
                    snippet,
                    relevance_score: score,
                    source_table: "unified_workflows".to_string(),
                });
            }
        }
        Ok(collected)
    });

    match outcome {
        Ok(items) => results.extend(items),
        Err(e) => warn!("Unified search: workflows query failed: {}", e),
    }
}

/// Truncate a string to a maximum character length, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        let long = "a".repeat(200);
        let result = truncate_str(&long, 120);
        assert!(result.ends_with("..."));
        // 120 chars + "..."
        assert_eq!(result.len(), 123);
    }

    #[test]
    fn test_truncate_str_exact() {
        let exact = "a".repeat(120);
        assert_eq!(truncate_str(&exact, 120), exact);
    }
}
