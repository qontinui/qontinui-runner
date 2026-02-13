//! MCP query tool for hybrid retrieval.
//!
//! Exposes the hybrid RAG search system as an HTTP endpoint that can be
//! called by Claude sessions or external tools. Supports SQL-only,
//! vector-only, and hybrid (SQL filter + vector re-rank) queries.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::database::embedding_client::EmbeddingClient;
use crate::database::hybrid_search::{self, HybridSearchConfig};
use crate::database::query_builder::SafeQueryBuilder;
use crate::mcp::types::{ApiResponse, ApiState};

/// Create routes for the query tool.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/query", post(query_runner_data))
}

/// Query type selector.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryType {
    /// SQL-only: structured filters, aggregations.
    Sql,
    /// Vector-only: semantic similarity search.
    Vector,
    /// Hybrid: SQL filter → vector re-rank.
    Hybrid,
}

/// Request payload for the query_runner_data tool.
#[derive(Debug, Deserialize)]
pub struct QueryRunnerDataRequest {
    /// Query type: "sql", "vector", or "hybrid".
    pub query_type: QueryType,
    /// Target table to search.
    pub table: String,
    /// Search text (used for vector and hybrid queries).
    pub search_text: Option<String>,
    /// Structured filters as key-value pairs.
    #[serde(default)]
    pub filters: std::collections::HashMap<String, String>,
    /// Order by (column, direction) pairs.
    #[serde(default)]
    pub order_by: Option<(String, String)>,
    /// Maximum results.
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Columns to select (SQL-only mode).
    #[serde(default)]
    pub select_columns: Vec<String>,
    /// GROUP BY columns (SQL-only mode for aggregations).
    #[serde(default)]
    pub group_by: Vec<String>,
}

fn default_limit() -> usize {
    20
}

/// Response payload for the query tool.
#[derive(Debug, Serialize)]
pub struct QueryRunnerDataResponse {
    /// Number of results returned.
    pub count: usize,
    /// The results as JSON values.
    pub results: Vec<serde_json::Value>,
    /// Query metadata.
    pub metadata: QueryMetadata,
}

/// Metadata about the executed query.
#[derive(Debug, Serialize)]
pub struct QueryMetadata {
    pub query_type: String,
    pub table: String,
    pub filters_applied: usize,
    pub embedding_used: bool,
}

/// Handle the query_runner_data MCP tool request.
pub async fn query_runner_data(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<QueryRunnerDataRequest>,
) -> Result<Json<ApiResponse<QueryRunnerDataResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    info!(
        "Query runner data: type={:?}, table={}, filters={}",
        request.query_type,
        request.table,
        request.filters.len()
    );

    let result = match request.query_type {
        QueryType::Sql => execute_sql_query(&state, &request),
        QueryType::Vector => execute_vector_query(&state, &request).await,
        QueryType::Hybrid => execute_hybrid_query(&state, &request).await,
    };

    match result {
        Ok(response) => Ok(Json(ApiResponse::success(response))),
        Err(e) => {
            error!("Query runner data failed: {}", e);
            Err((
                StatusCode::BAD_REQUEST,
                Json(crate::mcp::types::api_error(format!("Query failed: {}", e))),
            ))
        }
    }
}

/// Execute a SQL-only query using SafeQueryBuilder.
fn execute_sql_query(
    state: &Arc<ApiState>,
    request: &QueryRunnerDataRequest,
) -> Result<QueryRunnerDataResponse, String> {
    state.app_state.checkpoint_db.with_conn(|conn| {
        let mut builder = SafeQueryBuilder::new(&request.table)?;

        // Apply column selection
        if !request.select_columns.is_empty() {
            let col_refs: Vec<&str> = request.select_columns.iter().map(|s| s.as_str()).collect();
            builder = builder.select(&col_refs);
        }

        // Apply filters
        for (key, value) in &request.filters {
            builder = builder.where_eq(key, value.clone());
        }

        // Apply GROUP BY
        for col in &request.group_by {
            builder = builder.group_by(col);
        }

        // Apply ORDER BY
        if let Some((col, dir)) = &request.order_by {
            builder = builder.order_by(col, dir);
        }

        builder = builder.limit(request.limit);

        // Execute and collect as JSON values
        let results = builder.query(conn, |row| {
            // Convert each row to a JSON object
            let column_count = row.as_ref().column_count();
            let mut obj = serde_json::Map::new();
            for i in 0..column_count {
                let col_name = row.as_ref().column_name(i).unwrap_or("?").to_string();
                let value = match row.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Null) => serde_json::Value::Null,
                    Ok(rusqlite::types::ValueRef::Integer(n)) => serde_json::json!(n),
                    Ok(rusqlite::types::ValueRef::Real(f)) => serde_json::json!(f),
                    Ok(rusqlite::types::ValueRef::Text(s)) => {
                        let text = String::from_utf8_lossy(s).to_string();
                        // Try to parse as JSON if it looks like JSON
                        if (text.starts_with('{') || text.starts_with('['))
                            && serde_json::from_str::<serde_json::Value>(&text).is_ok()
                        {
                            serde_json::from_str(&text).unwrap()
                        } else {
                            serde_json::Value::String(text)
                        }
                    }
                    Ok(rusqlite::types::ValueRef::Blob(b)) => {
                        serde_json::json!(format!("<blob:{} bytes>", b.len()))
                    }
                    Err(_) => serde_json::Value::Null,
                };
                obj.insert(col_name, value);
            }
            Ok(serde_json::Value::Object(obj))
        })?;

        Ok(QueryRunnerDataResponse {
            count: results.len(),
            results,
            metadata: QueryMetadata {
                query_type: "sql".to_string(),
                table: request.table.clone(),
                filters_applied: request.filters.len(),
                embedding_used: false,
            },
        })
    })
}

/// Execute a vector-only query.
async fn execute_vector_query(
    state: &Arc<ApiState>,
    request: &QueryRunnerDataRequest,
) -> Result<QueryRunnerDataResponse, String> {
    let search_text = request
        .search_text
        .as_deref()
        .ok_or("search_text is required for vector queries")?;

    // Compute query embedding
    let client = EmbeddingClient::new();
    let query_embedding = client.compute_text_embedding(search_text).await?;

    execute_hybrid_with_embedding(state, request, &query_embedding)
}

/// Execute a hybrid query (SQL filter + vector re-rank).
async fn execute_hybrid_query(
    state: &Arc<ApiState>,
    request: &QueryRunnerDataRequest,
) -> Result<QueryRunnerDataResponse, String> {
    let search_text = request
        .search_text
        .as_deref()
        .ok_or("search_text is required for hybrid queries")?;

    // Compute query embedding
    let client = EmbeddingClient::new();
    let query_embedding = client.compute_text_embedding(search_text).await?;

    execute_hybrid_with_embedding(state, request, &query_embedding)
}

/// Execute a hybrid query with a pre-computed embedding.
fn execute_hybrid_with_embedding(
    state: &Arc<ApiState>,
    request: &QueryRunnerDataRequest,
    query_embedding: &[f32],
) -> Result<QueryRunnerDataResponse, String> {
    let config = HybridSearchConfig {
        limit: request.limit,
        ..Default::default()
    };

    state.app_state.checkpoint_db.with_conn(|conn| {
        let results: Vec<serde_json::Value> = match request.table.as_str() {
            "task_run_findings" => {
                let category = request.filters.get("category").map(|s| s.as_str());
                let status = request.filters.get("status").map(|s| s.as_str());
                let found = hybrid_search::hybrid_search_findings(
                    conn,
                    query_embedding,
                    category,
                    status,
                    &config,
                )?;
                found
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.item.id,
                            "task_run_id": r.item.task_run_id,
                            "category": r.item.category,
                            "severity": r.item.severity,
                            "title": r.item.title,
                            "description": r.item.description,
                            "status": r.item.status,
                            "file_path": r.item.file_path,
                            "detected_at": r.item.detected_at,
                            "similarity": r.similarity,
                            "hybrid_score": r.hybrid_score,
                        })
                    })
                    .collect()
            }
            "task_runs" => {
                let status = request.filters.get("status").map(|s| s.as_str());
                let category = request.filters.get("task_type").map(|s| s.as_str());
                let found = hybrid_search::hybrid_search_task_runs(
                    conn,
                    query_embedding,
                    status,
                    category,
                    &config,
                )?;
                found
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.item.id,
                            "task_name": r.item.task_name,
                            "status": r.item.status,
                            "workflow_name": r.item.workflow_name,
                            "summary": r.item.summary,
                            "created_at": r.item.created_at,
                            "similarity": r.similarity,
                            "hybrid_score": r.hybrid_score,
                        })
                    })
                    .collect()
            }
            "error_events" => {
                let search_text = request
                    .search_text
                    .as_deref()
                    .unwrap_or("");
                let severity = request.filters.get("severity").map(|s| s.as_str());
                let source = request.filters.get("log_source_name").map(|s| s.as_str());
                let found = hybrid_search::hybrid_search_error_events(
                    conn,
                    search_text,
                    query_embedding,
                    severity,
                    source,
                    &config,
                )?;
                found
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.item.id,
                            "log_source_name": r.item.log_source_name,
                            "severity": r.item.severity,
                            "message": r.item.message,
                            "error_type": r.item.error_type,
                            "status": r.item.status,
                            "occurrence_count": r.item.occurrence_count,
                            "last_seen_at": r.item.last_seen_at,
                            "similarity": r.similarity,
                            "hybrid_score": r.hybrid_score,
                        })
                    })
                    .collect()
            }
            "task_knowledge" => {
                let category = request.filters.get("category").map(|s| s.as_str());
                let found = hybrid_search::hybrid_search_knowledge(
                    conn,
                    query_embedding,
                    category,
                    &config,
                )?;
                found
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.item.id,
                            "task_run_id": r.item.task_run_id,
                            "category": r.item.category,
                            "content": r.item.content,
                            "confidence": r.item.confidence,
                            "is_resolved": r.item.is_resolved,
                            "created_at": r.item.created_at,
                            "similarity": r.similarity,
                            "hybrid_score": r.hybrid_score,
                        })
                    })
                    .collect()
            }
            "unified_workflows" => {
                let category = request.filters.get("category").map(|s| s.as_str());
                let found = hybrid_search::hybrid_search_workflows(
                    conn,
                    query_embedding,
                    category,
                    &config,
                )?;
                found
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.item.id,
                            "name": r.item.name,
                            "description": r.item.description,
                            "category": r.item.category,
                            "setup_step_count": r.item.setup_step_count,
                            "verification_step_count": r.item.verification_step_count,
                            "agentic_step_count": r.item.agentic_step_count,
                            "created_at": r.item.created_at,
                            "similarity": r.similarity,
                            "hybrid_score": r.hybrid_score,
                        })
                    })
                    .collect()
            }
            table => {
                return Err(format!(
                    "Hybrid/vector search not supported for table '{}'. \
                     Supported: task_run_findings, task_runs, error_events, task_knowledge, unified_workflows",
                    table
                ));
            }
        };

        Ok(QueryRunnerDataResponse {
            count: results.len(),
            results,
            metadata: QueryMetadata {
                query_type: match request.query_type {
                    QueryType::Vector => "vector",
                    QueryType::Hybrid => "hybrid",
                    QueryType::Sql => "sql",
                }
                .to_string(),
                table: request.table.clone(),
                filters_applied: request.filters.len(),
                embedding_used: true,
            },
        })
    })
}
