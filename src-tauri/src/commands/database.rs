//! Database maintenance commands
//!
//! This module provides database maintenance and optimization operations:
//! - Optimizing the database (VACUUM + ANALYZE)
//! - Getting database statistics
//! - Running EXPLAIN QUERY PLAN for debugging

use crate::database::DatabaseStats;
use crate::error::AppError;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tauri::State;
use tracing::info;

use super::compartments::StorageCompartment;
use super::CommandResponse;

// Migrated to StorageCompartment (Workstream C).

/// Optimize the database for better performance.
///
/// Runs VACUUM and ANALYZE to:
/// - Rebuild the database file and reclaim unused space
/// - Update statistics for the query planner
///
/// # Arguments
/// * `run_integrity_check` - Whether to run integrity check before VACUUM
/// * `state` - The application state containing the database connection
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with optimization statistics
/// * `Err(String)` - Error message if optimization fails
#[tauri::command]
pub fn optimize_database(
    run_integrity_check: Option<bool>,
    _state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    info!(
        "Optimizing database (integrity_check={})",
        run_integrity_check.unwrap_or(false)
    );

    // checkpoint_db removed — SQLite optimization is no longer applicable.
    // PG databases are maintained via standard PostgreSQL VACUUM/ANALYZE.
    info!("optimize_database called — no-op after checkpoint_db removal");

    Ok(CommandResponse {
        success: true,
        message: Some(
            "No-op: SQLite checkpoint_db has been removed. Use PostgreSQL maintenance tools."
                .to_string(),
        ),
        data: None,
    })
}

/// Get database statistics.
///
/// Returns comprehensive database statistics including:
/// - Total size in bytes
/// - Page count and size
/// - Free page count
/// - WAL statistics
/// - Row counts per table
///
/// # Arguments
/// * `state` - The application state containing the database connection
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with database statistics
/// * `Err(String)` - Error message if statistics cannot be retrieved
async fn get_database_stats_impl(
    state: &StorageCompartment,
) -> Result<CommandResponse, AppError> {
    info!("Getting database statistics");

    let stats: DatabaseStats = state
        .pg_db()
        .get_database_stats()
        .await
        .map_err(AppError::DatabaseError)?;

    let message = format!(
        "Database size: {:.2} MB, {} tables, {} total rows",
        stats.total_size_bytes as f64 / (1024.0 * 1024.0),
        stats.table_counts.len(),
        stats.table_counts.iter().map(|t| t.row_count).sum::<i64>()
    );

    info!("{}", message);

    Ok(CommandResponse {
        success: true,
        message: Some(message),
        data: Some(serde_json::to_value(&stats)?),
    })
}

#[tauri::command]
pub async fn get_database_stats(
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    get_database_stats_impl(&state).await.map_err(String::from)
}

/// Run EXPLAIN QUERY PLAN on a query for debugging.
///
/// Returns the query execution plan which shows how SQLite will execute the query,
/// including which indexes will be used.
///
/// # Arguments
/// * `query` - The SQL query to analyze
/// * `state` - The application state containing the database connection
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with query plan
/// * `Err(String)` - Error message if query plan cannot be generated
async fn explain_query_plan_impl(
    query: String,
    state: &StorageCompartment,
) -> Result<CommandResponse, AppError> {
    info!("Running EXPLAIN QUERY PLAN for query: {}", query);

    let plan = state
        .pg_db()
        .explain_query_plan(&query)
        .await
        .map_err(AppError::DatabaseError)?;

    Ok(CommandResponse {
        success: true,
        message: Some("Query plan generated".to_string()),
        data: Some(serde_json::json!({
            "query": query,
            "plan": plan
        })),
    })
}

#[tauri::command]
pub async fn explain_query_plan(
    query: String,
    state: State<'_, StorageCompartment>,
) -> Result<CommandResponse, String> {
    explain_query_plan_impl(query, &state)
        .await
        .map_err(String::from)
}

/// Build the Tauri plugin that registers this module's command handlers.
///
/// See `commands/mod.rs` for the migration guide explaining the plugin pattern.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_database")
        .invoke_handler(tauri::generate_handler![
            optimize_database,
            get_database_stats,
            explain_query_plan,
        ])
        .build()
}
