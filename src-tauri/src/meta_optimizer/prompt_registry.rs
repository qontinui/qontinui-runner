//! CRUD operations for the prompt_registry table.
//!
//! Stores prompt variants for pipeline agents. The optimizer creates new variants;
//! humans activate them from the UI.

use std::sync::Arc;
use rusqlite::params;
use tokio::runtime::Handle;
use tracing::info;

use super::types::PromptVariant;
use crate::database::CheckpointDb;
use crate::database::pg::PgDb;

/// Get the currently active prompt for a given agent type.
/// Returns None if no active variant exists (pipeline should use its default).
pub fn get_active_prompt(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    agent_type: &str,
) -> Result<Option<PromptVariant>, String> {
    let _ = db;
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_active_prompt(agent_type))
    })
}

/// Get the active prompt using SQLite only (for contexts without PG access).
/// Retained for backward compatibility in compute_group_metrics_inner.
#[allow(dead_code)]
pub(crate) fn get_active_prompt_sqlite(
    db: &CheckpointDb,
    agent_type: &str,
) -> Result<Option<PromptVariant>, String> {
    let agent_type = agent_type.to_string();
    db.with_conn(move |conn| {
        let result = conn.query_row(
            r#"SELECT id, agent_type, variant_name, prompt_content, version,
                      is_active, source_recommendation_id, performance_metrics,
                      created_at, updated_at
               FROM prompt_registry WHERE agent_type = ?1 AND is_active = 1 LIMIT 1"#,
            params![agent_type],
            |row| Ok(PromptVariant {
                id: row.get(0)?, agent_type: row.get(1)?, variant_name: row.get(2)?,
                prompt_content: row.get(3)?, version: row.get(4)?, is_active: row.get::<_, i32>(5)? != 0,
                source_recommendation_id: row.get(6)?, performance_metrics: row.get(7)?,
                created_at: row.get(8)?, updated_at: row.get(9)?,
            }),
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
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    variant_name: &str,
    prompt_content: &str,
    source_recommendation_id: Option<&str>,
) -> Result<PromptVariant, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.create_prompt_variant(agent_type, variant_name, prompt_content, source_recommendation_id))
    })
}

/// Activate a prompt variant (deactivating any previously active variant for that agent_type).
pub fn activate_variant(db: &CheckpointDb, pg_db: &Arc<PgDb>, variant_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.activate_variant(variant_id))
    })
}

/// List all prompt variants, optionally filtered by agent type.
pub fn list_variants(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    agent_type: Option<&str>,
) -> Result<Vec<PromptVariant>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.list_variants(agent_type))
    })
}

/// Get a prompt variant by agent_type and version number.
/// Returns None if no variant with that version exists.
pub fn get_prompt_by_version(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    agent_type: &str,
    version: i32,
) -> Result<Option<PromptVariant>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_prompt_by_version(agent_type, version))
    })
}

/// Update performance metrics for a prompt variant.
pub fn update_performance_metrics(
    db: &CheckpointDb,
    pg_db: &Arc<PgDb>,
    variant_id: &str,
    metrics_json: &str,
) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.update_performance_metrics(variant_id, metrics_json))
    })
}

// ── PG dual-write wrappers ─────────────────────────────────────────────

/// Create a prompt variant with PG dual-write (fire-and-forget).
#[deprecated(note = "Use create_variant directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn create_variant_with_pg(db: &CheckpointDb, pg_db: &std::sync::Arc<crate::database::pg::PgDb>, agent_type: &str, variant_name: &str, prompt_content: &str, source_recommendation_id: Option<&str>) -> Result<PromptVariant, String> {
    create_variant(db, pg_db, agent_type, variant_name, prompt_content, source_recommendation_id)
}

/// Activate a prompt variant with PG dual-write (fire-and-forget).
#[deprecated(note = "Use activate_variant directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn activate_variant_with_pg(db: &CheckpointDb, pg_db: &std::sync::Arc<crate::database::pg::PgDb>, variant_id: &str) -> Result<(), String> {
    activate_variant(db, pg_db, variant_id)
}

/// Update performance metrics with PG dual-write (fire-and-forget).
#[deprecated(note = "Use update_performance_metrics directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn update_performance_metrics_with_pg(db: &CheckpointDb, pg_db: &std::sync::Arc<crate::database::pg::PgDb>, variant_id: &str, metrics_json: &str) -> Result<(), String> {
    update_performance_metrics(db, pg_db, variant_id, metrics_json)
}

// ── PG-primary read wrappers ─────────────────────────────────────────────

/// Get the active prompt with PG-primary read.
#[allow(dead_code)]
pub fn get_active_prompt_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: &str,
) -> Result<Option<PromptVariant>, String> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let pg = pg_db.clone();
        let at = agent_type.to_string();
        if let Ok(result) = handle.block_on(pg.get_active_prompt(&at)) {
            return Ok(result);
        }
    }
    get_active_prompt(db, pg_db, agent_type)
}

/// List all prompt variants with PG-primary read.
#[deprecated(note = "Use list_variants directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn list_variants_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: Option<&str>,
) -> Result<Vec<PromptVariant>, String> {
    list_variants(db, pg_db, agent_type)
}

/// Get a prompt variant by version with PG-primary read.
#[allow(dead_code)]
pub fn get_prompt_by_version_with_pg(
    db: &CheckpointDb,
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: &str,
    version: i32,
) -> Result<Option<PromptVariant>, String> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let pg = pg_db.clone();
        let at = agent_type.to_string();
        if let Ok(result) = handle.block_on(pg.get_prompt_by_version(&at, version)) {
            return Ok(result);
        }
    }
    get_prompt_by_version(db, pg_db, agent_type, version)
}

#[cfg(all(test, feature = "sqlite_tests"))]
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
        let variant =
            create_variant(&db, "planner", "improved_v1", "You are a planner.", None).unwrap();
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
