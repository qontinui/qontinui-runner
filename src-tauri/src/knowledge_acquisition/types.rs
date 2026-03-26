use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Budget for a single search session
#[derive(Debug, Clone)]
pub struct SearchBudget {
    pub max_actions: usize,
    pub max_total_content_bytes: usize,
    pub per_result_cap_bytes: usize,
    pub timeout_per_call: Duration,
    pub actions_used: usize,
    pub content_bytes_used: usize,
}

impl Default for SearchBudget {
    fn default() -> Self {
        Self {
            max_actions: 5,
            max_total_content_bytes: 80_000,
            per_result_cap_bytes: 50_000,
            timeout_per_call: Duration::from_secs(30),
            actions_used: 0,
            content_bytes_used: 0,
        }
    }
}

impl SearchBudget {
    pub fn can_search(&self) -> bool {
        self.actions_used < self.max_actions
            && self.content_bytes_used < self.max_total_content_bytes
    }

    pub fn record_action(&mut self, content_bytes: usize) {
        self.actions_used += 1;
        self.content_bytes_used += content_bytes;
    }

}

/// Which search provider produced a result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    DuckDuckGo,
    Tavily,
    UiBridge,
    Sploitus,
    OsvDev,
    Perplexity,
    LocalKnowledge,
}

impl SearchProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Tavily => "tavily",
            Self::UiBridge => "ui_bridge",
            Self::Sploitus => "sploitus",
            Self::OsvDev => "osv_dev",
            Self::Perplexity => "perplexity",
            Self::LocalKnowledge => "local_knowledge",
        }
    }

    /// Whether this provider requires an API key
    pub fn requires_api_key(&self) -> bool {
        matches!(self, Self::Tavily | Self::Perplexity)
    }
}

/// Domain classification for search queries
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeDomain {
    ErrorResolution,
    ApiDocumentation,
    VulnerabilityIntelligence,
    ToolingStandards,
    General,
}

/// Unified search result from any provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeResult {
    pub provider: SearchProvider,
    pub query: String,
    pub title: String,
    pub content: String,
    pub url: Option<String>,
    pub relevance_score: Option<f64>,
    pub metadata: KnowledgeMetadata,
}

impl KnowledgeResult {
    /// Truncate content to the per-result byte cap
    pub fn truncate_content(&mut self, max_bytes: usize) {
        if self.content.len() > max_bytes {
            // Find a safe UTF-8 boundary
            let mut end = max_bytes;
            while end > 0 && !self.content.is_char_boundary(end) {
                end -= 1;
            }
            self.content.truncate(end);
            self.content.push_str("\n\n[... truncated]");
        }
    }

    /// Whether this result's content exceeds the summarization threshold
    pub fn needs_summarization(&self) -> bool {
        self.content.len() > 3000
    }
}

/// Additional metadata attached to a search result
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnowledgeMetadata {
    pub domain: Option<KnowledgeDomain>,
    pub cvss_score: Option<f64>,
    pub cve_id: Option<String>,
    pub ecosystem: Option<String>,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
}

/// Configuration for the knowledge acquisition system
#[derive(Debug, Clone)]
pub struct KnowledgeAcquisitionConfig {
    pub enabled: bool,
    pub tavily_api_key: Option<String>,
    pub perplexity_api_key: Option<String>,
    pub max_actions_per_session: usize,
    pub max_content_bytes: usize,
    pub per_result_cap_bytes: usize,
    pub timeout_per_call_secs: u64,
}

impl Default for KnowledgeAcquisitionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tavily_api_key: std::env::var("TAVILY_API_KEY").ok(),
            perplexity_api_key: std::env::var("PERPLEXITY_API_KEY").ok(),
            max_actions_per_session: 5,
            max_content_bytes: 80_000,
            per_result_cap_bytes: 50_000,
            timeout_per_call_secs: 30,
        }
    }
}

impl KnowledgeAcquisitionConfig {
    pub fn to_budget(&self) -> SearchBudget {
        SearchBudget {
            max_actions: self.max_actions_per_session,
            max_total_content_bytes: self.max_content_bytes,
            per_result_cap_bytes: self.per_result_cap_bytes,
            timeout_per_call: Duration::from_secs(self.timeout_per_call_secs),
            actions_used: 0,
            content_bytes_used: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_budget_default() {
        let budget = SearchBudget::default();
        assert!(budget.can_search());
        assert_eq!(budget.max_actions, 5);
        assert_eq!(budget.max_total_content_bytes, 80_000);
    }

    #[test]
    fn test_budget_exhaustion() {
        let mut budget = SearchBudget::default();
        for _ in 0..5 {
            budget.record_action(1000);
        }
        assert!(!budget.can_search());
    }

    #[test]
    fn test_budget_byte_limit() {
        let mut budget = SearchBudget::default();
        budget.record_action(80_001);
        assert!(!budget.can_search());
    }

    #[test]
    fn test_result_truncation() {
        let mut result = KnowledgeResult {
            provider: SearchProvider::DuckDuckGo,
            query: "test".to_string(),
            title: "Test".to_string(),
            content: "a".repeat(60_000),
            url: None,
            relevance_score: None,
            metadata: KnowledgeMetadata::default(),
        };
        result.truncate_content(50_000);
        assert!(result.content.len() <= 50_020); // 50000 + "[... truncated]"
    }

    #[test]
    fn test_provider_as_str() {
        assert_eq!(SearchProvider::DuckDuckGo.as_str(), "duckduckgo");
        assert_eq!(SearchProvider::OsvDev.as_str(), "osv_dev");
    }

    #[test]
    fn test_needs_summarization() {
        let short = KnowledgeResult {
            provider: SearchProvider::DuckDuckGo,
            query: "q".to_string(),
            title: "t".to_string(),
            content: "short".to_string(),
            url: None,
            relevance_score: None,
            metadata: KnowledgeMetadata::default(),
        };
        assert!(!short.needs_summarization());

        let long = KnowledgeResult {
            content: "x".repeat(4000),
            ..short
        };
        assert!(long.needs_summarization());
    }
}
