use crate::database::embedding_client::EmbeddingClient;
use crate::database::embeddings::store_knowledge_embedding;
use crate::database::hybrid_search::{hybrid_search_knowledge, HybridSearchConfig};
use crate::database::CheckpointDb;
use crate::knowledge_acquisition::summarizer;
use crate::knowledge_acquisition::types::KnowledgeResult;

const DEDUP_SIMILARITY_THRESHOLD: f32 = 0.85;
const EXTERNAL_KNOWLEDGE_CATEGORY: &str = "external_knowledge";
const SYSTEM_AGENT_TYPE: &str = "system";

/// Result of ingesting a knowledge result
#[derive(Debug)]
pub enum IngestOutcome {
    /// Stored as new knowledge entry
    Stored { id: String },
    /// Skipped due to duplicate detection
    Duplicate { existing_id: String },
    /// Skipped — embedding service unavailable
    EmbeddingUnavailable,
    /// Failed to ingest
    Failed { error: String },
}

/// Ingest search results into task_knowledge with deduplication and embedding.
///
/// Pipeline:
/// 1. Summarize if content > 3000 chars
/// 2. Compute embedding
/// 3. Check for duplicates via hybrid_search (cosine > 0.85 = skip)
/// 4. Insert into task_knowledge
/// 5. Store embedding blob
pub async fn ingest_results(
    results: &[KnowledgeResult],
    task_run_id: &str,
    db: &CheckpointDb,
) -> Vec<IngestOutcome> {
    let embedding_client = EmbeddingClient::new();

    // Check if embedding service is available
    if !embedding_client.is_available().await {
        eprintln!("[ingestor] Embedding service unavailable, skipping ingestion");
        return results
            .iter()
            .map(|_| IngestOutcome::EmbeddingUnavailable)
            .collect();
    }

    let mut outcomes = Vec::with_capacity(results.len());

    for result in results {
        let outcome = ingest_single(result, task_run_id, db, &embedding_client).await;
        outcomes.push(outcome);
    }

    outcomes
}

/// Ingest a single result
async fn ingest_single(
    result: &KnowledgeResult,
    task_run_id: &str,
    db: &CheckpointDb,
    embedding_client: &EmbeddingClient,
) -> IngestOutcome {
    // Step 1: Summarize if needed
    let content = if summarizer::needs_summarization(&result.content) {
        match summarizer::summarize_for_storage(&result.content, &result.query, None).await {
            Ok(summary) => summary,
            Err(_) => {
                // Fallback: use first 2000 chars
                result.content.chars().take(2000).collect::<String>()
            }
        }
    } else {
        result.content.clone()
    };

    // Step 2: Compute embedding
    let embedding = match embedding_client.compute_text_embedding(&content).await {
        Ok(emb) => emb,
        Err(e) => {
            return IngestOutcome::Failed {
                error: format!("Embedding computation failed: {e}"),
            };
        }
    };

    // Step 3: Dedup check via hybrid_search
    let dedup_config = HybridSearchConfig {
        sql_weight: 0.0,      // pure vector search for dedup
        vector_weight: 1.0,
        limit: 1,
        min_similarity: DEDUP_SIMILARITY_THRESHOLD,
    };

    match db.get_conn() {
        Ok(conn) => {
            match hybrid_search_knowledge(
                &conn,
                &embedding,
                Some(EXTERNAL_KNOWLEDGE_CATEGORY),
                &dedup_config,
            ) {
                Ok(existing) => {
                    if let Some(top) = existing.first() {
                        if top.similarity >= DEDUP_SIMILARITY_THRESHOLD {
                            return IngestOutcome::Duplicate {
                                existing_id: top.item.id.clone(),
                            };
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[ingestor] Dedup search failed, proceeding with insert: {e}");
                }
            }
        }
        Err(e) => {
            eprintln!("[ingestor] DB connection failed for dedup check, proceeding with insert: {e}");
        }
    }

    // Step 4: Build evidence JSON
    let evidence = build_evidence(result);

    // Confidence: structured data (OSV/Sploitus) = high, web search = medium
    let confidence = match result.provider {
        crate::knowledge_acquisition::SearchProvider::OsvDev
        | crate::knowledge_acquisition::SearchProvider::Sploitus => "high",
        _ => "medium",
    };

    // Step 5: Insert into task_knowledge
    let stored = match db.create_task_knowledge(
        task_run_id,
        EXTERNAL_KNOWLEDGE_CATEGORY,
        SYSTEM_AGENT_TYPE,
        0, // iteration
        &content,
        Some(&evidence),
        confidence,
        &[], // no related files
    ) {
        Ok(stored) => stored,
        Err(e) => {
            return IngestOutcome::Failed {
                error: format!("DB insert failed: {e}"),
            };
        }
    };

    // Step 6: Store embedding
    if let Ok(conn) = db.get_conn() {
        if let Err(e) = store_knowledge_embedding(&conn, &stored.id, &embedding) {
            eprintln!("[ingestor] Failed to store embedding for {}: {e}", stored.id);
        }
    }

    IngestOutcome::Stored { id: stored.id }
}

/// Build evidence JSON from a search result
fn build_evidence(result: &KnowledgeResult) -> String {
    let mut evidence = serde_json::json!({
        "provider": result.provider.as_str(),
        "query": result.query,
    });

    if let Some(ref url) = result.url {
        evidence["url"] = serde_json::Value::String(url.clone());
    }

    if let Some(score) = result.relevance_score {
        evidence["relevance_score"] = serde_json::json!(score);
    }

    if let Some(ref cve) = result.metadata.cve_id {
        evidence["cve_id"] = serde_json::Value::String(cve.clone());
    }

    if let Some(cvss) = result.metadata.cvss_score {
        evidence["cvss_score"] = serde_json::json!(cvss);
    }

    serde_json::to_string(&evidence).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge_acquisition::types::{KnowledgeMetadata, SearchProvider};

    #[test]
    fn test_build_evidence_full() {
        let result = KnowledgeResult {
            provider: SearchProvider::OsvDev,
            query: "lodash vulnerability".to_string(),
            title: "CVE-2021-23337".to_string(),
            content: "Details...".to_string(),
            url: Some("https://osv.dev/vulnerability/CVE-2021-23337".to_string()),
            relevance_score: Some(1.0),
            metadata: KnowledgeMetadata {
                cve_id: Some("CVE-2021-23337".to_string()),
                cvss_score: Some(7.2),
                ..Default::default()
            },
        };

        let evidence = build_evidence(&result);
        let parsed: serde_json::Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(parsed["provider"], "osv_dev");
        assert_eq!(parsed["cve_id"], "CVE-2021-23337");
        assert_eq!(parsed["cvss_score"], 7.2);
    }

    #[test]
    fn test_build_evidence_minimal() {
        let result = KnowledgeResult {
            provider: SearchProvider::DuckDuckGo,
            query: "test".to_string(),
            title: "Test".to_string(),
            content: "Content".to_string(),
            url: None,
            relevance_score: None,
            metadata: KnowledgeMetadata::default(),
        };

        let evidence = build_evidence(&result);
        let parsed: serde_json::Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(parsed["provider"], "duckduckgo");
        assert!(parsed.get("url").is_none());
    }
}
