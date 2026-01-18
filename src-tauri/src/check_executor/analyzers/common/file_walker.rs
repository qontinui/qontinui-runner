//! File walking utilities for analyzers

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Configuration for file walking
pub struct WalkConfig {
    pub extensions: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub max_depth: Option<usize>,
}

impl Default for WalkConfig {
    fn default() -> Self {
        Self {
            extensions: vec![],
            exclude_patterns: vec![
                "node_modules".to_string(),
                "__pycache__".to_string(),
                ".git".to_string(),
                ".venv".to_string(),
                "venv".to_string(),
                "target".to_string(),
                "dist".to_string(),
                "build".to_string(),
            ],
            max_depth: None,
        }
    }
}

/// Walk directory and collect matching files
pub fn walk_files(root: &Path, config: &WalkConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let mut walker = WalkDir::new(root).follow_links(false);

    if let Some(depth) = config.max_depth {
        walker = walker.max_depth(depth);
    }

    for entry in walker
        .into_iter()
        .filter_entry(|e| !should_skip(e.path(), &config.exclude_patterns))
        .flatten()
    {
        if entry.file_type().is_file() {
            if config.extensions.is_empty() {
                files.push(entry.path().to_path_buf());
            } else if let Some(ext) = entry.path().extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if config
                    .extensions
                    .iter()
                    .any(|e| e.to_lowercase() == ext_str)
                {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
    }

    files
}

fn should_skip(path: &Path, exclude_patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();
    for pattern in exclude_patterns {
        if path_str.contains(pattern.as_str()) {
            return true;
        }
    }
    false
}
