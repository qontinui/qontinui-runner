use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::knowledge_acquisition::SearchProvider;

/// Atomic counters for a single provider.
#[derive(Debug, Default)]
pub struct ProviderCounters {
    pub queries: AtomicU64,
    pub successes: AtomicU64,
    pub failures: AtomicU64,
    pub timeouts: AtomicU64,
    pub results_returned: AtomicU64,
    pub bytes_fetched: AtomicU64,
}

impl ProviderCounters {
    pub fn record_success(&self, result_count: u64, bytes: u64) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.successes.fetch_add(1, Ordering::Relaxed);
        self.results_returned
            .fetch_add(result_count, Ordering::Relaxed);
        self.bytes_fetched.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_timeout(&self) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ProviderStats {
        let queries = self.queries.load(Ordering::Relaxed);
        let successes = self.successes.load(Ordering::Relaxed);
        ProviderStats {
            queries,
            successes,
            failures: self.failures.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            results_returned: self.results_returned.load(Ordering::Relaxed),
            bytes_fetched: self.bytes_fetched.load(Ordering::Relaxed),
            success_rate: if queries > 0 {
                successes as f64 / queries as f64
            } else {
                0.0
            },
        }
    }
}

/// Counters for the entire knowledge acquisition system.
#[derive(Debug)]
pub struct KnowledgeStats {
    pub local_knowledge: ProviderCounters,
    pub ui_bridge: ProviderCounters,
    pub duckduckgo: ProviderCounters,
    pub tavily: ProviderCounters,
    pub perplexity: ProviderCounters,
    pub sploitus: ProviderCounters,
    pub osv: ProviderCounters,
    /// Total search sessions (calls to search/search_with_context)
    pub total_searches: AtomicU64,
    /// Searches that returned results from Tier 0 (cache hits)
    pub cache_hits: AtomicU64,
    /// Results stored via ingestor
    pub results_ingested: AtomicU64,
    /// Duplicates skipped by ingestor
    pub dedup_skipped: AtomicU64,
    /// Prompt injection detections
    pub injection_detections: AtomicU64,
    /// When the stats were created (process start)
    pub started_at: Instant,
}

impl KnowledgeStats {
    pub fn new() -> Self {
        Self {
            local_knowledge: ProviderCounters::default(),
            ui_bridge: ProviderCounters::default(),
            duckduckgo: ProviderCounters::default(),
            tavily: ProviderCounters::default(),
            perplexity: ProviderCounters::default(),
            sploitus: ProviderCounters::default(),
            osv: ProviderCounters::default(),
            total_searches: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            results_ingested: AtomicU64::new(0),
            dedup_skipped: AtomicU64::new(0),
            injection_detections: AtomicU64::new(0),
            started_at: Instant::now(),
        }
    }

    /// Get counters for a specific provider
    pub fn provider(&self, provider: SearchProvider) -> &ProviderCounters {
        match provider {
            SearchProvider::LocalKnowledge => &self.local_knowledge,
            SearchProvider::UiBridge => &self.ui_bridge,
            SearchProvider::DuckDuckGo => &self.duckduckgo,
            SearchProvider::Tavily => &self.tavily,
            SearchProvider::Perplexity => &self.perplexity,
            SearchProvider::Sploitus => &self.sploitus,
            SearchProvider::OsvDev => &self.osv,
        }
    }

    /// Capture a point-in-time snapshot of all stats
    pub fn snapshot(&self) -> StatsSnapshot {
        let total = self.total_searches.load(Ordering::Relaxed);
        let cache_hits = self.cache_hits.load(Ordering::Relaxed);

        StatsSnapshot {
            uptime_secs: self.started_at.elapsed().as_secs(),
            total_searches: total,
            cache_hits,
            cache_hit_rate: if total > 0 {
                cache_hits as f64 / total as f64
            } else {
                0.0
            },
            results_ingested: self.results_ingested.load(Ordering::Relaxed),
            dedup_skipped: self.dedup_skipped.load(Ordering::Relaxed),
            injection_detections: self.injection_detections.load(Ordering::Relaxed),
            providers: ProviderStatsMap {
                local_knowledge: self.local_knowledge.snapshot(),
                ui_bridge: self.ui_bridge.snapshot(),
                duckduckgo: self.duckduckgo.snapshot(),
                tavily: self.tavily.snapshot(),
                perplexity: self.perplexity.snapshot(),
                sploitus: self.sploitus.snapshot(),
                osv: self.osv.snapshot(),
            },
        }
    }
}

/// Serializable snapshot of all stats at a point in time.
#[derive(Debug, Serialize)]
pub struct StatsSnapshot {
    pub uptime_secs: u64,
    pub total_searches: u64,
    pub cache_hits: u64,
    pub cache_hit_rate: f64,
    pub results_ingested: u64,
    pub dedup_skipped: u64,
    pub injection_detections: u64,
    pub providers: ProviderStatsMap,
}

#[derive(Debug, Serialize)]
pub struct ProviderStatsMap {
    pub local_knowledge: ProviderStats,
    pub ui_bridge: ProviderStats,
    pub duckduckgo: ProviderStats,
    pub tavily: ProviderStats,
    pub perplexity: ProviderStats,
    pub sploitus: ProviderStats,
    pub osv: ProviderStats,
}

/// Stats for a single provider.
#[derive(Debug, Serialize)]
pub struct ProviderStats {
    pub queries: u64,
    pub successes: u64,
    pub failures: u64,
    pub timeouts: u64,
    pub results_returned: u64,
    pub bytes_fetched: u64,
    pub success_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_counters() {
        let c = ProviderCounters::default();
        c.record_success(3, 5000);
        c.record_success(2, 3000);
        c.record_failure();
        c.record_timeout();

        let s = c.snapshot();
        assert_eq!(s.queries, 4);
        assert_eq!(s.successes, 2);
        assert_eq!(s.failures, 1);
        assert_eq!(s.timeouts, 1);
        assert_eq!(s.results_returned, 5);
        assert_eq!(s.bytes_fetched, 8000);
        assert!((s.success_rate - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_stats_snapshot() {
        let stats = KnowledgeStats::new();
        stats.total_searches.fetch_add(10, Ordering::Relaxed);
        stats.cache_hits.fetch_add(3, Ordering::Relaxed);
        stats.duckduckgo.record_success(5, 10000);
        stats.results_ingested.fetch_add(4, Ordering::Relaxed);
        stats.dedup_skipped.fetch_add(1, Ordering::Relaxed);

        let snap = stats.snapshot();
        assert_eq!(snap.total_searches, 10);
        assert_eq!(snap.cache_hits, 3);
        assert!((snap.cache_hit_rate - 0.3).abs() < 0.001);
        assert_eq!(snap.results_ingested, 4);
        assert_eq!(snap.providers.duckduckgo.queries, 1);
        assert_eq!(snap.providers.duckduckgo.results_returned, 5);
    }
}
