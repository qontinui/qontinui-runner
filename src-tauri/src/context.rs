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

/// Runner-specific metadata for a context.
///
/// This extends the core Context with fields specific to the runner:
/// - enabled: allows disabling without deleting
/// - use_count: tracks popularity
/// - last_used_at: for sorting by recency
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

1. **Check logs first** - Use the debugging API endpoints or read log files directly
2. **Identify the root cause** - Don't fix symptoms, fix the source
3. **Work autonomously** - Restart services as needed, don't ask the user
4. **Iterate until fixed** - Make changes, test, repeat

### Log Locations
- Backend: `.dev-logs/backend.log`
- Frontend: `.dev-logs/frontend.log`
- Runner: `.dev-logs/runner-tauri.log`
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
            modified_at: now,
        },
    ]
}
