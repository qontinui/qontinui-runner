//! Cache-Aware Request Builder for Anthropic Messages API
//!
//! Produces structured request bodies with `cache_control` breakpoints on stable
//! content blocks. Cached blocks are served at 10% of base input token price on
//! subsequent reads, yielding 25-40% cost savings on multi-iteration workflows
//! where system prompts, schema context, and generation rules remain unchanged.
//!
//! # Anthropic Prompt Caching Protocol
//!
//! - Header: `anthropic-beta: prompt-caching-2024-07-31`
//! - Place `"cache_control": {"type": "ephemeral"}` on content blocks in the
//!   `system` array or in `messages[].content[]`.
//! - Minimum block size depends on the model — see [`min_cacheable_chars`].
//!   Haiku 4.5: 4096 tokens. Haiku 3.5: 2048 tokens. Sonnet/Opus: 1024 tokens.
//!   Sub-threshold blocks are silently NOT cached (Anthropic returns 0 cache
//!   reads with no error). Source:
//!   <https://docs.claude.com/en/docs/build-with-claude/prompt-caching>
//! - Cache TTL: 5 minutes default; 1-hour ephemeral tier available at 2x cost.
//! - Pricing: write = 1.25x base input; read = 0.1x base input.

use serde::Serialize;
use tracing::debug;

// =============================================================================
// StructuredPrompt — the high-level prompt representation
// =============================================================================

/// A prompt separated into stable (cacheable) and dynamic blocks.
///
/// Produced by the orchestrator's prompt assembly functions.
/// Consumed by the routing layer to build cache-aware API requests.
#[derive(Debug, Clone)]
pub struct StructuredPrompt {
    /// System blocks that are stable across iterations within a run.
    /// These get `cache_control: {"type": "ephemeral"}` markers.
    /// Order: orchestrator instructions, schema context, generation rules,
    /// verification plan, skills context.
    pub cached_system_blocks: Vec<String>,

    /// System blocks that change per iteration.
    /// These are sent without cache_control markers.
    /// Contains: cross-iteration context, error monitor, memory, feedback.
    pub dynamic_system_blocks: Vec<String>,

    /// The user message content (the task prompt itself).
    pub user_message: String,
}

impl StructuredPrompt {
    /// Create a StructuredPrompt with no cached blocks (backward compat).
    pub fn uncached(prompt: String) -> Self {
        Self {
            cached_system_blocks: Vec::new(),
            dynamic_system_blocks: Vec::new(),
            user_message: prompt,
        }
    }

    /// Flatten to a single string (for CLI providers that don't support caching).
    pub fn to_flat_string(&self) -> String {
        let mut parts = Vec::new();
        for block in &self.cached_system_blocks {
            parts.push(block.as_str());
        }
        for block in &self.dynamic_system_blocks {
            parts.push(block.as_str());
        }
        parts.push(&self.user_message);
        parts.join("\n\n")
    }

    /// Estimate total character count across all blocks.
    pub fn total_chars(&self) -> usize {
        self.cached_system_blocks
            .iter()
            .map(|b| b.len())
            .sum::<usize>()
            + self
                .dynamic_system_blocks
                .iter()
                .map(|b| b.len())
                .sum::<usize>()
            + self.user_message.len()
    }

    /// Check if there are any cacheable blocks.
    pub fn has_cached_blocks(&self) -> bool {
        !self.cached_system_blocks.is_empty()
    }
}

// =============================================================================
// CacheAwareRequestBuilder — constructs the API JSON body
// =============================================================================

/// A content block in the system prompt array, with optional cache control.
#[derive(Debug, Clone, Serialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    control_type: String,
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self {
            control_type: "ephemeral".to_string(),
        }
    }
}

/// Builds a Claude Messages API request body with cache_control breakpoints.
pub struct CacheAwareRequestBuilder {
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    system_blocks: Vec<SystemBlock>,
}

/// Minimum cacheable block size in characters, per Claude model family.
///
/// Anthropic silently returns 0 cache reads for blocks smaller than the
/// model's documented token minimum; the `cache_control` marker is parsed
/// and discarded. We work in characters (not tokens) because the builder
/// operates on `String::len()`. Threshold = documented_token_min × 4
/// chars-per-token (English/code average; Claude's tokenizer trends ~3.5
/// chars/token, so a block at the char threshold is typically ≥ the token
/// threshold).
///
/// - Haiku 4.x (e.g. `claude-haiku-4-5-*`): 4096 tokens → 16384 chars
/// - Haiku 3.x (e.g. `claude-haiku-3-5-*`): 2048 tokens → 8192 chars
/// - Sonnet / Opus / unknown: 1024 tokens → 4096 chars
pub(super) fn min_cacheable_chars(model: &str) -> usize {
    let m = model.to_lowercase();
    if m.contains("haiku-4") {
        16384
    } else if m.contains("haiku") {
        8192
    } else {
        4096
    }
}

impl CacheAwareRequestBuilder {
    /// Create a new builder for the given model.
    pub fn new(model: &str, max_tokens: u32) -> Self {
        Self {
            model: model.to_string(),
            max_tokens,
            temperature: None,
            system_blocks: Vec::new(),
        }
    }

    /// Set temperature override.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// Add a system block that should be cached (stable across iterations).
    ///
    /// Only applies `cache_control` if the block is large enough to benefit.
    /// Small blocks are still added to the system prompt but without the marker.
    pub fn add_cached_system_block(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let should_cache = text.len() >= min_cacheable_chars(&self.model);
        self.system_blocks.push(SystemBlock {
            block_type: "text".to_string(),
            text,
            cache_control: if should_cache {
                Some(CacheControl::ephemeral())
            } else {
                None
            },
        });
    }

    /// Add a system block that changes per-call (dynamic context).
    pub fn add_dynamic_system_block(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.system_blocks.push(SystemBlock {
            block_type: "text".to_string(),
            text,
            cache_control: None,
        });
    }

    /// Build from a StructuredPrompt.
    pub fn from_structured_prompt(prompt: &StructuredPrompt, model: &str, max_tokens: u32) -> Self {
        let mut builder = Self::new(model, max_tokens);

        // Merge small cached blocks together for more efficient caching.
        // Per-model thresholds: Sonnet/Opus 1024 tokens, Haiku 3.5 2048,
        // Haiku 4.5 4096 — see `min_cacheable_chars`.
        let merged_cached = merge_small_blocks(&prompt.cached_system_blocks, model);
        for block in merged_cached {
            builder.add_cached_system_block(block);
        }

        for block in &prompt.dynamic_system_blocks {
            builder.add_dynamic_system_block(block.clone());
        }

        builder
    }

    /// Build the final JSON request body.
    ///
    /// Produces:
    /// ```json
    /// {
    ///   "model": "...",
    ///   "max_tokens": 4096,
    ///   "system": [
    ///     {"type":"text", "text":"...", "cache_control":{"type":"ephemeral"}},
    ///     {"type":"text", "text":"..."}
    ///   ],
    ///   "messages": [{"role":"user", "content":"..."}]
    /// }
    /// ```
    pub fn build(&self, user_message: &str) -> serde_json::Value {
        let cached_count = self
            .system_blocks
            .iter()
            .filter(|b| b.cache_control.is_some())
            .count();
        let total_cached_chars: usize = self
            .system_blocks
            .iter()
            .filter(|b| b.cache_control.is_some())
            .map(|b| b.text.len())
            .sum();

        debug!(
            "CacheAwareRequestBuilder: {} system blocks ({} cached, ~{} cached chars), user message {} chars",
            self.system_blocks.len(),
            cached_count,
            total_cached_chars,
            user_message.len(),
        );

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [{
                "role": "user",
                "content": user_message
            }]
        });

        if !self.system_blocks.is_empty() {
            body["system"] =
                serde_json::to_value(&self.system_blocks).unwrap_or_else(|_| serde_json::json!([]));
        }

        if let Some(temp) = self.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        body
    }

    /// Build for a multimodal user message (content blocks array).
    pub fn build_multimodal(&self, user_content: &serde_json::Value) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [{
                "role": "user",
                "content": user_content
            }]
        });

        if !self.system_blocks.is_empty() {
            body["system"] =
                serde_json::to_value(&self.system_blocks).unwrap_or_else(|_| serde_json::json!([]));
        }

        if let Some(temp) = self.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        body
    }
}

/// Merge small blocks together so they meet the minimum cacheable size.
///
/// Adjacent blocks that are individually too small to cache are concatenated
/// with `\n\n---\n\n` separators until the merged result is large enough.
/// The threshold is per-model — see [`min_cacheable_chars`].
fn merge_small_blocks(blocks: &[String], model: &str) -> Vec<String> {
    if blocks.is_empty() {
        return Vec::new();
    }

    let threshold = min_cacheable_chars(model);
    let mut result = Vec::new();
    let mut accumulator = String::new();

    let flush_accumulator = |accumulator: &mut String, result: &mut Vec<String>| {
        if !accumulator.is_empty() {
            result.push(std::mem::take(accumulator));
        }
    };

    for block in blocks {
        if block.is_empty() {
            continue;
        }

        // Blocks already large enough to cache on their own are passed
        // through unchanged — merging them with unrelated small blocks
        // would invalidate cache keys whenever the small blocks change.
        if block.len() >= threshold {
            flush_accumulator(&mut accumulator, &mut result);
            result.push(block.clone());
            continue;
        }

        if accumulator.is_empty() {
            accumulator = block.clone();
        } else {
            accumulator.push_str("\n\n---\n\n");
            accumulator.push_str(block);
        }

        // Flush once the merged small blocks cross the cache threshold.
        if accumulator.len() >= threshold {
            flush_accumulator(&mut accumulator, &mut result);
        }
    }

    flush_accumulator(&mut accumulator, &mut result);

    result
}

// =============================================================================
// Helpers
// =============================================================================

/// The anthropic-beta header value required for prompt caching.
pub const PROMPT_CACHING_BETA_HEADER: &str = "prompt-caching-2024-07-31";

/// Parse cache token counts from an Anthropic API response body.
///
/// Returns (cache_creation_input_tokens, cache_read_input_tokens).
pub fn parse_cache_tokens(json: &serde_json::Value) -> (u64, u64) {
    let creation = json["usage"]["cache_creation_input_tokens"]
        .as_u64()
        .unwrap_or(0);
    let read = json["usage"]["cache_read_input_tokens"]
        .as_u64()
        .unwrap_or(0);
    (creation, read)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_structured_prompt_flat_string() {
        let prompt = StructuredPrompt {
            cached_system_blocks: vec!["System instructions".to_string()],
            dynamic_system_blocks: vec!["Iteration context".to_string()],
            user_message: "Fix the bug".to_string(),
        };
        let flat = prompt.to_flat_string();
        assert!(flat.contains("System instructions"));
        assert!(flat.contains("Iteration context"));
        assert!(flat.contains("Fix the bug"));
    }

    #[test]
    fn test_structured_prompt_uncached() {
        let prompt = StructuredPrompt::uncached("Simple prompt".to_string());
        assert!(!prompt.has_cached_blocks());
        assert_eq!(prompt.to_flat_string(), "Simple prompt");
    }

    #[test]
    fn test_builder_basic() {
        let mut builder = CacheAwareRequestBuilder::new("claude-sonnet-4-20250514", 4096);
        // 5000 chars passes Sonnet's 4096-char threshold.
        builder.add_cached_system_block("x".repeat(5000));
        builder.add_dynamic_system_block("dynamic context".to_string());
        let body = builder.build("Hello");

        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["max_tokens"], 4096);

        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);

        // First block should have cache_control
        assert!(system[0]["cache_control"].is_object());
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");

        // Second block should not
        assert!(system[1]["cache_control"].is_null());

        // User message
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Hello");
    }

    #[test]
    fn test_builder_small_block_no_cache() {
        let mut builder = CacheAwareRequestBuilder::new("claude-sonnet-4-20250514", 4096);
        builder.add_cached_system_block("tiny".to_string());
        let body = builder.build("Hello");

        let system = body["system"].as_array().unwrap();
        // Small block should NOT have cache_control
        assert!(system[0]["cache_control"].is_null());
    }

    #[test]
    fn test_builder_empty_blocks_skipped() {
        let mut builder = CacheAwareRequestBuilder::new("claude-sonnet-4-20250514", 4096);
        builder.add_cached_system_block(String::new());
        builder.add_dynamic_system_block(String::new());
        let body = builder.build("Hello");

        // No system field when all blocks are empty
        assert!(body.get("system").is_none());
    }

    #[test]
    fn test_merge_small_blocks() {
        let blocks = vec!["small1".to_string(), "small2".to_string(), "x".repeat(5000)];
        let merged = merge_small_blocks(&blocks, "claude-sonnet-4-20250514");
        // First two should be merged, third stands alone (5000 ≥ Sonnet's 4096-char threshold).
        assert_eq!(merged.len(), 2);
        assert!(merged[0].contains("small1"));
        assert!(merged[0].contains("small2"));
        assert_eq!(merged[1].len(), 5000);
    }

    #[test]
    fn test_parse_cache_tokens() {
        let json = serde_json::json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 8000,
                "cache_read_input_tokens": 0
            }
        });
        let (creation, read) = parse_cache_tokens(&json);
        assert_eq!(creation, 8000);
        assert_eq!(read, 0);
    }

    #[test]
    fn test_parse_cache_tokens_missing() {
        let json = serde_json::json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        });
        let (creation, read) = parse_cache_tokens(&json);
        assert_eq!(creation, 0);
        assert_eq!(read, 0);
    }

    #[test]
    fn test_from_structured_prompt() {
        let prompt = StructuredPrompt {
            cached_system_blocks: vec!["x".repeat(5000), "y".repeat(5000)],
            dynamic_system_blocks: vec!["dynamic".to_string()],
            user_message: "task".to_string(),
        };
        let builder = CacheAwareRequestBuilder::from_structured_prompt(
            &prompt,
            "claude-sonnet-4-20250514",
            4096,
        );
        let body = builder.build(&prompt.user_message);

        let system = body["system"].as_array().unwrap();
        // Two large cached blocks + one dynamic block
        assert_eq!(system.len(), 3);
        assert!(system[0]["cache_control"].is_object());
        assert!(system[1]["cache_control"].is_object());
        assert!(system[2]["cache_control"].is_null());
    }

    #[test]
    fn test_builder_haiku_4_higher_threshold() {
        // 5000 chars passes Sonnet's 4096-char threshold but NOT Haiku 4.5's 16384.
        let mut b = CacheAwareRequestBuilder::new("claude-haiku-4-5-20251001", 4096);
        b.add_cached_system_block("x".repeat(5000));
        let body = b.build("Hello");
        assert!(body["system"][0]["cache_control"].is_null());

        let mut big = CacheAwareRequestBuilder::new("claude-haiku-4-5-20251001", 4096);
        big.add_cached_system_block("y".repeat(20000));
        assert!(big.build("Hello")["system"][0]["cache_control"].is_object());
    }

    #[test]
    fn test_min_cacheable_chars_per_model() {
        assert_eq!(min_cacheable_chars("claude-sonnet-4-20250514"), 4096);
        assert_eq!(min_cacheable_chars("claude-opus-4-5-20251101"), 4096);
        assert_eq!(min_cacheable_chars("claude-haiku-3-5-20241022"), 8192);
        assert_eq!(min_cacheable_chars("claude-haiku-4-5-20251001"), 16384);
        assert_eq!(min_cacheable_chars("Claude-Haiku-4-5"), 16384);
        assert_eq!(min_cacheable_chars("some-other-model"), 4096);
    }

    #[test]
    fn test_temperature_override() {
        let builder =
            CacheAwareRequestBuilder::new("claude-sonnet-4-20250514", 4096).temperature(0.5);
        let body = builder.build("Hello");
        assert_eq!(body["temperature"], 0.5);
    }
}
