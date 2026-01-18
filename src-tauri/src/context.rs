//! AI Context System
//!
//! This module provides storage and management for AI contexts - reusable knowledge
//! snippets that provide domain-specific guidance to AI tasks.
//!
//! Contexts can be:
//! - Project-scoped: Stored in the project config, exported with the project
//! - User-scoped: Stored locally in the runner, personal to the user
//! - Built-in: Shipped with the runner, read-only examples
//!
//! The core Context type matches qontinui-schemas. Runner-specific fields
//! (scope, enabled, usage stats) are stored separately in ContextMetadata.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const CONTEXTS_FILE: &str = "contexts.json";

// ============================================================================
// Core Types (matching qontinui-schemas)
// ============================================================================

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

// ============================================================================
// Runner-Specific Extensions
// ============================================================================

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

// ============================================================================
// User Context Library (stored locally)
// ============================================================================

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

// ============================================================================
// File Operations
// ============================================================================

/// Get the contexts directory path in the app data directory
fn get_contexts_dir() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner")
        .join("contexts");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create contexts directory: {}", e))?;
    }

    Ok(app_data_dir)
}

/// Get the contexts file path
fn get_contexts_path() -> Result<PathBuf, String> {
    get_contexts_dir().map(|dir| dir.join(CONTEXTS_FILE))
}

/// Load the user context library from disk
pub fn load_user_context_library() -> UserContextLibrary {
    match get_contexts_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(library) => {
                            info!("Loaded user context library from {:?}", path);
                            library
                        }
                        Err(e) => {
                            error!("Failed to parse contexts file: {}", e);
                            UserContextLibrary::new()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read contexts file: {}", e);
                        UserContextLibrary::new()
                    }
                }
            } else {
                info!("No contexts file found, using empty library");
                UserContextLibrary::new()
            }
        }
        Err(e) => {
            error!("Failed to get contexts path: {}", e);
            UserContextLibrary::new()
        }
    }
}

/// Save the user context library to disk
pub fn save_user_context_library(library: &UserContextLibrary) -> Result<(), String> {
    let path = get_contexts_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize contexts: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write contexts file: {}", e))?;

    info!("Saved user context library to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations for User Contexts
// ============================================================================

/// Get all user contexts
pub fn get_all_user_contexts() -> Vec<Context> {
    load_user_context_library().contexts
}

/// Get a user context by ID
pub fn get_user_context(id: &str) -> Option<Context> {
    load_user_context_library()
        .contexts
        .into_iter()
        .find(|c| c.id == id)
}

/// Create a new user context
pub fn create_user_context(
    name: String,
    content: String,
    category: Option<String>,
    tags: Vec<String>,
    auto_include: Option<ContextAutoInclude>,
) -> Result<Context, String> {
    let mut library = load_user_context_library();

    let context = Context::new(name, content, category, tags, auto_include);
    let created = context.clone();

    // Add context and its metadata
    library.contexts.push(context);
    library
        .metadata
        .push(ContextMetadata::new(created.id.clone()));

    save_user_context_library(&library)?;

    info!("Created user context: {} ({})", created.name, created.id);
    Ok(created)
}

/// Update an existing user context
pub fn update_user_context(
    id: &str,
    name: Option<String>,
    content: Option<String>,
    category: Option<Option<String>>,
    tags: Option<Vec<String>>,
    auto_include: Option<Option<ContextAutoInclude>>,
) -> Result<Context, String> {
    let mut library = load_user_context_library();

    let context = library
        .contexts
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("Context not found: {}", id))?;

    // Update fields if provided
    if let Some(name) = name {
        context.name = name;
    }
    if let Some(content) = content {
        context.content = content;
    }
    if let Some(category) = category {
        context.category = category;
    }
    if let Some(tags) = tags {
        context.tags = tags;
    }
    if let Some(auto_include) = auto_include {
        context.auto_include = auto_include;
    }

    // Update modification timestamp
    context.modified_at = chrono::Utc::now().to_rfc3339();

    let updated = context.clone();
    save_user_context_library(&library)?;

    info!("Updated user context: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Delete a user context
pub fn delete_user_context(id: &str) -> Result<(), String> {
    let mut library = load_user_context_library();

    let initial_len = library.contexts.len();
    library.contexts.retain(|c| c.id != id);

    if library.contexts.len() == initial_len {
        return Err(format!("Context not found: {}", id));
    }

    // Also remove metadata
    library.metadata.retain(|m| m.context_id != id);

    save_user_context_library(&library)?;
    info!("Deleted user context: {}", id);
    Ok(())
}

/// Get all unique categories from user contexts
pub fn get_user_context_categories() -> Vec<String> {
    let library = load_user_context_library();
    let mut categories: Vec<String> = library
        .contexts
        .iter()
        .filter_map(|c| c.category.clone())
        .filter(|c| !c.is_empty())
        .collect();

    categories.sort();
    categories.dedup();
    categories
}

/// Search user contexts by query (searches name, content, category, and tags)
pub fn search_user_contexts(query: &str) -> Vec<Context> {
    let query_lower = query.to_lowercase();
    load_user_context_library()
        .contexts
        .into_iter()
        .filter(|c| {
            c.name.to_lowercase().contains(&query_lower)
                || c.content.to_lowercase().contains(&query_lower)
                || c.category
                    .as_ref()
                    .map(|cat| cat.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
                || c.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

// ============================================================================
// Context Metadata Operations
// ============================================================================

/// Record that a context was used (increments use_count, updates last_used_at)
pub fn record_context_use(context_id: &str) -> Result<(), String> {
    let mut library = load_user_context_library();
    let metadata = library.get_or_create_metadata(context_id);
    metadata.record_use();
    save_user_context_library(&library)?;
    info!("Recorded use of context: {}", context_id);
    Ok(())
}

/// Enable or disable a context
pub fn set_context_enabled(context_id: &str, enabled: bool) -> Result<(), String> {
    let mut library = load_user_context_library();
    let metadata = library.get_or_create_metadata(context_id);
    metadata.enabled = enabled;
    save_user_context_library(&library)?;
    info!(
        "Set context {} enabled={}",
        context_id,
        if enabled { "true" } else { "false" }
    );
    Ok(())
}

/// Set the web sync status for a context
pub fn set_web_sync_status(context_id: &str, status: Option<WebSyncStatus>) -> Result<(), String> {
    let mut library = load_user_context_library();
    let metadata = library.get_or_create_metadata(context_id);
    metadata.web_sync_status = status.clone();
    save_user_context_library(&library)?;
    info!("Set context {} web_sync_status={:?}", context_id, status);
    Ok(())
}

/// Get metadata for a context
pub fn get_context_metadata(context_id: &str) -> Option<ContextMetadata> {
    load_user_context_library()
        .metadata
        .into_iter()
        .find(|m| m.context_id == context_id)
}

// ============================================================================
// Auto-Include Evaluation
// ============================================================================

/// Evaluate if a context should be auto-included based on its rules.
///
/// Returns true if any of the auto-include rules match.
pub fn should_auto_include(
    context: &Context,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> bool {
    let Some(ref rules) = context.auto_include else {
        return false;
    };

    let task_lower = task_prompt.to_lowercase();

    // Check task mentions
    if let Some(ref mentions) = rules.task_mentions {
        if mentions
            .iter()
            .any(|m| task_lower.contains(&m.to_lowercase()))
        {
            return true;
        }
    }

    // Check action types
    if let Some(ref types) = rules.action_types {
        if types
            .iter()
            .any(|t| action_types.iter().any(|a| a.eq_ignore_ascii_case(t)))
        {
            return true;
        }
    }

    // Check error patterns (regex)
    if let Some(ref patterns) = rules.error_patterns {
        for pattern in patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                if recent_errors.iter().any(|e| re.is_match(e)) {
                    return true;
                }
            }
        }
    }

    false
}

// ============================================================================
// Context Resolution and Injection
// ============================================================================

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

/// Resolve which contexts should be included in a prompt.
///
/// This function:
/// 1. Looks up explicit context IDs from user, project, and builtin sources
/// 2. If auto_detect is true, evaluates all enabled contexts against the task/config
/// 3. Merges and deduplicates the results
///
/// # Arguments
/// * `explicit_ids` - Context IDs explicitly selected by the user
/// * `auto_detect` - Whether to automatically detect applicable contexts
/// * `task_prompt` - The task prompt (for task mention matching)
/// * `action_types` - Action types from loaded config (for action type matching)
/// * `recent_errors` - Recent error messages (for error pattern matching)
pub fn resolve_contexts(
    explicit_ids: &[String],
    auto_detect: bool,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> ResolvedContexts {
    let mut result = ResolvedContexts::default();
    let mut included_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Gather all available contexts (user + builtin)
    let user_contexts = get_all_user_contexts();
    let builtin_contexts = get_builtin_contexts();
    let library = load_user_context_library();

    // Create a lookup map
    let all_contexts: Vec<&Context> = user_contexts
        .iter()
        .chain(builtin_contexts.iter())
        .collect();

    // Step 1: Resolve explicit IDs
    for id in explicit_ids {
        if included_ids.contains(id) {
            continue;
        }

        if let Some(ctx) = all_contexts.iter().find(|c| c.id == *id) {
            // Check if enabled (for user contexts)
            let is_enabled = library
                .metadata
                .iter()
                .find(|m| m.context_id == *id)
                .map(|m| m.enabled)
                .unwrap_or(true); // Builtin contexts are always enabled

            if is_enabled {
                result.explicit.push((*ctx).clone());
                included_ids.insert(id.clone());
            }
        }
    }

    // Step 2: Auto-detect if enabled
    if auto_detect {
        for ctx in &all_contexts {
            if included_ids.contains(&ctx.id) {
                continue;
            }

            // Check if enabled
            let is_enabled = library
                .metadata
                .iter()
                .find(|m| m.context_id == ctx.id)
                .map(|m| m.enabled)
                .unwrap_or(true);

            if !is_enabled {
                continue;
            }

            // Check auto-include rules
            if let Some(ref rules) = ctx.auto_include {
                let task_lower = task_prompt.to_lowercase();

                // Check task mentions
                if let Some(ref mentions) = rules.task_mentions {
                    for mention in mentions {
                        if task_lower.contains(&mention.to_lowercase()) {
                            result.auto_detected.push((*ctx).clone());
                            result.auto_detect_reasons.push(AutoDetectReason {
                                context_id: ctx.id.clone(),
                                reason: "taskMention".to_string(),
                                matched_trigger: mention.clone(),
                            });
                            included_ids.insert(ctx.id.clone());
                            break;
                        }
                    }
                }

                // Skip if already included
                if included_ids.contains(&ctx.id) {
                    continue;
                }

                // Check action types
                if let Some(ref types) = rules.action_types {
                    for action_type in types {
                        if action_types
                            .iter()
                            .any(|a| a.eq_ignore_ascii_case(action_type))
                        {
                            result.auto_detected.push((*ctx).clone());
                            result.auto_detect_reasons.push(AutoDetectReason {
                                context_id: ctx.id.clone(),
                                reason: "actionType".to_string(),
                                matched_trigger: action_type.clone(),
                            });
                            included_ids.insert(ctx.id.clone());
                            break;
                        }
                    }
                }

                // Skip if already included
                if included_ids.contains(&ctx.id) {
                    continue;
                }

                // Check error patterns (regex)
                if let Some(ref patterns) = rules.error_patterns {
                    for pattern in patterns {
                        if let Ok(re) = regex::Regex::new(pattern) {
                            for error in recent_errors {
                                if re.is_match(error) {
                                    result.auto_detected.push((*ctx).clone());
                                    result.auto_detect_reasons.push(AutoDetectReason {
                                        context_id: ctx.id.clone(),
                                        reason: "errorPattern".to_string(),
                                        matched_trigger: pattern.clone(),
                                    });
                                    included_ids.insert(ctx.id.clone());
                                    break;
                                }
                            }
                        }
                        if included_ids.contains(&ctx.id) {
                            break;
                        }
                    }
                }
            }
        }
    }

    result
}

/// Format a single context for injection into a prompt.
///
/// The format uses XML-like tags for clear delineation:
/// ```text
/// <context name="Context Name" category="category">
/// [Context content here]
/// </context>
/// ```
fn format_context(ctx: &Context) -> String {
    let category_attr = ctx
        .category
        .as_ref()
        .map(|c| format!(" category=\"{}\"", c))
        .unwrap_or_default();

    format!(
        "<context name=\"{}\"{}>\n{}\n</context>",
        ctx.name, category_attr, ctx.content
    )
}

/// Format resolved contexts into a prompt section for injection.
///
/// Returns None if there are no contexts to inject.
/// Returns Some(String) with formatted contexts if there are any.
pub fn format_contexts_for_prompt(resolved: &ResolvedContexts) -> Option<String> {
    let all_contexts: Vec<&Context> = resolved
        .explicit
        .iter()
        .chain(resolved.auto_detected.iter())
        .collect();

    if all_contexts.is_empty() {
        return None;
    }

    let mut output = String::new();
    output.push_str("## Relevant Context\n\n");
    output.push_str("The following context has been provided to guide your response:\n\n");

    for ctx in &all_contexts {
        output.push_str(&format_context(ctx));
        output.push_str("\n\n");
    }

    output.push_str("---\n\n");

    Some(output)
}

/// Inject contexts into a prompt.
///
/// This is the main entry point for prompt enhancement. It:
/// 1. Resolves which contexts to include (explicit + auto-detected)
/// 2. Formats the contexts
/// 3. Prepends them to the original prompt
/// 4. Returns the enhanced prompt and the list of context IDs that were used
///
/// # Arguments
/// * `base_prompt` - The original prompt content
/// * `context_ids` - Explicitly selected context IDs (can be empty)
/// * `auto_detect` - Whether to auto-detect additional contexts
/// * `task_prompt` - Task prompt for auto-detection (often same as base_prompt)
/// * `action_types` - Action types from loaded config
/// * `recent_errors` - Recent error messages for error pattern matching
///
/// # Returns
/// A tuple of (enhanced_prompt, context_ids_used)
pub fn inject_contexts(
    base_prompt: &str,
    context_ids: &[String],
    auto_detect: bool,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> (String, Vec<String>) {
    let resolved = resolve_contexts(
        context_ids,
        auto_detect,
        task_prompt,
        action_types,
        recent_errors,
    );

    // Collect all context IDs that were used
    let used_ids: Vec<String> = resolved
        .explicit
        .iter()
        .chain(resolved.auto_detected.iter())
        .map(|c| c.id.clone())
        .collect();

    // Format contexts for injection
    let context_section = format_contexts_for_prompt(&resolved);

    // Build the enhanced prompt
    let enhanced = match context_section {
        Some(section) => format!("{}{}", section, base_prompt),
        None => base_prompt.to_string(),
    };

    (enhanced, used_ids)
}

/// Record that multiple contexts were used in a task.
///
/// This updates use_count and last_used_at for each context.
pub fn record_contexts_used(context_ids: &[String]) {
    for id in context_ids {
        if let Err(e) = record_context_use(id) {
            tracing::warn!("Failed to record context use for {}: {}", id, e);
        }
    }
}

// ============================================================================
// Built-in Contexts
// ============================================================================

/// Get built-in contexts shipped with the runner.
///
/// These are read-only example contexts that users can copy to their own library.
pub fn get_builtin_contexts() -> Vec<Context> {
    let now = chrono::Utc::now().to_rfc3339();

    vec![
        Context {
            id: "builtin-debugging".to_string(),
            name: "Debugging Guide".to_string(),
            content: r#"## Debugging Guide

When debugging issues:

1. **Check the Iteration Bundle first** - Look for `## Iteration N Data Bundle` in your prompt
   - Pre-Execution Results show step success/failure
   - Application Logs are captured and included
   - Errors and warnings are highlighted
2. **Identify the root cause** - Don't fix symptoms, fix the source
3. **Work autonomously** - Restart services as needed, don't ask the user
4. **Iterate until fixed** - Make changes, test, repeat

### Log Data is Bundled

All relevant logs are delivered in the Iteration Bundle - no file searching required:
- Application logs from user-defined sources
- GUI automation events (if workflow has GUI steps)
- Playwright test results (if workflow has test steps)
- Screenshots from automation steps

Only access raw log files in `.dev-logs/` as a fallback if bundled data is insufficient.
"#
            .to_string(),
            category: Some("debugging".to_string()),
            tags: vec!["debugging".to_string(), "logs".to_string()],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "debug".to_string(),
                    "error".to_string(),
                    "fix".to_string(),
                    "issue".to_string(),
                ]),
                error_patterns: Some(vec!["error".to_string(), "exception".to_string()]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-no-backward-compat".to_string(),
            name: "No Backward Compatibility".to_string(),
            content: r#"## Project Philosophy: No Backward Compatibility

This project is in active development. Backward compatibility is NOT a priority.

### When You Find Legacy Code
- **Fix the source** - Don't add compatibility shims
- **Delete deprecated code** - Don't mark as @deprecated and leave it
- **Update schemas at the source** - Don't add normalization layers
- **Re-export old configs** - If an old config doesn't match, have the user re-export

### Anti-Patterns to Avoid
- Adding `|| legacyValue` fallbacks
- Creating migration layers for old formats
- Maintaining both old and new field names
- Adding "handle both cases" code
"#
            .to_string(),
            category: Some("philosophy".to_string()),
            tags: vec!["philosophy".to_string(), "standards".to_string()],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "legacy".to_string(),
                    "backward".to_string(),
                    "compatibility".to_string(),
                    "deprecated".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-runner-architecture".to_string(),
            name: "Runner Architecture & Logs".to_string(),
            content: r#"## Runner Architecture & Iteration Bundle System

The qontinui-runner executes GUI automation and spawns AI sessions. All relevant data is bundled and delivered to you automatically - no file searching required.

### CRITICAL: When Pre-Execution Runs

**Pre-execution steps (GUI automation, screenshots, Playwright tests) run at the START of each new iteration, BEFORE your AI session starts.**

The iteration lifecycle is:
1. **Runner starts new iteration** → Runs all pre-execution steps (clicks, screenshots, tests)
2. **Runner captures results** → Creates the Iteration Data Bundle
3. **Runner spawns AI session** → Delivers the bundle to you
4. **AI session works** → You analyze results, make code changes, debug
5. **AI session signals [WORK_COMPLETE]** → System runs verification
6. **System verifies** → Deterministic checks (build, tests) + AI verification (if configured)
7. **If verification fails** → Go to step 1 for next iteration with feedback
8. **If verification passes** → System marks task complete

**Key implications:**
- The Pre-Execution Results you see are from steps that ALREADY RAN
- To get NEW screenshots or automation results, you must END your current iteration
- You CANNOT re-run pre-execution steps mid-iteration - they only run at iteration start
- If you're resuming mid-iteration after a restart, pre-execution will NOT re-run

### Iteration Data Bundle

When your session starts, the runner provides a complete **Iteration Data Bundle** containing everything you need. Look for the `## Iteration N Data Bundle` section in your prompt. It contains:

1. **Pre-Execution Results** - Step-by-step results from automation (ALREADY COMPLETED)
   - Which steps succeeded or failed (with checkmarks/crosses)
   - Error messages for any failures
   - Screenshot paths for each step (use Read tool to view)
   - Duration for each step

2. **Application Logs** - User-defined log sources (e.g., backend.log, frontend.log)
   - Captured ONLY during this iteration
   - Errors and warnings are highlighted at the top
   - Full log content follows

3. **Runner GUI Automation Logs** (if workflow has GUI steps)
   - Image recognition table with template names, confidence, and thresholds
   - Failed matches explained with confidence vs threshold
   - Action timeline in JSON format

4. **Playwright Test Results** (if workflow has Playwright steps)
   - Overall pass/fail status
   - Individual test spec results
   - Error messages for failed tests
   - Failure screenshot paths

5. **Previous Iteration Findings** - Your structured findings from prior iterations
   - Status indicators (resolved, in progress, pending)
   - Helps you avoid repeating work

### Key Principles

- **Everything is bundled** - Don't search log files; data is delivered to you
- **Relevance-filtered** - Only logs relevant to your workflow's step types are included
- **Iteration-scoped** - Only data from the current iteration, not historical noise
- **Source-labeled** - Every piece of data says where it came from
- **Pre-execution is past tense** - Results show what ALREADY happened, not what will happen

### Start Here

1. Find the `## Iteration N Data Bundle` section (near the end of your prompt)
2. Check the Pre-Execution Results table for failures
3. If GUI steps failed, check the Image Recognition table for low confidence matches
4. If tests failed, check Playwright Results for error details
5. Review Application Logs for related errors

### Getting Fresh Screenshots

If you need new screenshots after making code changes:
1. Complete your current work (make fixes, commit if needed)
2. Signal `[WORK_COMPLETE]` to trigger verification (which includes fresh pre-execution)
3. If verification fails, you'll get a new iteration with fresh screenshots and test results
4. Your next AI session will receive updated data in the Iteration Bundle

**Do NOT try to manually trigger screenshots or re-run automation steps** - the system handles this as part of verification.

### Raw Log Files (Fallback Only)

If you need raw files (rare), they're in `.dev-logs/`:

| File | Contains |
|------|----------|
| `runner-general.jsonl` | Executor events |
| `runner-actions.jsonl` | Action execution |
| `runner-image-recognition.jsonl` | Template matching |
| `screenshots/` | Annotated screenshots |

### Accessing Data

**Your Iteration Bundle contains everything you need.** Don't search for log files - they're already provided.

If you need data NOT in the bundle (rare):

**Raw log files (.dev-logs/):**
| File | What it contains |
|------|------------------|
| `runner-general.jsonl` | Executor events |
| `runner-actions.jsonl` | Action/tree events |
| `runner-image-recognition.jsonl` | Image match results |
| `runner-playwright.jsonl` | Playwright test results |
| `screenshots/*.png` | Annotated screenshots |

**Database (direct file access):**
- Windows: `C:\Users\<USER>\AppData\Roaming\com.qontinui.runner\runner.db`
- macOS/Linux: `~/.config/com.qontinui.runner/runner.db`
- Tables: `task_runs` (status, output, findings), `run_details` (execution data)

**MCP tools (if available in your session):**
- `get_task_runs`, `get_task_run`, `read_runner_logs`, `list_screenshots`
- Note: MCP tools may not be available in all runner-spawned sessions

### What NOT to Do

- Don't search for log files - they're already in your Iteration Bundle
- Don't read source code to understand architecture - use this context
- Don't look at historical data from other sessions - focus on this iteration
- Don't try to re-run pre-execution steps - they already ran at iteration start
- Don't expect new screenshots mid-iteration - end the iteration first
"#
            .to_string(),
            category: Some("architecture".to_string()),
            tags: vec!["runner".to_string(), "logs".to_string(), "debugging".to_string(), "architecture".to_string()],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "runner".to_string(),
                    "log".to_string(),
                    "automation".to_string(),
                    "workflow".to_string(),
                    "screenshot".to_string(),
                    "execution".to_string(),
                ]),
                error_patterns: Some(vec![
                    "failed".to_string(),
                    "error".to_string(),
                    "not found".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-multi-step-guide".to_string(),
            name: "Multi-Step Task Guide".to_string(),
            content: r#"## Multi-Step Task Guide

You are a **Worker Agent** running in the qontinui-runner orchestrator system.

## ⚠️ CRITICAL: WORKER AGENTS CANNOT MARK TASKS COMPLETE

**You are a WORKER agent.** You signal when work is done, but the **SYSTEM** decides if the task is complete.

**How it works:**
1. You do work and signal `[WORK_COMPLETE]` when you believe the work is done
2. The system runs **deterministic verification** (build, tests, type checks)
3. If needed, the system runs **AI verification** (screenshot evaluation by a separate agent)
4. Only if ALL verification passes does the system mark the task complete

**You CANNOT declare task completion.** That's the orchestrator's job.

## Output Markers

### [WORK_COMPLETE] - Signal that you believe work is done

Output this when you've completed your work and want the system to verify:
```
I've fixed the validation bug in auth.ts. Build passes locally.

[WORK_COMPLETE]
```

**What happens after [WORK_COMPLETE]:**
1. System runs deterministic checks (build, tests, linting)
2. If deterministic checks pass, system may run AI verification (screenshot review)
3. If ALL verification passes → Task marked complete
4. If ANY verification fails → New iteration with feedback, you continue working

### [NEED_REPLAN] - Request plan revision

Output this when you discover the plan is fundamentally wrong:
```
[NEED_REPLAN] The validation errors aren't from the frontend. They're coming from
the i18n service's locale configuration. The plan should target the backend.
```

### [FINDING:type] - Record discoveries

Document important findings for the knowledge base:
```
[FINDING:root_cause] The login timeout is caused by a missing database index on user_sessions.created_at
[FINDING:bug] The retry logic doesn't handle 503 responses correctly
[FINDING:observation] The API returns stale cache data for 30 seconds after updates
```

## Key Architecture Rules

### YOU are a WORKER - What you CAN do:
- ✅ Make code changes
- ✅ Run tests locally (to check your work)
- ✅ Signal `[WORK_COMPLETE]` when you think you're done
- ✅ Request `[NEED_REPLAN]` if the approach is wrong
- ✅ Record `[FINDING:type]` discoveries

### The SYSTEM decides - What you CANNOT do:
- ❌ Declare the task complete (that's `[TASK_COMPLETE]` - deprecated, don't use)
- ❌ Skip verification ("trust me it works")
- ❌ Determine if your work is "good enough"

### Verification is NOT Optional

Even if you're confident your fix is correct, the system will verify:
- **Deterministic checks**: Build must pass, tests must pass, no type errors
- **AI verification** (if configured): Screenshot evaluation by isolated agent

If verification fails, you get feedback and continue. The loop continues until verification passes or max iterations are reached.

## How Iterations Work

```
TASK START:
  ├─ Planning phase (creates verification plan)
  │
  ├─ ITERATION 1:
  │   ├─ Pre-execution runs (screenshots, tests, GUI automation)
  │   ├─ Worker session starts with Iteration Bundle
  │   ├─ Worker works... outputs [WORK_COMPLETE]
  │   ├─ DETERMINISTIC VERIFICATION runs (build, tests)
  │   ├─ If fails → feedback generated, go to iteration 2
  │   │
  ├─ ITERATION 2:
  │   ├─ Pre-execution runs AGAIN (fresh results)
  │   ├─ Worker receives feedback from failed verification
  │   ├─ Worker fixes issues... outputs [WORK_COMPLETE]
  │   ├─ DETERMINISTIC VERIFICATION runs
  │   ├─ If passes → AI VERIFICATION runs (if configured)
  │   ├─ If passes → TASK COMPLETE (system decides)
  │   └─ If fails → feedback, continue...
  │
  └─ (continues until verification passes or max iterations)
```

### Verification Feedback

When verification fails, your next iteration includes feedback like:
```
## Deterministic Verification Failed

### ❌ build_success
- error: TS2345: Argument of type 'string' is not assignable to parameter of type 'number'
  at src/auth.ts:42

### ❌ unit_tests
- FAIL src/__tests__/auth.test.ts
  ● validateEmail › should reject invalid emails
    Expected: false, Received: true
```

Use this feedback to guide your next fix.

## Iteration Data Bundle

Each session receives a complete **Iteration Data Bundle**:

1. **Pre-Execution Results** - Automation step results (ALREADY COMPLETED)
2. **Application Logs** - Captured from user-defined log sources
3. **GUI Automation Logs** - Image recognition and action events
4. **Playwright Results** - Test results (if applicable)
5. **Previous Findings** - Your structured findings from prior iterations
6. **Verification Feedback** - Details on what failed (if this isn't iteration 1)

**Start with the bundle** - all relevant data is provided.

## Getting Fresh Results

If you made changes and want fresh screenshots/test results:

**Option 1: Signal work complete (recommended)**
```
I've fixed the CSS layout. Ready for verification.

[WORK_COMPLETE]
```
The system will run verification, which includes fresh pre-execution.

**Option 2: Let session end naturally**
Simply stop responding. The system will start a new iteration with fresh data.

## ❌ What You Should NEVER Do

**DO NOT manually trigger GUI automation.** The runner handles this.

| ❌ WRONG | ✅ CORRECT |
|----------|-----------|
| "Let me reload the config..." | "I've made the fix. [WORK_COMPLETE]" |
| "I'll call the runner API..." | "The system will verify my changes." |
| Output `[TASK_COMPLETE]` | Output `[WORK_COMPLETE]` |

**Your job:**
1. Analyze results from the Iteration Bundle
2. Make code fixes
3. Signal `[WORK_COMPLETE]` when done

**System's job (NOT yours):**
1. Run deterministic verification (build, tests)
2. Run AI verification (if configured)
3. Decide if task is complete
4. Generate feedback if verification fails

## Avoiding Infinite Loops

- Don't repeat the same fix that already failed
- Check Previous Findings - the issue may already be tracked
- If the plan is wrong, request `[NEED_REPLAN]`
- If stuck, explain what's blocking you
"#
            .to_string(),
            category: Some("workflow".to_string()),
            tags: vec!["multi-step".to_string(), "workflow".to_string(), "runner".to_string()],
            auto_include: None, // Always injected for runner-triggered sessions
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-input-validation".to_string(),
            name: "Input Validation Guide".to_string(),
            content: r#"## Input Validation: Debugging Click Position Issues

When "Capture Input for Validation" is enabled in the Advanced settings, the runner records actual mouse events and compares them to reported click positions. This helps debug coordinate calculation bugs.

### When to Use This Feature

Enable input validation when clicks are:
- Missing their targets despite correct image recognition
- Landing in wrong locations on multi-monitor setups
- Offset due to DPI scaling issues
- Affected by coordinate transformation bugs

### Understanding the Data

When enabled, the Iteration Bundle includes an **Input Validation** section with:

1. **Summary** - Overview of discrepancies found
   - Total clicks vs captured clicks
   - Number of significant discrepancies
   - Max/average offset in pixels

2. **Click Position Comparisons Table**
   - **Reported**: Where the automation engine said it would click
   - **Actual**: Where the mouse actually clicked (captured by input monitor)
   - **Δ px**: Distance between reported and actual positions
   - **Status**: ✅ OK, ⚠️ OFFSET, or ❓ N/A

3. **Highlighted Discrepancies** - Details on each significant offset

### Interpreting Results

| Discrepancy | Likely Cause |
|-------------|--------------|
| Consistent offset (e.g., always +1920px X) | Multi-monitor coordinate issue |
| Scaled offset (e.g., 1.5x or 2x) | DPI scaling not applied |
| Random small offsets (< 5px) | Normal - click targets still hit |
| Large variable offsets | Coordinate transformation bug |

### Common Fixes

**Multi-monitor offset**: Check that monitor bounds are correctly calculated and the target monitor index is correct.

**DPI scaling**: Ensure coordinates are scaled by the display's scale factor before clicking.

**Wrong monitor**: Verify the image recognition is searching the correct monitor and coordinates are relative to that monitor.

### Raw Events File

The raw input events are saved to `.dev-logs/input_events/{session_id}_events.jsonl` and include:
- All mouse clicks with exact timestamps and positions
- Mouse movements (sampled)
- Keyboard events
- Frame numbers for video correlation
"#
            .to_string(),
            category: Some("debugging".to_string()),
            tags: vec!["debugging".to_string(), "input".to_string(), "coordinates".to_string(), "multi-monitor".to_string()],
            auto_include: None, // Explicitly added when "Capture Input for Validation" is enabled
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-service-restart".to_string(),
            name: "Service Restart Commands".to_string(),
            content: r#"## Service Restart Commands

Use these commands to restart development services after making code changes.

### Restart Commands

**To restart services, use PowerShell:**

```powershell
# Restart backend (FastAPI)
cd {{WORKSPACE}}; .\dev-start.ps1 -Backend

# Restart frontend (Next.js)
cd {{WORKSPACE}}; .\dev-start.ps1 -Frontend

# Restart API service
cd {{WORKSPACE}}; .\dev-start.ps1 -Api

# Restart all services
cd {{WORKSPACE}}; .\dev-start.ps1 -All
```

**Alternative (if dev-start.ps1 is not available):**

```powershell
# Backend - kill and restart
Get-Process -Name python -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle -match 'backend' } | Stop-Process -Force
cd {{WORKSPACE}}\backend && poetry run python run.py

# Frontend - kill and restart
Get-Process -Name node -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -match 'next' } | Stop-Process -Force
cd {{WORKSPACE}}\frontend && npm run dev

# Tauri runner - restart
Stop-Process -Name qontinui-runner -Force -ErrorAction SilentlyContinue
cd {{WORKSPACE}}\qontinui-runner && npm run tauri dev
```

### Customization

This is a built-in context. To customize for your environment:

1. Go to **Contexts** tab in the runner
2. Create a new User Context named "Service Restart Commands"
3. Add your custom restart commands
4. Your version will override this built-in version

### Placeholder

`{{WORKSPACE}}` is replaced with your workspace root path at runtime.
"#
            .to_string(),
            category: Some("development".to_string()),
            tags: vec!["restart".to_string(), "services".to_string(), "development".to_string()],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "restart".to_string(),
                    "service".to_string(),
                    "backend".to_string(),
                    "frontend".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now,
        },
    ]
}

// ============================================================================
// Project Context Functions
// ============================================================================

/// Get all contexts from a loaded configuration.
///
/// Returns contexts stored in the project config as Context objects.
pub fn get_project_contexts_from_config(contexts_json: &[serde_json::Value]) -> Vec<Context> {
    contexts_json
        .iter()
        .filter_map(|v| serde_json::from_value::<Context>(v.clone()).ok())
        .collect()
}

/// Find a project context by ID in a configuration.
pub fn get_project_context_from_config(
    contexts_json: &[serde_json::Value],
    context_id: &str,
) -> Option<Context> {
    get_project_contexts_from_config(contexts_json)
        .into_iter()
        .find(|c| c.id == context_id)
}

/// Add a context to a configuration's contexts array.
///
/// Returns the updated contexts array.
pub fn add_project_context_to_config(
    contexts_json: &mut Vec<serde_json::Value>,
    context: Context,
) -> Result<(), String> {
    // Check for duplicate ID
    if get_project_context_from_config(contexts_json, &context.id).is_some() {
        return Err(format!("Context with ID '{}' already exists", context.id));
    }

    let json_value = serde_json::to_value(&context)
        .map_err(|e| format!("Failed to serialize context: {}", e))?;
    contexts_json.push(json_value);

    info!(
        "Added project context '{}' (id: {})",
        context.name, context.id
    );
    Ok(())
}

/// Update a context in a configuration's contexts array.
///
/// Returns the updated contexts array.
pub fn update_project_context_in_config(
    contexts_json: &mut Vec<serde_json::Value>,
    context: Context,
) -> Result<(), String> {
    let index = contexts_json
        .iter()
        .position(|v| {
            v.get("id")
                .and_then(|id| id.as_str())
                .map(|id| id == context.id)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("Context with ID '{}' not found", context.id))?;

    let json_value = serde_json::to_value(&context)
        .map_err(|e| format!("Failed to serialize context: {}", e))?;
    contexts_json[index] = json_value;

    info!(
        "Updated project context '{}' (id: {})",
        context.name, context.id
    );
    Ok(())
}

/// Delete a context from a configuration's contexts array.
pub fn delete_project_context_from_config(
    contexts_json: &mut Vec<serde_json::Value>,
    context_id: &str,
) -> Result<(), String> {
    let index = contexts_json
        .iter()
        .position(|v| {
            v.get("id")
                .and_then(|id| id.as_str())
                .map(|id| id == context_id)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("Context with ID '{}' not found", context_id))?;

    contexts_json.remove(index);

    info!("Deleted project context (id: {})", context_id);
    Ok(())
}

/// Create a new project context with the given details.
pub fn create_project_context(
    name: String,
    content: String,
    category: Option<String>,
    tags: Vec<String>,
    auto_include: Option<ContextAutoInclude>,
) -> Context {
    Context::new(name, content, category, tags, auto_include)
}

// ============================================================================
// Multi-Step Task Guide (special handling)
// ============================================================================

/// ID of the builtin Multi-Step Task Guide context.
pub const MULTI_STEP_GUIDE_ID: &str = "builtin-multi-step-guide";

/// ID of the builtin Service Restart Commands context.
pub const SERVICE_RESTART_ID: &str = "builtin-service-restart";

/// Get the Service Restart Commands context, preferring user override if exists.
///
/// Checks if the user has a context with the same name ("Service Restart Commands").
/// If so, returns the user's version (allowing customization).
/// Otherwise, returns the builtin version.
pub fn get_service_restart_commands() -> Context {
    // Check for user override by name
    let user_contexts = get_all_user_contexts();
    if let Some(user_override) = user_contexts
        .into_iter()
        .find(|c| c.name == "Service Restart Commands")
    {
        info!(
            "Using user-customized Service Restart Commands (id: {})",
            user_override.id
        );
        return user_override;
    }

    // Return builtin version
    get_builtin_contexts()
        .into_iter()
        .find(|c| c.id == SERVICE_RESTART_ID)
        .expect("Builtin Service Restart Commands should exist")
}

/// Get the Multi-Step Task Guide context, preferring user override if exists.
///
/// Checks if the user has a context with the same name ("Multi-Step Task Guide").
/// If so, returns the user's version (allowing customization).
/// Otherwise, returns the builtin version.
pub fn get_multi_step_guide() -> Context {
    // Check for user override by name
    let user_contexts = get_all_user_contexts();
    if let Some(user_override) = user_contexts
        .into_iter()
        .find(|c| c.name == "Multi-Step Task Guide")
    {
        info!(
            "Using user-customized Multi-Step Task Guide (id: {})",
            user_override.id
        );
        return user_override;
    }

    // Return builtin version
    get_builtin_contexts()
        .into_iter()
        .find(|c| c.id == MULTI_STEP_GUIDE_ID)
        .expect("Builtin Multi-Step Task Guide should exist")
}

/// Format a context for injection into a prompt (used for special contexts).
pub fn format_single_context(ctx: &Context) -> String {
    let category_attr = ctx
        .category
        .as_ref()
        .map(|c| format!(" category=\"{}\"", c))
        .unwrap_or_default();

    format!(
        "<context name=\"{}\"{}>\n{}\n</context>",
        ctx.name, category_attr, ctx.content
    )
}
