//! Comparison Snapshots API
//!
//! CRUD endpoints for saving and retrieving reference snapshots
//! used by the UI Bridge compare step and the App Comparison wizard.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info};

use crate::mcp::types::ApiState;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSnapshot {
    pub id: String,
    pub name: String,
    pub app_url: String,
    pub snapshot_data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSnapshotRequest {
    pub name: String,
    pub app_url: String,
    pub snapshot_data: serde_json::Value,
    pub project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListSnapshotsQuery {
    pub project_id: Option<String>,
}

// ============================================================================
// Database Helpers
// ============================================================================

/// Ensure the comparison_snapshots table exists.
pub fn ensure_table(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS comparison_snapshots (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            app_url TEXT NOT NULL,
            snapshot_data TEXT NOT NULL,
            project_id TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_comparison_snapshots_project
            ON comparison_snapshots(project_id);",
    )
}

fn list_snapshots(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
) -> rusqlite::Result<Vec<SavedSnapshot>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match project_id {
        Some(pid) => (
            "SELECT id, name, app_url, snapshot_data, project_id, created_at \
             FROM comparison_snapshots WHERE project_id = ?1 ORDER BY created_at DESC"
                .to_string(),
            vec![Box::new(pid.to_string())],
        ),
        None => (
            "SELECT id, name, app_url, snapshot_data, project_id, created_at \
             FROM comparison_snapshots ORDER BY created_at DESC"
                .to_string(),
            vec![],
        ),
    };

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        let snapshot_data_str: String = row.get(3)?;
        let snapshot_data: serde_json::Value =
            serde_json::from_str(&snapshot_data_str).unwrap_or(serde_json::Value::Null);
        Ok(SavedSnapshot {
            id: row.get(0)?,
            name: row.get(1)?,
            app_url: row.get(2)?,
            snapshot_data,
            project_id: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;

    rows.collect()
}

fn get_snapshot(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<SavedSnapshot>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, app_url, snapshot_data, project_id, created_at \
         FROM comparison_snapshots WHERE id = ?1",
    )?;

    let mut rows = stmt.query_map([id], |row| {
        let snapshot_data_str: String = row.get(3)?;
        let snapshot_data: serde_json::Value =
            serde_json::from_str(&snapshot_data_str).unwrap_or(serde_json::Value::Null);
        Ok(SavedSnapshot {
            id: row.get(0)?,
            name: row.get(1)?,
            app_url: row.get(2)?,
            snapshot_data,
            project_id: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;

    match rows.next() {
        Some(Ok(snap)) => Ok(Some(snap)),
        Some(Err(e)) => Err(e),
        None => Ok(None),
    }
}

fn insert_snapshot(conn: &rusqlite::Connection, snapshot: &SavedSnapshot) -> rusqlite::Result<()> {
    let snapshot_data_str = serde_json::to_string(&snapshot.snapshot_data).unwrap_or_default();
    conn.execute(
        "INSERT INTO comparison_snapshots (id, name, app_url, snapshot_data, project_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            snapshot.id,
            snapshot.name,
            snapshot.app_url,
            snapshot_data_str,
            snapshot.project_id,
            snapshot.created_at,
        ],
    )?;
    Ok(())
}

fn delete_snapshot_by_id(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<bool> {
    let rows_affected = conn.execute("DELETE FROM comparison_snapshots WHERE id = ?1", [id])?;
    Ok(rows_affected > 0)
}

// ============================================================================
// Handlers
// ============================================================================

async fn handle_list(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ListSnapshotsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Database error: {}", e),
        )
    })?;

    if let Err(e) = ensure_table(&conn) {
        error!("Failed to ensure comparison_snapshots table: {}", e);
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    match list_snapshots(&conn, query.project_id.as_deref()) {
        Ok(snapshots) => Ok(Json(serde_json::json!({ "snapshots": snapshots }))),
        Err(e) => {
            error!("Failed to list comparison snapshots: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
    }
}

async fn handle_get(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<SavedSnapshot>, (StatusCode, String)> {
    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Database error: {}", e),
        )
    })?;

    if let Err(e) = ensure_table(&conn) {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    match get_snapshot(&conn, &id) {
        Ok(Some(snap)) => Ok(Json(snap)),
        Ok(None) => Err((StatusCode::NOT_FOUND, format!("Snapshot {} not found", id))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn handle_create(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<CreateSnapshotRequest>,
) -> Result<Json<SavedSnapshot>, (StatusCode, String)> {
    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Database error: {}", e),
        )
    })?;

    if let Err(e) = ensure_table(&conn) {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    let snapshot = SavedSnapshot {
        id: uuid::Uuid::new_v4().to_string(),
        name: body.name,
        app_url: body.app_url,
        snapshot_data: body.snapshot_data,
        project_id: body.project_id,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    match insert_snapshot(&conn, &snapshot) {
        Ok(()) => {
            info!(
                "Saved comparison snapshot: {} ({})",
                snapshot.name, snapshot.id
            );
            Ok(Json(snapshot))
        }
        Err(e) => {
            error!("Failed to save comparison snapshot: {}", e);
            Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
    }
}

async fn handle_delete(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let conn = state.app_state.checkpoint_db.connection().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Database error: {}", e),
        )
    })?;

    if let Err(e) = ensure_table(&conn) {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    match delete_snapshot_by_id(&conn, &id) {
        Ok(true) => {
            info!("Deleted comparison snapshot: {}", id);
            Ok(Json(serde_json::json!({ "success": true })))
        }
        Ok(false) => Err((StatusCode::NOT_FOUND, format!("Snapshot {} not found", id))),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/comparison-snapshots",
            get(handle_list).post(handle_create),
        )
        .route(
            "/comparison-snapshots/:id",
            get(handle_get).delete(handle_delete),
        )
}
