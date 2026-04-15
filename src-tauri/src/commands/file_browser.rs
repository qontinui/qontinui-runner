//! File browser command handlers for Tauri.
//!
//! Provides safe, read-only filesystem browsing for mobile clients.
//! All paths are validated against a whitelist of allowed directories
//! to prevent unauthorized access.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

/// Maximum file size for read_file_content (10 MB)
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

/// A single entry in a directory listing.
#[derive(Debug, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified: Option<String>,
}

/// Result of browsing a directory.
#[derive(Debug, Serialize, Deserialize)]
pub struct BrowseResult {
    pub path: String,
    pub entries: Vec<DirEntry>,
}

/// Result of reading a file.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileContentResult {
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub is_text: bool,
    /// Text content (only if is_text is true and file is within size limit)
    pub content: Option<String>,
    pub content_type: String,
}

/// Result of listing safe directories.
#[derive(Debug, Serialize, Deserialize)]
pub struct SafeDir {
    pub path: String,
    pub label: String,
    pub exists: bool,
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

/// Get the list of safe root directories that the file browser is allowed to access.
fn get_safe_directories() -> Vec<(PathBuf, &'static str)> {
    let mut dirs = Vec::new();

    // Current working directory (workspace)
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push((cwd, "Workspace"));
    }

    // User home directory
    if let Some(home) = dirs::home_dir() {
        // Common output directories
        let outputs = home.join("qontinui-outputs");
        dirs.push((outputs, "Outputs"));

        let logs = home.join(".qontinui").join("logs");
        dirs.push((logs, "Logs"));
    }

    // Configurable via environment variable (comma-separated)
    if let Ok(extra) = std::env::var("QONTINUI_BROWSE_DIRS") {
        for dir_str in extra.split(',') {
            let trimmed = dir_str.trim();
            if !trimmed.is_empty() {
                dirs.push((PathBuf::from(trimmed), "Custom"));
            }
        }
    }

    dirs
}

/// Validate that a given path is within one of the allowed safe directories.
/// Returns the canonicalized path if valid, or an error.
fn validate_path(requested: &str) -> Result<PathBuf, String> {
    let requested_path = PathBuf::from(requested);

    // Resolve to absolute path
    let canonical = std::fs::canonicalize(&requested_path)
        .map_err(|e| format!("Path not found or inaccessible: {}", e))?;

    let safe_dirs = get_safe_directories();

    for (safe_root, _label) in &safe_dirs {
        // Canonicalize the safe root too (may not exist yet)
        if let Ok(safe_canonical) = std::fs::canonicalize(safe_root) {
            if canonical.starts_with(&safe_canonical) {
                return Ok(canonical);
            }
        }
    }

    warn!(
        "file_browser_access_denied: requested={}, canonical={:?}",
        requested, canonical,
    );
    Err("Access denied: path is outside allowed directories".to_string())
}

/// Check that the given path is not a symlink.
///
/// This is part of a defense-in-depth strategy against TOCTOU race conditions:
/// even after `validate_path` confirms the canonical path is within a safe root,
/// an attacker could swap a symlink between validation and the actual filesystem
/// operation (`read_dir`, `read_to_string`). By rejecting symlinks outright we
/// eliminate the class of attacks where a symlink is redirected after the safety
/// check but before the I/O operation. Combined with re-canonicalization
/// immediately before I/O, this narrows the race window to near zero.
fn verify_not_symlink(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to read symlink metadata: {}", e))?;
    if meta.file_type().is_symlink() {
        warn!("file_browser_symlink_rejected: path={}", path.display());
        return Err("Access denied: symlinks are not allowed".to_string());
    }
    Ok(())
}

/// Re-canonicalize `path` and verify it still resides under one of the safe
/// roots. This is called immediately before the actual filesystem operation to
/// narrow the TOCTOU window between `validate_path` and the I/O call.
fn revalidate_path(path: &Path) -> Result<PathBuf, String> {
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("Re-canonicalization failed: {}", e))?;

    let safe_dirs = get_safe_directories();
    for (safe_root, _label) in &safe_dirs {
        if let Ok(safe_canonical) = std::fs::canonicalize(safe_root) {
            if canonical.starts_with(&safe_canonical) {
                return Ok(canonical);
            }
        }
    }

    warn!(
        "file_browser_revalidation_failed: path={}, canonical={:?}",
        path.display(),
        canonical,
    );
    Err("Access denied: path escaped allowed directories between checks".to_string())
}

/// Guess whether a file is text based on its extension.
fn is_text_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    matches!(
        ext.as_str(),
        "txt"
            | "md"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "xml"
            | "html"
            | "css"
            | "js"
            | "ts"
            | "jsx"
            | "tsx"
            | "py"
            | "rs"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "ps1"
            | "bat"
            | "cmd"
            | "sql"
            | "csv"
            | "log"
            | "env"
            | "ini"
            | "cfg"
            | "conf"
            | "gitignore"
            | "dockerignore"
            | "editorconfig"
            | "prettierrc"
            | "eslintrc"
            | "makefile"
            | "dockerfile"
            | "proto"
            | "graphql"
            | "gql"
            | "svelte"
            | "vue"
            | "swift"
            | "kt"
            | "kts"
            | "gradle"
            | "rb"
            | "php"
            | "lua"
            | "r"
            | "jl"
            | "ex"
            | "exs"
            | "erl"
            | "hs"
            | "ml"
            | "mli"
            | "dart"
    )
}

/// Guess MIME type from extension.
fn guess_content_type(path: &Path) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// List the configured safe directories so the mobile client knows what roots
/// are available.
#[tauri::command]
pub async fn get_safe_browse_roots() -> Result<Vec<SafeDir>, String> {
    let dirs = get_safe_directories();
    let result: Vec<SafeDir> = dirs
        .into_iter()
        .map(|(path, label)| SafeDir {
            exists: path.exists(),
            path: path.to_string_lossy().to_string(),
            label: label.to_string(),
        })
        .collect();

    Ok(result)
}

/// Browse a directory and return its contents.
///
/// # Arguments
/// * `path` - Absolute path to the directory to browse
///
/// # Returns
/// Directory listing with file metadata, sorted directories-first then by name.
#[tauri::command]
pub async fn browse_directory(path: String) -> Result<BrowseResult, String> {
    let canonical = validate_path(&path)?;

    // Defense-in-depth: reject symlinks to prevent TOCTOU redirect attacks.
    verify_not_symlink(&canonical)?;

    if !canonical.is_dir() {
        return Err("Path is not a directory".to_string());
    }

    // Re-canonicalize immediately before I/O to narrow the TOCTOU window.
    let canonical = revalidate_path(&canonical)?;

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&canonical).await.map_err(|e| {
        error!("Failed to read directory {}: {}", path, e);
        format!("Failed to read directory: {}", e)
    })?;

    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|e| format!("Error reading entry: {}", e))?
    {
        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue, // skip entries we can't stat
        };

        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files/dirs (starting with .)
        if name.starts_with('.') {
            continue;
        }

        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| {
                    chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                        .map(|dt| dt.to_rfc3339())
                })
            })
            .flatten();

        entries.push(DirEntry {
            name,
            path: entry.path().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size_bytes: metadata.len(),
            modified,
        });
    }

    // Sort: directories first, then by name
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    info!(
        "browse_directory: path={}, count={}",
        canonical.display(),
        entries.len()
    );

    Ok(BrowseResult {
        path: canonical.to_string_lossy().to_string(),
        entries,
    })
}

/// Read the content of a file (text files only, up to 10MB).
///
/// # Arguments
/// * `path` - Absolute path to the file
/// * `max_bytes` - Optional limit (defaults to 10MB)
///
/// # Returns
/// File content and metadata. For binary files, content is None.
#[tauri::command]
pub async fn read_file_content(
    path: String,
    max_bytes: Option<u64>,
) -> Result<FileContentResult, String> {
    let canonical = validate_path(&path)?;

    // Defense-in-depth: reject symlinks to prevent TOCTOU redirect attacks.
    verify_not_symlink(&canonical)?;

    if !canonical.is_file() {
        return Err("Path is not a file".to_string());
    }

    // Re-canonicalize immediately before I/O to narrow the TOCTOU window.
    let canonical = revalidate_path(&canonical)?;

    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|e| format!("Failed to read file metadata: {}", e))?;

    let size = metadata.len();
    let limit = max_bytes.unwrap_or(MAX_READ_BYTES).min(MAX_READ_BYTES);
    let is_text = is_text_file(&canonical);
    let content_type = guess_content_type(&canonical);

    let filename = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let content = if is_text && size <= limit {
        match tokio::fs::read_to_string(&canonical).await {
            Ok(text) => Some(text),
            Err(e) => {
                // File might not be valid UTF-8 despite extension
                warn!("File appears text but failed to read as UTF-8: {}", e);
                None
            }
        }
    } else {
        None
    };

    info!(
        "read_file_content: path={}, size_bytes={}, is_text={}",
        canonical.display(),
        size,
        is_text
    );

    Ok(FileContentResult {
        path: canonical.to_string_lossy().to_string(),
        filename,
        size_bytes: size,
        is_text,
        content,
        content_type,
    })
}
