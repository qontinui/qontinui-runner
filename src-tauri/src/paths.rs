//! Centralized path resolution for qontinui-runner.
//!
//! This module provides a single source of truth for all file system paths used by the runner.
//! All paths are configurable via settings, with sensible cross-platform defaults.
//!
//! # Default Locations (when no override is configured)
//!
//! | Platform | Dev Logs Directory |
//! |----------|-------------------|
//! | Windows  | `C:\Users\<user>\AppData\Local\qontinui-runner\dev-logs` |
//! | macOS    | `~/Library/Application Support/qontinui-runner/dev-logs` |
//! | Linux    | `~/.local/share/qontinui-runner/dev-logs` |
//!
//! # Configuration
//!
//! Paths can be overridden in settings.json:
//! ```json
//! {
//!   "paths": {
//!     "dev_logs_dir": "D:\\qontinui_parent_directory\\.dev-logs"
//!   }
//! }
//! ```

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::debug;

/// Cached dev logs directory to avoid repeated settings lookups
static DEV_LOGS_DIR_CACHE: OnceLock<PathBuf> = OnceLock::new();

/// Get the base dev logs directory.
///
/// Resolution order:
/// 1. Settings override (`paths.dev_logs_dir`)
/// 2. Default: `<app_data>/qontinui-runner/dev-logs`
///
/// The directory is created if it doesn't exist.
pub fn get_dev_logs_dir() -> PathBuf {
    DEV_LOGS_DIR_CACHE
        .get_or_init(|| {
            // Check settings override
            if let Some(override_path) = crate::settings::get_dev_logs_dir_override() {
                let path = PathBuf::from(&override_path);
                debug!("Using dev_logs_dir override: {}", path.display());
                ensure_dir_exists(&path);
                return path;
            }

            // Use portable default
            let default_path = get_default_dev_logs_dir();
            debug!("Using default dev_logs_dir: {}", default_path.display());
            ensure_dir_exists(&default_path);
            default_path
        })
        .clone()
}

/// Get the default dev logs directory (cross-platform).
fn get_default_dev_logs_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qontinui-runner")
        .join("dev-logs")
}

/// Ensure a directory exists, creating it if necessary.
fn ensure_dir_exists(path: &PathBuf) {
    if !path.exists() {
        if let Err(e) = std::fs::create_dir_all(path) {
            tracing::warn!("Failed to create directory {}: {}", path.display(), e);
        }
    }
}

// ============================================================================
// Subdirectory accessors
// ============================================================================

/// Get the screenshots directory (`<dev_logs>/screenshots`).
pub fn get_screenshots_dir() -> PathBuf {
    let path = get_dev_logs_dir().join("screenshots");
    ensure_dir_exists(&path);
    path
}

/// Get the state explorer directory (`<dev_logs>/state-explorer`).
pub fn get_state_explorer_dir() -> PathBuf {
    let path = get_dev_logs_dir().join("state-explorer");
    ensure_dir_exists(&path);
    path
}

/// Get the state explorer screenshots directory (`<dev_logs>/state-explorer/screenshots`).
pub fn get_state_explorer_screenshots_dir() -> PathBuf {
    let path = get_state_explorer_dir().join("screenshots");
    ensure_dir_exists(&path);
    path
}

/// Get the API images directory (`<dev_logs>/api-images`).
pub fn get_api_images_dir() -> PathBuf {
    let path = get_dev_logs_dir().join("api-images");
    ensure_dir_exists(&path);
    path
}

/// Get the workflow debug log path (`<dev_logs>/workflow-debug.log`).
pub fn get_workflow_debug_log_path() -> PathBuf {
    get_dev_logs_dir().join("workflow-debug.log")
}

/// Get the AI output JSONL path (`<dev_logs>/ai-output.jsonl`).
pub fn get_ai_output_jsonl_path() -> PathBuf {
    get_dev_logs_dir().join("ai-output.jsonl")
}

/// Get the runner general JSONL path (`<dev_logs>/runner-general.jsonl`).
pub fn get_runner_general_jsonl_path() -> PathBuf {
    get_dev_logs_dir().join("runner-general.jsonl")
}

/// Get the runner actions JSONL path (`<dev_logs>/runner-actions.jsonl`).
pub fn get_runner_actions_jsonl_path() -> PathBuf {
    get_dev_logs_dir().join("runner-actions.jsonl")
}

/// Get the runner image recognition JSONL path (`<dev_logs>/runner-image-recognition.jsonl`).
pub fn get_runner_image_recognition_jsonl_path() -> PathBuf {
    get_dev_logs_dir().join("runner-image-recognition.jsonl")
}

/// Get the runner playwright JSONL path (`<dev_logs>/runner-playwright.jsonl`).
pub fn get_runner_playwright_jsonl_path() -> PathBuf {
    get_dev_logs_dir().join("runner-playwright.jsonl")
}

/// Get the component render log path (`<dev_logs>/runner-render.jsonl`).
/// Used for testing that UI components render the correct data.
pub fn get_render_log_path() -> PathBuf {
    get_dev_logs_dir().join("runner-render.jsonl")
}

/// Get the DOM capture log path (`<dev_logs>/dom-capture.jsonl`).
pub fn get_dom_capture_log_path() -> PathBuf {
    get_dev_logs_dir().join("dom-capture.jsonl")
}

/// Get the tracing spans JSONL path (`<dev_logs>/runner-spans.jsonl`).
/// Contains all tracing spans for real-time debugging and performance analysis.
pub fn get_runner_spans_jsonl_path() -> PathBuf {
    get_dev_logs_dir().join("runner-spans.jsonl")
}

/// Clear the cached dev logs directory.
/// Call this after settings are changed to pick up new values.
pub fn clear_cache() {
    // OnceLock doesn't support clearing, so we'd need a different approach
    // for runtime updates. For now, settings changes require restart.
    tracing::info!("Path cache clear requested - restart required for path changes to take effect");
}

// ============================================================================
// Path utilities
// ============================================================================

/// Convert a path to a string, handling non-UTF8 paths gracefully.
pub fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Get the dev logs directory as a string.
pub fn get_dev_logs_dir_string() -> String {
    path_to_string(&get_dev_logs_dir())
}

/// Resolve a working directory path, converting relative paths to absolute.
///
/// If the path is already absolute, returns it unchanged.
/// If relative, resolves against the workspace root (the parent directory of qontinui-runner).
/// Falls back to the current working directory if workspace detection fails.
///
/// Always returns an absolute path with normalized separators. On Windows,
/// returning a relative path to `Command::current_dir()` causes OS error 267
/// (ERROR_DIRECTORY) when the process CWD is invalid or the relative path
/// can't be resolved by CreateProcessW. Mixed path separators (forward and
/// back slashes) can also cause issues with some Windows process APIs.
pub fn resolve_working_directory(wd: &str) -> PathBuf {
    let path = PathBuf::from(wd);
    if path.is_absolute() {
        return normalize_path_separators(path);
    }

    // Get workspace paths: workspace_root is parent of runner_dir
    let workspace_paths = crate::mcp::shared::get_workspace_paths_internal().ok();
    let workspace_root = workspace_paths.as_ref().map(|(root, _, _)| root.clone());
    // runner_dir = workspace_root/qontinui-runner (the runner project directory)
    let runner_dir = workspace_root
        .as_ref()
        .map(|root| root.join("qontinui-runner"));

    // Try runner_dir first — working directories like ".." are typically relative
    // to the runner project, not the workspace root. E.g. ".." from runner_dir
    // goes to workspace_root, whereas ".." from workspace_root escapes the workspace.
    if let Some(ref runner) = runner_dir {
        if runner.exists() {
            let resolved = runner.join(&path);
            if resolved.exists() {
                // Canonicalize to resolve ".." components — embedded ".." in paths
                // can break npx/cmd.exe module resolution on Windows
                if let Ok(canonical) = std::fs::canonicalize(&resolved) {
                    return normalize_path_separators(canonical);
                }
                return normalize_path_separators(resolved);
            }
        }
    }

    // Try workspace root next (for paths like "workflow-ui" or "qontinui-schemas")
    if let Some(ref root) = workspace_root {
        let resolved = root.join(&path);
        if resolved.exists() {
            if let Ok(canonical) = std::fs::canonicalize(&resolved) {
                return normalize_path_separators(canonical);
            }
            return normalize_path_separators(resolved);
        }
    }

    // Fall back to CWD resolution
    if let Ok(cwd) = std::env::current_dir() {
        let resolved = cwd.join(&path);
        if resolved.exists() {
            if let Ok(canonical) = std::fs::canonicalize(&resolved) {
                return normalize_path_separators(canonical);
            }
            return normalize_path_separators(resolved);
        }
    }

    // Neither resolved to an existing path, but we MUST return an absolute path.
    // Prefer runner-dir-based path, then workspace-root-based, over a relative path,
    // since relative paths cause OS error 267 on Windows.
    if let Some(runner) = runner_dir {
        if runner.exists() {
            let absolute = runner.join(&path);
            tracing::warn!(
                "resolve_working_directory: '{}' not found at '{}', returning unverified runner-relative path",
                wd,
                absolute.display()
            );
            return normalize_path_separators(absolute);
        }
    }

    if let Some(root) = workspace_root {
        let absolute = root.join(&path);
        tracing::warn!(
            "resolve_working_directory: '{}' not found at '{}', returning unverified absolute path",
            wd,
            absolute.display()
        );
        return normalize_path_separators(absolute);
    }

    if let Ok(cwd) = std::env::current_dir() {
        let absolute = cwd.join(&path);
        tracing::warn!(
            "resolve_working_directory: '{}' not found, using CWD-relative: {}",
            wd,
            absolute.display()
        );
        return normalize_path_separators(absolute);
    }

    tracing::error!(
        "resolve_working_directory: could not resolve '{}' to an absolute path — \
         no workspace root or CWD available",
        wd
    );
    path
}

/// Normalize path separators to the platform-native separator.
/// On Windows, converts forward slashes to backslashes to avoid mixed-separator
/// paths that can cause issues with CreateProcessW and cmd.exe.
/// Also strips the UNC `\\?\` prefix that `std::fs::canonicalize` adds on Windows,
/// since cmd.exe and npx do not handle UNC paths correctly.
fn normalize_path_separators(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy().replace('/', "\\");
        // Strip \\?\ UNC prefix from canonicalized paths — cmd.exe chokes on them
        let s = s.strip_prefix("\\\\?\\").unwrap_or(&s);
        PathBuf::from(s)
    }
    #[cfg(not(windows))]
    {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_dev_logs_dir_is_valid() {
        let path = get_default_dev_logs_dir();
        assert!(
            path.ends_with("qontinui-runner/dev-logs")
                || path.ends_with("qontinui-runner\\dev-logs")
        );
    }

    #[test]
    fn test_subdirectories() {
        let base = get_dev_logs_dir();
        assert!(get_screenshots_dir().starts_with(&base));
        assert!(get_state_explorer_dir().starts_with(&base));
        assert!(get_api_images_dir().starts_with(&base));
    }
}
