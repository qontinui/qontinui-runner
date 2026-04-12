//! File Browser MCP API
//!
//! Endpoints for browsing the runner's filesystem from mobile.
//! Provides safe directory roots, directory listing, and text file reading.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::mcp::types::ApiState;

// Max file size to read (1 MB)
const MAX_READ_BYTES: u64 = 1_048_576;

// =============================================================================
// Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct SafeDir {
    pub path: String,
    pub label: String,
    pub exists: bool,
}

#[derive(Debug, Serialize)]
pub struct BrowseDirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrowseResult {
    pub path: String,
    pub entries: Vec<BrowseDirEntry>,
}

#[derive(Debug, Serialize)]
pub struct FileContentResult {
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub is_text: bool,
    pub content: Option<String>,
    pub content_type: String,
}

#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    pub path: String,
}

// =============================================================================
// Helpers
// =============================================================================

fn get_safe_roots() -> Vec<SafeDir> {
    let mut roots = Vec::new();

    // Home directory
    if let Some(home) = dirs::home_dir() {
        let p = home.to_string_lossy().to_string();
        roots.push(SafeDir {
            exists: home.exists(),
            path: p,
            label: "Home".to_string(),
        });
    }

    // Desktop
    if let Some(desktop) = dirs::desktop_dir() {
        let p = desktop.to_string_lossy().to_string();
        roots.push(SafeDir {
            exists: desktop.exists(),
            path: p,
            label: "Desktop".to_string(),
        });
    }

    // Documents
    if let Some(docs) = dirs::document_dir() {
        let p = docs.to_string_lossy().to_string();
        roots.push(SafeDir {
            exists: docs.exists(),
            path: p,
            label: "Documents".to_string(),
        });
    }

    // Current working directory
    if let Ok(cwd) = std::env::current_dir() {
        let p = cwd.to_string_lossy().to_string();
        roots.push(SafeDir {
            exists: true,
            path: p,
            label: "Working Directory".to_string(),
        });
    }

    roots
}

fn is_path_safe(path: &Path) -> bool {
    let roots = get_safe_roots();
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    for root in &roots {
        if let Ok(root_canonical) = PathBuf::from(&root.path).canonicalize() {
            if canonical.starts_with(&root_canonical) {
                return true;
            }
        }
    }
    false
}

fn guess_content_type(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "text/x-rust",
        Some("ts" | "tsx") => "text/typescript",
        Some("js" | "jsx") => "text/javascript",
        Some("py") => "text/x-python",
        Some("json") => "application/json",
        Some("toml") => "text/x-toml",
        Some("yaml" | "yml") => "text/yaml",
        Some("md") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("xml") => "text/xml",
        Some("sql") => "text/x-sql",
        Some("sh" | "bash" | "zsh") => "text/x-shellscript",
        Some("txt" | "log" | "env" | "gitignore" | "editorconfig") => "text/plain",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn is_text_content_type(ct: &str) -> bool {
    ct.starts_with("text/") || ct == "application/json"
}

// =============================================================================
// Handlers
// =============================================================================

async fn list_roots(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let roots = get_safe_roots();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": roots
    })))
}

async fn browse_directory(
    State(_state): State<Arc<ApiState>>,
    Query(req): Query<BrowseQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = Path::new(&req.path);

    if !path.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Not a directory: {}", req.path),
        ));
    }

    if !is_path_safe(path) {
        return Err((
            StatusCode::FORBIDDEN,
            "Path is outside allowed directories".to_string(),
        ));
    }

    let mut entries = Vec::new();

    match std::fs::read_dir(path) {
        Ok(dir_entries) => {
            for entry in dir_entries.flatten() {
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip hidden files
                if name.starts_with('.') {
                    continue;
                }
                let full_path = entry.path().to_string_lossy().to_string();
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .map(|d| {
                                chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                                    .map(|dt| dt.to_rfc3339())
                            })
                    })
                    .flatten();

                entries.push(BrowseDirEntry {
                    name,
                    path: full_path,
                    is_dir: metadata.is_dir(),
                    size_bytes: if metadata.is_file() { metadata.len() } else { 0 },
                    modified,
                });
            }
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read directory: {}", e),
            ));
        }
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "path": req.path,
            "entries": entries
        }
    })))
}

async fn read_file(
    State(_state): State<Arc<ApiState>>,
    Query(req): Query<ReadQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = Path::new(&req.path);

    if !path.is_file() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Not a file: {}", req.path),
        ));
    }

    if !is_path_safe(path) {
        return Err((
            StatusCode::FORBIDDEN,
            "Path is outside allowed directories".to_string(),
        ));
    }

    let metadata = std::fs::metadata(path)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Cannot read metadata: {}", e)))?;

    let content_type = guess_content_type(path);
    let is_text = is_text_content_type(&content_type);
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let content = if is_text && metadata.len() <= MAX_READ_BYTES {
        std::fs::read_to_string(path).ok()
    } else {
        None
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "path": req.path,
            "filename": filename,
            "size_bytes": metadata.len(),
            "is_text": is_text,
            "content": content,
            "content_type": content_type
        }
    })))
}

// =============================================================================
// Routes
// =============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/files/roots", get(list_roots))
        .route("/files/browse", get(browse_directory))
        .route("/files/read", get(read_file))
}
