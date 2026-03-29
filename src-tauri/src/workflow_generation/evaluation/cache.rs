//! Entailment Cache
//!
//! Caches evaluation results for criterion-step pairs to avoid redundant
//! LLM judge calls. Uses an in-memory HashMap with TTL expiration.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use super::ScoringTier;

pub struct EntailmentCache {
    entries: HashMap<u64, CachedEntailment>,
    max_size: usize,
    ttl: Duration,
}

pub struct CachedEntailment {
    pub score: f64,
    pub explanation: String,
    pub cached_at: Instant,
    pub tier: ScoringTier,
}

impl EntailmentCache {
    pub fn new(max_size: usize, ttl_seconds: u64) -> Self {
        Self {
            entries: HashMap::new(),
            max_size,
            ttl: Duration::from_secs(ttl_seconds),
        }
    }

    /// Default cache: 1000 entries, 1 hour TTL.
    pub fn default_cache() -> Self {
        Self::new(1000, 3600)
    }

    /// Compute cache key from criterion text + step text + step type.
    fn cache_key(criterion_text: &str, step_text: &str, step_type: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        criterion_text.hash(&mut hasher);
        step_text.hash(&mut hasher);
        step_type.hash(&mut hasher);
        hasher.finish()
    }

    /// Look up a cached entailment score.
    pub fn get(
        &self,
        criterion_text: &str,
        step_text: &str,
        step_type: &str,
    ) -> Option<&CachedEntailment> {
        let key = Self::cache_key(criterion_text, step_text, step_type);
        self.entries.get(&key).and_then(|entry| {
            if entry.cached_at.elapsed() < self.ttl {
                Some(entry)
            } else {
                None
            }
        })
    }

    /// Store an entailment score in the cache.
    pub fn put(
        &mut self,
        criterion_text: &str,
        step_text: &str,
        step_type: &str,
        score: f64,
        explanation: String,
        tier: ScoringTier,
    ) {
        // Evict expired entries if at capacity
        if self.entries.len() >= self.max_size {
            self.evict_expired();
        }
        // If still at capacity after eviction, remove oldest
        if self.entries.len() >= self.max_size {
            if let Some(&oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| v.cached_at)
                .map(|(k, _)| k)
            {
                self.entries.remove(&oldest_key);
            }
        }

        let key = Self::cache_key(criterion_text, step_text, step_type);
        self.entries.insert(
            key,
            CachedEntailment {
                score,
                explanation,
                cached_at: Instant::now(),
                tier,
            },
        );
    }

    /// Remove expired entries.
    fn evict_expired(&mut self) {
        self.entries.retain(|_, v| v.cached_at.elapsed() < self.ttl);
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let valid = self
            .entries
            .values()
            .filter(|v| v.cached_at.elapsed() < self.ttl)
            .count();
        CacheStats {
            total_entries: self.entries.len(),
            valid_entries: valid,
            max_size: self.max_size,
            ttl_seconds: self.ttl.as_secs(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    pub total_entries: usize,
    pub valid_entries: usize,
    pub max_size: usize,
    pub ttl_seconds: u64,
}
