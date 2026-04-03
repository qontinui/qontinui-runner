//! API Surface Diff — compare two API surface scans to detect changes:
//! added/removed endpoints, new/resolved orphans, new/broken connections.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::info;

use super::api_surface::{
    ApiConnection, ApiSurface, McpRoute, OrphanedEndpoint, PgMethod, TauriCommand,
};
use crate::mcp::types::{ApiResponse, ApiState};

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSurfaceDiff {
    pub added_commands: Vec<TauriCommand>,
    pub removed_commands: Vec<String>,
    pub added_routes: Vec<McpRoute>,
    pub removed_routes: Vec<String>,
    pub added_pg_methods: Vec<PgMethod>,
    pub removed_pg_methods: Vec<String>,
    pub new_orphans: Vec<OrphanedEndpoint>,
    pub resolved_orphans: Vec<String>,
    pub new_connections: Vec<ApiConnection>,
    pub broken_connections: Vec<ApiConnection>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSurfaceSnapshot {
    pub id: i64,
    pub total_endpoints: i32,
    pub orphan_count: i32,
    pub summary: String,
    pub created_at: String,
}

// ─── Routes ──────────────────────────────────────────────────────────────────

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/api-surface/diff", post(handle_diff))
        .route("/api-surface/snapshots", get(handle_list_snapshots))
        .route("/api-surface/snapshot", post(handle_save_snapshot))
}

// ─── Handlers ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffRequest {
    current: ApiSurface,
}

async fn handle_diff(
    State(state): State<Arc<ApiState>>,
    Json(input): Json<DiffRequest>,
) -> Result<Json<ApiResponse<ApiSurfaceDiff>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    let previous = load_latest_snapshot(pg).await;

    let diff = match previous {
        Some(prev) => compute_diff(&prev, &input.current),
        None => ApiSurfaceDiff {
            added_commands: input.current.tauri_commands.clone(),
            removed_commands: Vec::new(),
            added_routes: input.current.mcp_routes.clone(),
            removed_routes: Vec::new(),
            added_pg_methods: input.current.pg_methods.clone(),
            removed_pg_methods: Vec::new(),
            new_orphans: input.current.orphans.clone(),
            resolved_orphans: Vec::new(),
            new_connections: input.current.connections.clone(),
            broken_connections: Vec::new(),
            summary: "First scan — no previous snapshot to compare against".into(),
        },
    };

    Ok(Json(ApiResponse {
        success: true,
        data: Some(diff),
        error: None,
        error_detail: None,
    }))
}

async fn handle_save_snapshot(
    State(state): State<Arc<ApiState>>,
    Json(surface): Json<ApiSurface>,
) -> Result<Json<ApiResponse<ApiSurfaceSnapshot>>, (axum::http::StatusCode, Json<ApiResponse<()>>)>
{
    let pg = &state.app_state.pg_db;
    let conn = pg.pool().get().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("PG pool error: {}", e)),
                error_detail: None,
            }),
        )
    })?;

    let total_endpoints = (surface.tauri_commands.len()
        + surface.mcp_routes.len()
        + surface.pg_methods.len()
        + surface.clorinde_queries.len()) as i32;
    let orphan_count = surface.orphans.len() as i32;

    let scan_json = serde_json::to_string(&surface).map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Serialize scan: {}", e)),
                error_detail: None,
            }),
        )
    })?;
    let summary = format!(
        "{} commands, {} routes, {} PgDb, {} queries, {} tables, {} orphans",
        surface.summary.total_tauri_commands,
        surface.summary.total_mcp_routes,
        surface.summary.total_pg_methods,
        surface.summary.total_clorinde_queries,
        surface.summary.total_db_tables,
        surface.summary.total_orphans,
    );

    let row = conn
        .query_one(
            "INSERT INTO api_surface_snapshots (scan_json, summary, total_endpoints, orphan_count) \
             VALUES ($1, $2, $3, $4) RETURNING id, created_at::text",
            &[&scan_json, &summary, &total_endpoints, &orphan_count],
        )
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some(format!("PG insert snapshot: {}", e)),
                    error_detail: None,
                }),
            )
        })?;

    let id: i64 = row.get(0);
    let created_at: String = row.get(1);

    info!(
        "Saved API surface snapshot #{} — {} endpoints, {} orphans",
        id, total_endpoints, orphan_count
    );

    Ok(Json(ApiResponse {
        success: true,
        data: Some(ApiSurfaceSnapshot {
            id,
            total_endpoints,
            orphan_count,
            summary,
            created_at,
        }),
        error: None,
        error_detail: None,
    }))
}

async fn handle_list_snapshots(
    State(state): State<Arc<ApiState>>,
) -> Result<
    Json<ApiResponse<Vec<ApiSurfaceSnapshot>>>,
    (axum::http::StatusCode, Json<ApiResponse<()>>),
> {
    let pg = &state.app_state.pg_db;
    let conn = pg.pool().get().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("PG pool error: {}", e)),
                error_detail: None,
            }),
        )
    })?;

    let rows = conn
        .query(
            "SELECT id, total_endpoints, orphan_count, summary, created_at::text \
             FROM api_surface_snapshots ORDER BY created_at DESC LIMIT 50",
            &[],
        )
        .await
        .map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some(format!("PG query snapshots: {}", e)),
                    error_detail: None,
                }),
            )
        })?;

    let snapshots: Vec<ApiSurfaceSnapshot> = rows
        .iter()
        .map(|r| ApiSurfaceSnapshot {
            id: r.get(0),
            total_endpoints: r.get(1),
            orphan_count: r.get(2),
            summary: r.get(3),
            created_at: r.get(4),
        })
        .collect();

    Ok(Json(ApiResponse {
        success: true,
        data: Some(snapshots),
        error: None,
        error_detail: None,
    }))
}

// ─── Internal helpers ────────────────────────────────────────────────────────

async fn load_latest_snapshot(pg: &crate::database::pg::PgDb) -> Option<ApiSurface> {
    let conn = pg.pool().get().await.ok()?;
    let row = conn
        .query_opt(
            "SELECT scan_json FROM api_surface_snapshots ORDER BY created_at DESC LIMIT 1",
            &[],
        )
        .await
        .ok()??;
    let json: String = row.get(0);
    serde_json::from_str(&json).ok()
}

/// Compute a rich diff between two API surface scans using proper set operations.
fn compute_diff(previous: &ApiSurface, current: &ApiSurface) -> ApiSurfaceDiff {
    // Command diff
    let prev_cmd_names: HashSet<&str> = previous
        .tauri_commands
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let curr_cmd_names: HashSet<&str> = current
        .tauri_commands
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let added_commands: Vec<TauriCommand> = current
        .tauri_commands
        .iter()
        .filter(|c| !prev_cmd_names.contains(c.name.as_str()))
        .cloned()
        .collect();
    let removed_commands: Vec<String> = prev_cmd_names
        .difference(&curr_cmd_names)
        .map(|s| s.to_string())
        .collect();

    // Route diff
    let prev_route_keys: HashSet<String> = previous
        .mcp_routes
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    let curr_route_keys: HashSet<String> = current
        .mcp_routes
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    let added_routes: Vec<McpRoute> = current
        .mcp_routes
        .iter()
        .filter(|r| !prev_route_keys.contains(&format!("{} {}", r.method, r.path)))
        .cloned()
        .collect();
    let removed_routes: Vec<String> = prev_route_keys
        .difference(&curr_route_keys)
        .cloned()
        .collect();

    // PgDb method diff
    let prev_pg_names: HashSet<&str> = previous
        .pg_methods
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    let curr_pg_names: HashSet<&str> = current.pg_methods.iter().map(|m| m.name.as_str()).collect();
    let added_pg_methods: Vec<PgMethod> = current
        .pg_methods
        .iter()
        .filter(|m| !prev_pg_names.contains(m.name.as_str()))
        .cloned()
        .collect();
    let removed_pg_methods: Vec<String> = prev_pg_names
        .difference(&curr_pg_names)
        .map(|s| s.to_string())
        .collect();

    // Orphan diff
    let prev_orphan_names: HashSet<&str> =
        previous.orphans.iter().map(|o| o.name.as_str()).collect();
    let curr_orphan_names: HashSet<&str> =
        current.orphans.iter().map(|o| o.name.as_str()).collect();
    let new_orphans: Vec<OrphanedEndpoint> = current
        .orphans
        .iter()
        .filter(|o| !prev_orphan_names.contains(o.name.as_str()))
        .cloned()
        .collect();
    let resolved_orphans: Vec<String> = prev_orphan_names
        .difference(&curr_orphan_names)
        .map(|s| s.to_string())
        .collect();

    // Connection diff — proper set operations using (from_type:from_name → to_type:to_name) as key
    let conn_key = |c: &ApiConnection| -> String {
        format!(
            "{}:{}->{}:{}",
            c.from_type, c.from_name, c.to_type, c.to_name
        )
    };
    let prev_conn_keys: HashSet<String> = previous.connections.iter().map(conn_key).collect();
    let curr_conn_keys: HashSet<String> = current.connections.iter().map(conn_key).collect();
    let new_connections: Vec<ApiConnection> = current
        .connections
        .iter()
        .filter(|c| !prev_conn_keys.contains(&conn_key(c)))
        .cloned()
        .collect();
    let broken_connections: Vec<ApiConnection> = previous
        .connections
        .iter()
        .filter(|c| !curr_conn_keys.contains(&conn_key(c)))
        .cloned()
        .collect();

    // Build summary
    let mut parts = Vec::new();
    if !added_commands.is_empty() {
        parts.push(format!("+{} commands", added_commands.len()));
    }
    if !removed_commands.is_empty() {
        parts.push(format!("-{} commands", removed_commands.len()));
    }
    if !added_routes.is_empty() {
        parts.push(format!("+{} routes", added_routes.len()));
    }
    if !removed_routes.is_empty() {
        parts.push(format!("-{} routes", removed_routes.len()));
    }
    if !added_pg_methods.is_empty() {
        parts.push(format!("+{} PgDb methods", added_pg_methods.len()));
    }
    if !removed_pg_methods.is_empty() {
        parts.push(format!("-{} PgDb methods", removed_pg_methods.len()));
    }
    if !new_orphans.is_empty() {
        parts.push(format!("+{} orphans", new_orphans.len()));
    }
    if !resolved_orphans.is_empty() {
        parts.push(format!("-{} orphans resolved", resolved_orphans.len()));
    }
    if !new_connections.is_empty() {
        parts.push(format!("+{} connections", new_connections.len()));
    }
    if !broken_connections.is_empty() {
        parts.push(format!("-{} connections broken", broken_connections.len()));
    }

    let summary = if parts.is_empty() {
        "No changes since last scan".to_string()
    } else {
        format!("Since last scan: {}", parts.join(", "))
    };

    ApiSurfaceDiff {
        added_commands,
        removed_commands,
        added_routes,
        removed_routes,
        added_pg_methods,
        removed_pg_methods,
        new_orphans,
        resolved_orphans,
        new_connections,
        broken_connections,
        summary,
    }
}
