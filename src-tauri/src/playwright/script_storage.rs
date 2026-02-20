//! Playwright script storage
//!
//! Contains PlaywrightScript, PlaywrightLibrary, file I/O, and CRUD operations.

use super::results::PlaywrightResult;
use super::types::{
    default_browser, default_timeout_seconds, default_version, DisplayMode, SyncStatus, TESTS_FILE,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

// ============================================================================
// Data Types
// ============================================================================

/// A saved Playwright script
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightScript {
    /// Local unique identifier (UUID v4)
    pub id: String,
    /// Cloud ID (set after first sync)
    #[serde(default)]
    pub cloud_id: Option<String>,
    /// Display name for the script
    pub name: String,
    /// Natural language description of what this test does
    #[serde(default)]
    pub description: String,
    /// Additional instructions for AI code generation/refinement (not part of the test description)
    #[serde(default)]
    pub ai_instructions: Option<String>,
    /// Target web application URL (base URL for tests)
    #[serde(default)]
    pub target_url: String,
    /// The complete .spec.ts file content
    pub script_content: String,
    /// Category for organization (e.g., "E2E", "Smoke", "Regression")
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,
    /// Timeout for test execution in seconds
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
    /// Display mode: headless, headed, or connect_existing
    #[serde(default)]
    pub display_mode: DisplayMode,
    /// Browser to use: chromium, firefox, webkit
    #[serde(default = "default_browser")]
    pub browser: String,
    /// Sync status for cloud backup
    #[serde(default)]
    pub sync_status: SyncStatus,
    /// Cloud version (for conflict detection)
    #[serde(default)]
    pub cloud_version: Option<u32>,
    /// Last sync timestamp (ISO 8601)
    #[serde(default)]
    pub last_synced_at: Option<String>,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
    /// Last execution result (cached)
    #[serde(default)]
    pub last_result: Option<PlaywrightResult>,
    /// Workflow objective - what this script aims to accomplish
    /// Used for AI to verify success beyond just "test passed"
    #[serde(default)]
    pub workflow_objective: Option<String>,
    /// Success criteria - specific things to verify after script passes
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// Whether this script is used as workflow automation (not traditional testing)
    /// Automation scripts are expected to pass; verification is what matters
    #[serde(default)]
    pub is_workflow_automation: bool,
}

impl PlaywrightScript {
    /// Create a new script with all fields specified
    #[allow(clippy::too_many_arguments)]
    pub fn with_details(
        name: String,
        description: String,
        ai_instructions: Option<String>,
        target_url: String,
        script_content: String,
        category: String,
        tags: Vec<String>,
        timeout_seconds: u32,
        display_mode: DisplayMode,
        browser: String,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            cloud_id: None,
            name,
            description,
            ai_instructions,
            target_url,
            script_content,
            category,
            tags,
            timeout_seconds,
            display_mode,
            browser,
            sync_status: SyncStatus::default(),
            cloud_version: None,
            last_synced_at: None,
            created_at: now.clone(),
            modified_at: now,
            last_result: None,
            workflow_objective: None,
            success_criteria: Vec::new(),
            is_workflow_automation: false,
        }
    }

    /// Create a new script with minimal fields (for simple use cases and tests)
    #[cfg(test)]
    pub fn new(name: String, script_content: String) -> Self {
        Self::with_details(
            name,
            String::new(),
            None,
            String::new(),
            script_content,
            String::new(),
            Vec::new(),
            default_timeout_seconds(),
            DisplayMode::default(),
            default_browser(),
        )
    }
}

/// The script library containing all saved scripts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightLibrary {
    /// Version of the library format (for future migrations)
    #[serde(default = "default_version")]
    pub version: String,
    /// All saved scripts
    #[serde(default)]
    pub scripts: Vec<PlaywrightScript>,
}

impl Default for PlaywrightLibrary {
    fn default() -> Self {
        Self {
            version: default_version(),
            scripts: Vec::new(),
        }
    }
}

// ============================================================================
// File Operations
// ============================================================================

/// Get the playwright directory path in the app data directory
fn get_playwright_dir() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner")
        .join("playwright");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create playwright directory: {}", e))?;
    }

    Ok(app_data_dir)
}

/// Get the tests file path.
/// Includes migration logic: if old "playwright-scripts.json" exists and new
/// "playwright-tests.json" doesn't, renames it automatically.
fn get_scripts_path() -> Result<PathBuf, String> {
    let dir = get_playwright_dir()?;
    let new_path = dir.join(TESTS_FILE);
    let old_path = dir.join("playwright-scripts.json");

    // Migration: rename old file to new if needed
    if old_path.exists() && !new_path.exists() {
        info!(
            "Migrating playwright tests file: {:?} -> {:?}",
            old_path, new_path
        );
        if let Err(e) = std::fs::rename(&old_path, &new_path) {
            tracing::warn!(
                "Failed to migrate playwright tests file (will try old path): {}",
                e
            );
            // If rename fails, fall back to old path
            return Ok(old_path);
        }
    }

    Ok(new_path)
}

/// Get the results directory for test reports
pub fn get_results_dir() -> Result<PathBuf, String> {
    let dir = get_playwright_dir()?.join("results");
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create results directory: {}", e))?;
    }
    Ok(dir)
}

/// Load the script library from disk
pub fn load_script_library() -> PlaywrightLibrary {
    match get_scripts_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(library) => {
                            info!("Loaded playwright script library from {:?}", path);
                            library
                        }
                        Err(e) => {
                            error!("Failed to parse playwright scripts file: {}", e);
                            PlaywrightLibrary::default()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read playwright scripts file: {}", e);
                        PlaywrightLibrary::default()
                    }
                }
            } else {
                info!("No playwright scripts file found, using empty library");
                PlaywrightLibrary::default()
            }
        }
        Err(e) => {
            error!("Failed to get playwright scripts path: {}", e);
            PlaywrightLibrary::default()
        }
    }
}

/// Save the script library to disk
pub fn save_script_library(library: &PlaywrightLibrary) -> Result<(), String> {
    let path = get_scripts_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize playwright scripts: {}", e))?;

    fs::write(&path, contents)
        .map_err(|e| format!("Failed to write playwright scripts file: {}", e))?;

    info!("Saved playwright script library to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations
// ============================================================================

/// Get all scripts
pub fn get_all_scripts() -> Vec<PlaywrightScript> {
    load_script_library().scripts
}

/// Get a script by ID
pub fn get_script(id: &str) -> Option<PlaywrightScript> {
    load_script_library()
        .scripts
        .into_iter()
        .find(|s| s.id == id)
}

/// Create a new script
#[allow(clippy::too_many_arguments)]
pub fn create_script(
    name: String,
    description: String,
    ai_instructions: Option<String>,
    target_url: String,
    script_content: String,
    category: String,
    tags: Vec<String>,
    timeout_seconds: u32,
    display_mode: DisplayMode,
    browser: String,
) -> Result<PlaywrightScript, String> {
    let mut library = load_script_library();

    let script = PlaywrightScript::with_details(
        name,
        description,
        ai_instructions,
        target_url,
        script_content,
        category,
        tags,
        timeout_seconds,
        display_mode,
        browser,
    );
    let created = script.clone();

    library.scripts.push(script);
    save_script_library(&library)?;

    info!(
        "Created playwright script: {} ({})",
        created.name, created.id
    );
    Ok(created)
}

/// Update an existing script
#[allow(clippy::too_many_arguments)]
pub fn update_script(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    ai_instructions: Option<String>,
    target_url: Option<String>,
    script_content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    timeout_seconds: Option<u32>,
    display_mode: Option<DisplayMode>,
    browser: Option<String>,
) -> Result<PlaywrightScript, String> {
    let mut library = load_script_library();

    let script = library
        .scripts
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("Script not found: {}", id))?;

    if let Some(name) = name {
        script.name = name;
    }
    if let Some(description) = description {
        script.description = description;
    }
    if let Some(ai_instructions) = ai_instructions {
        script.ai_instructions = Some(ai_instructions);
    }
    if let Some(target_url) = target_url {
        script.target_url = target_url;
    }
    if let Some(script_content) = script_content {
        script.script_content = script_content;
    }
    if let Some(category) = category {
        script.category = category;
    }
    if let Some(tags) = tags {
        script.tags = tags;
    }
    if let Some(timeout_seconds) = timeout_seconds {
        script.timeout_seconds = timeout_seconds;
    }
    if let Some(display_mode) = display_mode {
        script.display_mode = display_mode;
    }
    if let Some(browser) = browser {
        script.browser = browser;
    }

    script.modified_at = chrono::Utc::now().to_rfc3339();

    // Mark as locally modified if previously synced
    if script.sync_status == SyncStatus::Synced {
        script.sync_status = SyncStatus::LocalModified;
    }

    let updated = script.clone();
    save_script_library(&library)?;

    info!(
        "Updated playwright script: {} ({})",
        updated.name, updated.id
    );
    Ok(updated)
}

/// Delete a script by ID
pub fn delete_script(id: &str) -> Result<(), String> {
    let mut library = load_script_library();

    let initial_len = library.scripts.len();
    library.scripts.retain(|s| s.id != id);

    if library.scripts.len() == initial_len {
        return Err(format!("Script not found: {}", id));
    }

    save_script_library(&library)?;

    info!("Deleted playwright script: {}", id);
    Ok(())
}

/// Import scripts from a JSON array
pub fn import_scripts(scripts_json: &str) -> Result<Vec<PlaywrightScript>, String> {
    let imported: Vec<PlaywrightScript> = serde_json::from_str(scripts_json)
        .map_err(|e| format!("Failed to parse import JSON: {}", e))?;

    let mut library = load_script_library();
    let mut created = Vec::new();

    for mut script in imported {
        // Generate new ID to avoid conflicts
        script.id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        script.created_at = now.clone();
        script.modified_at = now;
        script.sync_status = SyncStatus::LocalOnly;
        script.cloud_id = None;
        script.cloud_version = None;
        script.last_synced_at = None;

        created.push(script.clone());
        library.scripts.push(script);
    }

    save_script_library(&library)?;

    info!("Imported {} playwright scripts", created.len());
    Ok(created)
}

/// Export all scripts as JSON
pub fn export_scripts() -> Result<String, String> {
    let library = load_script_library();
    serde_json::to_string_pretty(&library.scripts)
        .map_err(|e| format!("Failed to serialize scripts for export: {}", e))
}

/// Get all unique categories from existing scripts
pub fn get_categories() -> Vec<String> {
    let library = load_script_library();
    let mut categories: Vec<String> = library
        .scripts
        .iter()
        .map(|s| s.category.clone())
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort();
    categories.dedup();
    categories
}

/// Get all unique tags from existing scripts
pub fn get_all_tags() -> Vec<String> {
    let library = load_script_library();
    let mut tags: Vec<String> = library
        .scripts
        .iter()
        .flat_map(|s| s.tags.clone())
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

/// Search scripts by name, description, category, or tags
pub fn search_scripts(query: &str) -> Vec<PlaywrightScript> {
    let query_lower = query.to_lowercase();
    let library = load_script_library();

    library
        .scripts
        .into_iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&query_lower)
                || s.description.to_lowercase().contains(&query_lower)
                || s.category.to_lowercase().contains(&query_lower)
                || s.target_url.to_lowercase().contains(&query_lower)
                || s.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

/// Duplicate a script with a new name
pub fn duplicate_script(id: &str, new_name: Option<String>) -> Result<PlaywrightScript, String> {
    let original = get_script(id).ok_or_else(|| format!("Script not found: {}", id))?;

    let name = new_name.unwrap_or_else(|| format!("{} (Copy)", original.name));

    create_script(
        name,
        original.description,
        original.ai_instructions,
        original.target_url,
        original.script_content,
        original.category,
        original.tags,
        original.timeout_seconds,
        original.display_mode,
        original.browser,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_script_creation() {
        let script = PlaywrightScript::new(
            "Test Script".to_string(),
            "test('example', async () => {});".to_string(),
        );
        assert!(!script.id.is_empty());
        assert_eq!(script.name, "Test Script");
        assert_eq!(script.timeout_seconds, 120);
        assert_eq!(script.display_mode, DisplayMode::Headless);
        assert_eq!(script.browser, "chromium");
        assert_eq!(script.sync_status, SyncStatus::LocalOnly);
    }

    #[test]
    fn test_script_with_details() {
        let script = PlaywrightScript::with_details(
            "E2E Test".to_string(),
            "Tests the login flow".to_string(),
            Some("Test the login flow end-to-end".to_string()),
            "http://localhost:3000".to_string(),
            "test content".to_string(),
            "E2E".to_string(),
            vec!["login".to_string(), "auth".to_string()],
            120,
            DisplayMode::Headed,
            "firefox".to_string(),
        );
        assert_eq!(script.category, "E2E");
        assert_eq!(script.timeout_seconds, 120);
        assert_eq!(script.display_mode, DisplayMode::Headed);
        assert_eq!(script.browser, "firefox");
    }

    #[test]
    fn test_library_default() {
        let library = PlaywrightLibrary::default();
        assert!(library.scripts.is_empty());
        assert_eq!(library.version, "1.0.0");
    }
}
