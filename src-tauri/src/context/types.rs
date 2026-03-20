//! Core type definitions for the AI context system.
//!
//! Contains types matching qontinui-schemas and runner-specific extensions.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Rules for automatically including a context in AI tasks.
///
/// When an AI task is created, the runner evaluates these rules to determine
/// which contexts should be automatically included. Multiple rules are OR'd
/// together (any match triggers inclusion).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextAutoInclude {
    /// Keywords in task prompt that trigger inclusion (case-insensitive)
    #[serde(
        default,
        rename = "taskMentions",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_mentions: Option<Vec<String>>,

    /// Action types in loaded config that trigger inclusion (e.g., 'CLICK', 'FIND')
    #[serde(
        default,
        rename = "actionTypes",
        skip_serializing_if = "Option::is_none"
    )]
    pub action_types: Option<Vec<String>>,

    /// Regex patterns in recent logs that trigger inclusion
    #[serde(
        default,
        rename = "errorPatterns",
        skip_serializing_if = "Option::is_none"
    )]
    pub error_patterns: Option<Vec<String>>,

    /// Glob patterns for files being worked on (e.g., '*.rs', 'src/api/**')
    #[serde(
        default,
        rename = "filePatterns",
        skip_serializing_if = "Option::is_none"
    )]
    pub file_patterns: Option<Vec<String>>,
}

/// AI context for providing domain knowledge to AI tasks.
///
/// This type matches the schema from qontinui-schemas and is used for
/// both project contexts (stored in config) and user contexts (stored locally).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Unique identifier (UUID v4 or prefixed like 'ctx-schema-flow')
    pub id: String,

    /// Human-readable name for display
    pub name: String,

    /// Markdown content injected into AI prompts
    pub content: String,

    /// Category for organization (e.g., 'architecture', 'debugging', 'philosophy')
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// Tags for flexible grouping and search
    #[serde(default)]
    pub tags: Vec<String>,

    /// Rules for automatic inclusion in AI tasks
    #[serde(
        default,
        rename = "autoInclude",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_include: Option<ContextAutoInclude>,

    /// ISO 8601 creation timestamp
    #[serde(rename = "createdAt")]
    pub created_at: String,

    /// ISO 8601 last modification timestamp
    #[serde(rename = "modifiedAt")]
    pub modified_at: String,
}

impl Context {
    /// Create a new context with the given details
    pub fn new(
        name: String,
        content: String,
        category: Option<String>,
        tags: Vec<String>,
        auto_include: Option<ContextAutoInclude>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: format!("ctx-{}", Uuid::new_v4()),
            name,
            content,
            category,
            tags,
            auto_include,
            created_at: now.clone(),
            modified_at: now,
        }
    }
}

/// Scope of a context - where it's stored and who can access it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContextScope {
    /// Stored in the project config, exported with the project
    Project,
    /// Stored locally in the runner, personal to the user
    User,
    /// Shipped with the runner, read-only
    Builtin,
}

/// Status of syncing a project context to qontinui-web
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebSyncStatus {
    /// Context needs user approval to sync
    Pending,
    /// Context has been synced to qontinui-web
    Synced,
    /// User dismissed the sync (chose not to sync)
    Dismissed,
}

/// Runner-specific metadata for a context.
///
/// This extends the core Context with fields specific to the runner:
/// - enabled: allows disabling without deleting
/// - use_count: tracks popularity
/// - last_used_at: for sorting by recency
/// - web_sync_status: for project contexts, tracks sync to qontinui-web
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    /// Reference to the context ID
    pub context_id: String,

    /// Whether this context is enabled (can be disabled without deleting)
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Number of times this context has been used
    #[serde(default)]
    pub use_count: u32,

    /// ISO 8601 timestamp of last use
    #[serde(
        default,
        rename = "lastUsedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<String>,

    /// Sync status for project contexts (pending approval to sync to qontinui-web)
    #[serde(
        default,
        rename = "webSyncStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub web_sync_status: Option<WebSyncStatus>,
}

fn default_enabled() -> bool {
    true
}

impl ContextMetadata {
    pub fn new(context_id: String) -> Self {
        Self {
            context_id,
            enabled: true,
            use_count: 0,
            last_used_at: None,
            web_sync_status: None,
        }
    }

    pub fn record_use(&mut self) {
        self.use_count += 1;
        self.last_used_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

/// A context with its runner-specific metadata combined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextWithMetadata {
    /// The core context data
    #[serde(flatten)]
    pub context: Context,

    /// The scope of this context
    pub scope: ContextScope,

    /// Whether this context is enabled
    pub enabled: bool,

    /// Number of times this context has been used
    #[serde(default, rename = "useCount")]
    pub use_count: u32,

    /// ISO 8601 timestamp of last use
    #[serde(
        default,
        rename = "lastUsedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<String>,

    /// Sync status for project contexts (pending approval to sync to qontinui-web)
    #[serde(
        default,
        rename = "webSyncStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub web_sync_status: Option<WebSyncStatus>,
}

/// The user's local context library containing user-created contexts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserContextLibrary {
    /// Schema version for future migrations
    pub version: String,

    /// User-created contexts
    pub contexts: Vec<Context>,

    /// Metadata for all contexts (user, project, builtin)
    pub metadata: Vec<ContextMetadata>,
}

impl UserContextLibrary {
    /// Create a new empty library
    pub fn new() -> Self {
        Self {
            version: "1.0.0".to_string(),
            contexts: Vec::new(),
            metadata: Vec::new(),
        }
    }

    /// Get metadata for a context, creating default if not exists
    pub fn get_or_create_metadata(&mut self, context_id: &str) -> &mut ContextMetadata {
        if !self.metadata.iter().any(|m| m.context_id == context_id) {
            self.metadata
                .push(ContextMetadata::new(context_id.to_string()));
        }
        self.metadata
            .iter_mut()
            .find(|m| m.context_id == context_id)
            .unwrap()
    }
}

/// Result of resolving which contexts to include
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResolvedContexts {
    /// Contexts that were explicitly selected by ID
    pub explicit: Vec<Context>,
    /// Contexts that were auto-detected based on rules
    pub auto_detected: Vec<Context>,
    /// Auto-detection details for each context
    pub auto_detect_reasons: Vec<AutoDetectReason>,
}

/// Reason a context was auto-detected
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDetectReason {
    /// Context ID
    pub context_id: String,
    /// Why this context was auto-detected
    pub reason: String,
    /// The specific trigger that matched
    pub matched_trigger: String,
}
