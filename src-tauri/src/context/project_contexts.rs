//! Project context operations (stored in project config JSON).

#![allow(dead_code)]

use tracing::info;

use super::types::{Context, ContextAutoInclude};

/// Get all contexts from a loaded configuration.
///
/// Returns contexts stored in the project config as Context objects.
pub fn get_project_contexts_from_config(contexts_json: &[serde_json::Value]) -> Vec<Context> {
    contexts_json
        .iter()
        .filter_map(|v| serde_json::from_value::<Context>(v.clone()).ok())
        .collect()
}

/// Find a project context by ID in a configuration.
pub fn get_project_context_from_config(
    contexts_json: &[serde_json::Value],
    context_id: &str,
) -> Option<Context> {
    get_project_contexts_from_config(contexts_json)
        .into_iter()
        .find(|c| c.id == context_id)
}

/// Add a context to a configuration's contexts array.
///
/// Returns the updated contexts array.
pub fn add_project_context_to_config(
    contexts_json: &mut Vec<serde_json::Value>,
    context: Context,
) -> Result<(), String> {
    // Check for duplicate ID
    if get_project_context_from_config(contexts_json, &context.id).is_some() {
        return Err(format!("Context with ID '{}' already exists", context.id));
    }

    let json_value = serde_json::to_value(&context)
        .map_err(|e| format!("Failed to serialize context: {}", e))?;
    contexts_json.push(json_value);

    info!(
        "Added project context '{}' (id: {})",
        context.name, context.id
    );
    Ok(())
}

/// Update a context in a configuration's contexts array.
///
/// Returns the updated contexts array.
pub fn update_project_context_in_config(
    contexts_json: &mut [serde_json::Value],
    context: Context,
) -> Result<(), String> {
    let index = contexts_json
        .iter()
        .position(|v| {
            v.get("id")
                .and_then(|id| id.as_str())
                .map(|id| id == context.id)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("Context with ID '{}' not found", context.id))?;

    let json_value = serde_json::to_value(&context)
        .map_err(|e| format!("Failed to serialize context: {}", e))?;
    contexts_json[index] = json_value;

    info!(
        "Updated project context '{}' (id: {})",
        context.name, context.id
    );
    Ok(())
}

/// Delete a context from a configuration's contexts array.
pub fn delete_project_context_from_config(
    contexts_json: &mut Vec<serde_json::Value>,
    context_id: &str,
) -> Result<(), String> {
    let index = contexts_json
        .iter()
        .position(|v| {
            v.get("id")
                .and_then(|id| id.as_str())
                .map(|id| id == context_id)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("Context with ID '{}' not found", context_id))?;

    contexts_json.remove(index);

    info!("Deleted project context (id: {})", context_id);
    Ok(())
}

/// Create a new project context with the given details.
pub fn create_project_context(
    name: String,
    content: String,
    category: Option<String>,
    tags: Vec<String>,
    auto_include: Option<ContextAutoInclude>,
) -> Context {
    Context::new(name, content, category, tags, auto_include)
}
