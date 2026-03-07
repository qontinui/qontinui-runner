//! Async HTTP client for computing text embeddings via the qontinui-api.
//!
//! Uses the existing `POST /api/embeddings/compute-text` endpoint which returns
//! 384-dimensional MiniLM-L6-v2 embeddings.

use crate::str_utils::truncate_str;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Default URL for the qontinui-api embedding service.
// Use 127.0.0.1 to avoid IPv6 resolution delays on Windows
const DEFAULT_EMBEDDING_URL: &str = "http://127.0.0.1:8001/api/embeddings/compute-text";

/// Request payload for the embedding API.
#[derive(Serialize)]
struct EmbeddingRequest {
    text: String,
    model: String,
}

/// Request payload for batch embedding API.
#[derive(Serialize)]
struct BatchEmbeddingRequest {
    texts: Vec<String>,
    model: String,
}

/// Response from the embedding API.
#[derive(Deserialize)]
struct EmbeddingResponse {
    embedding: Vec<f32>,
}

/// Response from the batch embedding API.
#[derive(Deserialize)]
struct BatchEmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

/// Client for computing text embeddings via the qontinui-api.
#[derive(Clone)]
pub struct EmbeddingClient {
    client: reqwest::Client,
    base_url: String,
}

impl EmbeddingClient {
    /// Create a new embedding client with default URL.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            base_url: DEFAULT_EMBEDDING_URL.to_string(),
        }
    }

    /// Create a new embedding client with a custom URL.
    pub fn with_url(url: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            base_url: url.to_string(),
        }
    }

    /// Compute a text embedding for a single text string.
    ///
    /// Returns a 384-dimensional f32 vector.
    pub async fn compute_text_embedding(&self, text: &str) -> Result<Vec<f32>, String> {
        // Truncate very long texts to avoid API issues (MiniLM has 256 token limit)
        let truncated = if text.len() > 2000 {
            truncate_str(text, 2000)
        } else {
            text
        };

        let request = EmbeddingRequest {
            text: truncated.to_string(),
            model: "minilm".to_string(),
        };

        let response = self
            .client
            .post(&self.base_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Embedding API request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Embedding API returned {}: {}", status, body));
        }

        let result: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse embedding response: {}", e))?;

        debug!(
            "Computed embedding for text ({} chars) -> {} dimensions",
            text.len(),
            result.embedding.len()
        );

        Ok(result.embedding)
    }

    /// Compute embeddings for multiple texts in a single batch request.
    ///
    /// Falls back to individual requests if the batch endpoint is not available.
    pub async fn compute_batch_embeddings(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Try batch endpoint first
        let batch_url = self.base_url.replace("compute-text", "compute-batch");
        let truncated_texts: Vec<String> = texts
            .iter()
            .map(|t| {
                if t.len() > 2000 {
                    t[..2000].to_string()
                } else {
                    t.clone()
                }
            })
            .collect();

        let request = BatchEmbeddingRequest {
            texts: truncated_texts,
            model: "minilm".to_string(),
        };

        let response = self.client.post(&batch_url).json(&request).send().await;

        match response {
            Ok(resp) if resp.status().is_success() => {
                let result: BatchEmbeddingResponse = resp
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse batch embedding response: {}", e))?;
                return Ok(result.embeddings);
            }
            Ok(resp) => {
                warn!(
                    "Batch embedding endpoint returned {}, falling back to individual requests",
                    resp.status()
                );
            }
            Err(e) => {
                warn!(
                    "Batch embedding endpoint unavailable ({}), falling back to individual requests",
                    e
                );
            }
        }

        // Fallback: compute individually
        let mut embeddings = Vec::with_capacity(texts.len());
        for text in texts {
            let embedding = self.compute_text_embedding(text).await?;
            embeddings.push(embedding);
        }
        Ok(embeddings)
    }

    /// Check if the embedding API is available.
    pub async fn is_available(&self) -> bool {
        // Quick health check with a short text
        match self
            .client
            .post(&self.base_url)
            .json(&EmbeddingRequest {
                text: "test".to_string(),
                model: "minilm".to_string(),
            })
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

impl Default for EmbeddingClient {
    fn default() -> Self {
        Self::new()
    }
}
