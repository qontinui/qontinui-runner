//! Project context operations.
//!
//! Project contexts can be loaded from two sources:
//! 1. **Project config JSON** — contexts stored as JSON values in a config array
//! 2. **`.qontinui/contexts/` directory** — markdown files in the project root
//!
//! The directory-based approach follows the same pattern as `.qontinui/constitution.md`
//! and `.qontinui/constraints.toml`. Each `.md` file becomes a context with its
//! filename as the name and optional YAML frontmatter for metadata.

#![allow(dead_code)]

use std::path::Path;
use tracing::{debug, info, warn};

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

// ============================================================================
// Directory-based project contexts (.qontinui/contexts/*.md)
// ============================================================================

/// Load project contexts from `.qontinui/contexts/` in the given project directory.
///
/// Each `.md` file becomes a context. The file may contain optional YAML frontmatter
/// delimited by `---` lines at the top:
///
/// ```markdown
/// ---
/// category: architecture
/// tags: [api, rest, endpoints]
/// autoInclude:
///   taskMentions: [api, endpoint, route]
/// ---
///
/// # API Design Guidelines
/// ...actual context content...
/// ```
///
/// If no frontmatter is present, the entire file is used as content.
/// The context name is derived from the filename (e.g., `api-guidelines.md` → "api-guidelines").
/// Context IDs are deterministic: `proj-ctx-{filename_stem}` so they remain stable across loads.
pub fn load_project_contexts_from_dir(project_path: &str) -> Vec<Context> {
    let ctx_dir = Path::new(project_path).join(".qontinui").join("contexts");

    if !ctx_dir.is_dir() {
        debug!("No .qontinui/contexts/ directory at {}", ctx_dir.display());
        return Vec::new();
    }

    let entries = match std::fs::read_dir(&ctx_dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "Failed to read .qontinui/contexts/ at {}: {}",
                ctx_dir.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut contexts = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for entry in entries.flatten() {
        let path = entry.path();

        // Only process .md files
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let raw_content = match std::fs::read_to_string(&path) {
            Ok(c) if !c.trim().is_empty() => c,
            Ok(_) => {
                debug!("Skipping empty project context file: {}", path.display());
                continue;
            }
            Err(e) => {
                warn!(
                    "Failed to read project context file {}: {}",
                    path.display(),
                    e
                );
                continue;
            }
        };

        // Parse optional YAML frontmatter
        let (frontmatter, content) = parse_frontmatter(&raw_content);

        // Derive name from frontmatter or filename
        let name = frontmatter
            .as_ref()
            .and_then(|fm| fm.name.clone())
            .unwrap_or_else(|| stem.clone());

        let category = frontmatter.as_ref().and_then(|fm| fm.category.clone());
        let tags = frontmatter
            .as_ref()
            .and_then(|fm| fm.tags.clone())
            .unwrap_or_default();
        let auto_include = frontmatter.and_then(|fm| fm.auto_include);

        // Deterministic ID so the context is stable across reloads
        let id = format!("proj-ctx-{}", stem);

        contexts.push(Context {
            id,
            name,
            content,
            category,
            tags,
            auto_include,
            created_at: now.clone(),
            modified_at: now.clone(),
        });
    }

    if !contexts.is_empty() {
        info!(
            "Loaded {} project context(s) from {}",
            contexts.len(),
            ctx_dir.display()
        );
    }

    contexts
}

/// Load project contexts from the current working directory.
///
/// Convenience wrapper that uses `std::env::current_dir()`.
pub fn get_project_contexts() -> Vec<Context> {
    match std::env::current_dir() {
        Ok(cwd) => match cwd.to_str() {
            Some(path) => load_project_contexts_from_dir(path),
            None => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

// ============================================================================
// Frontmatter parsing
// ============================================================================

/// Optional YAML frontmatter fields for project context markdown files.
#[derive(Debug, Clone, serde::Deserialize, Default)]
struct ContextFrontmatter {
    /// Override the context name (default: filename stem)
    name: Option<String>,
    /// Category for organization
    category: Option<String>,
    /// Tags for grouping
    tags: Option<Vec<String>>,
    /// Auto-include rules
    #[serde(rename = "autoInclude")]
    auto_include: Option<ContextAutoInclude>,
}

/// Parse YAML frontmatter from a markdown string.
///
/// Frontmatter is delimited by `---` lines at the top of the file.
/// Returns (parsed frontmatter, remaining content).
fn parse_frontmatter(raw: &str) -> (Option<ContextFrontmatter>, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (None, raw.to_string());
    }

    // Find the closing `---`
    let after_first = &trimmed[3..];
    let closing = after_first.find("\n---");
    match closing {
        Some(pos) => {
            let yaml_str = &after_first[..pos];
            let content = &after_first[pos + 4..]; // skip "\n---"

            match serde_yaml::from_str::<ContextFrontmatter>(yaml_str) {
                Ok(fm) => (Some(fm), content.trim_start_matches('\n').to_string()),
                Err(e) => {
                    warn!("Failed to parse frontmatter YAML: {}", e);
                    (None, raw.to_string())
                }
            }
        }
        None => {
            // No closing ---, treat entire file as content
            (None, raw.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let raw = "---\ncategory: architecture\ntags: [api, rest]\n---\n\n# API Guidelines\nContent here.";
        let (fm, content) = parse_frontmatter(raw);
        let fm = fm.unwrap();
        assert_eq!(fm.category, Some("architecture".to_string()));
        assert_eq!(fm.tags, Some(vec!["api".to_string(), "rest".to_string()]));
        assert!(content.starts_with("# API Guidelines"));
    }

    #[test]
    fn test_parse_frontmatter_without_yaml() {
        let raw = "# Just Content\nNo frontmatter here.";
        let (fm, content) = parse_frontmatter(raw);
        assert!(fm.is_none());
        assert_eq!(content, raw);
    }

    #[test]
    fn test_parse_frontmatter_with_auto_include() {
        let raw = "---\nautoInclude:\n  taskMentions: [api, endpoint]\n---\n\nContent";
        let (fm, content) = parse_frontmatter(raw);
        let fm = fm.unwrap();
        let ai = fm.auto_include.unwrap();
        assert_eq!(
            ai.task_mentions,
            Some(vec!["api".to_string(), "endpoint".to_string()])
        );
        assert_eq!(content, "Content");
    }

    #[test]
    fn test_load_nonexistent_dir() {
        let contexts = load_project_contexts_from_dir("/nonexistent/path");
        assert!(contexts.is_empty());
    }
}
