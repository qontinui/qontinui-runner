//! CRUD operations for the prompt_registry table.
//!
//! Stores prompt variants for pipeline agents. The optimizer creates new variants;
//! humans activate them from the UI.

use rusqlite::params;
use tracing::info;

use crate::database::CheckpointDb;
use super::types::PromptVariant;

/// Get the currently active prompt for a given agent type.
/// Returns None if no active variant exists (pipeline should use its default).
pub fn get_active_prompt(db: &CheckpointDb, agent_type: &str) -> Result<Option<PromptVariant>, String> {
    let agent_type = agent_type.to_string();
    db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT id, agent_type, variant_name, prompt_content, version,
                      is_active, source_recommendation_id, performance_metrics,
                      created_at, updated_at
               FROM prompt_registry
               WHERE agent_type = ?1 AND is_active = 1
               LIMIT 1"#,
            params![agent_type],
            |row| {
                Ok(PromptVariant {
                    id: row.get(0)?,
                    agent_type: row.get(1)?,
                    variant_name: row.get(2)?,
                    prompt_content: row.get(3)?,
                    version: row.get(4)?,
                    is_active: row.get::<_, i32>(5)? != 0,
                    source_recommendation_id: row.get(6)?,
                    performance_metrics: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            },
        );

        match result {
            Ok(variant) => Ok(Some(variant)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(format!("Failed to get active prompt: {}", e)),
        }
    })
}

/// Create a new prompt variant (initially inactive).
pub fn create_variant(
    db: &CheckpointDb,
    agent_type: &str,
    variant_name: &str,
    prompt_content: &str,
    source_recommendation_id: Option<&str>,
) -> Result<PromptVariant, String> {
    let id = format!("pv-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let agent_type = agent_type.to_string();
    let variant_name = variant_name.to_string();
    let prompt_content = prompt_content.to_string();
    let source_rec_id = source_recommendation_id.map(|s| s.to_string());
    let id_clone = id.clone();
    let now_clone = now.clone();

    db.with_conn(move |conn| {
        // Determine next version for this agent_type + variant_name
        let next_version: i32 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM prompt_registry WHERE agent_type = ?1 AND variant_name = ?2",
                params![agent_type, variant_name],
                |row| row.get(0),
            )
            .unwrap_or(1);

        conn.execute(
            r#"INSERT INTO prompt_registry
               (id, agent_type, variant_name, prompt_content, version, is_active,
                source_recommendation_id, performance_metrics, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, '{}', ?7, ?7)"#,
            params![
                id_clone,
                agent_type,
                variant_name,
                prompt_content,
                next_version,
                source_rec_id,
                now_clone,
            ],
        )
        .map_err(|e| format!("Failed to create prompt variant: {}", e))?;

        info!("Created prompt variant {} for agent {} (v{})", id_clone, agent_type, next_version);

        Ok(PromptVariant {
            id: id_clone,
            agent_type,
            variant_name,
            prompt_content,
            version: next_version,
            is_active: false,
            source_recommendation_id: source_rec_id,
            performance_metrics: Some("{}".to_string()),
            created_at: now_clone.clone(),
            updated_at: now_clone,
        })
    })
}

/// Activate a prompt variant (deactivating any previously active variant for that agent_type).
pub fn activate_variant(db: &CheckpointDb, variant_id: &str) -> Result<(), String> {
    let variant_id = variant_id.to_string();
    let now = chrono::Utc::now().to_rfc3339();

    db.with_conn(move |conn| {
        // Get the agent_type for this variant
        let agent_type: String = conn
            .query_row(
                "SELECT agent_type FROM prompt_registry WHERE id = ?1",
                params![variant_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Variant not found: {}", e))?;

        // Deactivate all variants for this agent_type
        conn.execute(
            "UPDATE prompt_registry SET is_active = 0, updated_at = ?1 WHERE agent_type = ?2",
            params![now, agent_type],
        )
        .map_err(|e| format!("Failed to deactivate variants: {}", e))?;

        // Activate the requested variant
        conn.execute(
            "UPDATE prompt_registry SET is_active = 1, updated_at = ?1 WHERE id = ?2",
            params![now, variant_id],
        )
        .map_err(|e| format!("Failed to activate variant: {}", e))?;

        info!("Activated prompt variant {} for agent {}", variant_id, agent_type);
        Ok(())
    })
}

/// List all prompt variants, optionally filtered by agent type.
pub fn list_variants(
    db: &CheckpointDb,
    agent_type: Option<&str>,
) -> Result<Vec<PromptVariant>, String> {
    let agent_type = agent_type.map(|s| s.to_string());

    db.with_conn(move |conn| {
        let (sql, param): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ref at) = agent_type {
            (
                r#"SELECT id, agent_type, variant_name, prompt_content, version,
                          is_active, source_recommendation_id, performance_metrics,
                          created_at, updated_at
                   FROM prompt_registry WHERE agent_type = ?1
                   ORDER BY agent_type, variant_name, version DESC"#
                    .to_string(),
                vec![Box::new(at.clone())],
            )
        } else {
            (
                r#"SELECT id, agent_type, variant_name, prompt_content, version,
                          is_active, source_recommendation_id, performance_metrics,
                          created_at, updated_at
                   FROM prompt_registry
                   ORDER BY agent_type, variant_name, version DESC"#
                    .to_string(),
                vec![],
            )
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params_from_iter(param.iter()), |row| {
                Ok(PromptVariant {
                    id: row.get(0)?,
                    agent_type: row.get(1)?,
                    variant_name: row.get(2)?,
                    prompt_content: row.get(3)?,
                    version: row.get(4)?,
                    is_active: row.get::<_, i32>(5)? != 0,
                    source_recommendation_id: row.get(6)?,
                    performance_metrics: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })
            .map_err(|e| format!("Failed to query variants: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    })
}

/// Update performance metrics for a prompt variant.
pub fn update_performance_metrics(
    db: &CheckpointDb,
    variant_id: &str,
    metrics_json: &str,
) -> Result<(), String> {
    let variant_id = variant_id.to_string();
    let metrics_json = metrics_json.to_string();
    let now = chrono::Utc::now().to_rfc3339();

    db.with_conn(move |conn| {
        conn.execute(
            "UPDATE prompt_registry SET performance_metrics = ?1, updated_at = ?2 WHERE id = ?3",
            params![metrics_json, now, variant_id],
        )
        .map_err(|e| format!("Failed to update metrics: {}", e))?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::CheckpointDb;

    fn setup_test_db() -> CheckpointDb {
        CheckpointDb::new_in_memory().unwrap()
    }

    #[test]
    fn test_create_and_get_active_prompt() {
        let db = setup_test_db();

        // Create a variant — it should be inactive by default
        let variant = create_variant(&db, "planner", "improved_v1", "You are a planner.", None).unwrap();
        assert!(!variant.is_active);
        assert_eq!(variant.agent_type, "planner");
        assert_eq!(variant.variant_name, "improved_v1");
        assert_eq!(variant.prompt_content, "You are a planner.");
        assert_eq!(variant.version, 1);

        // No active prompt yet
        let active = get_active_prompt(&db, "planner").unwrap();
        assert!(active.is_none());

        // Activate it
        activate_variant(&db, &variant.id).unwrap();

        // Now it should be returned as the active prompt
        let active = get_active_prompt(&db, "planner").unwrap().unwrap();
        assert_eq!(active.id, variant.id);
        assert!(active.is_active);
    }

    #[test]
    fn test_activate_deactivates_others() {
        let db = setup_test_db();

        let v1 = create_variant(&db, "planner", "v1", "Prompt A", None).unwrap();
        let v2 = create_variant(&db, "planner", "v2", "Prompt B", None).unwrap();

        // Activate first
        activate_variant(&db, &v1.id).unwrap();
        let active = get_active_prompt(&db, "planner").unwrap().unwrap();
        assert_eq!(active.id, v1.id);

        // Activate second — first should be deactivated
        activate_variant(&db, &v2.id).unwrap();
        let active = get_active_prompt(&db, "planner").unwrap().unwrap();
        assert_eq!(active.id, v2.id);

        // Verify v1 is no longer active by listing all variants
        let all = list_variants(&db, Some("planner")).unwrap();
        let v1_entry = all.iter().find(|v| v.id == v1.id).unwrap();
        assert!(!v1_entry.is_active);
    }

    #[test]
    fn test_list_variants_filters_by_agent() {
        let db = setup_test_db();

        create_variant(&db, "planner", "v1", "Planner prompt", None).unwrap();
        create_variant(&db, "coder", "v1", "Coder prompt", None).unwrap();
        create_variant(&db, "coder", "v2", "Coder prompt v2", None).unwrap();

        let planner_variants = list_variants(&db, Some("planner")).unwrap();
        assert_eq!(planner_variants.len(), 1);
        assert_eq!(planner_variants[0].agent_type, "planner");

        let coder_variants = list_variants(&db, Some("coder")).unwrap();
        assert_eq!(coder_variants.len(), 2);
        assert!(coder_variants.iter().all(|v| v.agent_type == "coder"));

        // No filter returns all
        let all_variants = list_variants(&db, None).unwrap();
        assert_eq!(all_variants.len(), 3);
    }

    #[test]
    fn test_version_auto_increment() {
        let db = setup_test_db();

        let v1 = create_variant(&db, "planner", "default", "Prompt v1", None).unwrap();
        assert_eq!(v1.version, 1);

        let v2 = create_variant(&db, "planner", "default", "Prompt v2", None).unwrap();
        assert_eq!(v2.version, 2);

        let v3 = create_variant(&db, "planner", "default", "Prompt v3", None).unwrap();
        assert_eq!(v3.version, 3);

        // Different variant name starts at 1
        let other = create_variant(&db, "planner", "experimental", "Exp prompt", None).unwrap();
        assert_eq!(other.version, 1);

        // Different agent_type also starts at 1
        let coder_v1 = create_variant(&db, "coder", "default", "Coder prompt", None).unwrap();
        assert_eq!(coder_v1.version, 1);
    }
}
