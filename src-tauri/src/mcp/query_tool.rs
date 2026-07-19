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
use crate::database::hybrid_search::HybridSearchConfig;
use crate::mcp::types::{ApiResponse, ApiState};

/// Create routes for the query tool.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/query", post(query_runner_data))
}

/// Query type selector.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryType {
    /// SQL-only: structured filters, aggregations.
    Sql,
    /// Vector-only: semantic similarity search.
    Vector,
    /// Hybrid: SQL filter → vector re-rank.
    Hybrid,
    /// Web: external search via knowledge acquisition.
    Web,
}

/// Request payload for the query_runner_data tool.
#[derive(Debug, Clone, Deserialize)]
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

    let query_type = request.query_type;
    let result = match query_type {
        QueryType::Sql => execute_sql_query_pg(&state, &request).await,
        QueryType::Vector => execute_vector_query(&state, &request).await,
        QueryType::Hybrid => execute_hybrid_query(&state, &request).await,
        QueryType::Web => execute_web_query(&request).await,
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

/// Valid table names for dynamic queries (prevents SQL injection via table names).
fn is_valid_table_pg(table: &str) -> bool {
    matches!(
        table,
        "task_runs"
            | "task_run_findings"
            | "task_run_automation"
            | "task_knowledge"
            | "task_knowledge_summaries"
            | "unified_workflows"
            | "learning_outcomes"
            | "learning_patterns"
            | "error_events"
            | "workflow_generation_feedback"
            | "task_run_events"
            | "task_run_screenshots"
            | "task_run_playwright_results"
            | "configs"
            | "sessions"
            | "session_events"
            | "verification_tests"
            | "test_results"
            | "checks"
            | "check_results"
            | "log_sources"
            | "recordings"
    )
}

/// Validate a column name (alphanumeric + underscores only).
fn is_valid_column(col: &str) -> bool {
    !col.is_empty() && col.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `(table, column)` pairs this generic query tool needs to bind as
/// `uuid::Uuid` rather than `String` — the two columns migration
/// `d7a3f1c8e024_realign_unified_workflows_to_model` changed from `text` to
/// `uuid`. Every other column of every other allowed table stays `text`, so
/// this stays a narrow allowlist rather than a blanket "try uuid first"
/// heuristic (which would misbind a text column that happens to hold a
/// uuid-shaped string).
fn is_uuid_column_pg(table: &str, column: &str) -> bool {
    matches!(
        (table, column),
        ("unified_workflows", "id")
            | ("unified_workflows", "created_by_user_id")
            | ("unified_workflows", "project_id")
            | ("workflow_generation_feedback", "workflow_id")
    )
}

/// `unified_workflows` columns the same migration converted from `text` to
/// `jsonb` — a filter on one of these needs to bind a parsed
/// [`serde_json::Value`], not a raw `String`, for the same reason
/// [`is_uuid_column_pg`] exists.
fn is_jsonb_column_pg(table: &str, column: &str) -> bool {
    table == "unified_workflows"
        && matches!(
            column,
            "tags"
                | "setup_steps"
                | "verification_steps"
                | "agentic_steps"
                | "completion_steps"
                | "context_ids"
                | "disabled_context_ids"
                | "log_source_selection"
                | "health_check_urls"
                | "stages"
                | "model_overrides"
                | "constraint_overrides"
                | "definition"
        )
}

/// Execute a SQL-only query via PostgreSQL with parameterized filters.
async fn execute_sql_query_pg(
    state: &Arc<ApiState>,
    request: &QueryRunnerDataRequest,
) -> Result<QueryRunnerDataResponse, String> {
    if !is_valid_table_pg(&request.table) {
        return Err(format!("Invalid table name: {}", request.table));
    }

    // Build SELECT clause
    let select_clause = if request.select_columns.is_empty() {
        "*".to_string()
    } else {
        let valid_cols: Vec<&str> = request
            .select_columns
            .iter()
            .filter(|c| is_valid_column(c))
            .map(|c| c.as_str())
            .collect();
        if valid_cols.is_empty() {
            "*".to_string()
        } else {
            valid_cols.join(", ")
        }
    };

    // Build WHERE clause with $N parameters. Most columns bind as text, but
    // some (see is_uuid_column_pg / is_jsonb_column_pg) are `uuid`/`jsonb`
    // post-migration — a plain String bind against those fails at the
    // tokio-postgres type-check layer before the query ever reaches
    // Postgres.
    let zero_match_response = || QueryRunnerDataResponse {
        count: 0,
        results: vec![],
        metadata: QueryMetadata {
            query_type: "sql".to_string(),
            table: request.table.clone(),
            filters_applied: request.filters.len(),
            embedding_used: false,
        },
    };
    let mut where_parts: Vec<String> = Vec::new();
    let mut param_values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();
    for (key, value) in &request.filters {
        if !is_valid_column(key) {
            continue;
        }
        if is_uuid_column_pg(&request.table, key) {
            let Ok(uuid_val) = uuid::Uuid::parse_str(value) else {
                // A value that can't parse as uuid can never match this
                // column — the query must return zero rows, NOT silently
                // drop the filter and run unscoped (that would return
                // arbitrary other rows as if they matched).
                return Ok(zero_match_response());
            };
            let idx = param_values.len() + 1;
            where_parts.push(format!("{} = ${}", key, idx));
            param_values.push(Box::new(uuid_val));
        } else if is_jsonb_column_pg(&request.table, key) {
            let Ok(json_val) = serde_json::from_str::<serde_json::Value>(value) else {
                // Same reasoning as the uuid branch above: an unparseable
                // value can never match a jsonb column.
                return Ok(zero_match_response());
            };
            let idx = param_values.len() + 1;
            where_parts.push(format!("{} = ${}", key, idx));
            param_values.push(Box::new(json_val));
        } else {
            let idx = param_values.len() + 1;
            where_parts.push(format!("{} = ${}", key, idx));
            param_values.push(Box::new(value.clone()));
        }
    }

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };

    // Build GROUP BY
    let group_clause = if request.group_by.is_empty() {
        String::new()
    } else {
        let valid: Vec<&str> = request
            .group_by
            .iter()
            .filter(|c| is_valid_column(c))
            .map(|c| c.as_str())
            .collect();
        if valid.is_empty() {
            String::new()
        } else {
            format!(" GROUP BY {}", valid.join(", "))
        }
    };

    // Build ORDER BY
    let order_clause = if let Some((col, dir)) = &request.order_by {
        if is_valid_column(col) {
            let direction = if dir.to_uppercase() == "DESC" {
                "DESC"
            } else {
                "ASC"
            };
            format!(" ORDER BY {} {}", col, direction)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let limit_clause = format!(" LIMIT {}", request.limit.min(1000));

    let sql = format!(
        "SELECT {} FROM {}{}{}{}{}",
        select_clause, request.table, where_clause, group_clause, order_clause, limit_clause
    );

    let pg = &state.app_state.pg_db;
    let conn = pg
        .pool()
        .get()
        .await
        .map_err(|e| format!("PG pool error: {}", e))?;

    // Build parameter references
    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_values
        .iter()
        .map(|v| v.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let rows: Vec<tokio_postgres::Row> = conn
        .query(&sql, &params)
        .await
        .map_err(|e| format!("PG query error: {}", e))?;

    // Convert rows to JSON
    let results: Vec<serde_json::Value> = rows
        .iter()
        .map(|row: &tokio_postgres::Row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in row.columns().iter().enumerate() {
                let name = col.name().to_string();
                let value = pg_column_to_json(row, i);
                obj.insert(name, value);
            }
            serde_json::Value::Object(obj)
        })
        .collect();

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
}

/// Convert a PostgreSQL row column to a JSON value.
fn pg_column_to_json(row: &tokio_postgres::Row, idx: usize) -> serde_json::Value {
    use tokio_postgres::types::Type;
    let col_type = row.columns()[idx].type_();

    // Try typed extraction based on column type
    match *col_type {
        Type::BOOL => row
            .try_get::<_, bool>(idx)
            .ok()
            .map(serde_json::Value::Bool)
            .unwrap_or(serde_json::Value::Null),
        Type::INT2 => row
            .try_get::<_, i16>(idx)
            .ok()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        Type::INT4 => row
            .try_get::<_, i32>(idx)
            .ok()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        Type::INT8 => row
            .try_get::<_, i64>(idx)
            .ok()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        Type::FLOAT4 => row
            .try_get::<_, f32>(idx)
            .ok()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        Type::FLOAT8 => row
            .try_get::<_, f64>(idx)
            .ok()
            .map(|v| serde_json::json!(v))
            .unwrap_or(serde_json::Value::Null),
        Type::JSONB | Type::JSON => row
            .try_get::<_, serde_json::Value>(idx)
            .ok()
            .unwrap_or(serde_json::Value::Null),
        Type::UUID => row
            .try_get::<_, uuid::Uuid>(idx)
            .ok()
            .map(|v| serde_json::Value::String(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        Type::TIMESTAMPTZ => row
            .try_get::<_, chrono::DateTime<chrono::Utc>>(idx)
            .ok()
            .map(|v| serde_json::Value::String(v.to_rfc3339()))
            .unwrap_or(serde_json::Value::Null),
        Type::TIMESTAMP => row
            .try_get::<_, chrono::NaiveDateTime>(idx)
            .ok()
            .map(|v| serde_json::Value::String(v.to_string()))
            .unwrap_or(serde_json::Value::Null),
        _ => {
            // Fallback: try as String, then try to parse as JSON
            match row.try_get::<_, String>(idx) {
                Ok(text) => {
                    if (text.starts_with('{') || text.starts_with('['))
                        && serde_json::from_str::<serde_json::Value>(&text).is_ok()
                    {
                        serde_json::from_str(&text).unwrap()
                    } else {
                        serde_json::Value::String(text)
                    }
                }
                Err(_) => serde_json::Value::Null,
            }
        }
    }
}

/// Execute a vector-only query via PG.
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

    execute_hybrid_with_embedding_pg(state, request, &query_embedding).await
}

/// Execute a hybrid query (SQL filter + vector re-rank) via PG.
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

    execute_hybrid_with_embedding_pg(state, request, &query_embedding).await
}

/// Execute a hybrid query with a pre-computed embedding via PostgreSQL.
///
/// Uses PG hybrid_search_knowledge for task_knowledge (has embedding support).
/// For other tables, falls back to ILIKE text search since PG embedding
/// hybrid search is not yet implemented for findings/task_runs/error_events/workflows.
async fn execute_hybrid_with_embedding_pg(
    state: &Arc<ApiState>,
    request: &QueryRunnerDataRequest,
    query_embedding: &[f32],
) -> Result<QueryRunnerDataResponse, String> {
    let pg = &state.app_state.pg_db;
    let config = HybridSearchConfig {
        limit: request.limit,
        ..Default::default()
    };

    let results: Vec<serde_json::Value> = match request.table.as_str() {
        "task_knowledge" => {
            let category = request.filters.get("category").map(|s| s.as_str());
            let found = pg
                .hybrid_search_knowledge(query_embedding, category, &config)
                .await?;
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
        "task_run_findings" => {
            // PG text-based search fallback for findings
            let search_text = request.search_text.as_deref().unwrap_or("");
            let task_run_id = request
                .filters
                .get("task_run_id")
                .map(|s| s.as_str())
                .unwrap_or("");
            if task_run_id.is_empty() {
                return Err("task_run_id filter is required for findings search".to_string());
            }
            let findings = pg
                .get_findings_for_task(task_run_id)
                .await
                .unwrap_or_default();
            let search_lower = search_text.to_lowercase();
            findings
                .into_iter()
                .filter(|f| {
                    if search_lower.is_empty() {
                        true
                    } else {
                        f.title.to_lowercase().contains(&search_lower)
                            || f.description.to_lowercase().contains(&search_lower)
                    }
                })
                .take(request.limit)
                .map(|f| {
                    serde_json::json!({
                        "id": f.id,
                        "task_run_id": f.task_run_id,
                        "category": f.category,
                        "severity": f.severity,
                        "title": f.title,
                        "description": f.description,
                        "status": f.status,
                        "file_path": f.code_context.as_ref().and_then(|c| c.file.as_deref()),
                        "detected_at": f.detected_at,
                    })
                })
                .collect()
        }
        "task_runs" => {
            // PG text-based search fallback for task runs
            let search_text = request.search_text.as_deref().unwrap_or("");
            let runs = pg
                .get_recent_task_runs((request.limit * 3) as u32, None)
                .await
                .unwrap_or_default();
            let search_lower = search_text.to_lowercase();
            runs.into_iter()
                .filter(|r| {
                    if search_lower.is_empty() {
                        true
                    } else {
                        r.task_name.to_lowercase().contains(&search_lower)
                            || r.summary
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&search_lower)
                    }
                })
                .take(request.limit)
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "task_name": r.task_name,
                        "status": r.status,
                        "workflow_name": r.workflow_name,
                        "summary": r.summary,
                        "created_at": r.created_at,
                    })
                })
                .collect()
        }
        "error_events" => {
            // PG text-based search for error events
            let severity = request.filters.get("severity").map(|s| s.as_str());
            let severities: Option<Vec<&str>> = severity.map(|s| vec![s]);
            let source = request.filters.get("log_source_name").map(|s| s.as_str());
            pg.query_error_events(
                None,
                None,
                severities.as_deref(),
                source,
                None,
                Some(request.limit as u32),
            )
            .await
            .unwrap_or_default()
        }
        "unified_workflows" => {
            // PG search for workflows (uses built-in search query)
            let search_text = request.search_text.as_deref().unwrap_or("");
            let workflows = if search_text.is_empty() {
                pg.list_unified_workflows().await.unwrap_or_default()
            } else {
                pg.search_unified_workflows(search_text)
                    .await
                    .unwrap_or_default()
            };
            workflows
                .into_iter()
                .take(request.limit)
                .map(|w| {
                    serde_json::json!({
                        "id": w.id,
                        "name": w.name,
                        "description": w.description,
                        "category": w.category,
                        "created_at": w.created_at,
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
                QueryType::Web => "web",
            }
            .to_string(),
            table: request.table.clone(),
            filters_applied: request.filters.len(),
            embedding_used: true,
        },
    })
}

/// Execute a web search via the knowledge acquisition system.
async fn execute_web_query(
    request: &QueryRunnerDataRequest,
) -> Result<QueryRunnerDataResponse, String> {
    let search_text = request
        .search_text
        .as_deref()
        .ok_or("search_text is required for web queries")?;

    let domain = match request.filters.get("domain").map(|s| s.as_str()) {
        Some("vulnerability") => {
            crate::knowledge_acquisition::KnowledgeDomain::VulnerabilityIntelligence
        }
        Some("error") => crate::knowledge_acquisition::KnowledgeDomain::ErrorResolution,
        Some("api") => crate::knowledge_acquisition::KnowledgeDomain::ApiDocumentation,
        _ => crate::knowledge_acquisition::KnowledgeDomain::General,
    };

    let ka = crate::knowledge_acquisition::KnowledgeAcquisition::new();
    let ctx = crate::knowledge_acquisition::SearchContext::system();
    let results = ka
        .search_with_context(search_text, domain, request.limit, ctx.as_ref())
        .await?;

    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "provider": r.provider.as_str(),
                "title": r.title,
                "content": r.content,
                "url": r.url,
                "relevance_score": r.relevance_score,
            })
        })
        .collect();

    Ok(QueryRunnerDataResponse {
        count: json_results.len(),
        results: json_results,
        metadata: QueryMetadata {
            query_type: "web".to_string(),
            table: "web_search".to_string(),
            filters_applied: request.filters.len(),
            embedding_used: false,
        },
    })
}
