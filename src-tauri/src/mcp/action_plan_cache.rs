//! Action Plan Cache
//!
//! Caches successful UI Bridge action plans keyed by page URL and element
//! fingerprint. When the same page with the same elements is encountered
//! again, the cached plan can be reused without an expensive LLM planning
//! call.
//!
//! Inspired by Skyvern's action caching with personalization: the cache
//! stores the structural plan (what actions to take on which elements)
//! separately from user-specific data (which values to enter). The
//! `user_detail_query`/`user_detail_answer` fields in `PlannedAction`
//! enable this separation.
//!
//! Cache keys are built from:
//!   1. Page URL (normalized: scheme + host + path, no query/fragment)
//!   2. Sorted SHA-256 hash of element attributes visible to the plan
//!
//! Cache entries expire after a configurable TTL (default: 1 hour).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Maximum number of cached plans (LRU eviction when exceeded).
const MAX_CACHE_ENTRIES: usize = 100;

/// Default time-to-live for cache entries.
const DEFAULT_TTL_SECS: u64 = 3600; // 1 hour

/// A cached action plan entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedActionPlan {
    /// The original action plan JSON (actions array with reasoning, targets, etc.)
    pub plan: serde_json::Value,
    /// The goal this plan achieves
    pub goal: Option<String>,
    /// Number of times this cache entry has been used
    pub hit_count: u32,
    /// Whether the last execution succeeded
    pub last_success: bool,
}

/// Internal cache entry with timing metadata.
struct CacheEntry {
    plan: CachedActionPlan,
    created_at: Instant,
    last_accessed: Instant,
    ttl: Duration,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.ttl
    }
}

/// Cache key: page URL + element fingerprint.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    /// Normalized page URL (scheme + host + path)
    url: String,
    /// Sorted concatenation of element hashes
    element_fingerprint: String,
}

/// Thread-safe action plan cache.
pub struct ActionPlanCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    ttl: Duration,
}

impl ActionPlanCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(DEFAULT_TTL_SECS),
        }
    }

    /// Build a cache key from a page URL and element snapshot.
    ///
    /// Elements are hashed by their stable attributes: id, type, role, label.
    /// The hashes are sorted to make the fingerprint order-independent.
    pub fn build_key(url: &str, elements: &serde_json::Value) -> Option<(String, String)> {
        // Normalize URL: strip query string and fragment
        let normalized_url = normalize_url(url);

        // Build element fingerprint from the snapshot
        let fingerprint = build_element_fingerprint(elements)?;

        Some((normalized_url, fingerprint))
    }

    /// Look up a cached action plan.
    pub fn get(&self, url: &str, element_fingerprint: &str) -> Option<CachedActionPlan> {
        let key = CacheKey {
            url: url.to_string(),
            element_fingerprint: element_fingerprint.to_string(),
        };

        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(entry) = entries.get_mut(&key) {
            if entry.is_expired() {
                entries.remove(&key);
                debug!("ACTION-PLAN-CACHE: expired entry for {}", url);
                return None;
            }
            entry.last_accessed = Instant::now();
            entry.plan.hit_count += 1;
            info!(
                "ACTION-PLAN-CACHE: hit for {} (hits: {})",
                url, entry.plan.hit_count
            );
            return Some(entry.plan.clone());
        }

        None
    }

    /// Store a successful action plan in the cache.
    pub fn put(
        &self,
        url: &str,
        element_fingerprint: &str,
        plan: serde_json::Value,
        goal: Option<String>,
    ) {
        let key = CacheKey {
            url: url.to_string(),
            element_fingerprint: element_fingerprint.to_string(),
        };

        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

        // Evict expired entries
        entries.retain(|_, v| !v.is_expired());

        // LRU eviction if at capacity
        if entries.len() >= MAX_CACHE_ENTRIES {
            // Find the least recently accessed entry
            if let Some(oldest_key) = entries
                .iter()
                .min_by_key(|(_, v)| v.last_accessed)
                .map(|(k, _)| k.clone())
            {
                entries.remove(&oldest_key);
            }
        }

        let now = Instant::now();
        entries.insert(
            key,
            CacheEntry {
                plan: CachedActionPlan {
                    plan,
                    goal,
                    hit_count: 0,
                    last_success: true,
                },
                created_at: now,
                last_accessed: now,
                ttl: self.ttl,
            },
        );

        info!(
            "ACTION-PLAN-CACHE: stored plan for {} ({} entries)",
            url,
            entries.len()
        );
    }

    /// Mark a cached plan as failed (reduces trust for future lookups).
    pub fn mark_failed(&self, url: &str, element_fingerprint: &str) {
        let key = CacheKey {
            url: url.to_string(),
            element_fingerprint: element_fingerprint.to_string(),
        };

        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.remove(&key).is_some() {
            info!("ACTION-PLAN-CACHE: removed failed plan for {}", url);
        }
    }

    /// Get cache statistics.
    pub fn stats(&self) -> serde_json::Value {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let total = entries.len();
        let total_hits: u32 = entries.values().map(|e| e.plan.hit_count).sum();
        serde_json::json!({
            "totalEntries": total,
            "totalHits": total_hits,
            "maxEntries": MAX_CACHE_ENTRIES,
            "ttlSeconds": DEFAULT_TTL_SECS,
        })
    }
}

/// Normalize a URL by stripping query string and fragment.
fn normalize_url(url: &str) -> String {
    // Find the earliest '?' or '#' and truncate
    let end = [url.find('?'), url.find('#')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(url.len());
    url[..end].to_string()
}

/// Build an order-independent fingerprint from element attributes.
///
/// Hashes each element's (id, type, role, label) tuple, sorts the hashes,
/// and concatenates them. This produces a stable fingerprint even if
/// elements appear in different order across snapshots.
fn build_element_fingerprint(elements: &serde_json::Value) -> Option<String> {
    let arr = elements.as_array()?;
    if arr.is_empty() {
        return None;
    }

    let mut hashes: Vec<String> = arr
        .iter()
        .filter_map(|el| {
            let id = el.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let el_type = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let role = el.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let label = el.get("label").and_then(|v| v.as_str()).unwrap_or("");

            // Skip elements without meaningful identifiers
            if id.is_empty() && label.is_empty() {
                return None;
            }

            // Simple hash: concatenate fields with separator
            // Using a lightweight hash since we don't need cryptographic strength
            let input = format!("{}|{}|{}|{}", id, el_type, role, label);
            let hash = simple_hash(&input);
            Some(hash)
        })
        .collect();

    if hashes.is_empty() {
        return None;
    }
    hashes.sort();
    Some(hashes.join(","))
}

/// Simple non-cryptographic hash for element fingerprinting.
/// Uses FNV-1a for speed (no need for crypto-grade hashing here).
fn simple_hash(input: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV prime
    }
    format!("{:016x}", hash)
}

/// Global action plan cache instance.
static ACTION_PLAN_CACHE: std::sync::LazyLock<ActionPlanCache> =
    std::sync::LazyLock::new(ActionPlanCache::new);

/// Get the global action plan cache.
pub fn global_action_plan_cache() -> &'static ActionPlanCache {
    &ACTION_PLAN_CACHE
}
