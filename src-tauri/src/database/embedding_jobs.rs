//! Background job for backfilling embeddings on existing rows.
//!
//! Runs periodically to compute embeddings for rows where the embedding
//! column is NULL but the source text exists. Rate-limited to avoid
//! overloading the qontinui-api embedding service.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::interval;
use tracing::{debug, info, warn};

use super::embedding_client::EmbeddingClient;
use super::embeddings::{
    self, store_error_event_embedding, store_finding_embedding, store_knowledge_embedding,
    store_learning_outcome_embedding, store_learning_pattern_embedding,
    store_reflection_fix_embedding, store_task_run_embedding, store_workflow_embedding,
    FindingEmbeddingColumn, TaskRunEmbeddingColumn,
};
use crate::database::CheckpointDb;

/// Configuration for the embedding backfill job.
pub struct EmbeddingJobConfig {
    /// How often to run the backfill cycle (default: 5 minutes).
    pub interval: Duration,
    /// Maximum rows to process per table per cycle (default: 100).
    pub batch_size: usize,
    /// Delay between individual API calls to avoid overload (default: 100ms).
    pub rate_limit_delay: Duration,
}

impl Default for EmbeddingJobConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(300),
            batch_size: 100,
            rate_limit_delay: Duration::from_millis(100),
        }
    }
}

/// Embedding table/column specification for backfill processing.
struct EmbeddingTarget {
    table: &'static str,
    id_column: &'static str,
    text_column: &'static str,
    embedding_column: &'static str,
}

const EMBEDDING_TARGETS: &[EmbeddingTarget] = &[
    EmbeddingTarget {
        table: "task_runs",
        id_column: "id",
        text_column: "prompt",
        embedding_column: "prompt_embedding",
    },
    EmbeddingTarget {
        table: "task_runs",
        id_column: "id",
        text_column: "summary",
        embedding_column: "summary_embedding",
    },
    EmbeddingTarget {
        table: "task_run_findings",
        id_column: "id",
        text_column: "title",
        embedding_column: "title_embedding",
    },
    EmbeddingTarget {
        table: "task_run_findings",
        id_column: "id",
        text_column: "description",
        embedding_column: "description_embedding",
    },
    EmbeddingTarget {
        table: "task_knowledge",
        id_column: "id",
        text_column: "content",
        embedding_column: "content_embedding",
    },
    EmbeddingTarget {
        table: "unified_workflows",
        id_column: "id",
        text_column: "description",
        embedding_column: "description_embedding",
    },
    EmbeddingTarget {
        table: "learning_outcomes",
        id_column: "id",
        text_column: "strategy",
        embedding_column: "context_embedding",
    },
    EmbeddingTarget {
        table: "learning_patterns",
        id_column: "id",
        text_column: "description",
        embedding_column: "description_embedding",
    },
    EmbeddingTarget {
        table: "error_events",
        id_column: "id",
        text_column: "message",
        embedding_column: "message_embedding",
    },
    EmbeddingTarget {
        table: "reflection_fixes",
        id_column: "id",
        text_column: "fix_description",
        embedding_column: "fix_description_embedding",
    },
];

/// Start the background embedding backfill job.
///
/// This spawns a task that periodically scans for rows missing
/// embeddings and computes them via the qontinui-api.
///
/// Uses `tauri::async_runtime::spawn()` to properly integrate with
/// Tauri's async runtime, allowing this to be called from setup().
pub fn start_embedding_job(
    db: Arc<CheckpointDb>,
    config: EmbeddingJobConfig,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let client = EmbeddingClient::new();
        let mut tick = interval(config.interval);

        // Initial delay to let the app start up
        tokio::time::sleep(Duration::from_secs(30)).await;
        info!(
            "Embedding backfill job started (interval: {:?})",
            config.interval
        );

        loop {
            tick.tick().await;

            // Check if the embedding API is available before processing
            if !client.is_available().await {
                debug!("Embedding API not available, skipping backfill cycle");
                continue;
            }

            let total_processed = run_backfill_cycle(&db, &client, &config).await;

            if total_processed > 0 {
                info!(
                    "Embedding backfill cycle complete: {} embeddings computed",
                    total_processed
                );
            }
        }
    })
}

/// Run one complete backfill cycle across all embedding targets.
async fn run_backfill_cycle(
    db: &Arc<CheckpointDb>,
    client: &EmbeddingClient,
    config: &EmbeddingJobConfig,
) -> usize {
    let mut total = 0;

    for target in EMBEDDING_TARGETS {
        let count = process_target(db, client, target, config).await;
        total += count;
    }

    total
}

/// Process a single embedding target (table + column pair).
async fn process_target(
    db: &Arc<CheckpointDb>,
    client: &EmbeddingClient,
    target: &EmbeddingTarget,
    config: &EmbeddingJobConfig,
) -> usize {
    // Get rows needing embeddings
    let rows = match db.with_conn(|conn| {
        embeddings::get_rows_needing_embeddings(
            conn,
            target.table,
            target.id_column,
            target.text_column,
            target.embedding_column,
            config.batch_size,
        )
    }) {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                "Failed to query rows needing embeddings for {}.{}: {}",
                target.table, target.embedding_column, e
            );
            return 0;
        }
    };

    if rows.is_empty() {
        return 0;
    }

    debug!(
        "Processing {} rows for {}.{}",
        rows.len(),
        target.table,
        target.embedding_column
    );

    let mut processed = 0;

    for (id, text) in &rows {
        // Compute embedding
        let embedding = match client.compute_text_embedding(text).await {
            Ok(emb) => emb,
            Err(e) => {
                warn!(
                    "Failed to compute embedding for {}.{} id={}: {}",
                    target.table, target.embedding_column, id, e
                );
                continue;
            }
        };

        // Store embedding
        let store_result =
            db.with_conn(|conn| store_embedding_by_target(conn, target, id, &embedding));

        match store_result {
            Ok(()) => processed += 1,
            Err(e) => {
                warn!(
                    "Failed to store embedding for {}.{} id={}: {}",
                    target.table, target.embedding_column, id, e
                );
            }
        }

        // Rate limit
        tokio::time::sleep(config.rate_limit_delay).await;
    }

    processed
}

/// Store an embedding using the appropriate per-table helper.
fn store_embedding_by_target(
    conn: &rusqlite::Connection,
    target: &EmbeddingTarget,
    id: &str,
    embedding: &[f32],
) -> Result<(), String> {
    match (target.table, target.embedding_column) {
        ("task_runs", "prompt_embedding") => {
            store_task_run_embedding(conn, id, TaskRunEmbeddingColumn::Prompt, embedding)
        }
        ("task_runs", "summary_embedding") => {
            store_task_run_embedding(conn, id, TaskRunEmbeddingColumn::Summary, embedding)
        }
        ("task_run_findings", "title_embedding") => {
            store_finding_embedding(conn, id, FindingEmbeddingColumn::Title, embedding)
        }
        ("task_run_findings", "description_embedding") => {
            store_finding_embedding(conn, id, FindingEmbeddingColumn::Description, embedding)
        }
        ("task_knowledge", "content_embedding") => store_knowledge_embedding(conn, id, embedding),
        ("unified_workflows", "description_embedding") => {
            store_workflow_embedding(conn, id, embedding)
        }
        ("learning_outcomes", "context_embedding") => {
            store_learning_outcome_embedding(conn, id, embedding)
        }
        ("learning_patterns", "description_embedding") => {
            store_learning_pattern_embedding(conn, id, embedding)
        }
        ("error_events", "message_embedding") => {
            // error_events uses integer IDs
            let int_id: i64 = id
                .parse()
                .map_err(|e| format!("Invalid error_event id: {}", e))?;
            store_error_event_embedding(conn, int_id, embedding)
        }
        ("reflection_fixes", "fix_description_embedding") => {
            store_reflection_fix_embedding(conn, id, embedding)
        }
        _ => Err(format!(
            "Unknown embedding target: {}.{}",
            target.table, target.embedding_column
        )),
    }
}
