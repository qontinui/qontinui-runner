//! Context commands
//!
//! This module handles AI context CRUD operations:
//! - Get all contexts (user, project, builtin combined)
//! - Create, update, delete user contexts
//! - Search contexts
//! - Enable/disable contexts
//! - Record context usage
//! - Evaluate auto-include rules

use crate::context::{self, Context, ContextAutoInclude, ContextScope, ContextWithMetadata};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::CommandResponse;

// ============================================================================
// Request/Response Types
// ============================================================================

/// Request to create a new context
#[derive(Debug, Deserialize)]
pub struct CreateContextRequest {
    pub name: String,
    pub content: String,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    #[serde(rename = "autoInclude")]
    pub auto_include: Option<ContextAutoInclude>,
}

/// Request to update an existing context
#[derive(Debug, Deserialize)]
pub struct UpdateContextRequest {
    pub name: Option<String>,
    pub content: Option<String>,
    pub category: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    #[serde(rename = "autoInclude")]
    pub auto_include: Option<Option<ContextAutoInclude>>,
}

/// Result from evaluating auto-include rules
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDetectResult {
    /// Context ID that matched
    #[serde(rename = "contextId")]
    pub context_id: String,
    /// Name of the context for display
    #[serde(rename = "contextName")]
    pub context_name: String,
    /// Reason for match: "taskMention", "actionType", "errorPattern"
    pub reason: String,
    /// The specific trigger that matched
    #[serde(rename = "matchedTrigger")]
    pub matched_trigger: String,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Combine user, project (TODO), and builtin contexts into a single list
fn get_all_contexts_combined() -> Vec<ContextWithMetadata> {
    let library = context::load_user_context_library();
    let mut all_contexts = Vec::new();

    // Add user contexts with metadata
    for ctx in &library.contexts {
        let metadata = library.metadata.iter().find(|m| m.context_id == ctx.id);
        all_contexts.push(ContextWithMetadata {
            context: ctx.clone(),
            scope: ContextScope::User,
            enabled: metadata.map(|m| m.enabled).unwrap_or(true),
            use_count: metadata.map(|m| m.use_count).unwrap_or(0),
            last_used_at: metadata.and_then(|m| m.last_used_at.clone()),
        });
    }

    // Add builtin contexts (always enabled, no usage tracking)
    for ctx in context::get_builtin_contexts() {
        all_contexts.push(ContextWithMetadata {
            context: ctx,
            scope: ContextScope::Builtin,
            enabled: true,
            use_count: 0,
            last_used_at: None,
        });
    }

    // TODO: Add project contexts when project context support is implemented

    all_contexts
}

/// Find which auto-include rule triggered for a context
fn find_auto_include_reason(
    context: &Context,
    task_prompt: &str,
    action_types: &[String],
    recent_errors: &[String],
) -> Option<(String, String)> {
    let Some(ref rules) = context.auto_include else {
        return None;
    };

    let task_lower = task_prompt.to_lowercase();

    // Check task mentions
    if let Some(ref mentions) = rules.task_mentions {
        for mention in mentions {
            if task_lower.contains(&mention.to_lowercase()) {
                return Some(("taskMention".to_string(), mention.clone()));
            }
        }
    }

    // Check action types
    if let Some(ref types) = rules.action_types {
        for t in types {
            if action_types.iter().any(|a| a.eq_ignore_ascii_case(t)) {
                return Some(("actionType".to_string(), t.clone()));
            }
        }
    }

    // Check error patterns (regex)
    if let Some(ref patterns) = rules.error_patterns {
        for pattern in patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                for error in recent_errors {
                    if re.is_match(error) {
                        return Some(("errorPattern".to_string(), pattern.clone()));
                    }
                }
            }
        }
    }

    None
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Get all contexts (user, project, builtin combined).
///
/// Returns all contexts from all sources with their metadata.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of ContextWithMetadata
/// * `Err(String)` - Error message if contexts cannot be loaded
#[tauri::command]
pub fn get_all_contexts() -> Result<CommandResponse, String> {
    info!("Getting all contexts");

    let contexts = get_all_contexts_combined();

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Retrieved {} contexts", contexts.len())),
        data: Some(
            serde_json::to_value(&contexts)
                .map_err(|e| format!("Failed to serialize contexts: {}", e))?,
        ),
    })
}

/// Get a single context by ID.
///
/// Searches across user, project, and builtin contexts.
///
/// # Arguments
/// * `id` - Context ID to find
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with ContextWithMetadata or null if not found
/// * `Err(String)` - Error message if lookup fails
#[tauri::command]
pub fn get_context(id: String) -> Result<CommandResponse, String> {
    info!("Getting context: {}", id);

    let all_contexts = get_all_contexts_combined();
    let context = all_contexts.into_iter().find(|c| c.context.id == id);

    Ok(CommandResponse {
        success: true,
        message: if context.is_some() {
            Some("Context found".to_string())
        } else {
            Some("Context not found".to_string())
        },
        data: Some(
            serde_json::to_value(&context)
                .map_err(|e| format!("Failed to serialize context: {}", e))?,
        ),
    })
}

/// Create a new user context.
///
/// # Arguments
/// * `request` - CreateContextRequest with context details
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with created Context
/// * `Err(String)` - Error message if creation fails
#[tauri::command]
pub fn create_context(request: CreateContextRequest) -> Result<CommandResponse, String> {
    info!("Creating context: {}", request.name);

    let created = context::create_user_context(
        request.name,
        request.content,
        request.category,
        request.tags.unwrap_or_default(),
        request.auto_include,
    )
    .map_err(|e| format!("Failed to create context: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Context '{}' created", created.name)),
        data: Some(
            serde_json::to_value(&created)
                .map_err(|e| format!("Failed to serialize context: {}", e))?,
        ),
    })
}

/// Update an existing user context.
///
/// # Arguments
/// * `id` - Context ID to update
/// * `request` - UpdateContextRequest with fields to update
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with updated Context
/// * `Err(String)` - Error message if update fails
#[tauri::command]
pub fn update_context(
    id: String,
    request: UpdateContextRequest,
) -> Result<CommandResponse, String> {
    info!("Updating context: {}", id);

    let updated = context::update_user_context(
        &id,
        request.name,
        request.content,
        request.category,
        request.tags,
        request.auto_include,
    )
    .map_err(|e| format!("Failed to update context: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Context '{}' updated", updated.name)),
        data: Some(
            serde_json::to_value(&updated)
                .map_err(|e| format!("Failed to serialize context: {}", e))?,
        ),
    })
}

/// Delete a user context.
///
/// # Arguments
/// * `id` - Context ID to delete
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if deletion fails
#[tauri::command]
pub fn delete_context(id: String) -> Result<CommandResponse, String> {
    info!("Deleting context: {}", id);

    context::delete_user_context(&id).map_err(|e| format!("Failed to delete context: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Context '{}' deleted", id)),
        data: None,
    })
}

/// Search contexts by query.
///
/// Searches name, content, category, and tags across all contexts.
///
/// # Arguments
/// * `query` - Search query string
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with matching ContextWithMetadata list
/// * `Err(String)` - Error message if search fails
#[tauri::command]
pub fn search_contexts(query: String) -> Result<CommandResponse, String> {
    info!("Searching contexts: {}", query);

    let query_lower = query.to_lowercase();
    let all_contexts = get_all_contexts_combined();

    let matches: Vec<_> = all_contexts
        .into_iter()
        .filter(|c| {
            c.context.name.to_lowercase().contains(&query_lower)
                || c.context.content.to_lowercase().contains(&query_lower)
                || c.context
                    .category
                    .as_ref()
                    .map(|cat| cat.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
                || c.context
                    .tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect();

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} matching contexts", matches.len())),
        data: Some(
            serde_json::to_value(&matches)
                .map_err(|e| format!("Failed to serialize contexts: {}", e))?,
        ),
    })
}

/// Get all unique categories.
///
/// Returns categories from all context sources (user, project, builtin).
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of category strings
/// * `Err(String)` - Error message if categories cannot be retrieved
#[tauri::command]
pub fn get_context_categories() -> Result<CommandResponse, String> {
    info!("Getting context categories");

    let all_contexts = get_all_contexts_combined();
    let mut categories: Vec<String> = all_contexts
        .into_iter()
        .filter_map(|c| c.context.category)
        .filter(|c| !c.is_empty())
        .collect();

    categories.sort();
    categories.dedup();

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} categories", categories.len())),
        data: Some(
            serde_json::to_value(&categories)
                .map_err(|e| format!("Failed to serialize categories: {}", e))?,
        ),
    })
}

/// Enable or disable a context.
///
/// # Arguments
/// * `id` - Context ID to update
/// * `enabled` - Whether the context should be enabled
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if update fails
#[tauri::command]
pub fn set_context_enabled(id: String, enabled: bool) -> Result<CommandResponse, String> {
    info!("Setting context {} enabled={}", id, enabled);

    context::set_context_enabled(&id, enabled)
        .map_err(|e| format!("Failed to update context enabled state: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!(
            "Context {} {}",
            id,
            if enabled { "enabled" } else { "disabled" }
        )),
        data: None,
    })
}

/// Record that a context was used.
///
/// Increments use_count and updates last_used_at.
///
/// # Arguments
/// * `id` - Context ID to record usage for
///
/// # Returns
/// * `Ok(CommandResponse)` - Success
/// * `Err(String)` - Error message if recording fails
#[tauri::command]
pub fn record_context_usage(id: String) -> Result<CommandResponse, String> {
    info!("Recording context usage: {}", id);

    context::record_context_use(&id)
        .map_err(|e| format!("Failed to record context usage: {}", e))?;

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Recorded usage for context {}", id)),
        data: None,
    })
}

/// Get builtin contexts.
///
/// Returns only the built-in contexts shipped with the runner.
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of ContextWithMetadata
/// * `Err(String)` - Error message if contexts cannot be loaded
#[tauri::command]
pub fn get_builtin_contexts_cmd() -> Result<CommandResponse, String> {
    info!("Getting builtin contexts");

    let builtin: Vec<ContextWithMetadata> = context::get_builtin_contexts()
        .into_iter()
        .map(|ctx| ContextWithMetadata {
            context: ctx,
            scope: ContextScope::Builtin,
            enabled: true,
            use_count: 0,
            last_used_at: None,
        })
        .collect();

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} builtin contexts", builtin.len())),
        data: Some(
            serde_json::to_value(&builtin)
                .map_err(|e| format!("Failed to serialize contexts: {}", e))?,
        ),
    })
}

/// Evaluate auto-include rules for contexts.
///
/// Checks which contexts should be automatically included based on:
/// - Task mentions in the prompt
/// - Action types in the loaded config
/// - Error patterns in recent logs
///
/// # Arguments
/// * `task_prompt` - The current task prompt
/// * `action_types` - Action types from the loaded config
/// * `recent_errors` - Recent error messages from logs
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with list of AutoDetectResult
/// * `Err(String)` - Error message if evaluation fails
#[tauri::command]
pub fn evaluate_auto_include(
    task_prompt: String,
    action_types: Vec<String>,
    recent_errors: Vec<String>,
) -> Result<CommandResponse, String> {
    info!(
        "Evaluating auto-include: prompt_len={}, action_types={:?}, errors={}",
        task_prompt.len(),
        action_types,
        recent_errors.len()
    );

    let all_contexts = get_all_contexts_combined();
    let mut results = Vec::new();

    for ctx in all_contexts {
        // Skip disabled contexts
        if !ctx.enabled {
            continue;
        }

        if let Some((reason, matched_trigger)) =
            find_auto_include_reason(&ctx.context, &task_prompt, &action_types, &recent_errors)
        {
            results.push(AutoDetectResult {
                context_id: ctx.context.id.clone(),
                context_name: ctx.context.name.clone(),
                reason,
                matched_trigger,
            });
        }
    }

    info!(
        "Auto-include evaluation found {} contexts to include",
        results.len()
    );

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Found {} contexts to auto-include", results.len())),
        data: Some(
            serde_json::to_value(&results)
                .map_err(|e| format!("Failed to serialize results: {}", e))?,
        ),
    })
}
