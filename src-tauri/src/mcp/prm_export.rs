//! PRM Training Data Export API
//!
//! HTTP endpoints for exporting step-level training data for Process Reward
//! Model training. The exported JSONL can be consumed by the qontinui-prm
//! Python service's data pipeline.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::mcp::types::ApiState;
use crate::workflow_generation::prm_export;

// ============================================================================
// Request / Response types
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    /// Minimum number of completed runs required (default: 10)
    #[serde(default = "default_min_runs")]
    pub min_runs: usize,
    /// Output format: "jsonl" (default) or "json"
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_min_runs() -> usize {
    10
}

fn default_format() -> String {
    "jsonl".to_string()
}

#[derive(Debug, Serialize)]
pub struct ExportResponse {
    pub stats: prm_export::PrmExportStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonl: Option<String>,
}

// ============================================================================
// Handlers
// ============================================================================

async fn export_handler(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<ExportQuery>,
) -> Result<Json<ExportResponse>, (StatusCode, Json<serde_json::Value>)> {
    info!(
        "PRM export requested (min_runs={}, format={})",
        query.min_runs, query.format
    );

    let min_runs = query.min_runs;
    let format = query.format.clone();

    let (examples, stats_val) = state
        .app_state
        .pg_db
        .export_prm_training_data(min_runs as i64)
        .await
        .map_err(|e| {
            error!("PRM export failed: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
        })?;

    let stats: prm_export::PrmExportStats = serde_json::from_value(stats_val).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Stats deserialization: {e}")})),
        )
    })?;

    let response = if format == "json" {
        let data = serde_json::to_value(&examples).map_err(|e| {
            error!("Failed to serialize PRM examples: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Serialization failed: {e}")})),
            )
        })?;
        ExportResponse {
            stats,
            data: Some(data),
            jsonl: None,
        }
    } else {
        // Convert JSON values to JSONL format
        let jsonl: String = examples
            .iter()
            .filter_map(|v| serde_json::to_string(v).ok())
            .collect::<Vec<_>>()
            .join("\n");
        ExportResponse {
            stats,
            data: None,
            jsonl: Some(jsonl),
        }
    };

    Ok(Json(response))
}

async fn export_stats_handler(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<prm_export::PrmExportStats>, (StatusCode, Json<serde_json::Value>)> {
    let stats_val = state
        .app_state
        .pg_db
        .export_prm_stats()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
        })?;

    let stats: prm_export::PrmExportStats = serde_json::from_value(stats_val).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Deserialization: {e}")})),
        )
    })?;

    Ok(Json(stats))
}

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/prm/export", get(export_handler))
        .route("/prm/stats", get(export_stats_handler))
}
