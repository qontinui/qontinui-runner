use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::knowledge_acquisition::types::{
    KnowledgeDomain, KnowledgeMetadata, KnowledgeResult, SearchProvider,
};

const PERPLEXITY_URL: &str = "https://api.perplexity.ai/chat/completions";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60); // Longer timeout — LLM synthesis
const DEFAULT_MODEL: &str = "sonar";

pub struct Perplexity {
    client: Client,
    api_key: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct PerplexityApiRequest {
    model: String,
    messages: Vec<PerplexityMessage>,
}

#[derive(Debug, Serialize)]
struct PerplexityMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct PerplexityApiResponse {
    #[serde(default)]
    choices: Vec<PerplexityChoice>,
    #[serde(default)]
    citations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PerplexityChoice {
    #[serde(default)]
    message: PerplexityResponseMessage,
}

#[derive(Debug, Default, Deserialize)]
struct PerplexityResponseMessage {
    #[serde(default)]
    content: String,
}

impl Perplexity {
    /// Create a new Perplexity provider.
    pub fn new(client: Client, api_key: String) -> Self {
        Self {
            client,
            api_key,
            model: DEFAULT_MODEL.to_string(),
        }
    }

    /// Try to create from environment variable
    pub fn from_env(client: Client) -> Option<Self> {
        let key = std::env::var("PERPLEXITY_API_KEY").ok()?;
        if key.is_empty() {
            return None;
        }
        Some(Self::new(client, key))
    }

    /// Deep research query with citations
    pub async fn search(&self, query: &str) -> Result<Vec<KnowledgeResult>, String> {
        let request = PerplexityApiRequest {
            model: self.model.clone(),
            messages: vec![
                PerplexityMessage {
                    role: "system".to_string(),
                    content: "Be precise and concise. Include specific version numbers, code examples, and URLs where relevant.".to_string(),
                },
                PerplexityMessage {
                    role: "user".to_string(),
                    content: query.to_string(),
                },
            ],
        };

        let resp = self
            .client
            .post(PERPLEXITY_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("Perplexity search failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Perplexity returned HTTP {status}: {body}"));
        }

        let response: PerplexityApiResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Perplexity response: {e}"))?;

        let answer = response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        if answer.is_empty() {
            return Ok(Vec::new());
        }

        let mut content = format!("## Research: {query}\n\n{answer}");

        if !response.citations.is_empty() {
            content.push_str("\n\n### Citations\n");
            for (i, citation) in response.citations.iter().enumerate() {
                content.push_str(&format!("{}. {citation}\n", i + 1));
            }
        }

        Ok(vec![KnowledgeResult {
            provider: SearchProvider::Perplexity,
            query: query.to_string(),
            title: format!("Research: {query}"),
            content,
            url: response.citations.first().cloned(),
            relevance_score: Some(0.9),
            metadata: KnowledgeMetadata {
                domain: Some(KnowledgeDomain::General),
                ..Default::default()
            },
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_perplexity_response() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": "Rust's ownership system ensures memory safety without a garbage collector."
                }
            }],
            "citations": [
                "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html",
                "https://blog.rust-lang.org/2024/02/19/ownership.html"
            ]
        }"#;

        let response: PerplexityApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.choices.len(), 1);
        assert!(response.choices[0]
            .message
            .content
            .contains("ownership"));
        assert_eq!(response.citations.len(), 2);
    }

    #[test]
    fn test_empty_response() {
        let json = r#"{"choices": [], "citations": []}"#;
        let response: PerplexityApiResponse = serde_json::from_str(json).unwrap();
        assert!(response.choices.is_empty());
    }
}
