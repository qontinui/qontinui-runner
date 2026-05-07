//! PostgreSQL CRUD for the `productivity_knowledge` table (migration v30).
//!
//! See §2 of `qontinui-dev-notes/plans/productivity-stack.md`. Each row is
//! one learning emitted by `/summarize-session` (Phase 4) or written
//! directly by Phase 5 reflection. Search proceeds via two paths:
//!
//! - **FTS** — fast keyword search over `area || ' ' || summary || ' ' || body`
//!   using the GIN index `idx_pk_fts`. Default surface for the
//!   knowledge-browser modal.
//! - **Vector** — cosine similarity on f32 little-endian embeddings stored as
//!   BYTEA. pgvector is not installed in this codebase; embeddings live in
//!   BYTEA per the existing convention (e.g. `task_runs.prompt_embedding`).
//!   Decoded into `Vec<f32>` in Rust and ranked client-side.
//!
//! UUID columns are cast to/from TEXT in queries because the runner does not
//! enable tokio-postgres `with-uuid-1`. UUIDs flow as canonical strings.

use super::PgDb;
use crate::database::embedding_client::EmbeddingClient;
use crate::database::embeddings::{
    blob_to_vector, cosine_similarity, vector_to_blob, EMBEDDING_BLOB_SIZE,
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

/// A row from the `productivity_knowledge` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeRow {
    pub id: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub area: String,
    pub summary: String,
    pub body: String,
    pub created_at: String,
    /// Whether the row has an embedding stored.
    pub has_embedding: bool,
}

/// A search hit — the knowledge row plus a relevance score (FTS rank, or
/// cosine similarity). Higher is more relevant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeHit {
    pub row: KnowledgeRow,
    pub score: f32,
}

/// Input shape for `insert_knowledge`. All ID fields are stringified UUIDs.
#[derive(Debug, Clone)]
pub struct InsertKnowledgeInput<'a> {
    pub task_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub area: &'a str,
    pub summary: &'a str,
    pub body: &'a str,
    /// Pre-computed embedding bytes (1536 = 384 × f32 LE). If `None` the
    /// row is inserted without an embedding and the vector path skips it.
    pub embedding: Option<&'a [u8]>,
}

fn row_from_pg(r: &tokio_postgres::Row) -> KnowledgeRow {
    let embedding: Option<Vec<u8>> = r.get(6);
    KnowledgeRow {
        id: r.get(0),
        task_id: r.get(1),
        session_id: r.get(2),
        area: r.get(3),
        summary: r.get(4),
        body: r.get(5),
        has_embedding: embedding.is_some(),
        created_at: r.get(7),
    }
}

const SELECT_COLUMNS: &str = r#"
    id::text, task_id::text, session_id, area, summary, body, embedding,
    created_at::text
"#;

impl PgDb {
    /// Insert a new knowledge row. Returns the inserted row.
    pub async fn insert_knowledge(
        &self,
        input: &InsertKnowledgeInput<'_>,
    ) -> Result<KnowledgeRow, String> {
        if let Some(emb) = input.embedding {
            if emb.len() != EMBEDDING_BLOB_SIZE {
                return Err(format!(
                    "embedding must be exactly {} bytes (f32 LE × 384), got {}",
                    EMBEDDING_BLOB_SIZE,
                    emb.len()
                ));
            }
        }

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // task_id is a UUID; we accept it as TEXT and cast in SQL so
        // tokio-postgres does not need with-uuid-1.
        let row = conn
            .query_one(
                r#"
                INSERT INTO productivity_knowledge
                    (task_id, session_id, area, summary, body, embedding)
                VALUES (
                    CASE WHEN $1::text IS NULL THEN NULL ELSE $1::uuid END,
                    $2, $3, $4, $5, $6
                )
                RETURNING id::text, task_id::text, session_id, area, summary, body,
                          embedding, created_at::text
                "#,
                &[
                    &input.task_id,
                    &input.session_id,
                    &input.area,
                    &input.summary,
                    &input.body,
                    &input.embedding,
                ],
            )
            .await
            .map_err(|e| format!("Failed to insert knowledge: {}", e))?;

        Ok(row_from_pg(&row))
    }

    /// Look up a knowledge row by its UUID.
    pub async fn get_knowledge_by_id(&self, id: &str) -> Result<Option<KnowledgeRow>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let id_uuid =
            Uuid::parse_str(id).map_err(|e| format!("invalid knowledge id uuid: {}", e))?;
        let row = conn
            .query_opt(
                &format!(
                    "SELECT {} FROM productivity_knowledge WHERE id = $1::uuid",
                    SELECT_COLUMNS
                ),
                &[&id_uuid],
            )
            .await
            .map_err(|e| format!("Failed to get knowledge: {}", e))?;

        Ok(row.as_ref().map(row_from_pg))
    }

    /// FTS-based search. `area` filter is exact-match when provided.
    /// Returns rows ordered by `ts_rank` descending, capped at `top_k`.
    pub async fn search_knowledge_fts(
        &self,
        query: &str,
        area: Option<&str>,
        top_k: i32,
    ) -> Result<Vec<KnowledgeHit>, String> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let limit = top_k.clamp(1, 200) as i64;

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // plainto_tsquery is the safest user-input-friendly path; it does
        // not require the caller to escape quoted phrases.
        let rows = if let Some(a) = area {
            conn.query(
                r#"
                SELECT id::text, task_id::text, session_id, area, summary, body,
                       embedding, created_at::text,
                       ts_rank(
                           to_tsvector('english', area || ' ' || summary || ' ' || body),
                           plainto_tsquery('english', $1)
                       ) AS rank
                FROM productivity_knowledge
                WHERE area = $2
                  AND to_tsvector('english', area || ' ' || summary || ' ' || body)
                      @@ plainto_tsquery('english', $1)
                ORDER BY rank DESC, created_at DESC
                LIMIT $3
                "#,
                &[&query, &a, &limit],
            )
            .await
        } else {
            conn.query(
                r#"
                SELECT id::text, task_id::text, session_id, area, summary, body,
                       embedding, created_at::text,
                       ts_rank(
                           to_tsvector('english', area || ' ' || summary || ' ' || body),
                           plainto_tsquery('english', $1)
                       ) AS rank
                FROM productivity_knowledge
                WHERE to_tsvector('english', area || ' ' || summary || ' ' || body)
                      @@ plainto_tsquery('english', $1)
                ORDER BY rank DESC, created_at DESC
                LIMIT $2
                "#,
                &[&query, &limit],
            )
            .await
        }
        .map_err(|e| format!("Failed to search knowledge (FTS): {}", e))?;

        Ok(rows
            .iter()
            .map(|r| {
                let row = row_from_pg(r);
                let rank: f32 = r.get(8);
                KnowledgeHit { row, score: rank }
            })
            .collect())
    }

    /// Vector cosine search. Decodes each candidate row's BYTEA embedding,
    /// computes cosine similarity in Rust, and returns the top-K. The
    /// candidate set is bounded by `area` (when provided) and a recency
    /// cap so we don't pull every row into memory on a large table.
    pub async fn search_knowledge_vector(
        &self,
        query_embedding: &[f32],
        area: Option<&str>,
        top_k: i32,
    ) -> Result<Vec<KnowledgeHit>, String> {
        if query_embedding.is_empty() {
            return Ok(Vec::new());
        }
        let limit = top_k.clamp(1, 200) as usize;

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        // Hard candidate cap so the in-memory cosine pass stays cheap.
        // 2K rows × 1536 bytes = 3 MB, comfortably under any tight budget.
        const CANDIDATE_CAP: i64 = 2000;

        let rows = if let Some(a) = area {
            conn.query(
                &format!(
                    r#"
                    SELECT {}
                    FROM productivity_knowledge
                    WHERE embedding IS NOT NULL AND area = $1
                    ORDER BY created_at DESC
                    LIMIT $2
                    "#,
                    SELECT_COLUMNS
                ),
                &[&a, &CANDIDATE_CAP],
            )
            .await
        } else {
            conn.query(
                &format!(
                    r#"
                    SELECT {}
                    FROM productivity_knowledge
                    WHERE embedding IS NOT NULL
                    ORDER BY created_at DESC
                    LIMIT $1
                    "#,
                    SELECT_COLUMNS
                ),
                &[&CANDIDATE_CAP],
            )
            .await
        }
        .map_err(|e| format!("Failed to fetch knowledge candidates: {}", e))?;

        let mut hits: Vec<KnowledgeHit> = rows
            .iter()
            .filter_map(|r| {
                let blob: Option<Vec<u8>> = r.get(6);
                let blob = blob?;
                let vec_b = blob_to_vector(&blob)?;
                if vec_b.len() != query_embedding.len() {
                    warn!(
                        "search_knowledge_vector: dimension mismatch (query={}, row={}); skipping",
                        query_embedding.len(),
                        vec_b.len()
                    );
                    return None;
                }
                let score = cosine_similarity(query_embedding, &vec_b);
                Some(KnowledgeHit {
                    row: row_from_pg(r),
                    score,
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// List the most-recent knowledge rows. Used by the dashboard's
    /// "recent learnings" feed.
    pub async fn list_recent_knowledge(&self, limit: i32) -> Result<Vec<KnowledgeRow>, String> {
        let limit = limit.clamp(1, 500) as i64;

        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    "SELECT {} FROM productivity_knowledge ORDER BY created_at DESC LIMIT $1",
                    SELECT_COLUMNS
                ),
                &[&limit],
            )
            .await
            .map_err(|e| format!("Failed to list recent knowledge: {}", e))?;

        Ok(rows.iter().map(row_from_pg).collect())
    }

    /// List every knowledge row attributed to a session.
    pub async fn list_knowledge_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<KnowledgeRow>, String> {
        let conn = self
            .pool()
            .get()
            .await
            .map_err(|e| format!("PG pool error: {}", e))?;

        let rows = conn
            .query(
                &format!(
                    r#"
                    SELECT {}
                    FROM productivity_knowledge
                    WHERE session_id = $1
                    ORDER BY created_at DESC
                    "#,
                    SELECT_COLUMNS
                ),
                &[&session_id],
            )
            .await
            .map_err(|e| format!("Failed to list knowledge for session: {}", e))?;

        Ok(rows.iter().map(row_from_pg).collect())
    }
}

/// Compute a 384-dim MiniLM embedding for `text` and return the canonical
/// 1536-byte little-endian f32 representation. Uses the existing
/// `EmbeddingClient` (HTTP to the local embedding service); falls back to
/// an empty Vec on transport/storage error so the caller can choose to
/// insert without an embedding.
///
/// This is the helper the `POST /productivity-knowledge` endpoint uses
/// when the agent does not pass `embedding_b64`.
pub async fn embed_text(text: &str) -> Vec<u8> {
    let client = EmbeddingClient::new();
    match client.compute_text_embedding(text).await {
        Ok(v) => vector_to_blob(&v),
        Err(e) => {
            warn!(
                "embed_text: compute_text_embedding failed: {} (returning empty bytes)",
                e
            );
            Vec::new()
        }
    }
}
