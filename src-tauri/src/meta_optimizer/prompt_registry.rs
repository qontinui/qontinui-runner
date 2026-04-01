//! CRUD operations for the prompt_registry table.
//!
//! Stores prompt variants for pipeline agents. The optimizer creates new variants;
//! humans activate them from the UI.

use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::info;

use super::types::PromptVariant;
use crate::database::pg::PgDb;

/// Get the currently active prompt for a given agent type.
/// Returns None if no active variant exists (pipeline should use its default).
pub fn get_active_prompt(
    pg_db: &Arc<PgDb>,
    agent_type: &str,
) -> Result<Option<PromptVariant>, String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.get_active_prompt(agent_type))
    })
}

/// Create a new prompt variant (initially inactive).
pub fn create_variant(
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
pub fn activate_variant(pg_db: &Arc<PgDb>, variant_id: &str) -> Result<(), String> {
    tokio::task::block_in_place(|| {
        Handle::current().block_on(pg_db.activate_variant(variant_id))
    })
}

/// List all prompt variants, optionally filtered by agent type.
pub fn list_variants(
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
pub fn create_variant_with_pg(pg_db: &std::sync::Arc<crate::database::pg::PgDb>, agent_type: &str, variant_name: &str, prompt_content: &str, source_recommendation_id: Option<&str>) -> Result<PromptVariant, String> {
    create_variant(pg_db, agent_type, variant_name, prompt_content, source_recommendation_id)
}

/// Activate a prompt variant with PG dual-write (fire-and-forget).
#[deprecated(note = "Use activate_variant directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn activate_variant_with_pg(pg_db: &std::sync::Arc<crate::database::pg::PgDb>, variant_id: &str) -> Result<(), String> {
    activate_variant(pg_db, variant_id)
}

/// Update performance metrics with PG dual-write (fire-and-forget).
#[deprecated(note = "Use update_performance_metrics directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn update_performance_metrics_with_pg(pg_db: &std::sync::Arc<crate::database::pg::PgDb>, variant_id: &str, metrics_json: &str) -> Result<(), String> {
    update_performance_metrics(pg_db, variant_id, metrics_json)
}

// ── PG-primary read wrappers ─────────────────────────────────────────────

/// Get the active prompt with PG-primary read.
#[allow(dead_code)]
pub fn get_active_prompt_with_pg(
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
    get_active_prompt(pg_db, agent_type)
}

/// List all prompt variants with PG-primary read.
#[deprecated(note = "Use list_variants directly — it is now PG-primary")]
#[allow(dead_code)]
pub fn list_variants_with_pg(
    pg_db: &std::sync::Arc<crate::database::pg::PgDb>,
    agent_type: Option<&str>,
) -> Result<Vec<PromptVariant>, String> {
    list_variants(pg_db, agent_type)
}

/// Get a prompt variant by version with PG-primary read.
#[allow(dead_code)]
pub fn get_prompt_by_version_with_pg(
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
    get_prompt_by_version(pg_db, agent_type, version)
}
