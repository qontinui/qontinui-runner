//! `/productivity-knowledge` HTTP endpoints — Phase 4 of the productivity
//! stack. Used by the `/summarize-session` slash command to persist
//! learnings, and by the knowledge-browser modal to search them.
//!
//! Insert path:
//! - `POST /productivity-knowledge` — body carries `task_id?`, `session_id?`,
//!   `area`, `summary`, `body`, and optional `embedding_b64` (base64 of 1536
//!   bytes = 384 × f32 LE). When the field is omitted the endpoint computes
//!   the embedding server-side via the existing pipeline at
//!   `database/embedding_client.rs`. Either path validates the bytes are
//!   exactly 1536 long before insert.
//!
//! Search path:
//! - `GET /productivity-knowledge/search?q=&area=&topK=` — FTS-only in v1.
//!   Vector search is exposed via the Tauri command in
//!   `commands/productivity.rs::search_knowledge` once a frontend wants it;
//!   the HTTP path is intentionally narrow.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::warn;

use crate::database::embeddings::EMBEDDING_BLOB_SIZE;
use crate::database::pg::productivity_knowledge::{
    embed_text, InsertKnowledgeInput, KnowledgeHit, KnowledgeRow,
};
use crate::mcp::types::ApiState;

#[derive(Debug, Clone, Deserialize)]
pub struct InsertKnowledgeRequest {
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub area: String,
    pub summary: String,
    pub body: String,
    /// Optional pre-computed embedding (base64 of 1536 bytes). When
    /// absent the endpoint embeds `body` via `embed_text` server-side.
    #[serde(default)]
    pub embedding_b64: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertKnowledgeResponse {
    pub row: KnowledgeRow,
    /// `true` when the embedding was computed server-side because the
    /// request omitted `embedding_b64`. Useful for /summarize-session
    /// to log whether the agent's pipeline or the runner's pipeline
    /// produced the bytes.
    pub embedded_server_side: bool,
}

async fn insert_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<InsertKnowledgeRequest>,
) -> Result<Json<InsertKnowledgeResponse>, (StatusCode, String)> {
    if req.area.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "area is required".to_string()));
    }
    if req.summary.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "summary is required".to_string()));
    }
    if req.body.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "body is required".to_string()));
    }

    let mut embedded_server_side = false;
    let embedding_bytes: Vec<u8> = match req.embedding_b64.as_deref() {
        Some(b64) if !b64.is_empty() => {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("embedding_b64 is not valid base64: {}", e),
                    )
                })?;
            if decoded.len() != EMBEDDING_BLOB_SIZE {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!(
                        "embedding_b64 must decode to exactly {} bytes, got {}",
                        EMBEDDING_BLOB_SIZE,
                        decoded.len()
                    ),
                ));
            }
            decoded
        }
        _ => {
            embedded_server_side = true;
            embed_text(&req.body).await
        }
    };

    // If server-side embedding failed (returned empty), insert without
    // it rather than failing the row write outright. The row is still
    // discoverable via FTS.
    let embedding_opt: Option<&[u8]> = if embedding_bytes.len() == EMBEDDING_BLOB_SIZE {
        Some(&embedding_bytes)
    } else {
        if embedded_server_side {
            warn!(
                "POST /productivity-knowledge: server-side embed produced {} bytes (expected {}); inserting without embedding",
                embedding_bytes.len(),
                EMBEDDING_BLOB_SIZE
            );
        }
        None
    };

    let row = state
        .app_state
        .pg_db
        .insert_knowledge(&InsertKnowledgeInput {
            task_id: req.task_id.as_deref(),
            session_id: req.session_id.as_deref(),
            area: &req.area,
            summary: &req.summary,
            body: &req.body,
            embedding: embedding_opt,
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(InsertKnowledgeResponse {
        row,
        embedded_server_side,
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchKnowledgeQuery {
    pub q: String,
    #[serde(default)]
    pub area: Option<String>,
    /// Default `15`; clamped 1..=100. Accepts either `topK` (camelCase,
    /// per the plan §6 contract) or `top_k` (snake_case).
    #[serde(default, alias = "topK")]
    pub top_k: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchKnowledgeResponse {
    pub query: String,
    pub area: Option<String>,
    pub top_k: i32,
    pub hits: Vec<KnowledgeHit>,
}

async fn search_knowledge_handler(
    State(state): State<Arc<ApiState>>,
    Query(q): Query<SearchKnowledgeQuery>,
) -> Result<Json<SearchKnowledgeResponse>, (StatusCode, String)> {
    if q.q.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "q is required".to_string()));
    }
    let top_k = q.top_k.unwrap_or(15).clamp(1, 100);

    let hits = state
        .app_state
        .pg_db
        .search_knowledge_fts(&q.q, q.area.as_deref(), top_k)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(SearchKnowledgeResponse {
        query: q.q,
        area: q.area,
        top_k,
        hits,
    }))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/productivity-knowledge", post(insert_knowledge_handler))
        .route(
            "/productivity-knowledge/search",
            get(search_knowledge_handler),
        )
}
