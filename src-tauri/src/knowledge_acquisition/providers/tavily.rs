use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::knowledge_acquisition::types::{
    KnowledgeDomain, KnowledgeMetadata, KnowledgeResult, SearchProvider,
};

const TAVILY_URL: &str = "https://api.tavily.com/search";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RAW_CONTENT_CAP: usize = 50_000;

pub struct Tavily {
    client: Client,
    api_key: String,
}

#[derive(Debug, Serialize)]
struct TavilyApiRequest {
    query: String,
    max_results: usize,
    search_depth: String,
    include_raw_content: bool,
    include_answer: bool,
}

#[derive(Debug, Deserialize)]
struct TavilyApiResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyApiResult>,
    #[serde(default)]
    query: String,
}

#[derive(Debug, Deserialize)]
struct TavilyApiResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    raw_content: Option<String>,
    #[serde(default)]
    score: f64,
}

impl Tavily {
    /// Create a new Tavily provider. Returns None if no API key is available.
    pub fn new(client: Client, api_key: String) -> Self {
        Self { client, api_key }
    }

    /// Try to create from environment variable
    pub fn from_env(client: Client) -> Option<Self> {
        let key = std::env::var("TAVILY_API_KEY").ok()?;
        if key.is_empty() {
            return None;
        }
        Some(Self::new(client, key))
    }

    /// Deep search with AI-synthesized answers and raw content
    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        search_depth: &str,
    ) -> Result<Vec<KnowledgeResult>, String> {
        let max_results = max_results.clamp(1, 10);
        let depth = match search_depth {
            "basic" => "basic",
            _ => "advanced",
        };

        let request = TavilyApiRequest {
            query: query.to_string(),
            max_results,
            search_depth: depth.to_string(),
            include_raw_content: true,
            include_answer: true,
        };

        let resp = self
            .client
            .post(TAVILY_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("Tavily search failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Tavily returned HTTP {status}: {body}"));
        }

        let response: TavilyApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Tavily response: {e}"))?;

        let mut results = Vec::new();

        // Include synthesized answer as first result if available
        if let Some(ref answer) = response.answer {
            if !answer.is_empty() {
                results.push(KnowledgeResult {
                    provider: SearchProvider::Tavily,
                    query: query.to_string(),
                    title: format!("AI Answer: {query}"),
                    content: format!("## AI-Synthesized Answer\n\n{answer}"),
                    url: None,
                    relevance_score: Some(1.0),
                    metadata: KnowledgeMetadata {
                        domain: Some(KnowledgeDomain::General),
                        ..Default::default()
                    },
                });
            }
        }

        // Add individual results
        for api_result in response.results {
            let content = format_tavily_result(&api_result);

            results.push(KnowledgeResult {
                provider: SearchProvider::Tavily,
                query: query.to_string(),
                title: api_result.title,
                content,
                url: Some(api_result.url),
                relevance_score: Some(api_result.score),
                metadata: KnowledgeMetadata {
                    domain: Some(KnowledgeDomain::General),
                    ..Default::default()
                },
            });
        }

        Ok(results)
    }
}

/// Format a Tavily result into markdown, capping raw content
fn format_tavily_result(result: &TavilyApiResult) -> String {
    let mut content = format!("## {}\n\n", result.title);
    content.push_str(&format!("**Relevance**: {:.2}\n", result.score));
    content.push_str(&format!("**Source**: {}\n\n", result.url));

    // Prefer raw content (full page) but cap it
    if let Some(ref raw) = result.raw_content {
        if !raw.is_empty() {
            let mut text = raw.clone();
            if text.len() > RAW_CONTENT_CAP {
                let mut end = RAW_CONTENT_CAP;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str("\n\n[... content truncated]");
            }
            content.push_str(&text);
            return content;
        }
    }

    // Fall back to snippet
    content.push_str(&result.content);
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tavily_response() {
        let json = r#"{
            "answer": "Rust's borrow checker ensures memory safety.",
            "results": [{
                "title": "Understanding Rust Borrowing",
                "url": "https://doc.rust-lang.org/book/ch04-02-references-and-borrowing.html",
                "content": "References and Borrowing chapter from The Rust Book.",
                "raw_content": "Full page content goes here...",
                "score": 0.95
            }],
            "query": "rust borrow checker"
        }"#;

        let response: TavilyApiResponse = serde_json::from_str(json).unwrap();
        assert!(response.answer.is_some());
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].score, 0.95);
    }

    #[test]
    fn test_format_tavily_result_truncation() {
        let result = TavilyApiResult {
            title: "Test".to_string(),
            url: "https://example.com".to_string(),
            content: "snippet".to_string(),
            raw_content: Some("x".repeat(60_000)),
            score: 0.8,
        };
        let formatted = format_tavily_result(&result);
        assert!(formatted.len() < 55_000);
        assert!(formatted.contains("[... content truncated]"));
    }

    #[test]
    fn test_format_tavily_result_fallback_to_snippet() {
        let result = TavilyApiResult {
            title: "Test".to_string(),
            url: "https://example.com".to_string(),
            content: "The snippet content".to_string(),
            raw_content: None,
            score: 0.7,
        };
        let formatted = format_tavily_result(&result);
        assert!(formatted.contains("The snippet content"));
    }
}
