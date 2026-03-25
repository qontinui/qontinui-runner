pub mod duckduckgo;
pub mod osv;
pub mod perplexity;
pub mod sploitus;
pub mod tavily;
pub mod ui_bridge_fetch;

use super::types::{KnowledgeAcquisitionConfig, SearchProvider};

/// Tracks which providers are available in the current environment
#[derive(Debug)]
pub struct AvailableProviders {
    pub duckduckgo: bool,
    pub tavily: bool,
    pub ui_bridge: bool,
    pub sploitus: bool,
    pub osv: bool,
    pub perplexity: bool,
}

impl AvailableProviders {
    /// Check provider availability based on config and environment
    pub fn detect(config: &KnowledgeAcquisitionConfig) -> Self {
        Self {
            // Free providers — always available
            duckduckgo: true,
            sploitus: true,
            osv: true,
            // Paid providers — require API keys
            tavily: config.tavily_api_key.is_some(),
            perplexity: config.perplexity_api_key.is_some(),
            // UI Bridge — checked dynamically at search time
            ui_bridge: false,
        }
    }

    pub fn is_available(&self, provider: SearchProvider) -> bool {
        match provider {
            SearchProvider::DuckDuckGo => self.duckduckgo,
            SearchProvider::Tavily => self.tavily,
            SearchProvider::UiBridge => self.ui_bridge,
            SearchProvider::Sploitus => self.sploitus,
            SearchProvider::OsvDev => self.osv,
            SearchProvider::Perplexity => self.perplexity,
            SearchProvider::LocalKnowledge => true,
        }
    }

    /// List all currently available providers
    pub fn list_available(&self) -> Vec<SearchProvider> {
        let all = [
            (SearchProvider::DuckDuckGo, self.duckduckgo),
            (SearchProvider::Tavily, self.tavily),
            (SearchProvider::UiBridge, self.ui_bridge),
            (SearchProvider::Sploitus, self.sploitus),
            (SearchProvider::OsvDev, self.osv),
            (SearchProvider::Perplexity, self.perplexity),
        ];
        all.iter()
            .filter(|(_, available)| *available)
            .map(|(provider, _)| *provider)
            .collect()
    }
}
