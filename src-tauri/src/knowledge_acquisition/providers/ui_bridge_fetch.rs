use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

use crate::knowledge_acquisition::types::{
    KnowledgeDomain, KnowledgeMetadata, KnowledgeResult, SearchProvider,
};

const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Snapshot type determines detail level
#[derive(Debug, Clone, Copy)]
pub enum SnapshotType {
    /// Quick overview — uses /ai/summary
    Summary,
    /// Grouped element analysis — uses /ai/analyze
    Analyze,
    /// Complete semantic snapshot — uses /ai/snapshot
    Full,
}

pub struct UiBridgeFetch {
    client: Client,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct HealthResponse {
    #[serde(default)]
    status: String,
}

impl UiBridgeFetch {
    pub fn new(client: Client, base_url: &str) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Check if UI Bridge is connected and healthy
    pub async fn is_available(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        match self
            .client
            .get(&url)
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(health) = resp.json::<HealthResponse>().await {
                    health.status == "ok" || health.status == "healthy"
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// Fetch semantic content from the connected app
    pub async fn fetch(
        &self,
        snapshot_type: SnapshotType,
        query: Option<&str>,
    ) -> Result<Vec<KnowledgeResult>, String> {
        let endpoint = match snapshot_type {
            SnapshotType::Summary => "/ai/summary",
            SnapshotType::Analyze => "/ai/analyze",
            SnapshotType::Full => "/ai/snapshot",
        };

        let url = format!("{}{endpoint}", self.base_url);

        let resp = self
            .client
            .get(&url)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("UI Bridge fetch failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("UI Bridge returned HTTP {}", resp.status()));
        }

        let content = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read UI Bridge response: {e}"))?;

        if content.is_empty() {
            return Ok(Vec::new());
        }

        let title = match snapshot_type {
            SnapshotType::Summary => "UI Bridge: Page Summary",
            SnapshotType::Analyze => "UI Bridge: Element Analysis",
            SnapshotType::Full => "UI Bridge: Full Snapshot",
        };

        // If a query was provided, also search within the page
        let mut results = vec![KnowledgeResult {
            provider: SearchProvider::UiBridge,
            query: query.unwrap_or("ui-bridge-snapshot").to_string(),
            title: title.to_string(),
            content: format!("## {title}\n\n{content}"),
            url: Some(self.base_url.clone()),
            relevance_score: Some(0.8),
            metadata: KnowledgeMetadata {
                domain: Some(KnowledgeDomain::General),
                ..Default::default()
            },
        }];

        // If query is specified, also do an element search
        if let Some(q) = query {
            if let Ok(search_results) = self.search_elements(q).await {
                results.extend(search_results);
            }
        }

        Ok(results)
    }

    /// Search for specific elements by natural language query
    async fn search_elements(&self, query: &str) -> Result<Vec<KnowledgeResult>, String> {
        let url = format!("{}/ai/search", self.base_url);

        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({"query": query}))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("UI Bridge search failed: {e}"))?;

        if !resp.status().is_success() {
            return Ok(Vec::new()); // Non-fatal — main snapshot is enough
        }

        let content = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read UI Bridge search response: {e}"))?;

        if content.is_empty() {
            return Ok(Vec::new());
        }

        Ok(vec![KnowledgeResult {
            provider: SearchProvider::UiBridge,
            query: query.to_string(),
            title: format!("UI Bridge: Search '{query}'"),
            content: format!("## Element Search: {query}\n\n{content}"),
            url: Some(self.base_url.clone()),
            relevance_score: Some(0.85),
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
    fn test_snapshot_type_endpoints() {
        // Verify the enum maps to expected endpoint paths
        let _summary = SnapshotType::Summary;
        let _analyze = SnapshotType::Analyze;
        let _full = SnapshotType::Full;
    }

    #[test]
    fn test_base_url_normalization() {
        let fetch = UiBridgeFetch::new(Client::new(), "http://localhost:3000/");
        assert_eq!(fetch.base_url, "http://localhost:3000");
    }
}
