//! Context Window Management Strategies
//!
//! Inspired by AutoGen's multiple context management strategies, this module
//! provides different approaches for managing AI context windows:
//!
//! - **Unbounded**: Keep everything (for small tasks)
//! - **BufferedWindow**: Keep last N items
//! - **TokenLimited**: Stay under a token budget
//! - **HeadAndTail**: Keep first N + last M items
//! - **SemanticPruning**: Keep most relevant items
//!
//! # Design Goals
//!
//! 1. **Task-Appropriate**: Different strategies for different task sizes
//! 2. **Configurable**: Easy to tune thresholds and behavior
//! 3. **Composable**: Strategies can be combined
//! 4. **Observable**: Track what was kept/removed
//!
//! # Usage
//!
//! ```rust,ignore
//! let strategy = ContextStrategy::TokenLimited(TokenLimitedConfig {
//!     max_tokens: 80000,
//!     preserve_system: true,
//!     removal_strategy: RemovalStrategy::MiddleOut,
//! });
//!
//! let result = strategy.apply(&context_items);
//! ```

use serde::{Deserialize, Serialize};
use tracing::info;

// ============================================================================
// Context Item
// ============================================================================

/// A context item that can be managed by strategies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    /// Unique identifier.
    pub id: String,

    /// Content of the item.
    pub content: String,

    /// Item category for prioritization.
    pub category: ItemCategory,

    /// Priority level (higher = more important).
    pub priority: Priority,

    /// Iteration when this item was created.
    pub iteration: u32,

    /// Estimated token count.
    pub tokens: usize,

    /// Whether this item must be preserved.
    pub pinned: bool,

    /// Optional relevance score (0.0 - 1.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance_score: Option<f32>,
}

impl ContextItem {
    /// Create a new context item.
    pub fn new(id: impl Into<String>, content: impl Into<String>, category: ItemCategory) -> Self {
        let content = content.into();
        let tokens = estimate_tokens(&content);
        Self {
            id: id.into(),
            content,
            category,
            priority: Priority::Normal,
            iteration: 0,
            tokens,
            pinned: false,
            relevance_score: None,
        }
    }

    /// Create a system/instruction item (always preserved).
    pub fn system(id: impl Into<String>, content: impl Into<String>) -> Self {
        let mut item = Self::new(id, content, ItemCategory::System);
        item.priority = Priority::Critical;
        item.pinned = true;
        item
    }

    /// Create a verification feedback item.
    pub fn feedback(id: impl Into<String>, content: impl Into<String>, iteration: u32) -> Self {
        let mut item = Self::new(id, content, ItemCategory::VerificationFeedback);
        item.priority = Priority::High;
        item.iteration = iteration;
        item
    }

    /// Create a finding item.
    pub fn finding(id: impl Into<String>, content: impl Into<String>, iteration: u32) -> Self {
        let mut item = Self::new(id, content, ItemCategory::Finding);
        item.iteration = iteration;
        item
    }

    /// Set the iteration.
    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = iteration;
        self
    }

    /// Set the priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Pin this item (prevent removal).
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Set relevance score.
    pub fn with_relevance(mut self, score: f32) -> Self {
        self.relevance_score = Some(score.clamp(0.0, 1.0));
        self
    }
}

/// Categories of context items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemCategory {
    /// System instructions (always kept).
    System,
    /// Goal and plan information.
    Goal,
    /// Verification feedback from failed checks.
    VerificationFeedback,
    /// Findings from workers.
    Finding,
    /// Observations and context.
    Observation,
    /// Solution attempts.
    Solution,
    /// Worker output/work done.
    WorkOutput,
    /// Other/general.
    Other,
}

impl ItemCategory {
    /// Get the default priority for this category.
    pub fn default_priority(&self) -> Priority {
        match self {
            Self::System => Priority::Critical,
            Self::Goal => Priority::Critical,
            Self::VerificationFeedback => Priority::High,
            Self::Finding => Priority::Normal,
            Self::Solution => Priority::Normal,
            Self::Observation => Priority::Low,
            Self::WorkOutput => Priority::Low,
            Self::Other => Priority::Low,
        }
    }
}

/// Priority levels for context items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// Can be removed first.
    Low = 0,
    /// Standard priority.
    Normal = 1,
    /// Important, preserve if possible.
    High = 2,
    /// Must preserve, never remove.
    Critical = 3,
}

// ============================================================================
// Context Strategy
// ============================================================================

/// Strategy for managing context window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextStrategy {
    /// Keep everything (no pruning).
    Unbounded,

    /// Keep last N items.
    BufferedWindow(BufferedWindowConfig),

    /// Stay under token limit.
    TokenLimited(TokenLimitedConfig),

    /// Keep first N + last M items.
    HeadAndTail(HeadAndTailConfig),

    /// Keep items by relevance score.
    SemanticPruning(SemanticPruningConfig),

    /// Combine multiple strategies.
    Composite(CompositeConfig),
}

impl Default for ContextStrategy {
    fn default() -> Self {
        Self::TokenLimited(TokenLimitedConfig::default())
    }
}

impl ContextStrategy {
    /// Apply this strategy to a list of context items.
    pub fn apply(&self, items: &[ContextItem]) -> StrategyResult {
        match self {
            Self::Unbounded => apply_unbounded(items),
            Self::BufferedWindow(config) => apply_buffered_window(items, config),
            Self::TokenLimited(config) => apply_token_limited(items, config),
            Self::HeadAndTail(config) => apply_head_and_tail(items, config),
            Self::SemanticPruning(config) => apply_semantic_pruning(items, config),
            Self::Composite(config) => apply_composite(items, config),
        }
    }

    /// Create a token-limited strategy with default settings.
    pub fn token_limited(max_tokens: usize) -> Self {
        Self::TokenLimited(TokenLimitedConfig {
            max_tokens,
            ..Default::default()
        })
    }

    /// Create a buffered window strategy.
    pub fn buffered(max_items: usize) -> Self {
        Self::BufferedWindow(BufferedWindowConfig {
            max_items,
            ..Default::default()
        })
    }

    /// Create a head-and-tail strategy.
    pub fn head_and_tail(head_items: usize, tail_items: usize) -> Self {
        Self::HeadAndTail(HeadAndTailConfig {
            head_items,
            tail_items,
            ..Default::default()
        })
    }
}

// ============================================================================
// Strategy Configurations
// ============================================================================

/// Configuration for buffered window strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferedWindowConfig {
    /// Maximum number of items to keep.
    pub max_items: usize,

    /// Whether to preserve pinned items beyond the window.
    #[serde(default = "default_true")]
    pub preserve_pinned: bool,

    /// Minimum items to keep per category.
    #[serde(default)]
    pub min_per_category: usize,
}

impl Default for BufferedWindowConfig {
    fn default() -> Self {
        Self {
            max_items: 50,
            preserve_pinned: true,
            min_per_category: 1,
        }
    }
}

/// Configuration for token-limited strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenLimitedConfig {
    /// Maximum tokens allowed.
    pub max_tokens: usize,

    /// Whether to always preserve system items.
    #[serde(default = "default_true")]
    pub preserve_system: bool,

    /// Strategy for removing items when over limit.
    #[serde(default)]
    pub removal_strategy: RemovalStrategy,

    /// Reserve tokens for response generation.
    #[serde(default = "default_reserve_tokens")]
    pub reserve_tokens: usize,
}

fn default_reserve_tokens() -> usize {
    4000 // Reserve 4k tokens for response
}

impl Default for TokenLimitedConfig {
    fn default() -> Self {
        Self {
            max_tokens: 80000,
            preserve_system: true,
            removal_strategy: RemovalStrategy::default(),
            reserve_tokens: default_reserve_tokens(),
        }
    }
}

/// Strategy for removing items when over token limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalStrategy {
    /// Remove oldest items first.
    OldestFirst,
    /// Remove from the middle, preserving head and tail.
    #[default]
    MiddleOut,
    /// Remove lowest priority items first.
    LowestPriority,
    /// Remove lowest relevance items first.
    LowestRelevance,
}

/// Configuration for head-and-tail strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadAndTailConfig {
    /// Number of items to keep from the beginning.
    pub head_items: usize,

    /// Number of items to keep from the end.
    pub tail_items: usize,

    /// Whether to include a summary of removed items.
    #[serde(default)]
    pub include_summary: bool,
}

impl Default for HeadAndTailConfig {
    fn default() -> Self {
        Self {
            head_items: 5,
            tail_items: 10,
            include_summary: true,
        }
    }
}

/// Configuration for semantic pruning strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticPruningConfig {
    /// Maximum tokens after pruning.
    pub max_tokens: usize,

    /// Minimum relevance score to keep.
    pub min_relevance: f32,

    /// Query to compute relevance against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance_query: Option<String>,

    /// Boost recent items by this factor.
    #[serde(default = "default_recency_boost")]
    pub recency_boost: f32,
}

fn default_recency_boost() -> f32 {
    0.1 // 10% boost per iteration proximity
}

impl Default for SemanticPruningConfig {
    fn default() -> Self {
        Self {
            max_tokens: 60000,
            min_relevance: 0.3,
            relevance_query: None,
            recency_boost: default_recency_boost(),
        }
    }
}

/// Configuration for composite strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeConfig {
    /// Strategies to apply in order.
    pub strategies: Vec<ContextStrategy>,

    /// How to combine results.
    #[serde(default)]
    pub combination: CombinationMode,
}

/// How to combine multiple strategies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombinationMode {
    /// Apply strategies sequentially (output of one is input to next).
    #[default]
    Sequential,
    /// Take intersection (item must pass all strategies).
    Intersection,
    /// Take union (item passes if any strategy keeps it).
    Union,
}

fn default_true() -> bool {
    true
}

// ============================================================================
// Strategy Result
// ============================================================================

/// Result of applying a context strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyResult {
    /// Items that were kept.
    pub kept_items: Vec<ContextItem>,

    /// IDs of items that were removed.
    pub removed_ids: Vec<String>,

    /// Total tokens in kept items.
    pub total_tokens: usize,

    /// Original token count before strategy.
    pub original_tokens: usize,

    /// Number of items removed.
    pub items_removed: usize,

    /// Optional summary of removed content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removal_summary: Option<String>,

    /// Strategy that was applied.
    pub strategy_used: String,
}

impl StrategyResult {
    /// Create a result with no changes.
    pub fn unchanged(items: &[ContextItem]) -> Self {
        let total_tokens: usize = items.iter().map(|i| i.tokens).sum();
        Self {
            kept_items: items.to_vec(),
            removed_ids: Vec::new(),
            total_tokens,
            original_tokens: total_tokens,
            items_removed: 0,
            removal_summary: None,
            strategy_used: "none".to_string(),
        }
    }

    /// Check if any items were removed.
    pub fn had_removals(&self) -> bool {
        self.items_removed > 0
    }

    /// Get the reduction percentage.
    pub fn reduction_percentage(&self) -> f32 {
        if self.original_tokens == 0 {
            return 0.0;
        }
        ((self.original_tokens - self.total_tokens) as f32 / self.original_tokens as f32) * 100.0
    }

    /// Build the final context string from kept items.
    pub fn build_context(&self) -> String {
        let mut context = String::new();

        for item in &self.kept_items {
            context.push_str(&item.content);
            context.push_str("\n\n");
        }

        if let Some(summary) = &self.removal_summary {
            context.push_str("---\n");
            context.push_str(summary);
            context.push('\n');
        }

        context
    }
}

// ============================================================================
// Strategy Implementations
// ============================================================================

/// Apply unbounded strategy (keep everything).
fn apply_unbounded(items: &[ContextItem]) -> StrategyResult {
    let mut result = StrategyResult::unchanged(items);
    result.strategy_used = "unbounded".to_string();
    result
}

/// Apply buffered window strategy.
fn apply_buffered_window(items: &[ContextItem], config: &BufferedWindowConfig) -> StrategyResult {
    let original_tokens: usize = items.iter().map(|i| i.tokens).sum();

    // Separate pinned items
    let (pinned, unpinned): (Vec<_>, Vec<_>) = items.iter().partition(|i| i.pinned);

    // Take last N unpinned items
    let kept_unpinned: Vec<_> = unpinned
        .into_iter()
        .rev()
        .take(config.max_items)
        .rev()
        .cloned()
        .collect();

    // Combine pinned + kept unpinned
    let mut kept_items: Vec<_> = if config.preserve_pinned {
        pinned.into_iter().cloned().collect()
    } else {
        Vec::new()
    };
    kept_items.extend(kept_unpinned);

    // Track removed
    let kept_ids: std::collections::HashSet<_> = kept_items.iter().map(|i| i.id.clone()).collect();
    let removed_ids: Vec<_> = items
        .iter()
        .filter(|i| !kept_ids.contains(&i.id))
        .map(|i| i.id.clone())
        .collect();

    let total_tokens: usize = kept_items.iter().map(|i| i.tokens).sum();
    let items_removed = removed_ids.len();

    StrategyResult {
        kept_items,
        removed_ids,
        total_tokens,
        original_tokens,
        items_removed,
        removal_summary: if items_removed > 0 {
            Some(format!(
                "[{} older items removed to fit window]",
                items_removed
            ))
        } else {
            None
        },
        strategy_used: "buffered_window".to_string(),
    }
}

/// Apply token-limited strategy.
fn apply_token_limited(items: &[ContextItem], config: &TokenLimitedConfig) -> StrategyResult {
    let original_tokens: usize = items.iter().map(|i| i.tokens).sum();
    let effective_limit = config.max_tokens.saturating_sub(config.reserve_tokens);

    // If under limit, keep everything
    if original_tokens <= effective_limit {
        let mut result = StrategyResult::unchanged(items);
        result.strategy_used = "token_limited".to_string();
        return result;
    }

    // Separate critical items (system, pinned)
    let (critical, removable): (Vec<_>, Vec<_>) = items
        .iter()
        .partition(|i| i.pinned || (config.preserve_system && i.category == ItemCategory::System));

    let critical_tokens: usize = critical.iter().map(|i| i.tokens).sum();
    let remaining_budget = effective_limit.saturating_sub(critical_tokens);

    // Apply removal strategy
    let mut kept_removable: Vec<ContextItem> = match config.removal_strategy {
        RemovalStrategy::OldestFirst => {
            let mut sorted: Vec<ContextItem> = removable.iter().map(|i| (*i).clone()).collect();
            sorted.sort_by_key(|i| std::cmp::Reverse(i.iteration));
            select_within_budget(&sorted, remaining_budget)
        }
        RemovalStrategy::MiddleOut => apply_middle_out_removal(&removable, remaining_budget),
        RemovalStrategy::LowestPriority => {
            let mut sorted: Vec<ContextItem> = removable.iter().map(|i| (*i).clone()).collect();
            sorted.sort_by_key(|i| std::cmp::Reverse(i.priority));
            select_within_budget(&sorted, remaining_budget)
        }
        RemovalStrategy::LowestRelevance => {
            let mut sorted: Vec<ContextItem> = removable.iter().map(|i| (*i).clone()).collect();
            sorted.sort_by(|a, b| {
                b.relevance_score
                    .unwrap_or(0.5)
                    .partial_cmp(&a.relevance_score.unwrap_or(0.5))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            select_within_budget(&sorted, remaining_budget)
        }
    };

    // Combine critical + kept removable
    let mut kept_items: Vec<_> = critical.into_iter().cloned().collect();
    kept_items.append(&mut kept_removable);

    // Sort by original order (by iteration)
    kept_items.sort_by_key(|i| i.iteration);

    // Track removed
    let kept_ids: std::collections::HashSet<_> = kept_items.iter().map(|i| i.id.clone()).collect();
    let removed_ids: Vec<_> = items
        .iter()
        .filter(|i| !kept_ids.contains(&i.id))
        .map(|i| i.id.clone())
        .collect();

    let total_tokens: usize = kept_items.iter().map(|i| i.tokens).sum();
    let items_removed = removed_ids.len();

    info!(
        "Token-limited strategy: {} -> {} tokens ({} items removed)",
        original_tokens, total_tokens, items_removed
    );

    StrategyResult {
        kept_items,
        removed_ids,
        total_tokens,
        original_tokens,
        items_removed,
        removal_summary: if items_removed > 0 {
            Some(format!(
                "[{} items ({} tokens) removed to fit {} token limit]",
                items_removed,
                original_tokens - total_tokens,
                config.max_tokens
            ))
        } else {
            None
        },
        strategy_used: "token_limited".to_string(),
    }
}

/// Select items within a token budget.
fn select_within_budget(items: &[ContextItem], budget: usize) -> Vec<ContextItem> {
    let mut result = Vec::new();
    let mut used = 0;

    for item in items {
        if used + item.tokens <= budget {
            result.push(item.clone());
            used += item.tokens;
        }
    }

    result
}

/// Apply middle-out removal (preserve head and tail).
fn apply_middle_out_removal(items: &[&ContextItem], budget: usize) -> Vec<ContextItem> {
    if items.is_empty() {
        return Vec::new();
    }

    let total_tokens: usize = items.iter().map(|i| i.tokens).sum();
    if total_tokens <= budget {
        return items.iter().map(|i| (*i).clone()).collect();
    }

    // Calculate how many to keep from each end
    let mut head_count = 0;
    let mut head_tokens = 0;
    let mut tail_count = 0;
    let mut tail_tokens = 0;

    // Alternate adding from head and tail until budget exhausted
    let mut i = 0;
    let mut j = items.len();

    while i < j && (head_tokens + tail_tokens) < budget {
        // Try adding from head
        if i < j && head_tokens + items[i].tokens + tail_tokens <= budget {
            head_tokens += items[i].tokens;
            head_count += 1;
            i += 1;
        }

        // Try adding from tail
        if i < j && head_tokens + tail_tokens + items[j - 1].tokens <= budget {
            j -= 1;
            tail_tokens += items[j].tokens;
            tail_count += 1;
        } else if head_tokens + items[i].tokens + tail_tokens > budget {
            break;
        }
    }

    // Collect head and tail items
    let mut result: Vec<ContextItem> = items[..head_count].iter().map(|i| (*i).clone()).collect();

    let tail_start = items.len() - tail_count;
    result.extend(items[tail_start..].iter().map(|i| (*i).clone()));

    result
}

/// Apply head-and-tail strategy.
fn apply_head_and_tail(items: &[ContextItem], config: &HeadAndTailConfig) -> StrategyResult {
    let original_tokens: usize = items.iter().map(|i| i.tokens).sum();

    if items.len() <= config.head_items + config.tail_items {
        let mut result = StrategyResult::unchanged(items);
        result.strategy_used = "head_and_tail".to_string();
        return result;
    }

    // Take head items
    let head: Vec<_> = items.iter().take(config.head_items).cloned().collect();

    // Take tail items
    let tail_start = items.len().saturating_sub(config.tail_items);
    let tail: Vec<_> = items.iter().skip(tail_start).cloned().collect();

    // Combine
    let mut kept_items = head;
    kept_items.extend(tail);

    // Track removed
    let kept_ids: std::collections::HashSet<_> = kept_items.iter().map(|i| i.id.clone()).collect();
    let removed_ids: Vec<_> = items
        .iter()
        .filter(|i| !kept_ids.contains(&i.id))
        .map(|i| i.id.clone())
        .collect();

    let total_tokens: usize = kept_items.iter().map(|i| i.tokens).sum();
    let items_removed = removed_ids.len();

    let removal_summary = if config.include_summary && items_removed > 0 {
        Some(format!(
            "[... {} middle items omitted (iterations {}-{}) ...]",
            items_removed,
            config.head_items + 1,
            items.len() - config.tail_items
        ))
    } else {
        None
    };

    StrategyResult {
        kept_items,
        removed_ids,
        total_tokens,
        original_tokens,
        items_removed,
        removal_summary,
        strategy_used: "head_and_tail".to_string(),
    }
}

/// Apply semantic pruning strategy.
fn apply_semantic_pruning(items: &[ContextItem], config: &SemanticPruningConfig) -> StrategyResult {
    let original_tokens: usize = items.iter().map(|i| i.tokens).sum();

    // Score items by relevance + recency
    let max_iteration = items.iter().map(|i| i.iteration).max().unwrap_or(1);
    let mut scored_items: Vec<_> = items
        .iter()
        .map(|item| {
            let base_score = item.relevance_score.unwrap_or(0.5);
            let recency_factor = if max_iteration > 0 {
                item.iteration as f32 / max_iteration as f32
            } else {
                1.0
            };
            let final_score = base_score + (recency_factor * config.recency_boost);

            // Critical items always score high
            let score = if item.pinned || item.priority == Priority::Critical {
                1.0
            } else {
                final_score.min(1.0)
            };

            (item.clone(), score)
        })
        .collect();

    // Sort by score (highest first)
    scored_items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Select items above threshold and within budget
    let mut kept_items = Vec::new();
    let mut total_tokens = 0;

    for (item, score) in scored_items.iter() {
        if score >= &config.min_relevance && total_tokens + item.tokens <= config.max_tokens {
            kept_items.push(item.clone());
            total_tokens += item.tokens;
        } else if item.pinned || item.priority == Priority::Critical {
            // Always keep critical items
            kept_items.push(item.clone());
            total_tokens += item.tokens;
        }
    }

    // Restore original order
    kept_items.sort_by_key(|i| i.iteration);

    // Track removed
    let kept_ids: std::collections::HashSet<_> = kept_items.iter().map(|i| i.id.clone()).collect();
    let removed_ids: Vec<_> = items
        .iter()
        .filter(|i| !kept_ids.contains(&i.id))
        .map(|i| i.id.clone())
        .collect();

    let items_removed = removed_ids.len();

    StrategyResult {
        kept_items,
        removed_ids,
        total_tokens,
        original_tokens,
        items_removed,
        removal_summary: if items_removed > 0 {
            Some(format!("[{} low-relevance items pruned]", items_removed))
        } else {
            None
        },
        strategy_used: "semantic_pruning".to_string(),
    }
}

/// Apply composite strategy.
fn apply_composite(items: &[ContextItem], config: &CompositeConfig) -> StrategyResult {
    if config.strategies.is_empty() {
        return StrategyResult::unchanged(items);
    }

    match config.combination {
        CombinationMode::Sequential => {
            let mut current_items = items.to_vec();
            let original_tokens: usize = items.iter().map(|i| i.tokens).sum();
            let mut all_removed = Vec::new();

            for strategy in &config.strategies {
                let result = strategy.apply(&current_items);
                all_removed.extend(result.removed_ids);
                current_items = result.kept_items;
            }

            let total_tokens: usize = current_items.iter().map(|i| i.tokens).sum();

            StrategyResult {
                kept_items: current_items,
                removed_ids: all_removed.clone(),
                total_tokens,
                original_tokens,
                items_removed: all_removed.len(),
                removal_summary: Some(format!(
                    "[Composite: {} strategies applied, {} items removed]",
                    config.strategies.len(),
                    all_removed.len()
                )),
                strategy_used: "composite_sequential".to_string(),
            }
        }
        CombinationMode::Intersection => {
            let original_tokens: usize = items.iter().map(|i| i.tokens).sum();

            // Item must be kept by ALL strategies
            let kept_sets: Vec<std::collections::HashSet<String>> = config
                .strategies
                .iter()
                .map(|s| {
                    let result = s.apply(items);
                    result.kept_items.iter().map(|i| i.id.clone()).collect()
                })
                .collect();

            let intersection: std::collections::HashSet<_> = kept_sets
                .into_iter()
                .reduce(|a, b| a.intersection(&b).cloned().collect())
                .unwrap_or_default();

            let kept_items: Vec<_> = items
                .iter()
                .filter(|i| intersection.contains(&i.id))
                .cloned()
                .collect();

            let removed_ids: Vec<_> = items
                .iter()
                .filter(|i| !intersection.contains(&i.id))
                .map(|i| i.id.clone())
                .collect();

            let total_tokens: usize = kept_items.iter().map(|i| i.tokens).sum();

            StrategyResult {
                kept_items,
                removed_ids: removed_ids.clone(),
                total_tokens,
                original_tokens,
                items_removed: removed_ids.len(),
                removal_summary: Some("[Composite: intersection of strategies]".to_string()),
                strategy_used: "composite_intersection".to_string(),
            }
        }
        CombinationMode::Union => {
            let original_tokens: usize = items.iter().map(|i| i.tokens).sum();

            // Item is kept if ANY strategy keeps it
            let kept_sets: Vec<std::collections::HashSet<String>> = config
                .strategies
                .iter()
                .map(|s| {
                    let result = s.apply(items);
                    result.kept_items.iter().map(|i| i.id.clone()).collect()
                })
                .collect();

            let union: std::collections::HashSet<_> = kept_sets
                .into_iter()
                .fold(std::collections::HashSet::new(), |a, b| {
                    a.union(&b).cloned().collect()
                });

            let kept_items: Vec<_> = items
                .iter()
                .filter(|i| union.contains(&i.id))
                .cloned()
                .collect();

            let removed_ids: Vec<_> = items
                .iter()
                .filter(|i| !union.contains(&i.id))
                .map(|i| i.id.clone())
                .collect();

            let total_tokens: usize = kept_items.iter().map(|i| i.tokens).sum();

            StrategyResult {
                kept_items,
                removed_ids: removed_ids.clone(),
                total_tokens,
                original_tokens,
                items_removed: removed_ids.len(),
                removal_summary: Some("[Composite: union of strategies]".to_string()),
                strategy_used: "composite_union".to_string(),
            }
        }
    }
}

// ============================================================================
// Token Estimation
// ============================================================================

/// Estimate token count from text.
///
/// Uses a conservative estimate of ~4 characters per token.
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() as f32 * 0.25).ceil() as usize + 10 // +10 for overhead
}

// ============================================================================
// Strategy Selection
// ============================================================================

/// Recommend a strategy based on context characteristics.
pub fn recommend_strategy(
    total_tokens: usize,
    item_count: usize,
    has_relevance_scores: bool,
) -> ContextStrategy {
    // Small context: keep everything
    if total_tokens < 20000 && item_count < 30 {
        return ContextStrategy::Unbounded;
    }

    // Medium context: buffered window
    if total_tokens < 50000 && item_count < 100 {
        return ContextStrategy::buffered(50);
    }

    // Large context with relevance: semantic pruning
    if has_relevance_scores && total_tokens > 60000 {
        return ContextStrategy::SemanticPruning(SemanticPruningConfig {
            max_tokens: 60000,
            min_relevance: 0.3,
            ..Default::default()
        });
    }

    // Large context: token-limited with middle-out
    ContextStrategy::TokenLimited(TokenLimitedConfig {
        max_tokens: 80000,
        removal_strategy: RemovalStrategy::MiddleOut,
        ..Default::default()
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_items(count: usize) -> Vec<ContextItem> {
        (0..count)
            .map(|i| {
                ContextItem::new(
                    format!("item_{}", i),
                    format!("Content for item {} with some text", i),
                    ItemCategory::Finding,
                )
                .with_iteration(i as u32)
            })
            .collect()
    }

    #[test]
    fn test_unbounded_keeps_all() {
        let items = create_test_items(10);
        let result = ContextStrategy::Unbounded.apply(&items);

        assert_eq!(result.kept_items.len(), 10);
        assert_eq!(result.items_removed, 0);
    }

    #[test]
    fn test_buffered_window_limits_items() {
        let items = create_test_items(20);
        let strategy = ContextStrategy::buffered(10);
        let result = strategy.apply(&items);

        assert_eq!(result.kept_items.len(), 10);
        assert_eq!(result.items_removed, 10);
        // Should keep the most recent items
        assert!(result.kept_items.iter().any(|i| i.id == "item_19"));
    }

    #[test]
    fn test_buffered_window_preserves_pinned() {
        let mut items = create_test_items(20);
        items[0] = items[0].clone().pinned(); // Pin the first item

        let strategy = ContextStrategy::buffered(5);
        let result = strategy.apply(&items);

        // Should have pinned item + 5 recent
        assert!(result.kept_items.iter().any(|i| i.id == "item_0"));
    }

    #[test]
    fn test_head_and_tail() {
        let items = create_test_items(20);
        let strategy = ContextStrategy::head_and_tail(3, 5);
        let result = strategy.apply(&items);

        assert_eq!(result.kept_items.len(), 8);
        // Should have first 3 and last 5
        assert!(result.kept_items.iter().any(|i| i.id == "item_0"));
        assert!(result.kept_items.iter().any(|i| i.id == "item_2"));
        assert!(result.kept_items.iter().any(|i| i.id == "item_19"));
        assert!(result.kept_items.iter().any(|i| i.id == "item_15"));
    }

    #[test]
    fn test_token_limited_under_limit() {
        let items = create_test_items(5);
        let strategy = ContextStrategy::token_limited(100000);
        let result = strategy.apply(&items);

        assert_eq!(result.kept_items.len(), 5);
        assert_eq!(result.items_removed, 0);
    }

    #[test]
    fn test_strategy_result_build_context() {
        let items = vec![
            ContextItem::new("1", "First item", ItemCategory::Goal),
            ContextItem::new("2", "Second item", ItemCategory::Finding),
        ];
        let result = StrategyResult::unchanged(&items);
        let context = result.build_context();

        assert!(context.contains("First item"));
        assert!(context.contains("Second item"));
    }

    #[test]
    fn test_recommend_strategy_small() {
        let strategy = recommend_strategy(10000, 20, false);
        assert!(matches!(strategy, ContextStrategy::Unbounded));
    }

    #[test]
    fn test_recommend_strategy_large() {
        let strategy = recommend_strategy(100000, 200, false);
        assert!(matches!(strategy, ContextStrategy::TokenLimited(_)));
    }

    #[test]
    fn test_semantic_pruning_respects_priority() {
        let items = vec![
            ContextItem::new("low", "Low relevance", ItemCategory::Other)
                .with_relevance(0.1)
                .with_priority(Priority::Low),
            ContextItem::new("high", "High priority", ItemCategory::Goal)
                .with_relevance(0.1)
                .with_priority(Priority::Critical),
        ];

        let strategy = ContextStrategy::SemanticPruning(SemanticPruningConfig {
            max_tokens: 1000,
            min_relevance: 0.5,
            ..Default::default()
        });

        let result = strategy.apply(&items);

        // High priority should be kept despite low relevance
        assert!(result.kept_items.iter().any(|i| i.id == "high"));
    }
}
