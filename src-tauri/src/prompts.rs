//! Prompt Library
//!
//! This module provides persistent storage for AI prompts that users can
//! save, organize, and run from the runner UI.
//!
//! Simplified model: every task runs until [TASK_COMPLETE] marker is found.
//! Multi-session support is controlled by max_sessions (optional limit).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const PROMPTS_FILE: &str = "prompts.json";

// ============================================================================
// Data Types
// ============================================================================

/// A saved prompt in the library
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPrompt {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for the prompt
    pub name: String,
    /// Optional description of what this prompt does
    #[serde(default)]
    pub description: String,
    /// The actual prompt content to send to the AI
    pub content: String,
    /// Category for organization (e.g., "Development", "Testing", "Deployment")
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum number of sessions (null = unlimited)
    /// Sessions continue until [TASK_COMPLETE] is found or this limit is reached
    #[serde(default)]
    pub max_sessions: Option<u32>,
    /// AI provider override (e.g., "claude_cli", "gemini_api")
    /// If not set, uses global settings
    #[serde(default)]
    pub provider: Option<String>,
    /// AI model override (e.g., "gemini-3-flash", "claude-sonnet-4")
    /// If not set, uses provider's default model
    #[serde(default)]
    pub model: Option<String>,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
}

impl SavedPrompt {
    /// Create a new prompt with all fields specified
    pub fn with_details(
        name: String,
        description: String,
        content: String,
        category: String,
        tags: Vec<String>,
        max_sessions: Option<u32>,
        provider: Option<String>,
        model: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            content,
            category,
            tags,
            max_sessions,
            provider,
            model,
            created_at: now.clone(),
            modified_at: now,
        }
    }
}

/// The prompt library containing all saved prompts
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptLibrary {
    /// Version of the library format (for future migrations)
    #[serde(default = "default_version")]
    pub version: String,
    /// All saved prompts
    #[serde(default)]
    pub prompts: Vec<SavedPrompt>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

// ============================================================================
// File Operations
// ============================================================================

/// Get the prompts file path in the app data directory
fn get_prompts_path() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {}", e))?;
    }

    Ok(app_data_dir.join(PROMPTS_FILE))
}

/// Load the prompt library from disk
pub fn load_prompt_library() -> PromptLibrary {
    match get_prompts_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(library) => {
                            info!("Loaded prompt library from {:?}", path);
                            library
                        }
                        Err(e) => {
                            error!("Failed to parse prompts file: {}", e);
                            PromptLibrary::default()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read prompts file: {}", e);
                        PromptLibrary::default()
                    }
                }
            } else {
                info!("No prompts file found, using empty library");
                PromptLibrary::default()
            }
        }
        Err(e) => {
            error!("Failed to get prompts path: {}", e);
            PromptLibrary::default()
        }
    }
}

/// Save the prompt library to disk
pub fn save_prompt_library(library: &PromptLibrary) -> Result<(), String> {
    let path = get_prompts_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize prompts: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write prompts file: {}", e))?;

    info!("Saved prompt library to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations
// ============================================================================

/// Get all prompts
pub fn get_all_prompts() -> Vec<SavedPrompt> {
    load_prompt_library().prompts
}

/// Get a prompt by ID
pub fn get_prompt(id: &str) -> Option<SavedPrompt> {
    load_prompt_library()
        .prompts
        .into_iter()
        .find(|p| p.id == id)
}

/// Create a new prompt
pub fn create_prompt(
    name: String,
    description: String,
    content: String,
    category: String,
    tags: Vec<String>,
    max_sessions: Option<u32>,
    provider: Option<String>,
    model: Option<String>,
) -> Result<SavedPrompt, String> {
    let mut library = load_prompt_library();

    let prompt = SavedPrompt::with_details(
        name,
        description,
        content,
        category,
        tags,
        max_sessions,
        provider,
        model,
    );
    let created = prompt.clone();

    library.prompts.push(prompt);
    save_prompt_library(&library)?;

    info!("Created prompt: {} ({})", created.name, created.id);
    Ok(created)
}

/// Update an existing prompt
pub fn update_prompt(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    max_sessions: Option<Option<u32>>,
    provider: Option<Option<String>>,
    model: Option<Option<String>>,
) -> Result<SavedPrompt, String> {
    let mut library = load_prompt_library();

    let prompt = library
        .prompts
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("Prompt not found: {}", id))?;

    if let Some(name) = name {
        prompt.name = name;
    }
    if let Some(description) = description {
        prompt.description = description;
    }
    if let Some(content) = content {
        prompt.content = content;
    }
    if let Some(category) = category {
        prompt.category = category;
    }
    if let Some(tags) = tags {
        prompt.tags = tags;
    }
    if let Some(max_sessions) = max_sessions {
        prompt.max_sessions = max_sessions;
    }
    if let Some(provider) = provider {
        prompt.provider = provider;
    }
    if let Some(model) = model {
        prompt.model = model;
    }

    prompt.modified_at = chrono::Utc::now().to_rfc3339();

    let updated = prompt.clone();
    save_prompt_library(&library)?;

    info!("Updated prompt: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Delete a prompt by ID
pub fn delete_prompt(id: &str) -> Result<(), String> {
    let mut library = load_prompt_library();

    let initial_len = library.prompts.len();
    library.prompts.retain(|p| p.id != id);

    if library.prompts.len() == initial_len {
        return Err(format!("Prompt not found: {}", id));
    }

    save_prompt_library(&library)?;

    info!("Deleted prompt: {}", id);
    Ok(())
}

/// Import prompts from a JSON array (for bulk import)
pub fn import_prompts(prompts_json: &str) -> Result<Vec<SavedPrompt>, String> {
    let imported: Vec<SavedPrompt> = serde_json::from_str(prompts_json)
        .map_err(|e| format!("Failed to parse import JSON: {}", e))?;

    let mut library = load_prompt_library();
    let mut created = Vec::new();

    for mut prompt in imported {
        // Generate new ID to avoid conflicts
        prompt.id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        prompt.created_at = now.clone();
        prompt.modified_at = now;

        created.push(prompt.clone());
        library.prompts.push(prompt);
    }

    save_prompt_library(&library)?;

    info!("Imported {} prompts", created.len());
    Ok(created)
}

/// Export all prompts as JSON (for backup/sharing)
pub fn export_prompts() -> Result<String, String> {
    let library = load_prompt_library();
    serde_json::to_string_pretty(&library.prompts)
        .map_err(|e| format!("Failed to serialize prompts for export: {}", e))
}

/// Get all unique categories from existing prompts
pub fn get_categories() -> Vec<String> {
    let library = load_prompt_library();
    let mut categories: Vec<String> = library
        .prompts
        .iter()
        .map(|p| p.category.clone())
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort();
    categories.dedup();
    categories
}

/// Get all unique tags from existing prompts
pub fn get_all_tags() -> Vec<String> {
    let library = load_prompt_library();
    let mut tags: Vec<String> = library
        .prompts
        .iter()
        .flat_map(|p| p.tags.clone())
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

/// Search prompts by name, description, category, or tags
pub fn search_prompts(query: &str) -> Vec<SavedPrompt> {
    let query_lower = query.to_lowercase();
    let library = load_prompt_library();

    library
        .prompts
        .into_iter()
        .filter(|p| {
            p.name.to_lowercase().contains(&query_lower)
                || p.description.to_lowercase().contains(&query_lower)
                || p.category.to_lowercase().contains(&query_lower)
                || p.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

/// Duplicate a prompt with a new name
pub fn duplicate_prompt(id: &str, new_name: Option<String>) -> Result<SavedPrompt, String> {
    let original = get_prompt(id).ok_or_else(|| format!("Prompt not found: {}", id))?;

    let name = new_name.unwrap_or_else(|| format!("{} (Copy)", original.name));

    create_prompt(
        name,
        original.description,
        original.content,
        original.category,
        original.tags,
        original.max_sessions,
        original.provider,
        original.model,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_with_details() {
        let prompt = SavedPrompt::with_details(
            "Test".to_string(),
            "Description".to_string(),
            "Content".to_string(),
            "Dev".to_string(),
            vec!["tag1".to_string()],
            Some(5),
            None,
            None,
        );
        assert_eq!(prompt.category, "Dev");
        assert_eq!(prompt.max_sessions, Some(5));
        assert_eq!(prompt.provider, None);
        assert_eq!(prompt.model, None);
    }

    #[test]
    fn test_prompt_unlimited_sessions() {
        let prompt = SavedPrompt::with_details(
            "Unlimited Test".to_string(),
            "A prompt with unlimited sessions".to_string(),
            "Content".to_string(),
            "Dev".to_string(),
            vec![],
            None,
            None,
            None,
        );
        assert_eq!(prompt.max_sessions, None);
    }

    #[test]
    fn test_prompt_with_gemini_provider() {
        let prompt = SavedPrompt::with_details(
            "Gemini Task".to_string(),
            "A task using Gemini".to_string(),
            "Fix linting errors".to_string(),
            "Dev".to_string(),
            vec!["lint".to_string()],
            None,
            Some("gemini_api".to_string()),
            Some("gemini-3-flash".to_string()),
        );
        assert_eq!(prompt.provider, Some("gemini_api".to_string()));
        assert_eq!(prompt.model, Some("gemini-3-flash".to_string()));
    }
}
