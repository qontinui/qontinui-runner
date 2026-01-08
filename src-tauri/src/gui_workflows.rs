//! GUI Workflows Library
//!
//! This module provides persistent storage for GUI Action Workflows that users can
//! create, save, and run from the GUI Workflow Builder tab.
//!
//! GUI Workflows are deterministic sequences of GUI actions:
//! - Click (on StateImages)
//! - Type (text input)
//! - Hotkey (keyboard shortcuts)
//! - Go to State (state navigation)

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const GUI_WORKFLOWS_FILE: &str = "gui_workflows.json";

// ============================================================================
// Data Types
// ============================================================================

/// A single step in a GUI Action Workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiWorkflowStep {
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

impl GuiWorkflowStep {
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

/// A saved GUI Action Workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiWorkflow {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for the workflow
    pub name: String,
    /// Description of what this workflow does
    #[serde(default)]
    pub description: String,
    /// The ordered list of steps
    #[serde(default)]
    pub steps: Vec<GuiWorkflowStep>,
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
    /// Number of times this workflow has been run
    #[serde(default)]
    pub run_count: u32,
}

impl GuiWorkflow {
    /// Create a new GUI workflow
    pub fn new(
        name: String,
        description: String,
        steps: Vec<GuiWorkflowStep>,
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

/// Get the path to the GUI workflows storage file
fn get_storage_path() -> PathBuf {
    // Store in the app's local data directory
    let base_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner");

    // Create directory if it doesn't exist
    if !base_dir.exists() {
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create GUI workflows storage directory: {}", e);
        }
    }

    base_dir.join(GUI_WORKFLOWS_FILE)
}

/// Load all GUI workflows from storage
pub fn load_workflows() -> Vec<GuiWorkflow> {
    let path = get_storage_path();

    if !path.exists() {
        info!("GUI workflows file does not exist, returning empty list");
        return Vec::new();
    }

    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(workflows) => {
                info!("Loaded GUI workflows from {:?}", path);
                workflows
            }
            Err(e) => {
                error!("Failed to parse GUI workflows file: {}", e);
                Vec::new()
            }
        },
        Err(e) => {
            error!("Failed to read GUI workflows file: {}", e);
            Vec::new()
        }
    }
}

/// Save all GUI workflows to storage
fn save_workflows(workflows: &[GuiWorkflow]) -> Result<(), String> {
    let path = get_storage_path();

    let content = serde_json::to_string_pretty(workflows)
        .map_err(|e| format!("Failed to serialize GUI workflows: {}", e))?;

    fs::write(&path, content).map_err(|e| format!("Failed to write GUI workflows file: {}", e))?;

    info!("Saved {} GUI workflows to {:?}", workflows.len(), path);
    Ok(())
}

/// Create a new GUI workflow
pub fn create_workflow(
    name: String,
    description: String,
    steps: Vec<GuiWorkflowStep>,
    category: String,
    tags: Vec<String>,
) -> Result<GuiWorkflow, String> {
    let mut workflows = load_workflows();

    let workflow = GuiWorkflow::new(name, description, steps, category, tags);

    workflows.push(workflow.clone());
    save_workflows(&workflows)?;

    info!("Created GUI workflow: {} ({})", workflow.name, workflow.id);
    Ok(workflow)
}

/// Get a single GUI workflow by ID
pub fn get_workflow(id: &str) -> Option<GuiWorkflow> {
    let workflows = load_workflows();
    workflows.into_iter().find(|w| w.id == id)
}

/// Update an existing GUI workflow
pub fn update_workflow(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    steps: Option<Vec<GuiWorkflowStep>>,
    category: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<GuiWorkflow, String> {
    let mut workflows = load_workflows();

    let workflow = workflows
        .iter_mut()
        .find(|w| w.id == id)
        .ok_or_else(|| format!("GUI workflow not found: {}", id))?;

    if let Some(n) = name {
        workflow.name = n;
    }
    if let Some(d) = description {
        workflow.description = d;
    }
    if let Some(s) = steps {
        workflow.steps = s;
    }
    if let Some(cat) = category {
        workflow.category = cat;
    }
    if let Some(t) = tags {
        workflow.tags = t;
    }

    workflow.modified_at = chrono::Utc::now().to_rfc3339();

    let updated = workflow.clone();
    save_workflows(&workflows)?;

    info!("Updated GUI workflow: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Increment the run count for a workflow
pub fn increment_run_count(id: &str) -> Result<(), String> {
    let mut workflows = load_workflows();

    let workflow = workflows
        .iter_mut()
        .find(|w| w.id == id)
        .ok_or_else(|| format!("GUI workflow not found: {}", id))?;

    workflow.run_count += 1;
    workflow.modified_at = chrono::Utc::now().to_rfc3339();

    save_workflows(&workflows)?;
    info!("Incremented run count for GUI workflow: {}", id);
    Ok(())
}

/// Delete a GUI workflow by ID
pub fn delete_workflow(id: &str) -> Result<(), String> {
    let mut workflows = load_workflows();
    let original_len = workflows.len();

    workflows.retain(|w| w.id != id);

    if workflows.len() == original_len {
        return Err(format!("GUI workflow not found: {}", id));
    }

    save_workflows(&workflows)?;
    info!("Deleted GUI workflow: {}", id);
    Ok(())
}

/// List all GUI workflows, optionally filtered by category
pub fn list_workflows(category: Option<&str>) -> Vec<GuiWorkflow> {
    let workflows = load_workflows();

    match category {
        Some(cat) => workflows
            .into_iter()
            .filter(|w| w.category.eq_ignore_ascii_case(cat))
            .collect(),
        None => workflows,
    }
}

/// Search GUI workflows by name, description, or tags
pub fn search_workflows(query: &str) -> Vec<GuiWorkflow> {
    let workflows = load_workflows();
    let query_lower = query.to_lowercase();

    workflows
        .into_iter()
        .filter(|w| {
            w.name.to_lowercase().contains(&query_lower)
                || w.description.to_lowercase().contains(&query_lower)
                || w.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

/// Get all unique categories from saved workflows
pub fn get_categories() -> Vec<String> {
    let workflows = load_workflows();
    let mut categories: Vec<String> = workflows
        .iter()
        .map(|w| w.category.clone())
        .filter(|c| !c.is_empty())
        .collect();

    categories.sort();
    categories.dedup();
    categories
}

/// Get all unique tags from saved workflows
pub fn get_tags() -> Vec<String> {
    let workflows = load_workflows();
    let mut tags: Vec<String> = workflows.iter().flat_map(|w| w.tags.clone()).collect();

    tags.sort();
    tags.dedup();
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_click_step() {
        let step = GuiWorkflowStep::click(
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
        let step = GuiWorkflowStep::type_text("Type Username".to_string(), "testuser".to_string());

        assert_eq!(step.action_type, "type");
        assert_eq!(step.text_input, Some("testuser".to_string()));
    }

    #[test]
    fn test_create_hotkey_step() {
        let step = GuiWorkflowStep::hotkey("Copy".to_string(), "Ctrl+C".to_string());

        assert_eq!(step.action_type, "hotkey");
        assert_eq!(step.hotkey, Some("Ctrl+C".to_string()));
    }

    #[test]
    fn test_create_go_to_state_step() {
        let step = GuiWorkflowStep::go_to_state(
            "Go to Dashboard".to_string(),
            vec!["state-1".to_string()],
            vec!["Dashboard".to_string()],
        );

        assert_eq!(step.action_type, "go_to_state");
        assert!(step.target_state_ids.is_some());
    }
}
