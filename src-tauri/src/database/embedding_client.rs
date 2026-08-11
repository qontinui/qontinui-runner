//! Async HTTP client for computing text embeddings locally via the runner's embedding service.
//!
//! Uses the `POST /api/embeddings/compute-text` endpoint which returns
//! 384-dimensional MiniLM-L6-v2 embeddings.

use crate::str_utils::truncate_str;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Default URL for the runner's local embedding service.
// Use 127.0.0.1 to avoid IPv6 resolution delays on Windows
const DEFAULT_EMBEDDING_URL: &str = "http://127.0.0.1:8001/api/embeddings/compute-text";

/// Canonical tag for the embedding space this client produces — the single
/// source of truth for every producer that ships vectors off this machine
/// (tenant memory records, tenant queries, `kind="embedding"` jobs).
///
/// **Why the window is in the tag.** A Phase 0 probe of plan
/// `2026-07-13-runner-paid-embedding` proved the runner's sentence-transformers
/// stack embeds over a **256-token** window while the web backend's fastembed
/// truncated at **128** — that window mismatch (not quantization) is why the two
/// spaces diverged (min cosine 0.71 across a shared corpus). The operator
/// adopted the native 256 window; 256 is sentence-transformers' default, so the
/// client needs no `max_seq_length` override to produce it. The tag names the
/// window because the window is the property that changed, and it is what lets a
/// consumer tell two otherwise identically-named MiniLM spaces apart.
///
/// Bump this string whenever the produced space changes (model, window, or
/// pooling) so stored vectors stay attributable to the space that made them.
pub const EMBEDDING_MODEL_TAG: &str = "minilm-l6-v2-256@sentence-transformers";

/// Why a width-checked embed did not yield a usable vector.
///
/// The two arms are kept apart because a caller must be able to tell them
/// apart: "the service did not answer" and "the service answered with
/// something that is not a vector in our space" call for different reporting,
/// and folding the second into the first is what let a model swap look like an
/// outage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedFailure {
    /// The service errored, was unreachable, or answered non-2xx.
    Unavailable(String),
    /// It answered 200 with a vector of the WRONG WIDTH.
    WrongWidth { got: usize },
}

impl std::fmt::Display for EmbedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "{e}"),
            Self::WrongWidth { got } => write!(
                f,
                "embedding service returned a {got}-dim vector, expected {}",
                crate::database::embeddings::EMBEDDING_DIM
            ),
        }
    }
}

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

/// Client for computing text embeddings via the runner's local embedding service.
#[derive(Clone)]
pub struct EmbeddingClient {
    client: reqwest::Client,
    base_url: String,
}

impl EmbeddingClient {
    /// The default embedding service URL (for health probes, config display, etc.).
    pub fn default_url() -> &'static str {
        DEFAULT_EMBEDDING_URL
    }

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
                    truncate_str(t, 2000).to_string()
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

    /// [`Self::compute_text_embedding`], refusing a vector of the wrong width.
    ///
    /// **Every producer that ships a vector OFF this machine must come through
    /// here.** Nothing downstream checks the width: this client does not, coord
    /// deliberately validates no dimension (the space is the backend's to own),
    /// and the backend then rejects a non-[`EMBEDDING_DIM`] vector with a 422.
    /// So an embedding service that answers 200 with a foreign-width vector — a
    /// model swap, a misconfigured deployment — fails CLOSED at the far end of
    /// a call chain that has already forgotten where the vector came from.
    ///
    /// Keeping the check in the client rather than at each call site is the
    /// point: the guard shipped at two of the three producers named in
    /// [`EMBEDDING_MODEL_TAG`]'s own doc comment, and a fourth producer would
    /// have had nothing to remind it.
    ///
    /// [`EMBEDDING_DIM`]: crate::database::embeddings::EMBEDDING_DIM
    pub async fn compute_text_embedding_checked(
        &self,
        text: &str,
    ) -> Result<Vec<f32>, EmbedFailure> {
        let embedding = self
            .compute_text_embedding(text)
            .await
            .map_err(EmbedFailure::Unavailable)?;
        match width_error(&embedding) {
            Some(e) => Err(e),
            None => Ok(embedding),
        }
    }

    /// [`Self::compute_batch_embeddings`], refusing the batch if ANY vector is
    /// the wrong width.
    ///
    /// All-or-nothing deliberately. The batch's consumers zip the vectors back
    /// onto their inputs positionally, so handing back a partial batch would
    /// mis-attribute every vector after the dropped one — far worse than
    /// declining the whole job and letting it be retried.
    ///
    /// The vector COUNT is not checked here: the callers that care compare it
    /// against their own inputs and have their own reporting for it.
    pub async fn compute_batch_embeddings_checked(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, EmbedFailure> {
        let embeddings = self
            .compute_batch_embeddings(texts)
            .await
            .map_err(EmbedFailure::Unavailable)?;
        for e in &embeddings {
            if let Some(err) = width_error(e) {
                return Err(err);
            }
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

/// The one place the width is enforced — `None` when the vector is in our
/// space. Free rather than inline at each checked method so the single- and
/// batch-vector paths cannot drift.
fn width_error(embedding: &[f32]) -> Option<EmbedFailure> {
    let got = embedding.len();
    (got != crate::database::embeddings::EMBEDDING_DIM).then_some(EmbedFailure::WrongWidth { got })
}

impl Default for EmbeddingClient {
    fn default() -> Self {
        Self::new()
    }
}

/// The width guard, which nothing downstream re-checks.
///
/// These assert against a service that ANSWERS — the failure mode the guard
/// exists for. A dead service is already covered elsewhere; a live one handing
/// back a foreign-width vector is the one that used to travel all the way to a
/// backend 422 while every local signal said "fine".
#[cfg(test)]
mod width_guard_tests {
    use super::*;
    use crate::database::embeddings::EMBEDDING_DIM;
    use axum::{routing::post, Json, Router};
    use serde_json::json;

    /// A fake embedding service that answers every shape with vectors of
    /// `dims` width — both the single and the batch endpoint, since
    /// `compute_batch_embeddings` derives the batch URL from this one.
    async fn spawn_embedder(dims: usize) -> String {
        let app = Router::new()
            .route(
                "/api/embeddings/compute-text",
                post(move || async move { Json(json!({ "embedding": vec![0.1f32; dims] })) }),
            )
            .route(
                "/api/embeddings/compute-batch",
                post(move |Json(body): Json<serde_json::Value>| async move {
                    let n = body
                        .get("texts")
                        .and_then(|t| t.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    Json(json!({ "embeddings": vec![vec![0.1f32; dims]; n] }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/api/embeddings/compute-text")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_right_width_vector_passes_through_unchanged() {
        let url = spawn_embedder(EMBEDDING_DIM).await;
        let got = EmbeddingClient::with_url(&url)
            .compute_text_embedding_checked("login")
            .await
            .expect("a correctly-sized vector must pass the guard");
        assert_eq!(got.len(), EMBEDDING_DIM);
    }

    /// The whole reason this method exists: a 200 with a foreign width is a
    /// FAILURE here, not a vector to be shipped.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_width_vector_is_refused_and_named_as_such() {
        let url = spawn_embedder(EMBEDDING_DIM / 2).await;
        let err = EmbeddingClient::with_url(&url)
            .compute_text_embedding_checked("login")
            .await
            .expect_err("a foreign-width vector must never reach a caller");
        assert_eq!(
            err,
            EmbedFailure::WrongWidth {
                got: EMBEDDING_DIM / 2
            },
            "a width failure must be distinguishable from an outage — callers \
             report and count the two differently"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dead_service_is_unavailable_not_a_width_failure() {
        let err = EmbeddingClient::with_url("http://127.0.0.1:1/api/embeddings/compute-text")
            .compute_text_embedding_checked("login")
            .await
            .expect_err("a dead service cannot yield a vector");
        assert!(
            matches!(err, EmbedFailure::Unavailable(_)),
            "an outage must not be reported as an off-spec vector: {err:?}"
        );
    }

    /// All-or-nothing: the batch's consumers zip positionally, so one bad
    /// vector poisons the whole batch rather than just its own slot.
    #[tokio::test(flavor = "multi_thread")]
    async fn one_wrong_width_vector_refuses_the_whole_batch() {
        let texts = vec!["a".to_string(), "b".to_string()];

        let ok_url = spawn_embedder(EMBEDDING_DIM).await;
        let ok = EmbeddingClient::with_url(&ok_url)
            .compute_batch_embeddings_checked(&texts)
            .await
            .expect("a right-width batch must pass");
        assert_eq!(ok.len(), 2);
        assert!(ok.iter().all(|e| e.len() == EMBEDDING_DIM));

        let bad_url = spawn_embedder(7).await;
        assert_eq!(
            EmbeddingClient::with_url(&bad_url)
                .compute_batch_embeddings_checked(&texts)
                .await
                .expect_err("an off-spec batch must be refused whole"),
            EmbedFailure::WrongWidth { got: 7 }
        );
    }
}
