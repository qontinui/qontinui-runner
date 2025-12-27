//! Prompt Library
//!
//! This module provides persistent storage for AI prompts that users can
//! save, organize, and run from the runner UI.
//!
//! Supports "workflow" prompts that automatically continue across multiple
//! AI sessions using checkpoint files.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const PROMPTS_FILE: &str = "prompts.json";

// ============================================================================
// Data Types
// ============================================================================

/// Configuration for multi-session workflow prompts
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowConfig {
    /// Enable workflow mode (auto-continue across sessions)
    #[serde(default)]
    pub enabled: bool,
    /// Path to checkpoint file that tracks workflow progress
    #[serde(default)]
    pub checkpoint_path: String,
    /// Field in checkpoint JSON that indicates current phase/step (e.g., "current_phase")
    #[serde(default = "default_phase_field")]
    pub phase_field: String,
    /// Value of phase_field that indicates workflow is complete
    #[serde(default = "default_completion_value")]
    pub completion_value: u32,
    /// Seconds without checkpoint update before considering workflow stalled
    #[serde(default = "default_stall_threshold")]
    pub stall_threshold_secs: u32,
    /// Prompt to use when spawning continuation sessions
    #[serde(default)]
    pub continuation_prompt: String,
    /// Auto-continue workflow on runner restart (default: true for prompts)
    /// Can be toggled during workflow execution via API
    #[serde(default = "default_auto_continue")]
    pub auto_continue: bool,
}

fn default_auto_continue() -> bool {
    true // Prompts default to auto-continue enabled
}

fn default_phase_field() -> String {
    "current_phase".to_string()
}

fn default_completion_value() -> u32 {
    12
}

fn default_stall_threshold() -> u32 {
    300 // 5 minutes
}

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
    /// The actual prompt content to send to Claude
    pub content: String,
    /// Category for organization (e.g., "Development", "Testing", "Deployment")
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,
    /// Maximum iterations for AI Developer sessions (default: 10)
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Workflow configuration for multi-session prompts
    #[serde(default)]
    pub workflow: WorkflowConfig,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
}

fn default_max_iterations() -> u32 {
    10
}

impl SavedPrompt {
    /// Create a new prompt with auto-generated ID and timestamps
    #[allow(dead_code)]
    pub fn new(name: String, content: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description: String::new(),
            content,
            category: String::new(),
            tags: Vec::new(),
            max_iterations: default_max_iterations(),
            workflow: WorkflowConfig::default(),
            created_at: now.clone(),
            modified_at: now,
        }
    }

    /// Create a new prompt with all fields specified
    pub fn with_details(
        name: String,
        description: String,
        content: String,
        category: String,
        tags: Vec<String>,
        max_iterations: u32,
        workflow: Option<WorkflowConfig>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            content,
            category,
            tags,
            max_iterations,
            workflow: workflow.unwrap_or_default(),
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
    max_iterations: u32,
    workflow: Option<WorkflowConfig>,
) -> Result<SavedPrompt, String> {
    let mut library = load_prompt_library();

    let prompt = SavedPrompt::with_details(
        name,
        description,
        content,
        category,
        tags,
        max_iterations,
        workflow,
    );
    let created = prompt.clone();

    library.prompts.push(prompt);
    save_prompt_library(&library)?;

    info!("Created prompt: {} ({})", created.name, created.id);
    Ok(created)
}

/// Update an existing prompt
#[allow(clippy::too_many_arguments)]
pub fn update_prompt(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    max_iterations: Option<u32>,
    workflow: Option<WorkflowConfig>,
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
    if let Some(max_iterations) = max_iterations {
        prompt.max_iterations = max_iterations;
    }
    if let Some(workflow) = workflow {
        prompt.workflow = workflow;
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
        original.max_iterations,
        Some(original.workflow),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_creation() {
        let prompt = SavedPrompt::new("Test Prompt".to_string(), "Test content".to_string());
        assert!(!prompt.id.is_empty());
        assert_eq!(prompt.name, "Test Prompt");
        assert_eq!(prompt.content, "Test content");
        assert_eq!(prompt.max_iterations, 10);
        assert!(!prompt.workflow.enabled);
    }

    #[test]
    fn test_prompt_with_details() {
        let prompt = SavedPrompt::with_details(
            "Test".to_string(),
            "Description".to_string(),
            "Content".to_string(),
            "Dev".to_string(),
            vec!["tag1".to_string()],
            5,
            None,
        );
        assert_eq!(prompt.category, "Dev");
        assert_eq!(prompt.max_iterations, 5);
    }

    #[test]
    fn test_prompt_with_workflow() {
        let workflow = WorkflowConfig {
            enabled: true,
            checkpoint_path: "/tmp/checkpoint.json".to_string(),
            phase_field: "current_phase".to_string(),
            completion_value: 12,
            stall_threshold_secs: 300,
            continuation_prompt: "Continue the workflow".to_string(),
        };
        let prompt = SavedPrompt::with_details(
            "Workflow Test".to_string(),
            "A workflow prompt".to_string(),
            "Content".to_string(),
            "Dev".to_string(),
            vec![],
            50,
            Some(workflow),
        );
        assert!(prompt.workflow.enabled);
        assert_eq!(prompt.workflow.completion_value, 12);
    }
}
