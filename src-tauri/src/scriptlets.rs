//! Scriptlet Library
//!
//! This module provides persistent storage for scriptlets - reusable text snippets
//! that capture learnings from AI debugging sessions and can be inserted into
//! Playwright script descriptions.
//!
//! Features:
//! - Store named text snippets with categories and tags
//! - Track which AI logs were used to generate each scriptlet
//! - Search and filter scriptlets

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const SCRIPTLETS_FILE: &str = "scriptlets.json";

// ============================================================================
// Data Types
// ============================================================================

/// A reusable text snippet for script descriptions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scriptlet {
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

    /// AI loop IDs that were used to generate this scriptlet
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_log_ids: Option<Vec<String>>,

    /// Creation timestamp (ISO 8601)
    pub created_at: String,

    /// Last modification timestamp (ISO 8601)
    pub modified_at: String,
}

impl Scriptlet {
    /// Create a new scriptlet with the given details
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

/// The scriptlet library containing all saved scriptlets
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScriptletLibrary {
    /// Schema version for future migrations
    pub version: String,

    /// All stored scriptlets
    pub scriptlets: Vec<Scriptlet>,
}

impl ScriptletLibrary {
    /// Create a new empty library
    pub fn new() -> Self {
        Self {
            version: "1.0.0".to_string(),
            scriptlets: Vec::new(),
        }
    }
}

// ============================================================================
// File Operations
// ============================================================================

/// Get the scriptlets directory path in the app data directory
fn get_scriptlets_dir() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner")
        .join("scriptlets");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create scriptlets directory: {}", e))?;
    }

    Ok(app_data_dir)
}

/// Get the scriptlets file path
fn get_scriptlets_path() -> Result<PathBuf, String> {
    get_scriptlets_dir().map(|dir| dir.join(SCRIPTLETS_FILE))
}

/// Load the scriptlet library from disk
pub fn load_scriptlet_library() -> ScriptletLibrary {
    match get_scriptlets_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(library) => {
                            info!("Loaded scriptlet library from {:?}", path);
                            library
                        }
                        Err(e) => {
                            error!("Failed to parse scriptlets file: {}", e);
                            ScriptletLibrary::new()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read scriptlets file: {}", e);
                        ScriptletLibrary::new()
                    }
                }
            } else {
                info!("No scriptlets file found, using empty library");
                ScriptletLibrary::new()
            }
        }
        Err(e) => {
            error!("Failed to get scriptlets path: {}", e);
            ScriptletLibrary::new()
        }
    }
}

/// Save the scriptlet library to disk
pub fn save_scriptlet_library(library: &ScriptletLibrary) -> Result<(), String> {
    let path = get_scriptlets_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize scriptlets: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write scriptlets file: {}", e))?;

    info!("Saved scriptlet library to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations
// ============================================================================

/// Get all scriptlets
pub fn get_all_scriptlets() -> Vec<Scriptlet> {
    load_scriptlet_library().scriptlets
}

/// Get a scriptlet by ID
pub fn get_scriptlet(id: &str) -> Option<Scriptlet> {
    load_scriptlet_library()
        .scriptlets
        .into_iter()
        .find(|s| s.id == id)
}

/// Create a new scriptlet
pub fn create_scriptlet(
    name: String,
    content: String,
    category: String,
    tags: Vec<String>,
    source_log_ids: Option<Vec<String>>,
) -> Result<Scriptlet, String> {
    let mut library = load_scriptlet_library();

    let scriptlet = Scriptlet::new(name, content, category, tags, source_log_ids);
    let created = scriptlet.clone();

    library.scriptlets.push(scriptlet);
    save_scriptlet_library(&library)?;

    info!("Created scriptlet: {} ({})", created.name, created.id);
    Ok(created)
}

/// Update an existing scriptlet
pub fn update_scriptlet(
    id: &str,
    name: Option<String>,
    content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<Scriptlet, String> {
    let mut library = load_scriptlet_library();

    let scriptlet = library
        .scriptlets
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("Scriptlet not found: {}", id))?;

    // Update fields if provided
    if let Some(name) = name {
        scriptlet.name = name;
    }
    if let Some(content) = content {
        scriptlet.content = content;
    }
    if let Some(category) = category {
        scriptlet.category = category;
    }
    if let Some(tags) = tags {
        scriptlet.tags = tags;
    }

    // Update modification timestamp
    scriptlet.modified_at = chrono::Utc::now().to_rfc3339();

    let updated = scriptlet.clone();
    save_scriptlet_library(&library)?;

    info!("Updated scriptlet: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Delete a scriptlet
pub fn delete_scriptlet(id: &str) -> Result<(), String> {
    let mut library = load_scriptlet_library();

    let initial_len = library.scriptlets.len();
    library.scriptlets.retain(|s| s.id != id);

    if library.scriptlets.len() == initial_len {
        return Err(format!("Scriptlet not found: {}", id));
    }

    save_scriptlet_library(&library)?;
    info!("Deleted scriptlet: {}", id);
    Ok(())
}

/// Get all unique categories
pub fn get_categories() -> Vec<String> {
    let library = load_scriptlet_library();
    let mut categories: Vec<String> = library
        .scriptlets
        .iter()
        .map(|s| s.category.clone())
        .filter(|c| !c.is_empty())
        .collect();

    categories.sort();
    categories.dedup();
    categories
}

/// Search scriptlets by query (searches name, content, category, and tags)
pub fn search_scriptlets(query: &str) -> Vec<Scriptlet> {
    let query_lower = query.to_lowercase();
    load_scriptlet_library()
        .scriptlets
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
