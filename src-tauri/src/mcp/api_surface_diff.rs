//! API Surface Diff — compare two API surface scans to detect changes:
//! added/removed endpoints, new/resolved orphans, new/broken connections.

use axum::{extract::State, routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::info;

use super::api_surface::ApiSurface;
use crate::mcp::types::{ApiResponse, ApiState};

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSurfaceDiff {
    pub added_commands: Vec<String>,
    pub removed_commands: Vec<String>,
    pub added_routes: Vec<String>,
    pub removed_routes: Vec<String>,
    pub added_pg_methods: Vec<String>,
    pub removed_pg_methods: Vec<String>,
    pub new_orphans: Vec<String>,
    pub resolved_orphans: Vec<String>,
    pub new_connections: usize,
    pub broken_connections: usize,
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
    // Load the most recent snapshot from PG
    let pg = &state.app_state.pg_db;
    let previous = load_latest_snapshot(pg).await;

    let diff = match previous {
        Some(prev) => compute_diff(&prev, &input.current),
        None => ApiSurfaceDiff {
            added_commands: input.current.tauri_commands.iter().map(|c| c.name.clone()).collect(),
            removed_commands: Vec::new(),
            added_routes: input.current.mcp_routes.iter().map(|r| format!("{} {}", r.method, r.path)).collect(),
            removed_routes: Vec::new(),
            added_pg_methods: input.current.pg_methods.iter().map(|m| m.name.clone()).collect(),
            removed_pg_methods: Vec::new(),
            new_orphans: input.current.orphans.iter().map(|o| o.name.clone()).collect(),
            resolved_orphans: Vec::new(),
            new_connections: input.current.connections.len(),
            broken_connections: 0,
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
) -> Result<Json<ApiResponse<ApiSurfaceSnapshot>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| {
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

    let scan_json = serde_json::to_string(&surface).unwrap_or_default();
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

    info!("Saved API surface snapshot #{} — {} endpoints, {} orphans", id, total_endpoints, orphan_count);

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
) -> Result<Json<ApiResponse<Vec<ApiSurfaceSnapshot>>>, (axum::http::StatusCode, Json<ApiResponse<()>>)> {
    let pg = &state.app_state.pg_db;
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| {
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

fn compute_diff(previous: &ApiSurface, current: &ApiSurface) -> ApiSurfaceDiff {
    let prev_cmds: HashSet<&str> = previous.tauri_commands.iter().map(|c| c.name.as_str()).collect();
    let curr_cmds: HashSet<&str> = current.tauri_commands.iter().map(|c| c.name.as_str()).collect();

    let prev_routes: HashSet<String> = previous.mcp_routes.iter().map(|r| format!("{} {}", r.method, r.path)).collect();
    let curr_routes: HashSet<String> = current.mcp_routes.iter().map(|r| format!("{} {}", r.method, r.path)).collect();

    let prev_pg: HashSet<&str> = previous.pg_methods.iter().map(|m| m.name.as_str()).collect();
    let curr_pg: HashSet<&str> = current.pg_methods.iter().map(|m| m.name.as_str()).collect();

    let prev_orphans: HashSet<&str> = previous.orphans.iter().map(|o| o.name.as_str()).collect();
    let curr_orphans: HashSet<&str> = current.orphans.iter().map(|o| o.name.as_str()).collect();

    let added_commands: Vec<String> = curr_cmds.difference(&prev_cmds).map(|s| s.to_string()).collect();
    let removed_commands: Vec<String> = prev_cmds.difference(&curr_cmds).map(|s| s.to_string()).collect();
    let added_routes: Vec<String> = curr_routes.difference(&prev_routes).cloned().collect();
    let removed_routes: Vec<String> = prev_routes.difference(&curr_routes).cloned().collect();
    let added_pg: Vec<String> = curr_pg.difference(&prev_pg).map(|s| s.to_string()).collect();
    let removed_pg: Vec<String> = prev_pg.difference(&curr_pg).map(|s| s.to_string()).collect();
    let new_orphans: Vec<String> = curr_orphans.difference(&prev_orphans).map(|s| s.to_string()).collect();
    let resolved_orphans: Vec<String> = prev_orphans.difference(&curr_orphans).map(|s| s.to_string()).collect();

    let conn_diff = current.connections.len() as i64 - previous.connections.len() as i64;

    let mut parts = Vec::new();
    if !added_commands.is_empty() { parts.push(format!("+{} commands", added_commands.len())); }
    if !removed_commands.is_empty() { parts.push(format!("-{} commands", removed_commands.len())); }
    if !added_routes.is_empty() { parts.push(format!("+{} routes", added_routes.len())); }
    if !removed_routes.is_empty() { parts.push(format!("-{} routes", removed_routes.len())); }
    if !new_orphans.is_empty() { parts.push(format!("+{} orphans", new_orphans.len())); }
    if !resolved_orphans.is_empty() { parts.push(format!("-{} orphans resolved", resolved_orphans.len())); }
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
        added_pg_methods: added_pg,
        removed_pg_methods: removed_pg,
        new_orphans,
        resolved_orphans,
        new_connections: if conn_diff > 0 { conn_diff as usize } else { 0 },
        broken_connections: if conn_diff < 0 { (-conn_diff) as usize } else { 0 },
        summary,
    }
}
