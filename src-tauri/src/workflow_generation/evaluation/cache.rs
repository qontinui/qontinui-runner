//! Entailment Cache
//!
//! Caches evaluation results for criterion-step pairs to avoid redundant
//! LLM judge calls. Uses an in-memory HashMap with TTL expiration, backed
//! by PostgreSQL as a write-through persistence layer so entries survive
//! restarts. PG is optional — the cache degrades gracefully to in-memory
//! only when PG is unavailable.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tracing::debug;

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

    /// Hash criterion text alone (for PG criterion_hash column).
    pub fn criterion_hash(criterion_text: &str) -> i64 {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        criterion_text.hash(&mut hasher);
        hasher.finish() as i64
    }

    /// Hash step text + step type (for PG step_hash column).
    pub fn step_hash(step_text: &str, step_type: &str) -> i64 {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        step_text.hash(&mut hasher);
        step_type.hash(&mut hasher);
        hasher.finish() as i64
    }

    /// Look up a cached entailment score (in-memory only).
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

    /// Look up in-memory first, then fall back to PG (sync version).
    /// If found in PG, promotes the entry into the in-memory cache.
    /// Returns owned data because we need to bridge the sync/async boundary.
    ///
    /// This is the primary read path for the write-through cache.
    /// Safe to call from sync contexts — spawns a blocking thread for PG.
    pub fn get_with_pg(
        &mut self,
        criterion_text: &str,
        step_text: &str,
        step_type: &str,
    ) -> Option<CachedEntailmentOwned> {
        // 1. Check in-memory
        let key = Self::cache_key(criterion_text, step_text, step_type);
        if let Some(entry) = self.entries.get(&key) {
            if entry.cached_at.elapsed() < self.ttl {
                return Some(CachedEntailmentOwned {
                    score: entry.score,
                    explanation: entry.explanation.clone(),
                    tier: entry.tier,
                });
            }
        }

        // 2. Check PG (bridge sync → async via tokio Handle)
        let pg_db = crate::database::pg::PgDb::try_global()?;
        let ch = Self::criterion_hash(criterion_text);
        let sh = Self::step_hash(step_text, step_type);

        let handle = tokio::runtime::Handle::try_current().ok()?;
        let pg_db_clone = pg_db.clone();
        let pg_result =
            std::thread::spawn(move || handle.block_on(pg_db_clone.get_entailment_cache(ch, sh)))
                .join()
                .ok()
                .flatten()?;

        let tier = pg_tier_to_scoring_tier(pg_result.tier.as_deref());

        // 3. Promote into in-memory cache
        let explanation = pg_result.explanation.unwrap_or_default();
        self.insert_in_memory(key, pg_result.score, explanation.clone(), tier);

        debug!(
            "Entailment cache PG hit: criterion_hash={}, step_hash={}",
            ch, sh
        );

        Some(CachedEntailmentOwned {
            score: pg_result.score,
            explanation,
            tier,
        })
    }

    /// Store an entailment score in both in-memory and PG caches.
    pub fn put(
        &mut self,
        criterion_text: &str,
        step_text: &str,
        step_type: &str,
        score: f64,
        explanation: String,
        tier: ScoringTier,
    ) {
        let key = Self::cache_key(criterion_text, step_text, step_type);
        self.insert_in_memory(key, score, explanation.clone(), tier);

        // Write-through to PG (fire-and-forget on a spawned task)
        let ch = Self::criterion_hash(criterion_text);
        let sh = Self::step_hash(step_text, step_type);
        let tier_str = scoring_tier_to_pg(tier).map(|s| s.to_string());
        let explanation_clone = Some(explanation);

        if let Some(pg_db) = crate::database::pg::PgDb::try_global() {
            tokio::spawn(async move {
                pg_db
                    .set_entailment_cache(
                        ch,
                        sh,
                        score,
                        explanation_clone.as_deref(),
                        tier_str.as_deref(),
                    )
                    .await;
            });
        }
    }

    /// Internal: insert into in-memory map with eviction logic.
    fn insert_in_memory(&mut self, key: u64, score: f64, explanation: String, tier: ScoringTier) {
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

/// Owned version of CachedEntailment returned from async PG lookups
/// (cannot return a borrow across an await point).
pub struct CachedEntailmentOwned {
    pub score: f64,
    pub explanation: String,
    pub tier: ScoringTier,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    pub total_entries: usize,
    pub valid_entries: usize,
    pub max_size: usize,
    pub ttl_seconds: u64,
}

// ============================================================================
// Tier conversion helpers
// ============================================================================

fn scoring_tier_to_pg(tier: ScoringTier) -> Option<&'static str> {
    Some(match tier {
        ScoringTier::Deterministic => "deterministic",
        ScoringTier::Judge => "judge",
        ScoringTier::Prm => "prm",
    })
}

fn pg_tier_to_scoring_tier(tier: Option<&str>) -> ScoringTier {
    match tier {
        Some("deterministic") => ScoringTier::Deterministic,
        Some("judge") => ScoringTier::Judge,
        Some("prm") => ScoringTier::Prm,
        _ => ScoringTier::Judge, // default fallback
    }
}
