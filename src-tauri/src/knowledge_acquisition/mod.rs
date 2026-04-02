pub mod ingestor;
pub mod providers;
pub mod stats;
pub mod summarizer;
pub mod types;
pub mod vuln_enrichment;

pub use types::{
    KnowledgeAcquisitionConfig, KnowledgeDomain, KnowledgeMetadata, KnowledgeResult,
    SearchBudget, SearchProvider,
};

use providers::duckduckgo::DuckDuckGo;
use providers::osv::Osv;
use providers::perplexity::Perplexity;
use providers::sploitus::{ExploitType, SortOrder, Sploitus};
use providers::tavily::Tavily;
use providers::ui_bridge_fetch::{SnapshotType, UiBridgeFetch};
use providers::AvailableProviders;
use reqwest::Client;
use stats::KnowledgeStats;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Global singleton — shares HTTP client, rate limiters, and provider instances
/// across all call sites. Created on first access.
static GLOBAL_INSTANCE: OnceLock<Arc<KnowledgeAcquisition>> = OnceLock::new();

/// Optional context for knowledge flywheel (Tier 0 lookup + Tier 3 storage).
/// When provided, search() will check local knowledge first and store results after.
/// All persistence goes through PostgreSQL (PgDb).
pub struct SearchContext {
    pub pg: Option<Arc<crate::database::pg::PgDb>>,
    pub task_run_id: String,
}

impl SearchContext {
    /// Create a system-level context (for API routes without a specific task).
    /// Uses PgDb::try_global() for PG.
    pub fn system() -> Option<Self> {
        let pg = crate::database::pg::PgDb::try_global();
        pg.as_ref()?;
        Some(Self {
            pg,
            task_run_id: "system".to_string(),
        })
    }

    /// Create with an explicit PG reference.
    pub fn with_pg(
        pg: Option<Arc<crate::database::pg::PgDb>>,
        task_run_id: String,
    ) -> Self {
        Self { pg, task_run_id }
    }
}

/// Coordinator for the knowledge acquisition system.
/// Implements tiered search: local knowledge → UI Bridge → web search → store results.
pub struct KnowledgeAcquisition {
    config: KnowledgeAcquisitionConfig,
    providers: AvailableProviders,
    duckduckgo: DuckDuckGo,
    tavily: Option<Tavily>,
    perplexity: Option<Perplexity>,
    sploitus: Sploitus,
    osv: Osv,
    ui_bridge: UiBridgeFetch,
    pub stats: KnowledgeStats,
}

impl KnowledgeAcquisition {
    /// Get or create the global singleton instance.
    pub fn new() -> Arc<Self> {
        GLOBAL_INSTANCE
            .get_or_init(|| {
                let config = KnowledgeAcquisitionConfig::default();
                Arc::new(Self::create(config))
            })
            .clone()
    }

    /// Create with explicit config (not cached — use for testing or custom config)
    pub fn with_config(config: KnowledgeAcquisitionConfig) -> Self {
        Self::create(config)
    }

    fn create(config: KnowledgeAcquisitionConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_per_call_secs))
            .build()
            .unwrap_or_default();

        let providers = AvailableProviders::detect(&config);
        let tavily = config
            .tavily_api_key
            .as_ref()
            .map(|key| Tavily::new(client.clone(), key.clone()))
            .or_else(|| Tavily::from_env(client.clone()));
        let perplexity = config
            .perplexity_api_key
            .as_ref()
            .map(|key| Perplexity::new(client.clone(), key.clone()))
            .or_else(|| Perplexity::from_env(client.clone()));
        let ui_bridge = UiBridgeFetch::new(client.clone(), "http://127.0.0.1:1420");

        Self {
            duckduckgo: DuckDuckGo::new(client.clone()),
            tavily,
            perplexity,
            sploitus: Sploitus::new(client.clone()),
            osv: Osv::new(client.clone()),
            ui_bridge,
            config,
            providers,
            stats: KnowledgeStats::new(),
        }
    }

    /// List available providers
    pub fn available_providers(&self) -> Vec<SearchProvider> {
        self.providers.list_available()
    }

    /// Full tiered search with knowledge flywheel
    pub async fn search(
        &self,
        query: &str,
        domain: KnowledgeDomain,
        max_results: usize,
    ) -> Result<Vec<KnowledgeResult>, String> {
        self.search_with_context(query, domain, max_results, None)
            .await
    }

    /// Search with optional context for Tier 0/3 flywheel
    pub async fn search_with_context(
        &self,
        query: &str,
        domain: KnowledgeDomain,
        max_results: usize,
        ctx: Option<&SearchContext>,
    ) -> Result<Vec<KnowledgeResult>, String> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        self.stats.total_searches.fetch_add(1, Ordering::Relaxed);
        let mut budget = self.config.to_budget();
        let mut all_results: Vec<KnowledgeResult> = Vec::new();

        // === Tier 0: Local knowledge (if DB available) ===
        if let Some(ctx) = ctx {
            match self.search_local(query, &mut budget, ctx).await {
                Ok(local_results) if !local_results.is_empty() => {
                    let count = local_results.len() as u64;
                    self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                    self.stats.local_knowledge.record_success(count, 0);
                    return Ok(local_results);
                }
                Ok(_) => {
                    // Queried successfully but no matches — record the attempt
                    self.stats.local_knowledge.record_success(0, 0);
                }
                Err(_) => {
                    self.stats.local_knowledge.record_failure();
                }
            }
        }

        // === Tier 1: UI Bridge (if connected) ===
        if budget.can_search() && domain != KnowledgeDomain::VulnerabilityIntelligence {
            if let Ok(true) = tokio::time::timeout(
                Duration::from_secs(3),
                self.ui_bridge.is_available(),
            )
            .await
            {
                match tokio::time::timeout(
                    budget.timeout_per_call,
                    self.ui_bridge.fetch(SnapshotType::Summary, Some(query)),
                )
                .await
                {
                    Ok(Ok(results)) if !results.is_empty() => {
                        let content_bytes: usize =
                            results.iter().map(|r| r.content.len()).sum();
                        self.stats
                            .ui_bridge
                            .record_success(results.len() as u64, content_bytes as u64);
                        budget.record_action(content_bytes);
                        all_results.extend(results);
                    }
                    Ok(Err(_)) => {
                        self.stats.ui_bridge.record_failure();
                    }
                    Err(_) => {
                        self.stats.ui_bridge.record_timeout();
                    }
                    Ok(Ok(_)) => {
                        // Empty results — still a successful query
                        self.stats.ui_bridge.record_success(0, 0);
                    }
                }
            }
        }

        // === Tier 2a: DuckDuckGo (free, fast) ===
        if budget.can_search() && self.providers.is_available(SearchProvider::DuckDuckGo) {
            match tokio::time::timeout(
                budget.timeout_per_call,
                self.duckduckgo.search(query, max_results),
            )
            .await
            {
                Ok(Ok(results)) => {
                    let content_bytes: usize = results.iter().map(|r| r.content.len()).sum();
                    self.stats
                        .duckduckgo
                        .record_success(results.len() as u64, content_bytes as u64);
                    budget.record_action(content_bytes);
                    all_results.extend(results);
                }
                Ok(Err(e)) => {
                    self.stats.duckduckgo.record_failure();
                    eprintln!("[knowledge_acquisition] DuckDuckGo search failed: {e}");
                }
                Err(_) => {
                    self.stats.duckduckgo.record_timeout();
                    eprintln!("[knowledge_acquisition] DuckDuckGo search timed out");
                }
            }
        }

        // === Tier 2b: Tavily escalation (if DuckDuckGo insufficient) ===
        if all_results.len() < 2
            && budget.can_search()
            && self.providers.is_available(SearchProvider::Tavily)
        {
            if let Some(ref tavily) = self.tavily {
                let depth = if domain == KnowledgeDomain::ApiDocumentation {
                    "advanced"
                } else {
                    "basic"
                };
                match tokio::time::timeout(
                    budget.timeout_per_call,
                    tavily.search(query, max_results, depth),
                )
                .await
                {
                    Ok(Ok(results)) => {
                        let content_bytes: usize =
                            results.iter().map(|r| r.content.len()).sum();
                        self.stats
                            .tavily
                            .record_success(results.len() as u64, content_bytes as u64);
                        budget.record_action(content_bytes);
                        all_results.extend(results);
                    }
                    Ok(Err(e)) => {
                        self.stats.tavily.record_failure();
                        eprintln!("[knowledge_acquisition] Tavily search failed: {e}");
                    }
                    Err(_) => {
                        self.stats.tavily.record_timeout();
                        eprintln!("[knowledge_acquisition] Tavily search timed out");
                    }
                }
            }
        }

        // === Tier 2c: Perplexity deep research (last resort) ===
        if all_results.is_empty()
            && budget.can_search()
            && self.providers.is_available(SearchProvider::Perplexity)
        {
            if let Some(ref perplexity) = self.perplexity {
                match tokio::time::timeout(budget.timeout_per_call, perplexity.search(query)).await
                {
                    Ok(Ok(results)) => {
                        let content_bytes: usize =
                            results.iter().map(|r| r.content.len()).sum();
                        self.stats
                            .perplexity
                            .record_success(results.len() as u64, content_bytes as u64);
                        budget.record_action(content_bytes);
                        all_results.extend(results);
                    }
                    Ok(Err(e)) => {
                        self.stats.perplexity.record_failure();
                        eprintln!("[knowledge_acquisition] Perplexity search failed: {e}");
                    }
                    Err(_) => {
                        self.stats.perplexity.record_timeout();
                        eprintln!("[knowledge_acquisition] Perplexity search timed out");
                    }
                }
            }
        }

        // Apply per-result cap and domain tagging
        for result in &mut all_results {
            result.truncate_content(self.config.per_result_cap_bytes);
            if result.metadata.domain.is_none() {
                result.metadata.domain = Some(domain);
            }
        }

        // Deduplicate by URL
        let mut seen_urls = std::collections::HashSet::new();
        all_results.retain(|r| {
            if let Some(ref url) = r.url {
                seen_urls.insert(url.clone())
            } else {
                true
            }
        });

        // === Tier 3: Store results in task_knowledge (knowledge flywheel) ===
        if !all_results.is_empty() {
            if let Some(ctx) = ctx {
                let outcomes =
                    ingestor::ingest_results(
                        &all_results,
                        &ctx.task_run_id,
                        ctx.pg.as_ref(),
                    )
                    .await;
                let stored = outcomes
                    .iter()
                    .filter(|o| matches!(o, ingestor::IngestOutcome::Stored { .. }))
                    .count();
                let deduped = outcomes
                    .iter()
                    .filter(|o| matches!(o, ingestor::IngestOutcome::Duplicate { .. }))
                    .count();
                let injections = outcomes
                    .iter()
                    .filter(|o| matches!(o, ingestor::IngestOutcome::Stored { injection_flagged: true, .. }))
                    .count();
                self.stats
                    .results_ingested
                    .fetch_add(stored as u64, Ordering::Relaxed);
                self.stats
                    .dedup_skipped
                    .fetch_add(deduped as u64, Ordering::Relaxed);
                self.stats
                    .injection_detections
                    .fetch_add(injections as u64, Ordering::Relaxed);
                if stored > 0 {
                    eprintln!(
                        "[knowledge_acquisition] Stored {stored}/{} results in task_knowledge ({deduped} deduped)",
                        all_results.len()
                    );
                }
            }
        }

        Ok(all_results)
    }

    /// Tier 0: Check local task_knowledge via hybrid_search.
    /// Prefers PG when available, falls back to SQLite.
    /// When hybrid search returns sparse results, supplements from unified memory query.
    async fn search_local(
        &self,
        query: &str,
        budget: &mut SearchBudget,
        ctx: &SearchContext,
    ) -> Result<Vec<KnowledgeResult>, String> {
        use crate::database::embedding_client::EmbeddingClient;
        use crate::database::hybrid_search::HybridSearchConfig;

        let embedding_client = EmbeddingClient::new();
        if !embedding_client.is_available().await {
            // Even without embeddings, try unified memory as fallback
            let supplemental = self.supplement_from_unified_memory(query, &[], ctx).await;
            if !supplemental.is_empty() {
                budget.record_action(0);
            }
            return Ok(supplemental);
        }

        let embedding = embedding_client
            .compute_text_embedding(query)
            .await
            .map_err(|e| format!("Embedding failed: {e}"))?;

        let config = HybridSearchConfig {
            sql_weight: 0.3,
            vector_weight: 0.7,
            limit: 5,
            min_similarity: 0.5,
        };

        // PG is the sole persistence backend
        let results = if let Some(ref pg) = ctx.pg {
            pg.hybrid_search_knowledge(&embedding, None, &config)
                .await
                .map_err(|e| format!("PG hybrid search failed: {e}"))?
        } else {
            return Err("No database backend available for hybrid search".to_string());
        };

        let mut knowledge_results: Vec<KnowledgeResult> = results
            .into_iter()
            .map(|r| KnowledgeResult {
                provider: SearchProvider::LocalKnowledge,
                query: query.to_string(),
                title: format!("Local: {}", r.item.category),
                content: r.item.content,
                url: None,
                relevance_score: Some(r.similarity as f64),
                metadata: KnowledgeMetadata::default(),
            })
            .collect();

        // Supplement from unified memory when hybrid search returns sparse results
        if knowledge_results.len() < 2 {
            let supplemental = self
                .supplement_from_unified_memory(query, &knowledge_results, ctx)
                .await;
            if !supplemental.is_empty() {
                eprintln!(
                    "[knowledge_acquisition] Supplemented {} hybrid results with {} from unified memory",
                    knowledge_results.len(),
                    supplemental.len(),
                );
                knowledge_results.extend(supplemental);

                // Dedup by title (unified memory results have no URL)
                let mut seen_titles = std::collections::HashSet::new();
                knowledge_results.retain(|r| seen_titles.insert(r.title.clone()));
            }
        }

        if knowledge_results.is_empty() {
            return Ok(Vec::new());
        }

        budget.record_action(0);

        Ok(knowledge_results)
    }

    /// Supplement sparse local results with unified memory query (observations,
    /// timeline, findings, fixes, graph nodes, etc.).
    /// Only activates when PG is available; silently returns empty on any error.
    async fn supplement_from_unified_memory(
        &self,
        query: &str,
        existing_results: &[KnowledgeResult],
        ctx: &SearchContext,
    ) -> Vec<KnowledgeResult> {
        use crate::memory::unified_query::{self, UnifiedMemoryQuery};

        let Some(ref pg) = ctx.pg else {
            return Vec::new();
        };

        let params = UnifiedMemoryQuery {
            query: query.to_string(),
            limit: 5,
            sources: None,
            from: None,
            to: None,
            min_score: Some(0.5),
        };

        match unified_query::query_memory(&params, pg, None).await {
            Ok(results) => {
                // Build a set of existing content prefixes to avoid near-duplicates
                let existing_prefixes: std::collections::HashSet<String> = existing_results
                    .iter()
                    .map(|r| r.content.chars().take(100).collect::<String>())
                    .collect();

                results
                    .into_iter()
                    .filter(|r| r.fused_score > 0.5)
                    .filter(|r| {
                        // Skip results whose content overlaps with existing hybrid results
                        let prefix: String = r.snippet.chars().take(100).collect();
                        !existing_prefixes.contains(&prefix)
                    })
                    .map(|r| KnowledgeResult {
                        provider: SearchProvider::LocalKnowledge,
                        query: query.to_string(),
                        title: format!("Memory[{:?}]: {}", r.source, r.title),
                        content: r.snippet,
                        url: None,
                        relevance_score: Some(r.fused_score),
                        metadata: KnowledgeMetadata::default(),
                    })
                    .collect()
            }
            Err(e) => {
                eprintln!(
                    "[knowledge_acquisition] Unified memory supplement failed: {e}"
                );
                Vec::new()
            }
        }
    }

    /// Vulnerability-specific search — queries Sploitus (and logs CVE IDs for OSV)
    pub async fn search_vulnerabilities(
        &self,
        query: &str,
    ) -> Result<Vec<KnowledgeResult>, String> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        self.stats.total_searches.fetch_add(1, Ordering::Relaxed);
        let mut budget = self.config.to_budget();
        let mut all_results: Vec<KnowledgeResult> = Vec::new();

        if budget.can_search() && self.providers.is_available(SearchProvider::Sploitus) {
            match tokio::time::timeout(
                budget.timeout_per_call,
                self.sploitus
                    .search(query, ExploitType::Exploits, SortOrder::Default, 10),
            )
            .await
            {
                Ok(Ok(results)) => {
                    let content_bytes: usize = results.iter().map(|r| r.content.len()).sum();
                    self.stats
                        .sploitus
                        .record_success(results.len() as u64, content_bytes as u64);
                    budget.record_action(content_bytes);
                    all_results.extend(results);
                }
                Ok(Err(e)) => {
                    self.stats.sploitus.record_failure();
                    eprintln!("[knowledge_acquisition] Sploitus search failed: {e}");
                }
                Err(_) => {
                    self.stats.sploitus.record_timeout();
                    eprintln!("[knowledge_acquisition] Sploitus search timed out");
                }
            }
        }

        if budget.can_search() && self.providers.is_available(SearchProvider::OsvDev) {
            let cve_ids = vuln_enrichment::extract_cve_ids(query);
            if !cve_ids.is_empty() {
                eprintln!(
                    "[knowledge_acquisition] CVE IDs detected: {:?} (use audit_dependencies for structured OSV lookups)",
                    cve_ids
                );
            }
        }

        for result in &mut all_results {
            result.truncate_content(self.config.per_result_cap_bytes);
        }

        Ok(all_results)
    }

    /// Fetch content from UI Bridge (for connected apps)
    pub async fn fetch_ui_bridge(
        &self,
        snapshot_type: SnapshotType,
        query: Option<&str>,
    ) -> Result<Vec<KnowledgeResult>, String> {
        match self.ui_bridge.fetch(snapshot_type, query).await {
            Ok(results) => {
                let bytes: usize = results.iter().map(|r| r.content.len()).sum();
                self.stats
                    .ui_bridge
                    .record_success(results.len() as u64, bytes as u64);
                Ok(results)
            }
            Err(e) => {
                self.stats.ui_bridge.record_failure();
                Err(e)
            }
        }
    }

    /// Deep API documentation search via Tavily
    pub async fn lookup_api_docs(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<Vec<KnowledgeResult>, String> {
        let (provider, result) = if let Some(ref tavily) = self.tavily {
            (SearchProvider::Tavily, tavily.search(query, max_results, "advanced").await)
        } else {
            (SearchProvider::DuckDuckGo, self.duckduckgo.search(query, max_results).await)
        };
        match &result {
            Ok(results) => {
                let bytes: usize = results.iter().map(|r| r.content.len()).sum();
                self.stats
                    .provider(provider)
                    .record_success(results.len() as u64, bytes as u64);
            }
            Err(_) => {
                self.stats.provider(provider).record_failure();
            }
        }
        result
    }

    /// Query OSV.dev for a specific package version
    pub async fn query_package_vulnerabilities(
        &self,
        name: &str,
        ecosystem: providers::osv::OsvEcosystem,
        version: &str,
    ) -> Result<Vec<KnowledgeResult>, String> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        match self.osv.query_package(name, ecosystem, version).await {
            Ok(results) => {
                let bytes: usize = results.iter().map(|r| r.content.len()).sum();
                self.stats.osv.record_success(results.len() as u64, bytes as u64);
                Ok(results)
            }
            Err(e) => {
                self.stats.osv.record_failure();
                Err(e)
            }
        }
    }

    /// Batch audit dependencies via OSV.dev
    pub async fn audit_dependencies(
        &self,
        packages: &[(String, providers::osv::OsvEcosystem, String)],
    ) -> Result<Vec<(String, Vec<KnowledgeResult>)>, String> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        match self.osv.query_batch(packages).await {
            Ok(results) => {
                let total_vulns: usize = results.iter().map(|(_, v)| v.len()).sum();
                self.stats.osv.record_success(total_vulns as u64, 0);
                Ok(results)
            }
            Err(e) => {
                self.stats.osv.record_failure();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
use crate::database::CheckpointDb;

    #[test]
    fn test_coordinator_creation() {
        let ka = KnowledgeAcquisition::with_config(KnowledgeAcquisitionConfig::default());
        let providers = ka.available_providers();
        assert!(providers.contains(&SearchProvider::DuckDuckGo));
        assert!(providers.contains(&SearchProvider::Sploitus));
        assert!(providers.contains(&SearchProvider::OsvDev));
    }

    #[test]
    fn test_disabled_config() {
        let config = KnowledgeAcquisitionConfig {
            enabled: false,
            ..Default::default()
        };
        let ka = KnowledgeAcquisition::with_config(config);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt.block_on(ka.search("test", KnowledgeDomain::General, 5));
        assert!(results.unwrap().is_empty());
    }

    #[test]
    fn test_budget_enforcement() {
        let mut budget = SearchBudget::default();
        assert!(budget.can_search());
        for _ in 0..5 {
            budget.record_action(100);
        }
        assert!(!budget.can_search());
    }

    #[test]
    fn test_stats_initialized() {
        let ka = KnowledgeAcquisition::with_config(KnowledgeAcquisitionConfig::default());
        let snap = ka.stats.snapshot();
        assert_eq!(snap.total_searches, 0);
        assert_eq!(snap.cache_hits, 0);
        assert_eq!(snap.providers.duckduckgo.queries, 0);
    }
}
