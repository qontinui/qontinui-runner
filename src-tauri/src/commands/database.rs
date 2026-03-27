//! Database maintenance commands
//!
//! This module provides database maintenance and optimization operations:
//! - Optimizing the database (VACUUM + ANALYZE)
//! - Getting database statistics
//! - Running EXPLAIN QUERY PLAN for debugging

use crate::database::{DatabaseOptimizeResult, DatabaseStats};
use std::sync::Arc;
use tauri::State;
use tracing::info;

use super::{AppState, CommandResponse};

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
    state: State<Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!(
        "Optimizing database (integrity_check={})",
        run_integrity_check.unwrap_or(false)
    );

    let result: DatabaseOptimizeResult = state
        .checkpoint_db
        .optimize_database(run_integrity_check.unwrap_or(false))?;

    let message = format!(
        "Database optimized in {}ms. Space reclaimed: {} bytes ({:.2} MB)",
        result.duration_ms,
        result.space_reclaimed_bytes,
        result.space_reclaimed_bytes as f64 / (1024.0 * 1024.0)
    );

    info!("{}", message);

    Ok(CommandResponse {
        success: true,
        message: Some(message),
        data: Some(
            serde_json::to_value(&result)
                .map_err(|e| format!("Failed to serialize optimization result: {}", e))?,
        ),
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
#[tauri::command]
pub async fn get_database_stats(state: State<'_, Arc<AppState>>) -> Result<CommandResponse, String> {
    info!("Getting database statistics");

    let stats: DatabaseStats = if let Some(pg) = &state.pg_db {
        pg.get_database_stats().await?
    } else {
        state.checkpoint_db.get_database_stats()?
    };

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
        data: Some(
            serde_json::to_value(&stats)
                .map_err(|e| format!("Failed to serialize database stats: {}", e))?,
        ),
    })
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
#[tauri::command]
pub async fn explain_query_plan(
    query: String,
    state: State<'_, Arc<AppState>>,
) -> Result<CommandResponse, String> {
    info!("Running EXPLAIN QUERY PLAN for query: {}", query);

    let plan = if let Some(pg) = &state.pg_db {
        pg.explain_query_plan(&query).await?
    } else {
        state.checkpoint_db.explain_query_plan(&query)?
    };

    Ok(CommandResponse {
        success: true,
        message: Some("Query plan generated".to_string()),
        data: Some(serde_json::json!({
            "query": query,
            "plan": plan
        })),
    })
}
