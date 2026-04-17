//! AI Workflows Library
//!
//! This module provides persistent storage for AI Builder workflows that users can
//! save, organize, and run from the AI Automation Builder tab.
//!
//! AI Workflows are distinct from Prompts - they contain:
//! - Execution steps (GUI automation, Playwright tests, prompts, state visits)
//! - Screenshot capture settings
//! - Goal description
//!
//! ## Type source
//!
//! Wire-facing DTO types (`AiWorkflow`, `ExecutionStep`) are defined in
//! `qontinui-types::ai_workflows` and re-exported here. This module adds
//! runtime behaviour: file-system persistence, CRUD operations, search/filter.

pub use qontinui_types::ai_workflows::*;

use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use uuid::Uuid;

const AI_WORKFLOWS_FILE: &str = "ai_workflows.json";

// ============================================================================
// Extension trait — construction helpers that depend on runtime crates
// ============================================================================

/// Extension methods on `AiWorkflow` that require runtime dependencies
/// (`uuid`, `chrono`) not available in the pure-DTO crate.
pub trait AiWorkflowExt {
    /// Create a new workflow with all fields specified.
    fn with_details(
        name: String,
        description: String,
        steps: Vec<ExecutionStep>,
        goal: String,
        max_iterations: Option<u32>,
        capture_input_validation: bool,
        category: String,
        tags: Vec<String>,
        context_ids: Vec<String>,
        disabled_context_ids: Vec<String>,
        auto_include_contexts: bool,
    ) -> Self;
}

impl AiWorkflowExt for AiWorkflow {
    fn with_details(
        name: String,
        description: String,
        steps: Vec<ExecutionStep>,
        goal: String,
        max_iterations: Option<u32>,
        capture_input_validation: bool,
        category: String,
        tags: Vec<String>,
        context_ids: Vec<String>,
        disabled_context_ids: Vec<String>,
        auto_include_contexts: bool,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            steps,
            goal,
            max_iterations,
            capture_input_validation,
            category,
            tags,
            context_ids,
            disabled_context_ids,
            auto_include_contexts,
            created_at: now.clone(),
            modified_at: now,
        }
    }
}

// ============================================================================
// Storage Functions
// ============================================================================

/// Get the path to the AI workflows storage file (per-runner for secondary instances).
fn get_storage_path() -> PathBuf {
    // Store in the app's local data directory
    let base = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner");
    let base_dir = crate::instance::scope_path(&base);

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
pub fn create_workflow(
    name: String,
    description: String,
    steps: Vec<ExecutionStep>,
    goal: String,
    max_iterations: Option<u32>,
    capture_input_validation: bool,
    category: String,
    tags: Vec<String>,
    context_ids: Vec<String>,
    disabled_context_ids: Vec<String>,
    auto_include_contexts: bool,
) -> Result<AiWorkflow, String> {
    let mut workflows = load_workflows();

    let workflow = AiWorkflow::with_details(
        name,
        description,
        steps,
        goal,
        max_iterations,
        capture_input_validation,
        category,
        tags,
        context_ids,
        disabled_context_ids,
        auto_include_contexts,
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
pub fn update_workflow(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    steps: Option<Vec<ExecutionStep>>,
    goal: Option<String>,
    max_iterations: Option<u32>,
    capture_input_validation: Option<bool>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    context_ids: Option<Vec<String>>,
    disabled_context_ids: Option<Vec<String>>,
    auto_include_contexts: Option<bool>,
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
        // NOTE: the legacy update API can't express "set to unlimited" (None)
        // because `Option<u32>` here means "partial update" rather than the
        // stored value. Callers wanting unlimited must use the newer
        // unified_workflows update path.
        workflow.max_iterations = Some(m);
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
    if let Some(ctx) = context_ids {
        workflow.context_ids = ctx;
    }
    if let Some(dis) = disabled_context_ids {
        workflow.disabled_context_ids = dis;
    }
    if let Some(auto) = auto_include_contexts {
        workflow.auto_include_contexts = auto;
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
