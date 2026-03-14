//! Step-Level Caching
//!
//! Caches the output of deterministic setup/verification steps keyed by step
//! config hash. When iterating in the supervisor's pipeline loop, steps whose
//! inputs haven't changed can be skipped by returning the cached result.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Cached result of a step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedStepResult {
    /// Hash of the step configuration that produced this result.
    pub config_hash: String,
    /// The step's output/result data.
    pub output: serde_json::Value,
    /// Whether the step succeeded.
    pub success: bool,
    /// Exit code if applicable.
    pub exit_code: Option<i32>,
    /// When this was cached (epoch millis).
    pub cached_at: u64,
    /// How long the original execution took.
    pub original_duration_ms: u64,
}

/// Thread-safe in-memory step cache.
#[derive(Debug, Clone)]
pub struct StepCache {
    entries: Arc<RwLock<HashMap<String, CachedStepResult>>>,
    /// Maximum age before cache entries expire.
    max_age: Duration,
    /// Maximum number of entries.
    max_entries: usize,
}

impl StepCache {
    pub fn new(max_age_secs: u64, max_entries: usize) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_age: Duration::from_secs(max_age_secs),
            max_entries,
        }
    }

    /// Default cache: 30 minute TTL, 500 entries max.
    pub fn default() -> Self {
        Self::new(1800, 500)
    }

    /// Compute a cache key from a step's configuration.
    ///
    /// Only deterministic steps are cacheable (command type, not prompt type).
    pub fn compute_cache_key(step: &serde_json::Value) -> Option<String> {
        // Only cache deterministic step types
        let step_type = step.get("type").and_then(|v| v.as_str())?;
        if step_type == "prompt" {
            return None; // Non-deterministic, never cache
        }

        // Build a hash from the step's functional configuration.
        // Include: type, command, check_type, url, expected values.
        // Exclude: id, name (cosmetic), retry config (behavioral).
        use std::collections::BTreeMap;
        let mut hash_input = BTreeMap::new();

        for key in &[
            "type",
            "command",
            "check_type",
            "url",
            "method",
            "expected_status",
            "expected_output",
            "health_check_url",
            "working_directory",
        ] {
            if let Some(val) = step.get(*key) {
                hash_input.insert(key.to_string(), val.to_string());
            }
        }

        let hash_str = format!("{:?}", hash_input);
        // Simple hash using the standard library
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        hash_str.hash(&mut hasher);
        Some(format!("{:016x}", hasher.finish()))
    }

    /// Look up a cached result.
    pub fn get(&self, cache_key: &str) -> Option<CachedStepResult> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(cache_key)?;

        // Check TTL
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let age = Duration::from_millis(now - entry.cached_at);

        if age > self.max_age {
            return None; // Expired
        }

        Some(entry.clone())
    }

    /// Store a result in the cache.
    pub fn put(&self, cache_key: String, result: CachedStepResult) {
        if let Ok(mut entries) = self.entries.write() {
            // Evict oldest if at capacity
            if entries.len() >= self.max_entries {
                if let Some(oldest_key) = entries
                    .iter()
                    .min_by_key(|(_, v)| v.cached_at)
                    .map(|(k, _)| k.clone())
                {
                    entries.remove(&oldest_key);
                }
            }
            entries.insert(cache_key, result);
        }
    }

    /// Invalidate all cache entries.
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let entries = self.entries.read().map(|e| e.len()).unwrap_or(0);
        CacheStats {
            total_entries: entries,
            max_entries: self.max_entries,
            max_age_secs: self.max_age.as_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_entries: usize,
    pub max_entries: usize,
    pub max_age_secs: u64,
}
