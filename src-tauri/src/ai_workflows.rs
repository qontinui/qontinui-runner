//! AI Workflows Library
//!
//! This module provides persistent storage for AI Builder workflows that users can
//! save, organize, and run from the AI Automation Builder tab.
//!
//! AI Workflows are distinct from Prompts - they contain:
//! - Execution steps (GUI automation, Playwright tests, prompts, state visits)
//! - Screenshot capture settings
//! - Session configuration (persistent vs standard mode)
//! - Goal description

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const AI_WORKFLOWS_FILE: &str = "ai_workflows.json";

// ============================================================================
// Data Types
// ============================================================================

/// A single step in an AI Builder execution sequence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    /// Unique identifier for this step
    pub id: String,
    /// Type of step: "workflow", "state", "playwright", or "prompt"
    #[serde(rename = "type")]
    pub step_type: String,
    /// Display name for the step
    pub name: String,
    /// Whether to capture a screenshot after this step
    #[serde(default, rename = "takeScreenshot")]
    pub take_screenshot: bool,
    /// For playwright steps: the script ID
    #[serde(default, rename = "playwrightScriptId")]
    pub playwright_script_id: Option<String>,
    /// For prompt steps: the prompt ID from the library
    #[serde(default, rename = "promptId")]
    pub prompt_id: Option<String>,
    /// For prompt steps: the actual prompt content
    #[serde(default, rename = "promptContent")]
    pub prompt_content: Option<String>,
}

/// A saved AI Builder workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiWorkflow {
    /// Unique identifier (UUID v4)
    pub id: String,
    /// Display name for the workflow
    pub name: String,
    /// Optional description of what this workflow does
    #[serde(default)]
    pub description: String,
    /// The ordered list of execution steps
    #[serde(default)]
    pub steps: Vec<ExecutionStep>,
    /// The goal/objective for this workflow
    #[serde(default)]
    pub goal: String,
    /// Maximum iterations for the AI loop
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Whether to use persistent session mode (spawns independent Claude sessions)
    #[serde(default)]
    pub persistent_session: bool,
    /// Whether to capture input for coordinate validation
    #[serde(default)]
    pub capture_input_validation: bool,
    /// Category for organization (e.g., "Testing", "Development")
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
}

fn default_max_iterations() -> u32 {
    10
}

impl AiWorkflow {
    /// Create a new workflow with auto-generated ID and timestamps
    #[allow(dead_code)]
    pub fn new(name: String, goal: String, steps: Vec<ExecutionStep>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description: String::new(),
            steps,
            goal,
            max_iterations: default_max_iterations(),
            persistent_session: false,
            capture_input_validation: false,
            category: String::new(),
            tags: Vec::new(),
            created_at: now.clone(),
            modified_at: now,
        }
    }

    /// Create a new workflow with all fields specified
    #[allow(clippy::too_many_arguments)]
    pub fn with_details(
        name: String,
        description: String,
        steps: Vec<ExecutionStep>,
        goal: String,
        max_iterations: u32,
        persistent_session: bool,
        capture_input_validation: bool,
        category: String,
        tags: Vec<String>,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            steps,
            goal,
            max_iterations,
            persistent_session,
            capture_input_validation,
            category,
            tags,
            created_at: now.clone(),
            modified_at: now,
        }
    }
}

// ============================================================================
// Storage Functions
// ============================================================================

/// Get the path to the AI workflows storage file
fn get_storage_path() -> PathBuf {
    // Store in the app's local data directory
    let base_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner");

    // Create directory if it doesn't exist
    if !base_dir.exists() {
        if let Err(e) = fs::create_dir_all(&base_dir) {
            error!("Failed to create AI workflows storage directory: {}", e);
        }
    }

    base_dir.join(AI_WORKFLOWS_FILE)
}

/// Load all AI workflows from storage
pub fn load_workflows() -> Vec<AiWorkflow> {
    let path = get_storage_path();

    if !path.exists() {
        info!("AI workflows file does not exist, returning empty list");
        return Vec::new();
    }

    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(workflows) => {
                info!("Loaded AI workflows from {:?}", path);
                workflows
            }
            Err(e) => {
                error!("Failed to parse AI workflows file: {}", e);
                Vec::new()
            }
        },
        Err(e) => {
            error!("Failed to read AI workflows file: {}", e);
            Vec::new()
        }
    }
}

/// Save all AI workflows to storage
fn save_workflows(workflows: &[AiWorkflow]) -> Result<(), String> {
    let path = get_storage_path();

    let content = serde_json::to_string_pretty(workflows)
        .map_err(|e| format!("Failed to serialize AI workflows: {}", e))?;

    fs::write(&path, content).map_err(|e| format!("Failed to write AI workflows file: {}", e))?;

    info!("Saved {} AI workflows to {:?}", workflows.len(), path);
    Ok(())
}

/// Create a new AI workflow
#[allow(clippy::too_many_arguments)]
pub fn create_workflow(
    name: String,
    description: String,
    steps: Vec<ExecutionStep>,
    goal: String,
    max_iterations: u32,
    persistent_session: bool,
    capture_input_validation: bool,
    category: String,
    tags: Vec<String>,
) -> Result<AiWorkflow, String> {
    let mut workflows = load_workflows();

    let workflow = AiWorkflow::with_details(
        name,
        description,
        steps,
        goal,
        max_iterations,
        persistent_session,
        capture_input_validation,
        category,
        tags,
    );

    workflows.push(workflow.clone());
    save_workflows(&workflows)?;

    info!("Created AI workflow: {} ({})", workflow.name, workflow.id);
    Ok(workflow)
}

/// Get a single AI workflow by ID
pub fn get_workflow(id: &str) -> Option<AiWorkflow> {
    let workflows = load_workflows();
    workflows.into_iter().find(|w| w.id == id)
}

/// Update an existing AI workflow
#[allow(clippy::too_many_arguments)]
pub fn update_workflow(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    steps: Option<Vec<ExecutionStep>>,
    goal: Option<String>,
    max_iterations: Option<u32>,
    persistent_session: Option<bool>,
    capture_input_validation: Option<bool>,
    category: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<AiWorkflow, String> {
    let mut workflows = load_workflows();

    let workflow = workflows
        .iter_mut()
        .find(|w| w.id == id)
        .ok_or_else(|| format!("AI workflow not found: {}", id))?;

    if let Some(n) = name {
        workflow.name = n;
    }
    if let Some(d) = description {
        workflow.description = d;
    }
    if let Some(s) = steps {
        workflow.steps = s;
    }
    if let Some(g) = goal {
        workflow.goal = g;
    }
    if let Some(m) = max_iterations {
        workflow.max_iterations = m;
    }
    if let Some(p) = persistent_session {
        workflow.persistent_session = p;
    }
    if let Some(c) = capture_input_validation {
        workflow.capture_input_validation = c;
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

    info!("Updated AI workflow: {} ({})", updated.name, updated.id);
    Ok(updated)
}

/// Delete an AI workflow by ID
pub fn delete_workflow(id: &str) -> Result<(), String> {
    let mut workflows = load_workflows();
    let original_len = workflows.len();

    workflows.retain(|w| w.id != id);

    if workflows.len() == original_len {
        return Err(format!("AI workflow not found: {}", id));
    }

    save_workflows(&workflows)?;
    info!("Deleted AI workflow: {}", id);
    Ok(())
}

/// List all AI workflows, optionally filtered by category
pub fn list_workflows(category: Option<&str>) -> Vec<AiWorkflow> {
    let workflows = load_workflows();

    match category {
        Some(cat) => workflows
            .into_iter()
            .filter(|w| w.category.eq_ignore_ascii_case(cat))
            .collect(),
        None => workflows,
    }
}

/// Search AI workflows by name or description
pub fn search_workflows(query: &str) -> Vec<AiWorkflow> {
    let workflows = load_workflows();
    let query_lower = query.to_lowercase();

    workflows
        .into_iter()
        .filter(|w| {
            w.name.to_lowercase().contains(&query_lower)
                || w.description.to_lowercase().contains(&query_lower)
                || w.goal.to_lowercase().contains(&query_lower)
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
