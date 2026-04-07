//! Macros Library
//!
//! This module provides persistent storage for Macros that users can
//! create, save, and run from the Macro Builder tab.
//!
//! Macros are deterministic sequences of GUI actions:
//! - Click (on StateImages)
//! - Type (text input)
//! - Hotkey (keyboard shortcuts)
//! - Go to State (state navigation)

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const MACROS_FILE: &str = "macros.json";
const LEGACY_GUI_WORKFLOWS_FILE: &str = "gui_workflows.json";

// ============================================================================
// Data Types
// ============================================================================

/// A single step in a Macro
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacroStep {
    /// Unique identifier for this step
    pub id: String,
    /// Action type: "click", "double_click", "right_click", "type", "hotkey", "go_to_state"
    pub action_type: String,
    /// Display name for the step
    pub name: String,

    // Click action configuration
    /// Target StateImage IDs (one or more) - first match will be clicked
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_image_ids: Option<Vec<String>>,
    /// Target StateImage names (for display)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_image_names: Option<Vec<String>>,

    // Type action configuration
    /// Text to type (for "type" action)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_input: Option<String>,

    // Hotkey action configuration
    /// Key combination string (for "hotkey" action), e.g., "Ctrl+C", "Alt+Tab"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,

    // Go to state configuration
    /// Target state IDs (one or more) - navigates to any of these
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_state_ids: Option<Vec<String>>,
    /// Target state names (for display)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_state_names: Option<Vec<String>>,

    // Timing configuration
    /// Pause after this step in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<u32>,

    // Execution settings
    /// Monitor index (0 = primary, None = all monitors)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_index: Option<i32>,
    /// Timeout for this step in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl MacroStep {
    /// Create a new click step
    pub fn click(
        name: String,
        target_image_ids: Vec<String>,
        target_image_names: Vec<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            action_type: "click".to_string(),
            name,
            target_image_ids: Some(target_image_ids),
            target_image_names: Some(target_image_names),
            text_input: None,
            hotkey: None,
            target_state_ids: None,
            target_state_names: None,
            pause_after_ms: None,
            monitor_index: None,
            timeout_seconds: None,
        }
    }

    /// Create a new type step
    pub fn type_text(name: String, text: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            action_type: "type".to_string(),
            name,
            target_image_ids: None,
            target_image_names: None,
            text_input: Some(text),
            hotkey: None,
            target_state_ids: None,
            target_state_names: None,
            pause_after_ms: None,
            monitor_index: None,
            timeout_seconds: None,
        }
    }

    /// Create a new hotkey step
    pub fn hotkey(name: String, keys: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            action_type: "hotkey".to_string(),
            name,
            target_image_ids: None,
            target_image_names: None,
            text_input: None,
            hotkey: Some(keys),
            target_state_ids: None,
            target_state_names: None,
            pause_after_ms: None,
            monitor_index: None,
            timeout_seconds: None,
        }
    }

    /// Create a new go_to_state step
    pub fn go_to_state(
        name: String,
        target_state_ids: Vec<String>,
        target_state_names: Vec<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            action_type: "go_to_state".to_string(),
            name,
            target_image_ids: None,
            target_image_names: None,
            text_input: None,
            hotkey: None,
            target_state_ids: Some(target_state_ids),
            target_state_names: Some(target_state_names),
            pause_after_ms: None,
            monitor_index: None,
            timeout_seconds: None,
        }
    }
}

/// A saved Macro
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Macro {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for the macro
    pub name: String,
    /// Description of what this macro does
    #[serde(default)]
    pub description: String,
    /// The ordered list of steps
    #[serde(default)]
    pub steps: Vec<MacroStep>,
    /// Category for organization
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
    /// Number of times this macro has been run
    #[serde(default)]
    pub run_count: u32,
}

impl Macro {
    /// Create a new Macro
    pub fn new(
        name: String,
        description: String,
        steps: Vec<MacroStep>,
        category: String,
        tags: Vec<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            steps,
            category,
            tags,
            created_at: now.clone(),
            modified_at: now,
            run_count: 0,
        }
    }
}

// ============================================================================
// Storage Functions
// ============================================================================

/// Get the base storage directory (per-runner for secondary instances).
fn get_storage_dir() -> PathBuf {
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner");
    let base_dir = crate::instance::scope_path(&base);

    // Create directory if it doesn't exist
    if !base_dir.exists() {
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create macros storage directory: {}", e);
        }
    }

    base_dir
}

/// Get the path to the macros storage file
fn get_storage_path() -> PathBuf {
    get_storage_dir().join(MACROS_FILE)
}

/// Get the path to the legacy GUI workflows file
fn get_legacy_storage_path() -> PathBuf {
    get_storage_dir().join(LEGACY_GUI_WORKFLOWS_FILE)
}

/// Migrate legacy gui_workflows.json to macros.json if needed
fn migrate_legacy_file() {
    let legacy_path = get_legacy_storage_path();
    let new_path = get_storage_path();

    // If new file already exists, no migration needed
    if new_path.exists() {
        return;
    }

    // If legacy file exists, rename it
    if legacy_path.exists() {
        info!(
            "Migrating legacy GUI workflows file to macros: {:?} -> {:?}",
            legacy_path, new_path
        );
        if let Err(e) = fs::rename(&legacy_path, &new_path) {
            error!("Failed to migrate legacy GUI workflows file: {}", e);
            // Try copy instead of rename as fallback
            if let Ok(content) = fs::read_to_string(&legacy_path) {
                if let Err(e) = fs::write(&new_path, content) {
                    error!("Failed to copy legacy GUI workflows file: {}", e);
                }
            }
        }
    }
}

/// Load all macros from storage
pub fn load_macros() -> Vec<Macro> {
    // Ensure legacy file is migrated
    migrate_legacy_file();

    let path = get_storage_path();

    if !path.exists() {
        info!("Macros file does not exist, returning empty list");
        return Vec::new();
    }

    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(macros) => {
                info!("Loaded macros from {:?}", path);
                macros
            }
            Err(e) => {
                error!("Failed to parse macros file: {}", e);
                Vec::new()
            }
        },
        Err(e) => {
            error!("Failed to read macros file: {}", e);
            Vec::new()
        }
    }
}

/// Save all macros to storage
fn save_macros(macros: &[Macro]) -> Result<(), String> {
    let path = get_storage_path();

    let content = serde_json::to_string_pretty(macros)
        .map_err(|e| format!("Failed to serialize macros: {}", e))?;

    fs::write(&path, content).map_err(|e| format!("Failed to write macros file: {}", e))?;

    info!("Saved {} macros to {:?}", macros.len(), path);
    Ok(())
}

/// Create a new macro
pub fn create_macro(
    name: String,
    description: String,
    steps: Vec<MacroStep>,
    category: String,
    tags: Vec<String>,
) -> Result<Macro, String> {
    let mut macros = load_macros();

    let new_macro = Macro::new(name, description, steps, category, tags);

    macros.push(new_macro.clone());
    save_macros(&macros)?;

    info!("Created macro: {} ({})", new_macro.name, new_macro.id);
    Ok(new_macro)
}

/// Get a single macro by ID
pub fn get_macro(id: &str) -> Option<Macro> {
    let macros = load_macros();
    macros.into_iter().find(|m| m.id == id)
}

/// Update an existing macro
pub fn update_macro(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    steps: Option<Vec<MacroStep>>,
    category: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<Macro, String> {
    let mut macros = load_macros();

    let macro_item = macros
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("Macro not found: {}", id))?;

    if let Some(n) = name {
        macro_item.name = n;
    }
    if let Some(d) = description {
        macro_item.description = d;
    }
    if let Some(s) = steps {
        macro_item.steps = s;
    }
    if let Some(cat) = category {
        macro_item.category = cat;
    }
    if let Some(t) = tags {
        macro_item.tags = t;
    }

    macro_item.modified_at = chrono::Utc::now().to_rfc3339();

    let updated = macro_item.clone();
    save_macros(&macros)?;

    info!("Updated macro: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Increment the run count for a macro
pub fn increment_run_count(id: &str) -> Result<(), String> {
    let mut macros = load_macros();

    let macro_item = macros
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("Macro not found: {}", id))?;

    macro_item.run_count += 1;
    macro_item.modified_at = chrono::Utc::now().to_rfc3339();

    save_macros(&macros)?;
    info!("Incremented run count for macro: {}", id);
    Ok(())
}

/// Delete a macro by ID
pub fn delete_macro(id: &str) -> Result<(), String> {
    let mut macros = load_macros();
    let original_len = macros.len();

    macros.retain(|m| m.id != id);

    if macros.len() == original_len {
        return Err(format!("Macro not found: {}", id));
    }

    save_macros(&macros)?;
    info!("Deleted macro: {}", id);
    Ok(())
}

/// List all macros, optionally filtered by category
pub fn list_macros(category: Option<&str>) -> Vec<Macro> {
    let macros = load_macros();

    match category {
        Some(cat) => macros
            .into_iter()
            .filter(|m| m.category.eq_ignore_ascii_case(cat))
            .collect(),
        None => macros,
    }
}

/// Search macros by name, description, or tags
pub fn search_macros(query: &str) -> Vec<Macro> {
    let macros = load_macros();
    let query_lower = query.to_lowercase();

    macros
        .into_iter()
        .filter(|m| {
            m.name.to_lowercase().contains(&query_lower)
                || m.description.to_lowercase().contains(&query_lower)
                || m.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

/// Get all unique categories from saved macros
pub fn get_categories() -> Vec<String> {
    let macros = load_macros();
    let mut categories: Vec<String> = macros
        .iter()
        .map(|m| m.category.clone())
        .filter(|c| !c.is_empty())
        .collect();

    categories.sort();
    categories.dedup();
    categories
}

/// Get all unique tags from saved macros
pub fn get_tags() -> Vec<String> {
    let macros = load_macros();
    let mut tags: Vec<String> = macros.iter().flat_map(|m| m.tags.clone()).collect();

    tags.sort();
    tags.dedup();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_click_step() {
        let step = MacroStep::click(
            "Click Login".to_string(),
            vec!["img-1".to_string()],
            vec!["Login Button".to_string()],
        );

        assert_eq!(step.action_type, "click");
        assert_eq!(step.name, "Click Login");
        assert!(step.target_image_ids.is_some());
        assert_eq!(step.target_image_ids.unwrap().len(), 1);
    }

    #[test]
    fn test_create_type_step() {
        let step = MacroStep::type_text("Type Username".to_string(), "testuser".to_string());

        assert_eq!(step.action_type, "type");
        assert_eq!(step.text_input, Some("testuser".to_string()));
    }

    #[test]
    fn test_create_hotkey_step() {
        let step = MacroStep::hotkey("Copy".to_string(), "Ctrl+C".to_string());

        assert_eq!(step.action_type, "hotkey");
        assert_eq!(step.hotkey, Some("Ctrl+C".to_string()));
    }

    #[test]
    fn test_create_go_to_state_step() {
        let step = MacroStep::go_to_state(
            "Go to Dashboard".to_string(),
            vec!["state-1".to_string()],
            vec!["Dashboard".to_string()],
        );

        assert_eq!(step.action_type, "go_to_state");
        assert!(step.target_state_ids.is_some());
    }
}
