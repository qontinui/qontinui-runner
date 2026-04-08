//! Memory Compression for the task orchestrator.
//!
//! Implements lazy context compression to prevent context overflow while
//! preserving important historical information. Inspired by agenticSeek's
//! memory compression pattern.
//!
//! Key features:
//! - Token estimation for context size awareness
//! - Lazy compression when threshold exceeded
//! - Summarization of old knowledge entries
//! - Preservation of recent context

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::database::pg::PgDb;
use crate::database::StoredTaskKnowledge;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for memory compression behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    /// Whether compression is enabled
    #[serde(default = "default_compression_enabled")]
    pub enabled: bool,

    /// Token threshold that triggers compression
    #[serde(default = "default_threshold_tokens")]
    pub threshold_tokens: usize,

    /// Target token count after compression
    #[serde(default = "default_target_tokens")]
    pub target_tokens: usize,

    /// Number of recent items to always keep (not compressed)
    #[serde(default = "default_keep_recent_items")]
    pub keep_recent_items: usize,

    /// Number of items to summarize together in a batch
    #[serde(default = "default_summarize_batch_size")]
    pub summarize_batch_size: usize,

    /// Average tokens per character (conservative estimate)
    #[serde(default = "default_tokens_per_char")]
    pub tokens_per_char: f32,
}

fn default_compression_enabled() -> bool {
    true
}

fn default_threshold_tokens() -> usize {
    80000 // ~80k tokens before compression
}

fn default_target_tokens() -> usize {
    60000 // Target ~60k tokens after compression
}

fn default_keep_recent_items() -> usize {
    5 // Keep 5 most recent items of each category
}

fn default_summarize_batch_size() -> usize {
    10 // Summarize 10 items at a time
}

fn default_tokens_per_char() -> f32 {
    0.25 // Conservative: ~4 chars per token
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: default_compression_enabled(),
            threshold_tokens: default_threshold_tokens(),
            target_tokens: default_target_tokens(),
            keep_recent_items: default_keep_recent_items(),
            summarize_batch_size: default_summarize_batch_size(),
            tokens_per_char: default_tokens_per_char(),
        }
    }
}

// ============================================================================
// Token Counting
// ============================================================================

/// Result of token estimation for a task's context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCount {
    /// Total estimated tokens across all categories
    pub total: usize,
    /// Tokens from findings
    pub findings: usize,
    /// Tokens from observations
    pub observations: usize,
    /// Tokens from verification feedback
    pub feedback: usize,
    /// Tokens from solutions
    pub solutions: usize,
    /// Tokens from environment/infrastructure issues
    pub environment: usize,
    /// Tokens from other categories
    pub other: usize,
    /// Number of knowledge entries counted
    pub entry_count: usize,
}

impl TokenCount {
    /// Create an empty token count.
    pub fn empty() -> Self {
        Self {
            total: 0,
            findings: 0,
            observations: 0,
            feedback: 0,
            solutions: 0,
            environment: 0,
            other: 0,
            entry_count: 0,
        }
    }

    /// Check if the token count exceeds a threshold.
    pub fn exceeds_threshold(&self, threshold: usize) -> bool {
        self.total > threshold
    }
}

/// Estimate token count from text using character-based heuristic.
///
/// This is a conservative estimate. For more accurate counting,
/// integrate with tiktoken or a similar library.
pub fn estimate_tokens(text: &str, tokens_per_char: f32) -> usize {
    (text.len() as f32 * tokens_per_char).ceil() as usize
}

/// Estimate token count for a knowledge entry.
pub fn estimate_entry_tokens(entry: &StoredTaskKnowledge, tokens_per_char: f32) -> usize {
    let mut total = estimate_tokens(&entry.content, tokens_per_char);

    if let Some(evidence) = &entry.evidence {
        total += estimate_tokens(evidence, tokens_per_char);
    }

    // Add some overhead for formatting
    total += 20;

    total
}

// ============================================================================
// Compression Results
// ============================================================================

/// Result of a compression operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionResult {
    /// Token count before compression
    pub original_tokens: usize,
    /// Token count after compression
    pub compressed_tokens: usize,
    /// Number of items that were summarized
    pub items_summarized: usize,
    /// Number of summary entries created
    pub summary_entries_created: usize,
    /// Categories that were compressed
    pub compressed_categories: Vec<String>,
}

impl CompressionResult {
    /// Calculate the compression ratio achieved.
    pub fn compression_ratio(&self) -> f32 {
        if self.original_tokens == 0 {
            return 1.0;
        }
        self.compressed_tokens as f32 / self.original_tokens as f32
    }

    /// Calculate tokens saved.
    pub fn tokens_saved(&self) -> usize {
        self.original_tokens.saturating_sub(self.compressed_tokens)
    }
}

/// A summary of compressed knowledge entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSummary {
    /// Unique identifier
    pub id: String,
    /// Task run this summary belongs to
    pub task_run_id: String,
    /// Category of knowledge summarized
    pub category: String,
    /// The summary text
    pub summary: String,
    /// Iterations covered by this summary
    pub covered_iterations: Vec<u32>,
    /// Number of items that were summarized
    pub item_count: usize,
    /// When the summary was created
    pub created_at: String,
}

// ============================================================================
// Compression Service
// ============================================================================

/// Service for managing context compression.
///
/// Reads live (non-archived) `task_knowledge` rows from PG, summarizes oldest
/// entries per category once the configured threshold is exceeded, persists
/// the summary as a new knowledge row, and archives the originals via
/// `PgDb::archive_task_knowledge`.
pub struct CompressionService {
    config: CompressionConfig,
    pg: Arc<PgDb>,
}

impl CompressionService {
    /// Create a new compression service backed by a PG handle.
    pub fn new(config: CompressionConfig, pg: Arc<PgDb>) -> Self {
        Self { config, pg }
    }

    /// Estimate the token count for a task's current context.
    pub async fn estimate_context_tokens(
        &self,
        task_run_id: &str,
    ) -> Result<TokenCount, String> {
        let all_knowledge = self
            .pg
            .list_task_knowledge(task_run_id, None, false, false)
            .await?;

        let mut count = TokenCount::empty();
        count.entry_count = all_knowledge.len();

        for entry in &all_knowledge {
            let tokens = estimate_entry_tokens(entry, self.config.tokens_per_char);
            count.total += tokens;

            match entry.category.as_str() {
                "finding" | "root_cause" | "bug" => count.findings += tokens,
                "observation" => count.observations += tokens,
                "verification_feedback" | "feedback" => count.feedback += tokens,
                "solution" | "fix" => count.solutions += tokens,
                "environment" | "infrastructure" => count.environment += tokens,
                _ => count.other += tokens,
            }
        }

        debug!(
            "Estimated {} tokens for task {} ({} entries)",
            count.total, task_run_id, count.entry_count
        );

        Ok(count)
    }

    /// Check if compression is needed and perform it if so.
    ///
    /// Returns Some(result) if compression was performed, None if not needed.
    pub async fn compress_if_needed(
        &self,
        task_run_id: &str,
    ) -> Result<Option<CompressionResult>, String> {
        if !self.config.enabled {
            debug!("Compression disabled, skipping");
            return Ok(None);
        }

        let token_count = self.estimate_context_tokens(task_run_id).await?;

        if !token_count.exceeds_threshold(self.config.threshold_tokens) {
            debug!(
                "Token count {} below threshold {}, no compression needed",
                token_count.total, self.config.threshold_tokens
            );
            return Ok(None);
        }

        info!(
            "Token count {} exceeds threshold {}, starting compression for task {}",
            token_count.total, self.config.threshold_tokens, task_run_id
        );

        let result = self.compress_knowledge(task_run_id, &token_count).await?;
        Ok(Some(result))
    }

    /// Perform compression on task knowledge.
    async fn compress_knowledge(
        &self,
        task_run_id: &str,
        original_count: &TokenCount,
    ) -> Result<CompressionResult, String> {
        let mut items_summarized = 0;
        let mut summary_entries_created = 0;
        let mut compressed_categories = Vec::new();

        // Compress each category that has significant content
        for (category, tokens) in [
            ("finding", original_count.findings),
            ("observation", original_count.observations),
            ("verification_feedback", original_count.feedback),
            ("solution", original_count.solutions),
            ("environment", original_count.environment),
        ] {
            // Only compress if this category has significant tokens
            if tokens > 1000 {
                let (summarized, summaries) =
                    self.compress_category(task_run_id, category).await?;
                items_summarized += summarized;
                summary_entries_created += summaries;
                if summarized > 0 {
                    compressed_categories.push(category.to_string());
                }
            }
        }

        // Re-estimate tokens after compression
        let new_count = self.estimate_context_tokens(task_run_id).await?;

        let result = CompressionResult {
            original_tokens: original_count.total,
            compressed_tokens: new_count.total,
            items_summarized,
            summary_entries_created,
            compressed_categories,
        };

        info!(
            "Compression complete: {} -> {} tokens ({:.1}% reduction), {} items summarized",
            result.original_tokens,
            result.compressed_tokens,
            (1.0 - result.compression_ratio()) * 100.0,
            result.items_summarized
        );

        Ok(result)
    }

    /// Compress a specific category of knowledge.
    ///
    /// Returns (items_summarized, summaries_created).
    async fn compress_category(
        &self,
        task_run_id: &str,
        category: &str,
    ) -> Result<(usize, usize), String> {
        let entries = self
            .pg
            .list_task_knowledge(task_run_id, Some(category), false, false)
            .await?;

        if entries.len() <= self.config.keep_recent_items {
            debug!(
                "Category {} has {} entries, below keep threshold {}, skipping",
                category,
                entries.len(),
                self.config.keep_recent_items
            );
            return Ok((0, 0));
        }

        // Sort by iteration (oldest first)
        let mut sorted_entries = entries;
        sorted_entries.sort_by_key(|e| e.iteration);

        // Keep recent items, compress older ones
        let compress_count = sorted_entries
            .len()
            .saturating_sub(self.config.keep_recent_items);
        let to_compress = &sorted_entries[..compress_count];

        if to_compress.is_empty() {
            return Ok((0, 0));
        }

        // Create summary for the batch
        let summary = self.create_summary(task_run_id, category, to_compress)?;

        // Build the persisted summary content
        let summary_content = format!(
            "[COMPRESSED SUMMARY - {} items from iterations {:?}]\n{}",
            summary.item_count, summary.covered_iterations, summary.summary
        );

        // Persist the summary as a new task_knowledge row BEFORE archiving
        // (the FK on summary_entry_id requires the summary row to exist first).
        let summary_id = uuid::Uuid::new_v4().to_string();
        let archived_ids: Vec<&str> = to_compress.iter().map(|e| e.id.as_str()).collect();
        let evidence_json = serde_json::json!({
            "archived_ids": archived_ids,
            "covered_iterations": summary.covered_iterations,
            "item_count": summary.item_count,
        })
        .to_string();
        let summary_iteration = to_compress
            .last()
            .map(|e| e.iteration as i32)
            .unwrap_or(0);

        self.pg
            .create_task_knowledge(
                &summary_id,
                task_run_id,
                &format!("{category}_compressed_summary"),
                "system",
                summary_iteration,
                &summary_content,
                Some(&evidence_json),
                "high",
                "[]",
            )
            .await?;

        // Best-effort embed so the summary itself is searchable via Tier 0.
        let _ = self
            .pg
            .embed_knowledge_content(&summary_id, &summary_content)
            .await;

        // Archive the originals — single round-trip via ANY($::text[])
        let id_strings: Vec<String> = to_compress.iter().map(|e| e.id.clone()).collect();
        let archived_count = self
            .pg
            .archive_task_knowledge(&id_strings, &summary_id)
            .await?;

        info!(
            "Compressed {} {} entries into summary {} for task {}",
            archived_count, category, summary_id, task_run_id
        );

        Ok((archived_count, 1))
    }

    /// Create a summary from multiple knowledge entries.
    fn create_summary(
        &self,
        task_run_id: &str,
        category: &str,
        entries: &[StoredTaskKnowledge],
    ) -> Result<KnowledgeSummary, String> {
        let covered_iterations: Vec<u32> = entries.iter().map(|e| e.iteration).collect();

        // Build a simple extractive summary
        // In a production system, this could use AI summarization
        let mut summary_parts = Vec::new();

        // Group by unique content patterns
        let mut seen_patterns = std::collections::HashSet::new();
        for entry in entries {
            // Create a simplified key for deduplication
            let key = entry.content.chars().take(50).collect::<String>();
            if seen_patterns.insert(key) {
                // Truncate long entries
                let content = if entry.content.len() > 200 {
                    format!("{}...", &entry.content[..200])
                } else {
                    entry.content.clone()
                };
                summary_parts.push(format!("- {}", content));
            }
        }

        // Limit summary size
        let max_items = 10;
        if summary_parts.len() > max_items {
            summary_parts.truncate(max_items);
            summary_parts.push(format!("... and {} more items", entries.len() - max_items));
        }

        let summary = KnowledgeSummary {
            id: uuid::Uuid::new_v4().to_string(),
            task_run_id: task_run_id.to_string(),
            category: category.to_string(),
            summary: summary_parts.join("\n"),
            covered_iterations,
            item_count: entries.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        Ok(summary)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        // With default 0.25 tokens per char
        assert_eq!(estimate_tokens("hello", 0.25), 2); // 5 * 0.25 = 1.25 -> 2
        assert_eq!(estimate_tokens("", 0.25), 0);
        assert_eq!(estimate_tokens("a".repeat(100).as_str(), 0.25), 25);
    }

    #[test]
    fn test_token_count_threshold() {
        let count = TokenCount {
            total: 50000,
            findings: 20000,
            observations: 10000,
            feedback: 10000,
            solutions: 5000,
            environment: 0,
            other: 5000,
            entry_count: 100,
        };

        assert!(count.exceeds_threshold(40000));
        assert!(!count.exceeds_threshold(60000));
    }

    #[test]
    fn test_compression_result_ratio() {
        let result = CompressionResult {
            original_tokens: 100000,
            compressed_tokens: 60000,
            items_summarized: 50,
            summary_entries_created: 5,
            compressed_categories: vec!["finding".to_string()],
        };

        assert!((result.compression_ratio() - 0.6).abs() < 0.01);
        assert_eq!(result.tokens_saved(), 40000);
    }

    #[test]
    fn test_default_config() {
        let config = CompressionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.threshold_tokens, 80000);
        assert_eq!(config.target_tokens, 60000);
        assert_eq!(config.keep_recent_items, 5);
    }
}

// ============================================================================
// LLM-Driven Iteration Summarization (TuriX-CUA inspired)
// ============================================================================

/// Compression mode for iteration history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IterationCompressionMode {
    /// Character-based truncation/merging (existing behavior, always available).
    #[default]
    CharBased,
    /// LLM-driven summarization using a Simple-tier model.
    /// Falls back to CharBased if the LLM call fails.
    LlmSummarized,
}

/// Summarize a list of iteration descriptions using an LLM.
///
/// Sends the iteration entries to a Simple-tier model with a summarization
/// prompt, returning a concise summary. Falls back to character-based
/// truncation if the LLM call fails.
///
/// # Arguments
/// * `entries` - Iteration descriptions to summarize (approach + result text).
/// * `max_output_chars` - Target maximum length for the summary.
///
/// # Returns
/// The LLM-generated summary, or an error if the call fails.
pub fn summarize_iterations_with_llm(
    entries: &[String],
    max_output_chars: usize,
) -> Result<String, String> {
    if entries.is_empty() {
        return Ok(String::new());
    }

    let entries_text = entries
        .iter()
        .enumerate()
        .map(|(i, e)| format!("Iteration {}: {}", i + 1, e))
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Summarize these past automation iterations into a concise paragraph \
         (max {} characters). Preserve: what was tried, what failed, and key findings. \
         Do not include preamble or meta-commentary — just the summary.\n\n\
         ---\n\n{}",
        max_output_chars, entries_text
    );

    // Use the Simple-tier model for cost efficiency
    let context = crate::ai_router::TaskContext::from_prompt(&prompt);

    let response = crate::ai_provider::run_prompt_with_model_override(
        &prompt,
        &context,
        None,                              // no doctor handle
        None,      // let routing pick the model (will route to simple for short prompt)
        None,      // default provider
        Some(0.3), // low temperature for factual summarization
        Some(max_output_chars as u32 / 3), // rough tokens estimate
        None,      // no fallback model
        None,      // no fallback provider
    );

    if response.success {
        let summary = response.output.trim().to_string();
        // Truncate at char boundary (not byte boundary) to avoid UTF-8 panics
        let summary = if summary.chars().count() > max_output_chars {
            summary.chars().take(max_output_chars).collect::<String>()
        } else {
            summary
        };
        info!(
            "LLM summarized {} iterations into {} chars",
            entries.len(),
            summary.len()
        );
        Ok(summary)
    } else {
        let error = response
            .error
            .unwrap_or_else(|| "Unknown error".to_string());
        warn!(
            "LLM summarization failed ({}), falling back to char-based",
            error
        );
        Err(error)
    }
}

/// Character-based fallback summarization.
///
/// Concatenates entries and truncates to max_chars. Used when LLM summarization
/// is unavailable or fails.
pub fn summarize_iterations_char_based(entries: &[String], max_chars: usize) -> String {
    let combined = entries.join(" | ");
    if combined.chars().count() <= max_chars {
        combined
    } else {
        // Truncate at char boundary to avoid UTF-8 panics
        let truncated: String = combined.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// Summarize iterations using the configured mode with automatic fallback.
///
/// Tries LLM summarization if mode is `LlmSummarized`, falls back to
/// `CharBased` on failure.
pub fn summarize_iterations(
    entries: &[String],
    max_chars: usize,
    mode: IterationCompressionMode,
) -> String {
    match mode {
        IterationCompressionMode::CharBased => summarize_iterations_char_based(entries, max_chars),
        IterationCompressionMode::LlmSummarized => {
            match summarize_iterations_with_llm(entries, max_chars) {
                Ok(summary) => summary,
                Err(_) => {
                    // Automatic fallback to char-based
                    summarize_iterations_char_based(entries, max_chars)
                }
            }
        }
    }
}
