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

use std::collections::HashSet;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use super::types::{Context, ContextAutoInclude};

/// Hard cap on the total size (in chars) of all ingested project + rule-file
/// context content combined. This is a context-window guard mandated by the
/// Track C risk section: a project with many large rule files must never be
/// allowed to blow up the prompt. When the running total would exceed this
/// cap, the offending context's content is deterministically truncated and a
/// marker is appended (rather than silently dropping the context entirely).
///
/// 80_000 chars ≈ 20K tokens — generous enough for real rule sets while
/// leaving room for the rest of the agentic prompt under typical 100K+ budgets.
pub const MAX_TOTAL_CONTEXT_CHARS: usize = 80_000;

/// Marker appended to content that was truncated by the size cap.
const TRUNCATION_MARKER: &str = "\n\n[...truncated by Qontinui context size cap...]";

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
    // Canonical Qontinui-native source.
    let mut contexts = load_qontinui_contexts_dir(project_path);

    // Additive interop: root CLAUDE.md / AGENTS.md / .cursorrules and
    // .cursor/rules/*.mdc. Path/glob-scoped where applicable.
    contexts.extend(load_rule_file_contexts(project_path));

    // De-dupe by content hash: the same file content ingested via two
    // conventions (e.g. a symlinked CLAUDE.md == AGENTS.md) collapses to one.
    dedupe_by_content(&mut contexts);

    // Hard size cap (context-window guard, MANDATORY per the plan's risk
    // section). Deterministically truncate rather than silently drop.
    enforce_total_size_cap(&mut contexts);

    contexts
}

/// Load the canonical `.qontinui/contexts/*.md` contexts (the original
/// behavior of `load_project_contexts_from_dir`).
fn load_qontinui_contexts_dir(project_path: &str) -> Vec<Context> {
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

// ============================================================================
// Rule-file interop ingestion (CLAUDE.md / AGENTS.md / .cursorrules / .mdc)
// ============================================================================
//
// These are additive, best-effort interop sources mirroring Minions'
// rule-file conventions. The canonical Qontinui-native source remains
// `.qontinui/contexts/*.md`; everything here augments it.
//
// ## Path/glob scoping model
//
// Each ingested rule-file context may carry an optional path scope, encoded
// into the standard [`ContextAutoInclude::file_patterns`] field:
//
// - **Root-level** rule files (root `CLAUDE.md` / `AGENTS.md` / `.cursorrules`)
//   are *always-attached*: no `file_patterns` ⇒ no scope filter.
// - A rule file located **under a subdirectory** (e.g. `src/foo/CLAUDE.md`)
//   attaches only when the task touches that subdirectory. Its scope glob is
//   derived as `<reldir>/**` (e.g. `src/foo/**`).
// - `.cursor/rules/*.mdc` files may declare explicit `globs:` frontmatter;
//   those are honored verbatim as the scope.
//
// Resolution-time filtering against the task's touched paths lives in
// `resolution.rs` (`context_matches_touched_paths`). A context with a scope
// attaches iff at least one touched path matches one of its globs. A context
// with no scope is unconditionally attached.

/// Ingest interop rule files from `project_path`.
fn load_rule_file_contexts(project_path: &str) -> Vec<Context> {
    let root = Path::new(project_path);
    let now = chrono::Utc::now().to_rfc3339();
    let mut contexts = Vec::new();

    // Root-level always-attached rule files. (file, id-suffix, name)
    for (rel, id_suffix, name) in [
        ("CLAUDE.md", "claude-md", "CLAUDE.md"),
        ("AGENTS.md", "agents-md", "AGENTS.md"),
        (".cursorrules", "cursorrules", ".cursorrules"),
    ] {
        let path = root.join(rel);
        if let Some(content) = read_nonempty(&path) {
            contexts.push(Context {
                id: format!("rule-{}", id_suffix),
                name: name.to_string(),
                content,
                category: Some("rule-file".to_string()),
                tags: vec!["rule-file".to_string()],
                // No file_patterns ⇒ always-attached.
                auto_include: None,
                created_at: now.clone(),
                modified_at: now.clone(),
            });
        }
    }

    // .cursor/rules/*.mdc — may carry explicit `globs:` frontmatter scope.
    let mdc_dir = root.join(".cursor").join("rules");
    if mdc_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&mdc_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("mdc") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let Some(raw) = read_nonempty(&path) else {
                    continue;
                };
                let (globs, content) = parse_mdc_frontmatter(&raw);
                let auto_include = globs.map(|patterns| ContextAutoInclude {
                    file_patterns: Some(patterns),
                    ..Default::default()
                });
                contexts.push(Context {
                    id: format!("rule-mdc-{}", stem),
                    name: format!(".cursor/rules/{}.mdc", stem),
                    content,
                    category: Some("rule-file".to_string()),
                    tags: vec!["rule-file".to_string(), "cursor".to_string()],
                    auto_include,
                    created_at: now.clone(),
                    modified_at: now.clone(),
                });
            }
        }
    }

    // Subdirectory-scoped CLAUDE.md / AGENTS.md: a rule file under a subdir
    // attaches only when the task touches that subdir. Scope = "<reldir>/**".
    for nested in find_nested_rule_files(root) {
        let Some(content) = read_nonempty(&nested) else {
            continue;
        };
        let rel = match nested.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let reldir = rel
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .filter(|s| !s.is_empty());
        let scope_glob = reldir.map(|d| format!("{}/**", d));
        let auto_include = scope_glob.map(|g| ContextAutoInclude {
            file_patterns: Some(vec![g]),
            ..Default::default()
        });
        // Sanitize the relative path into a deterministic, stable id.
        let id_part: String = rel_str
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        contexts.push(Context {
            id: format!("rule-nested-{}", id_part),
            name: rel_str,
            content,
            category: Some("rule-file".to_string()),
            tags: vec!["rule-file".to_string(), "scoped".to_string()],
            auto_include,
            created_at: now.clone(),
            modified_at: now.clone(),
        });
    }

    if !contexts.is_empty() {
        info!(
            "Ingested {} interop rule-file context(s) from {}",
            contexts.len(),
            root.display()
        );
    }

    contexts
}

/// Read a file, returning `Some(content)` only if it exists and is non-empty.
fn read_nonempty(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(c) if !c.trim().is_empty() => Some(c),
        Ok(_) => None,
        Err(_) => None,
    }
}

/// Find subdirectory-scoped `CLAUDE.md` / `AGENTS.md` rule files.
///
/// Walks the tree (bounded depth, skipping VCS/dependency dirs) and returns
/// every `CLAUDE.md` / `AGENTS.md` that is NOT at the repo root (root ones are
/// handled as always-attached above).
fn find_nested_rule_files(root: &Path) -> Vec<std::path::PathBuf> {
    const MAX_DEPTH: usize = 8;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        ".venv",
        "venv",
        "dist",
        "build",
        ".qontinui",
        ".cursor",
        "__pycache__",
    ];

    let mut found = Vec::new();
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if depth + 1 > MAX_DEPTH {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if SKIP_DIRS.contains(&name) || name.starts_with('.') {
                    continue;
                }
                stack.push((path, depth + 1));
            } else if file_type.is_file() && depth >= 1 {
                // depth >= 1 ⇒ not the repo root, so this is a nested rule file.
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if name == "CLAUDE.md" || name == "AGENTS.md" {
                    found.push(path);
                }
            }
        }
    }

    found.sort();
    found
}

/// Parse `.mdc` frontmatter, extracting an optional `globs:` scope.
///
/// Cursor `.mdc` files use YAML-ish frontmatter delimited by `---`. The
/// `globs` key may be a YAML list or a single comma-separated string. Returns
/// `(Some(globs), body)` when a non-empty `globs:` is present, else
/// `(None, full_content)`.
fn parse_mdc_frontmatter(raw: &str) -> (Option<Vec<String>>, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (None, raw.to_string());
    }
    let after_first = &trimmed[3..];
    let Some(pos) = after_first.find("\n---") else {
        return (None, raw.to_string());
    };
    let yaml_str = &after_first[..pos];
    let body = after_first[pos + 4..].trim_start_matches('\n').to_string();

    #[derive(serde::Deserialize, Default)]
    struct MdcFrontmatter {
        #[serde(default)]
        globs: Option<serde_yaml::Value>,
    }

    let fm: MdcFrontmatter = serde_yaml::from_str(yaml_str).unwrap_or_default();
    let globs = match fm.globs {
        Some(serde_yaml::Value::String(s)) => {
            let parts: Vec<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts)
            }
        }
        Some(serde_yaml::Value::Sequence(seq)) => {
            let parts: Vec<String> = seq
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts)
            }
        }
        _ => None,
    };

    (globs, body)
}

/// De-dupe contexts by content hash. The first occurrence wins (ordering:
/// `.qontinui/contexts` first, then root rule files, then `.mdc`, then nested),
/// so the canonical Qontinui-native source is preferred when content collides.
fn dedupe_by_content(contexts: &mut Vec<Context>) {
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    contexts.retain(|ctx| {
        let mut hasher = Sha256::new();
        hasher.update(ctx.content.as_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        seen.insert(digest)
    });
}

/// Enforce [`MAX_TOTAL_CONTEXT_CHARS`] across all ingested contexts.
///
/// Walks contexts in order, accumulating char counts. Once the running total
/// reaches the cap, the current context's content is deterministically
/// truncated to fit (with a marker) and all subsequent contexts are truncated
/// to empty-with-marker. Nothing is dropped from the list (so IDs/names remain
/// stable and visible), but oversized content never reaches the prompt.
fn enforce_total_size_cap(contexts: &mut [Context]) {
    let mut used = 0usize;
    for ctx in contexts.iter_mut() {
        let len = ctx.content.chars().count();
        if used >= MAX_TOTAL_CONTEXT_CHARS {
            ctx.content = TRUNCATION_MARKER.trim_start().to_string();
            continue;
        }
        let remaining = MAX_TOTAL_CONTEXT_CHARS - used;
        if len > remaining {
            let keep = remaining.saturating_sub(TRUNCATION_MARKER.chars().count());
            let truncated: String = ctx.content.chars().take(keep).collect();
            ctx.content = format!("{}{}", truncated, TRUNCATION_MARKER);
            used = MAX_TOTAL_CONTEXT_CHARS;
        } else {
            used += len;
        }
    }
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

    // ────────────────────────────────────────────────────────────────────
    // Track C1: rule-file ingestion, scoping, de-dupe, size cap
    // ────────────────────────────────────────────────────────────────────

    use std::fs;
    use tempfile::tempdir;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn find_ctx<'a>(ctxs: &'a [Context], id: &str) -> Option<&'a Context> {
        ctxs.iter().find(|c| c.id == id)
    }

    #[test]
    fn test_ingest_qontinui_contexts() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            ".qontinui/contexts/api.md",
            "# API\nGuidelines here.",
        );
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let c = find_ctx(&ctxs, "proj-ctx-api").expect("qontinui context present");
        assert!(c.content.contains("Guidelines here"));
    }

    #[test]
    fn test_ingest_claude_md() {
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "root claude rules");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let c = find_ctx(&ctxs, "rule-claude-md").expect("CLAUDE.md ingested");
        assert_eq!(c.content, "root claude rules");
        // Root rule files are always-attached (no scope).
        assert!(c.auto_include.is_none());
    }

    #[test]
    fn test_ingest_agents_md() {
        let dir = tempdir().unwrap();
        write(dir.path(), "AGENTS.md", "agents rules");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        assert!(find_ctx(&ctxs, "rule-agents-md").is_some());
    }

    #[test]
    fn test_ingest_cursorrules() {
        let dir = tempdir().unwrap();
        write(dir.path(), ".cursorrules", "legacy cursor rules");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let c = find_ctx(&ctxs, "rule-cursorrules").expect(".cursorrules ingested");
        assert_eq!(c.content, "legacy cursor rules");
    }

    #[test]
    fn test_ingest_mdc_with_globs() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            ".cursor/rules/backend.mdc",
            "---\nglobs:\n  - \"src/api/**\"\n  - \"src/db/**\"\n---\nBackend conventions.",
        );
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let c = find_ctx(&ctxs, "rule-mdc-backend").expect(".mdc ingested");
        assert!(c.content.contains("Backend conventions"));
        let patterns = c
            .auto_include
            .as_ref()
            .and_then(|ai| ai.file_patterns.clone())
            .expect("globs honored as file_patterns");
        assert_eq!(patterns, vec!["src/api/**", "src/db/**"]);
    }

    #[test]
    fn test_ingest_mdc_globs_comma_string() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            ".cursor/rules/x.mdc",
            "---\nglobs: \"*.rs, *.toml\"\n---\nbody",
        );
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let c = find_ctx(&ctxs, "rule-mdc-x").unwrap();
        let patterns = c
            .auto_include
            .as_ref()
            .unwrap()
            .file_patterns
            .clone()
            .unwrap();
        assert_eq!(patterns, vec!["*.rs", "*.toml"]);
    }

    #[test]
    fn test_nested_rule_file_is_scoped() {
        let dir = tempdir().unwrap();
        write(dir.path(), "src/foo/CLAUDE.md", "foo-specific rules");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let c = ctxs
            .iter()
            .find(|c| c.name == "src/foo/CLAUDE.md")
            .expect("nested CLAUDE.md ingested");
        let patterns = c
            .auto_include
            .as_ref()
            .and_then(|ai| ai.file_patterns.clone())
            .expect("nested rule file is path-scoped");
        assert_eq!(patterns, vec!["src/foo/**"]);
    }

    #[test]
    fn test_glob_scoping_attaches_only_for_matching_path() {
        use crate::context::resolution::context_matches_touched_paths;
        let dir = tempdir().unwrap();
        write(dir.path(), "src/foo/CLAUDE.md", "foo rules");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let scoped = ctxs.iter().find(|c| c.name == "src/foo/CLAUDE.md").unwrap();

        // Matches when a touched path is under src/foo/.
        assert!(context_matches_touched_paths(
            scoped,
            &["src/foo/bar.rs".to_string()]
        ));
        // Does not match an unrelated path.
        assert!(!context_matches_touched_paths(
            scoped,
            &["src/other/baz.rs".to_string()]
        ));
        // Does not match an empty touched set.
        assert!(!context_matches_touched_paths(scoped, &[]));
    }

    #[test]
    fn test_root_rule_always_attaches() {
        use crate::context::resolution::context_matches_touched_paths;
        let dir = tempdir().unwrap();
        write(dir.path(), "CLAUDE.md", "root rules");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let root = find_ctx(&ctxs, "rule-claude-md").unwrap();
        // Unscoped → attaches even with no touched paths.
        assert!(context_matches_touched_paths(root, &[]));
        assert!(context_matches_touched_paths(
            root,
            &["anything.rs".to_string()]
        ));
    }

    #[test]
    fn test_dedupe_identical_content() {
        let dir = tempdir().unwrap();
        // Same content via two conventions → collapses to one.
        write(dir.path(), "CLAUDE.md", "identical rules body");
        write(dir.path(), "AGENTS.md", "identical rules body");
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let count = ctxs
            .iter()
            .filter(|c| c.content == "identical rules body")
            .count();
        assert_eq!(count, 1, "duplicate content de-duped");
    }

    #[test]
    fn test_size_cap_truncates_deterministically() {
        let dir = tempdir().unwrap();
        // One oversized rule file well beyond the cap.
        let big = "x".repeat(MAX_TOTAL_CONTEXT_CHARS + 5_000);
        write(dir.path(), "CLAUDE.md", &big);
        let ctxs = load_project_contexts_from_dir(dir.path().to_str().unwrap());
        let total: usize = ctxs.iter().map(|c| c.content.chars().count()).sum();
        assert!(
            total <= MAX_TOTAL_CONTEXT_CHARS,
            "total {} exceeds cap {}",
            total,
            MAX_TOTAL_CONTEXT_CHARS
        );
        let c = find_ctx(&ctxs, "rule-claude-md").unwrap();
        assert!(
            c.content.contains("truncated by Qontinui context size cap"),
            "truncation marker present"
        );
    }

    #[test]
    fn test_parse_mdc_no_frontmatter() {
        let (globs, body) = parse_mdc_frontmatter("just a body, no fm");
        assert!(globs.is_none());
        assert_eq!(body, "just a body, no fm");
    }
}
