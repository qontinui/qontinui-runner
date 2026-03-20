//! CRUD operations for user contexts.

#![allow(dead_code)]

use std::fs;
use tracing::info;

use super::storage::{load_user_context_library, save_user_context_library};
use super::types::{Context, ContextAutoInclude, ContextMetadata};

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

/// Create a user context from a file path.
///
/// Reads the file content and creates a context with auto-detected name and category.
/// Filenames containing "claude" or "gemini" are categorized as "ai-instructions".
pub fn create_context_from_file(
    file_path: &str,
    name: Option<String>,
    category: Option<String>,
    tags: Vec<String>,
) -> Result<Context, String> {
    let path = std::path::Path::new(file_path);

    // Read file content
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file '{}': {}", file_path, e))?;

    if content.is_empty() {
        return Err(format!("File '{}' is empty", file_path));
    }

    // Derive name from filename if not provided
    let derived_name = name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported-context")
            .to_string()
    });

    // Auto-detect category from filename
    let filename_lower = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("")
        .to_lowercase();
    let derived_category = category.or_else(|| {
        if filename_lower.contains("claude") || filename_lower.contains("gemini") {
            Some("ai-instructions".to_string())
        } else {
            None
        }
    });

    // Build auto-include with filename-based task mentions
    let stem_lower = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let auto_include = Some(ContextAutoInclude {
        task_mentions: Some(vec![stem_lower]),
        ..Default::default()
    });

    create_user_context(derived_name, content, derived_category, tags, auto_include)
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
