//! Prompt Snippet Library
//!
//! This module provides persistent storage for prompt snippets - reusable text snippets
//! that capture learnings from AI debugging sessions and can be inserted into
//! Playwright test descriptions.
//!
//! Features:
//! - Store named text snippets with categories and tags
//! - Track which AI logs were used to generate each prompt snippet
//! - Search and filter prompt snippets

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info, warn};
use uuid::Uuid;

const PROMPT_SNIPPETS_FILE: &str = "prompt-snippets.json";

// ============================================================================
// Data Types
// ============================================================================

/// A reusable text snippet for test descriptions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSnippet {
    /// Unique identifier (UUID v4)
    pub id: String,

    /// Short descriptive name
    pub name: String,

    /// The actual text content to insert
    pub content: String,

    /// Category for organization (e.g., "Login", "Navigation", "Forms")
    pub category: String,

    /// Tags for flexible grouping
    #[serde(default)]
    pub tags: Vec<String>,

    /// AI loop IDs that were used to generate this prompt snippet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_log_ids: Option<Vec<String>>,

    /// Creation timestamp (ISO 8601)
    pub created_at: String,

    /// Last modification timestamp (ISO 8601)
    pub modified_at: String,
}

impl PromptSnippet {
    /// Create a new prompt snippet with the given details
    pub fn new(
        name: String,
        content: String,
        category: String,
        tags: Vec<String>,
        source_log_ids: Option<Vec<String>>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            content,
            category,
            tags,
            source_log_ids,
            created_at: now.clone(),
            modified_at: now,
        }
    }
}

/// The prompt snippet library containing all saved prompt snippets
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptSnippetLibrary {
    /// Schema version for future migrations
    pub version: String,

    /// All stored prompt snippets
    #[serde(alias = "scriptlets")]
    pub prompt_snippets: Vec<PromptSnippet>,
}

impl PromptSnippetLibrary {
    /// Create a new empty library
    pub fn new() -> Self {
        Self {
            version: "1.0.0".to_string(),
            prompt_snippets: Vec::new(),
        }
    }
}

// ============================================================================
// File Operations
// ============================================================================

/// Get the prompt snippets directory path in the app data directory.
/// Includes migration logic: if old "scriptlets" directory exists and new
/// "prompt-snippets" directory doesn't, renames it automatically.
///
/// Scoped per-runner for secondary instances.
fn get_prompt_snippets_dir() -> Result<PathBuf, String> {
    let raw_base = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner");
    let base_dir = crate::instance::scope_path(&raw_base);

    let new_dir = base_dir.join("prompt-snippets");
    let old_dir = base_dir.join("scriptlets");

    // Migration: rename old directory to new if needed
    if old_dir.exists() && !new_dir.exists() {
        info!(
            "Migrating prompt snippets directory: {:?} -> {:?}",
            old_dir, new_dir
        );
        if let Err(e) = fs::rename(&old_dir, &new_dir) {
            warn!(
                "Failed to migrate prompt snippets directory (will create new): {}",
                e
            );
        }
    }

    // Create directory if it doesn't exist
    if !new_dir.exists() {
        fs::create_dir_all(&new_dir)
            .map_err(|e| format!("Failed to create prompt snippets directory: {}", e))?;
    }

    Ok(new_dir)
}

/// Get the prompt snippets file path
fn get_prompt_snippets_path() -> Result<PathBuf, String> {
    get_prompt_snippets_dir().map(|dir| dir.join(PROMPT_SNIPPETS_FILE))
}

/// Load the prompt snippet library from disk.
/// Includes migration logic: if old "scriptlets.json" exists and new
/// "prompt-snippets.json" doesn't, renames it automatically.
pub fn load_prompt_snippet_library() -> PromptSnippetLibrary {
    match get_prompt_snippets_dir() {
        Ok(dir) => {
            let new_path = dir.join(PROMPT_SNIPPETS_FILE);
            let old_path = dir.join("scriptlets.json");

            // Migration: rename old file to new if needed
            if old_path.exists() && !new_path.exists() {
                info!(
                    "Migrating prompt snippets file: {:?} -> {:?}",
                    old_path, new_path
                );
                if let Err(e) = fs::rename(&old_path, &new_path) {
                    warn!(
                        "Failed to migrate prompt snippets file (will use old): {}",
                        e
                    );
                    // Fall through to try reading the old path
                }
            }

            let path = if new_path.exists() {
                new_path
            } else if old_path.exists() {
                old_path
            } else {
                info!("No prompt snippets file found, using empty library");
                return PromptSnippetLibrary::new();
            };

            match fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str(&contents) {
                    Ok(library) => {
                        info!("Loaded prompt snippet library from {:?}", path);
                        library
                    }
                    Err(e) => {
                        error!("Failed to parse prompt snippets file: {}", e);
                        PromptSnippetLibrary::new()
                    }
                },
                Err(e) => {
                    error!("Failed to read prompt snippets file: {}", e);
                    PromptSnippetLibrary::new()
                }
            }
        }
        Err(e) => {
            error!("Failed to get prompt snippets path: {}", e);
            PromptSnippetLibrary::new()
        }
    }
}

/// Save the prompt snippet library to disk
pub fn save_prompt_snippet_library(library: &PromptSnippetLibrary) -> Result<(), String> {
    let path = get_prompt_snippets_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize prompt snippets: {}", e))?;

    fs::write(&path, contents)
        .map_err(|e| format!("Failed to write prompt snippets file: {}", e))?;

    info!("Saved prompt snippet library to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations
// ============================================================================

/// Get all prompt snippets
pub fn get_all_prompt_snippets() -> Vec<PromptSnippet> {
    load_prompt_snippet_library().prompt_snippets
}

/// Get a prompt snippet by ID
pub fn get_prompt_snippet(id: &str) -> Option<PromptSnippet> {
    load_prompt_snippet_library()
        .prompt_snippets
        .into_iter()
        .find(|s| s.id == id)
}

/// Create a new prompt snippet
pub fn create_prompt_snippet(
    name: String,
    content: String,
    category: String,
    tags: Vec<String>,
    source_log_ids: Option<Vec<String>>,
) -> Result<PromptSnippet, String> {
    let mut library = load_prompt_snippet_library();

    let snippet = PromptSnippet::new(name, content, category, tags, source_log_ids);
    let created = snippet.clone();

    library.prompt_snippets.push(snippet);
    save_prompt_snippet_library(&library)?;

    info!("Created prompt snippet: {} ({})", created.name, created.id);
    Ok(created)
}

/// Update an existing prompt snippet
pub fn update_prompt_snippet(
    id: &str,
    name: Option<String>,
    content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<PromptSnippet, String> {
    let mut library = load_prompt_snippet_library();

    let snippet = library
        .prompt_snippets
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("Prompt snippet not found: {}", id))?;

    // Update fields if provided
    if let Some(name) = name {
        snippet.name = name;
    }
    if let Some(content) = content {
        snippet.content = content;
    }
    if let Some(category) = category {
        snippet.category = category;
    }
    if let Some(tags) = tags {
        snippet.tags = tags;
    }

    // Update modification timestamp
    snippet.modified_at = chrono::Utc::now().to_rfc3339();

    let updated = snippet.clone();
    save_prompt_snippet_library(&library)?;

    info!("Updated prompt snippet: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Delete a prompt snippet
pub fn delete_prompt_snippet(id: &str) -> Result<(), String> {
    let mut library = load_prompt_snippet_library();

    let initial_len = library.prompt_snippets.len();
    library.prompt_snippets.retain(|s| s.id != id);

    if library.prompt_snippets.len() == initial_len {
        return Err(format!("Prompt snippet not found: {}", id));
    }

    save_prompt_snippet_library(&library)?;
    info!("Deleted prompt snippet: {}", id);
    Ok(())
}

/// Get all unique categories
pub fn get_categories() -> Vec<String> {
    let library = load_prompt_snippet_library();
    let mut categories: Vec<String> = library
        .prompt_snippets
        .iter()
        .map(|s| s.category.clone())
        .filter(|c| !c.is_empty())
        .collect();

    categories.sort();
    categories.dedup();
    categories
}

/// Search prompt snippets by query (searches name, content, category, and tags)
pub fn search_prompt_snippets(query: &str) -> Vec<PromptSnippet> {
    let query_lower = query.to_lowercase();
    load_prompt_snippet_library()
        .prompt_snippets
        .into_iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&query_lower)
                || s.content.to_lowercase().contains(&query_lower)
                || s.category.to_lowercase().contains(&query_lower)
                || s.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}
