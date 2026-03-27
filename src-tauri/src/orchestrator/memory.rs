//! Multi-Tier Memory System Module
//!
//! Implements a hierarchical memory system inspired by CrewAI with different
//! memory tiers for different use cases and retention policies.
//!
//! ## Memory Tiers
//!
//! - **Short-Term Memory**: Current iteration context, cleared between runs
//! - **Long-Term Memory**: Persists across runs, stores learnings
//! - **Entity Memory**: Named entities and relationships
//! - **Semantic Memory**: (Future) Vector embeddings for similarity search
//!
//! ## Example
//!
//! ```rust
//! use qontinui_runner::orchestrator::memory::*;
//!
//! let mut memory = MemorySystem::new();
//!
//! // Store short-term context
//! memory.short_term.add(MemoryItem::observation("User clicked login button"));
//!
//! // Store long-term learning
//! memory.long_term.store_learning(Learning {
//!     category: "authentication".into(),
//!     insight: "Token refresh needed after 1 hour".into(),
//!     ..Default::default()
//! });
//! ```

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::database::pg::PgDb;
use crate::database::CheckpointDb;
use crate::memory::unified_query::{self, UnifiedMemoryQuery};

// ============================================================================
// Memory Item
// ============================================================================

/// A single item in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    /// Unique identifier.
    pub id: String,
    /// Type of memory item.
    pub item_type: MemoryItemType,
    /// The actual content.
    pub content: String,
    /// When this was created (RFC3339).
    pub created_at: String,
    /// When this was last accessed.
    pub last_accessed: String,
    /// Access count (for LRU-like eviction).
    pub access_count: u32,
    /// Importance score (0.0 to 1.0).
    pub importance: f32,
    /// Source of this memory (task, worker, user, etc.).
    pub source: String,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Related entity IDs.
    pub related_entities: Vec<String>,
    /// Additional metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Types of memory items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryItemType {
    /// An observation or fact.
    Observation,
    /// A decision that was made.
    Decision,
    /// An action that was taken.
    Action,
    /// An error or failure.
    Error,
    /// A learning or insight.
    Learning,
    /// User input or feedback.
    UserInput,
    /// System event.
    SystemEvent,
    /// Custom type.
    Custom,
}

impl Default for MemoryItem {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            item_type: MemoryItemType::Observation,
            content: String::new(),
            created_at: now.clone(),
            last_accessed: now,
            access_count: 0,
            importance: 0.5,
            source: "unknown".to_string(),
            tags: Vec::new(),
            related_entities: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}

impl MemoryItem {
    /// Create an observation memory item.
    pub fn observation(content: impl Into<String>) -> Self {
        Self {
            item_type: MemoryItemType::Observation,
            content: content.into(),
            ..Default::default()
        }
    }

    /// Create a decision memory item.
    pub fn decision(content: impl Into<String>) -> Self {
        Self {
            item_type: MemoryItemType::Decision,
            content: content.into(),
            importance: 0.7,
            ..Default::default()
        }
    }

    /// Create an action memory item.
    pub fn action(content: impl Into<String>) -> Self {
        Self {
            item_type: MemoryItemType::Action,
            content: content.into(),
            ..Default::default()
        }
    }

    /// Create an error memory item.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            item_type: MemoryItemType::Error,
            content: content.into(),
            importance: 0.8,
            ..Default::default()
        }
    }

    /// Set the source.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Set importance.
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Record an access (updates last_accessed and access_count).
    pub fn record_access(&mut self) {
        self.last_accessed = chrono::Utc::now().to_rfc3339();
        self.access_count += 1;
    }

    /// Get a display-friendly type string.
    pub fn item_type_str(&self) -> &'static str {
        match self.item_type {
            MemoryItemType::Observation => "observation",
            MemoryItemType::Decision => "decision",
            MemoryItemType::Action => "action",
            MemoryItemType::Error => "error",
            MemoryItemType::Learning => "learning",
            MemoryItemType::UserInput => "user_input",
            MemoryItemType::SystemEvent => "system_event",
            MemoryItemType::Custom => "custom",
        }
    }
}

/// Compute importance × recency score for sorting memory items.
/// Items that are both important and recent score highest.
fn importance_recency_score(item: &MemoryItem, now: &chrono::DateTime<chrono::Utc>) -> f64 {
    let created = chrono::DateTime::parse_from_rfc3339(&item.created_at)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let age_hours = created
        .map(|dt| now.signed_duration_since(dt).num_minutes() as f64 / 60.0)
        .unwrap_or(24.0);
    // Recency factor: 1.0 for just-created, decays over hours
    let recency = 1.0 / (1.0 + age_hours * 0.1);
    item.importance as f64 * recency
}

// ============================================================================
// Short-Term Memory
// ============================================================================

/// Short-term memory for current iteration context.
/// Cleared between task runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShortTermMemory {
    /// Memory items in order of addition.
    items: Vec<MemoryItem>,
    /// Maximum items to keep.
    max_items: usize,
    /// Current iteration.
    iteration: u32,
}

impl ShortTermMemory {
    /// Create new short-term memory.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            max_items: 100,
            iteration: 0,
        }
    }

    /// Create with a specific capacity.
    pub fn with_capacity(max_items: usize) -> Self {
        Self {
            max_items,
            ..Self::new()
        }
    }

    /// Add a memory item.
    pub fn add(&mut self, item: MemoryItem) {
        self.items.push(item);
        // Evict oldest if over capacity
        while self.items.len() > self.max_items {
            self.items.remove(0);
        }
    }

    /// Get recent items.
    pub fn get_recent(&self, limit: usize) -> Vec<&MemoryItem> {
        self.items.iter().rev().take(limit).collect()
    }

    /// Get items by type.
    pub fn get_by_type(&self, item_type: MemoryItemType) -> Vec<&MemoryItem> {
        self.items
            .iter()
            .filter(|i| i.item_type == item_type)
            .collect()
    }

    /// Get items with a specific tag.
    pub fn get_by_tag(&self, tag: &str) -> Vec<&MemoryItem> {
        self.items
            .iter()
            .filter(|i| i.tags.iter().any(|t| t == tag))
            .collect()
    }

    /// Get all items.
    pub fn all(&self) -> &[MemoryItem] {
        &self.items
    }

    /// Get item count.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Clear all items.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Set current iteration.
    pub fn set_iteration(&mut self, iteration: u32) {
        self.iteration = iteration;
    }

    /// Build a context string from recent memory.
    pub fn build_context(&self, limit: usize) -> String {
        self.get_recent(limit)
            .into_iter()
            .rev()
            .map(|item| format!("[{:?}] {}", item.item_type, item.content))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ============================================================================
// Long-Term Memory
// ============================================================================

/// A learning or insight from past task runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Learning {
    /// Unique identifier.
    pub id: String,
    /// Category of learning.
    pub category: String,
    /// The insight itself.
    pub insight: String,
    /// Confidence in this learning (0.0 to 1.0).
    pub confidence: f32,
    /// Number of times this learning has been confirmed.
    pub confirmations: u32,
    /// Task IDs where this learning was applied.
    pub applied_in_tasks: Vec<String>,
    /// When this was first learned.
    pub created_at: String,
    /// When this was last updated.
    pub updated_at: String,
    /// Tags for categorization.
    pub tags: Vec<String>,
}

impl Learning {
    /// Create a new learning.
    pub fn new(category: impl Into<String>, insight: impl Into<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            category: category.into(),
            insight: insight.into(),
            confidence: 0.5,
            confirmations: 1,
            applied_in_tasks: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            tags: Vec::new(),
        }
    }

    /// Set confidence.
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    /// Add a tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Record a confirmation.
    pub fn confirm(&mut self) {
        self.confirmations += 1;
        self.confidence = (self.confidence + 0.1).min(1.0);
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }

    /// Record an application in a task.
    pub fn apply_in_task(&mut self, task_id: &str) {
        if !self.applied_in_tasks.contains(&task_id.to_string()) {
            self.applied_in_tasks.push(task_id.to_string());
        }
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }
}

/// Long-term memory that persists across task runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LongTermMemory {
    /// Stored learnings.
    learnings: Vec<Learning>,
    /// Index by category.
    category_index: HashMap<String, Vec<usize>>,
}

impl LongTermMemory {
    /// Create new long-term memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a learning.
    pub fn store_learning(&mut self, learning: Learning) {
        let idx = self.learnings.len();
        let category = learning.category.clone();
        self.learnings.push(learning);

        // Update index
        self.category_index.entry(category).or_default().push(idx);
    }

    /// Get learnings by category.
    pub fn get_by_category(&self, category: &str) -> Vec<&Learning> {
        self.category_index
            .get(category)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&i| self.learnings.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all learnings sorted by confidence.
    pub fn get_by_confidence(&self, min_confidence: f32) -> Vec<&Learning> {
        let mut learnings: Vec<_> = self
            .learnings
            .iter()
            .filter(|l| l.confidence >= min_confidence)
            .collect();
        learnings.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        learnings
    }

    /// Search learnings by keyword.
    pub fn search(&self, query: &str) -> Vec<&Learning> {
        let query_lower = query.to_lowercase();
        self.learnings
            .iter()
            .filter(|l| {
                l.insight.to_lowercase().contains(&query_lower)
                    || l.category.to_lowercase().contains(&query_lower)
                    || l.tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    /// Get all learnings.
    pub fn all(&self) -> &[Learning] {
        &self.learnings
    }

    /// Get learning count.
    pub fn len(&self) -> usize {
        self.learnings.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.learnings.is_empty()
    }

    /// Find a learning by ID.
    pub fn find_by_id(&self, id: &str) -> Option<&Learning> {
        self.learnings.iter().find(|l| l.id == id)
    }

    /// Find a learning by ID (mutable).
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut Learning> {
        self.learnings.iter_mut().find(|l| l.id == id)
    }

    /// Get categories.
    pub fn categories(&self) -> Vec<&str> {
        self.category_index.keys().map(|s| s.as_str()).collect()
    }
}

// ============================================================================
// Entity Memory
// ============================================================================

/// An entity tracked in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier.
    pub id: String,
    /// Entity name.
    pub name: String,
    /// Entity type.
    pub entity_type: EntityType,
    /// Attributes of this entity.
    pub attributes: HashMap<String, serde_json::Value>,
    /// IDs of related entities.
    pub relationships: Vec<Relationship>,
    /// When first seen.
    pub first_seen: String,
    /// When last seen.
    pub last_seen: String,
    /// Number of times mentioned.
    pub mention_count: u32,
}

/// Types of entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    /// A file or document.
    File,
    /// A function or method.
    Function,
    /// A class or struct.
    Class,
    /// A module or package.
    Module,
    /// A variable or constant.
    Variable,
    /// An error or exception type.
    Error,
    /// A person or user.
    Person,
    /// A service or API.
    Service,
    /// A configuration item.
    Config,
    /// Custom entity type.
    Custom,
}

/// A relationship between entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    /// Type of relationship.
    pub relation_type: RelationType,
    /// Target entity ID.
    pub target_id: String,
    /// Confidence in this relationship.
    pub confidence: f32,
}

/// Types of relationships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// Calls or invokes.
    Calls,
    /// Is called by.
    CalledBy,
    /// Imports or uses.
    Imports,
    /// Imported by.
    ImportedBy,
    /// Contains or owns.
    Contains,
    /// Contained by.
    ContainedBy,
    /// Related to.
    RelatedTo,
    /// Depends on.
    DependsOn,
    /// Custom relationship.
    Custom,
}

impl Default for Entity {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            entity_type: EntityType::Custom,
            attributes: HashMap::new(),
            relationships: Vec::new(),
            first_seen: now.clone(),
            last_seen: now,
            mention_count: 1,
        }
    }
}

impl Entity {
    /// Create a new entity.
    pub fn new(name: impl Into<String>, entity_type: EntityType) -> Self {
        Self {
            name: name.into(),
            entity_type,
            ..Default::default()
        }
    }

    /// Add an attribute.
    pub fn with_attribute(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.attributes.insert(key.into(), value);
        self
    }

    /// Add a relationship.
    pub fn with_relationship(mut self, relation_type: RelationType, target_id: String) -> Self {
        self.relationships.push(Relationship {
            relation_type,
            target_id,
            confidence: 1.0,
        });
        self
    }

    /// Record a mention.
    pub fn record_mention(&mut self) {
        self.mention_count += 1;
        self.last_seen = chrono::Utc::now().to_rfc3339();
    }
}

/// Entity memory for tracking named entities and relationships.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityMemory {
    /// All entities by ID.
    entities: HashMap<String, Entity>,
    /// Name to ID index.
    name_index: HashMap<String, String>,
    /// Type to IDs index.
    type_index: HashMap<EntityType, Vec<String>>,
}

impl EntityMemory {
    /// Create new entity memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update an entity.
    pub fn upsert(&mut self, entity: Entity) {
        let id = entity.id.clone();
        let name = entity.name.clone();
        let entity_type = entity.entity_type;

        // Update indices
        self.name_index.insert(name, id.clone());
        self.type_index
            .entry(entity_type)
            .or_default()
            .push(id.clone());

        // Insert or update
        if let Some(existing) = self.entities.get_mut(&id) {
            existing.record_mention();
            existing.attributes.extend(entity.attributes);
            existing.relationships.extend(entity.relationships);
        } else {
            self.entities.insert(id, entity);
        }
    }

    /// Get entity by ID.
    pub fn get(&self, id: &str) -> Option<&Entity> {
        self.entities.get(id)
    }

    /// Get entity by name.
    pub fn get_by_name(&self, name: &str) -> Option<&Entity> {
        self.name_index
            .get(name)
            .and_then(|id| self.entities.get(id))
    }

    /// Get entities by type.
    pub fn get_by_type(&self, entity_type: EntityType) -> Vec<&Entity> {
        self.type_index
            .get(&entity_type)
            .map(|ids| ids.iter().filter_map(|id| self.entities.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get related entities.
    pub fn get_related(&self, id: &str) -> Vec<&Entity> {
        self.entities
            .get(id)
            .map(|entity| {
                entity
                    .relationships
                    .iter()
                    .filter_map(|rel| self.entities.get(&rel.target_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all entities.
    pub fn all(&self) -> Vec<&Entity> {
        self.entities.values().collect()
    }

    /// Get entity count.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Clear all entities.
    pub fn clear(&mut self) {
        self.entities.clear();
        self.name_index.clear();
        self.type_index.clear();
    }
}

// ============================================================================
// Memory System
// ============================================================================

/// Complete memory system with all tiers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemorySystem {
    /// Short-term memory (current iteration).
    pub short_term: ShortTermMemory,
    /// Long-term memory (persists across runs).
    pub long_term: LongTermMemory,
    /// Entity memory (tracked entities).
    pub entities: EntityMemory,
    /// Memory system metadata.
    pub metadata: MemoryMetadata,
}

/// Metadata about the memory system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryMetadata {
    /// When this memory system was created.
    pub created_at: String,
    /// Total items processed.
    pub total_items_processed: u64,
    /// Current task ID (if any).
    pub current_task_id: Option<String>,
}

impl MemorySystem {
    /// Create a new memory system.
    pub fn new() -> Self {
        Self {
            short_term: ShortTermMemory::new(),
            long_term: LongTermMemory::new(),
            entities: EntityMemory::new(),
            metadata: MemoryMetadata {
                created_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            },
        }
    }

    /// Set the current task.
    pub fn set_task(&mut self, task_id: &str) {
        self.metadata.current_task_id = Some(task_id.to_string());
    }

    /// Clear short-term memory (for new iteration).
    pub fn clear_short_term(&mut self) {
        self.short_term.clear();
    }

    /// Add an observation to short-term memory.
    pub fn observe(&mut self, content: impl Into<String>) {
        let item = MemoryItem::observation(content);
        self.short_term.add(item);
        self.metadata.total_items_processed += 1;
    }

    /// Record a decision.
    pub fn record_decision(&mut self, content: impl Into<String>) {
        let item = MemoryItem::decision(content);
        self.short_term.add(item);
        self.metadata.total_items_processed += 1;
    }

    /// Record an action.
    pub fn record_action(&mut self, content: impl Into<String>) {
        let item = MemoryItem::action(content);
        self.short_term.add(item);
        self.metadata.total_items_processed += 1;
    }

    /// Record an error.
    pub fn record_error(&mut self, content: impl Into<String>) {
        let item = MemoryItem::error(content);
        self.short_term.add(item);
        self.metadata.total_items_processed += 1;
    }

    /// Learn something new.
    pub fn learn(&mut self, category: impl Into<String>, insight: impl Into<String>) {
        let learning = Learning::new(category, insight);
        self.long_term.store_learning(learning);
    }

    /// Track an entity.
    pub fn track_entity(&mut self, name: impl Into<String>, entity_type: EntityType) {
        let entity = Entity::new(name, entity_type);
        self.entities.upsert(entity);
    }

    /// Get a summary of the memory system state.
    pub fn summary(&self) -> MemorySummary {
        MemorySummary {
            short_term_items: self.short_term.len(),
            long_term_learnings: self.long_term.len(),
            tracked_entities: self.entities.len(),
            total_processed: self.metadata.total_items_processed,
        }
    }

    /// Build context from recent memory for AI prompt.
    ///
    /// Sorts short-term items by importance × recency to surface the most
    /// relevant context first. High-importance recent items appear before
    /// low-importance older items.
    pub fn build_context(&self, max_items: usize) -> String {
        let mut context = String::new();

        // Get all short-term items, sorted by importance × recency
        let mut items: Vec<&MemoryItem> = self.short_term.all().iter().collect();
        let now = chrono::Utc::now();
        items.sort_by(|a, b| {
            let score_a = importance_recency_score(a, &now);
            let score_b = importance_recency_score(b, &now);
            score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        let selected: Vec<&MemoryItem> = items.into_iter().take(max_items).collect();
        if !selected.is_empty() {
            context.push_str("## Recent Context\n");
            for item in &selected {
                context.push_str(&format!("- [{}] {}\n", item.item_type_str(), item.content));
            }
            context.push('\n');
        }

        // Relevant learnings (high confidence)
        let learnings: Vec<_> = self.long_term.get_by_confidence(0.7);
        if !learnings.is_empty() {
            context.push_str("\n## Relevant Learnings\n");
            for learning in learnings.iter().take(5) {
                context.push_str(&format!("- [{}] {}\n", learning.category, learning.insight));
            }
        }

        context
    }

    /// Build context from memory for AI prompt, augmented with unified memory query.
    ///
    /// Uses `build_context()` as the base and appends results from the unified
    /// memory query (PG + SQLite + graph) when available. Falls back gracefully
    /// to just the base context if the unified query fails or returns no results.
    pub async fn build_context_unified(
        &self,
        max_items: usize,
        task_description: &str,
        pg: &PgDb,
        db: Arc<CheckpointDb>,
    ) -> String {
        let mut context = self.build_context(max_items);

        let params = UnifiedMemoryQuery {
            query: task_description.to_string(),
            limit: 10,
            sources: None,
            from: None,
            to: None,
            min_score: Some(0.3),
        };

        match unified_query::query_memory(&params, pg, db, None).await {
            Ok(results) if !results.is_empty() => {
                context.push_str("\n## Unified Memory\n");
                for r in results.iter().take(10) {
                    context.push_str(&format!(
                        "- [{}] {} (score: {:.2})\n  {}\n",
                        serde_json::to_string(&r.source)
                            .unwrap_or_default()
                            .trim_matches('"'),
                        r.title,
                        r.fused_score,
                        r.snippet.chars().take(200).collect::<String>(),
                    ));
                }
            }
            _ => {} // Graceful fallback — unified memory is optional
        }

        context
    }
}

/// Summary statistics for memory system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySummary {
    pub short_term_items: usize,
    pub long_term_learnings: usize,
    pub tracked_entities: usize,
    pub total_processed: u64,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_term_memory() {
        let mut stm = ShortTermMemory::new();
        stm.add(MemoryItem::observation("First observation"));
        stm.add(MemoryItem::observation("Second observation"));

        assert_eq!(stm.len(), 2);
        assert_eq!(stm.get_recent(1).len(), 1);
    }

    #[test]
    fn test_short_term_memory_capacity() {
        let mut stm = ShortTermMemory::with_capacity(3);
        for i in 0..5 {
            stm.add(MemoryItem::observation(format!("Item {}", i)));
        }

        assert_eq!(stm.len(), 3);
        // Should have items 2, 3, 4 (oldest evicted)
    }

    #[test]
    fn test_short_term_by_type() {
        let mut stm = ShortTermMemory::new();
        stm.add(MemoryItem::observation("Obs 1"));
        stm.add(MemoryItem::decision("Dec 1"));
        stm.add(MemoryItem::observation("Obs 2"));

        let observations = stm.get_by_type(MemoryItemType::Observation);
        assert_eq!(observations.len(), 2);

        let decisions = stm.get_by_type(MemoryItemType::Decision);
        assert_eq!(decisions.len(), 1);
    }

    #[test]
    fn test_long_term_memory() {
        let mut ltm = LongTermMemory::new();
        ltm.store_learning(Learning::new("testing", "Always mock external services"));
        ltm.store_learning(Learning::new("testing", "Use fixtures for database tests"));
        ltm.store_learning(Learning::new("debugging", "Check logs first"));

        assert_eq!(ltm.len(), 3);
        assert_eq!(ltm.get_by_category("testing").len(), 2);
        assert_eq!(ltm.get_by_category("debugging").len(), 1);
    }

    #[test]
    fn test_learning_confirmation() {
        let mut learning = Learning::new("test", "Test insight");
        assert_eq!(learning.confirmations, 1);
        assert!(learning.confidence < 0.6);

        learning.confirm();
        assert_eq!(learning.confirmations, 2);
        assert!(learning.confidence >= 0.6);
    }

    #[test]
    fn test_entity_memory() {
        let mut em = EntityMemory::new();
        em.upsert(Entity::new("main.rs", EntityType::File));
        em.upsert(Entity::new("calculate_total", EntityType::Function));

        assert_eq!(em.len(), 2);
        assert!(em.get_by_name("main.rs").is_some());
        assert_eq!(em.get_by_type(EntityType::File).len(), 1);
    }

    #[test]
    fn test_entity_relationships() {
        let mut em = EntityMemory::new();

        let main = Entity::new("main.rs", EntityType::File);
        let main_id = main.id.clone();
        em.upsert(main);

        let func = Entity::new("run", EntityType::Function)
            .with_relationship(RelationType::ContainedBy, main_id.clone());
        em.upsert(func);

        let related = em.get_related(&main_id);
        // main.rs doesn't have relationships, but run has a relationship to main.rs
        assert_eq!(related.len(), 0);
    }

    #[test]
    fn test_memory_system() {
        let mut memory = MemorySystem::new();

        memory.observe("Found a bug in login");
        memory.record_decision("Will fix by updating auth logic");
        memory.record_action("Modified auth.rs");
        memory.learn("authentication", "Session tokens expire after 1 hour");
        memory.track_entity("auth.rs", EntityType::File);

        let summary = memory.summary();
        assert_eq!(summary.short_term_items, 3);
        assert_eq!(summary.long_term_learnings, 1);
        assert_eq!(summary.tracked_entities, 1);
    }

    #[test]
    fn test_build_context() {
        let mut memory = MemorySystem::new();
        memory.observe("User reported login issue");
        memory.record_decision("Checking authentication flow");

        let context = memory.build_context(10);
        assert!(context.contains("Recent Context"));
        assert!(context.contains("login issue"));
    }

    #[test]
    fn test_memory_item_builder() {
        let item = MemoryItem::observation("Test")
            .with_source("test_worker")
            .with_importance(0.9)
            .with_tag("critical");

        assert_eq!(item.source, "test_worker");
        assert!((item.importance - 0.9).abs() < 0.01);
        assert!(item.tags.contains(&"critical".to_string()));
    }
}
