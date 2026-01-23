//! Workspace Scanner Module
//!
//! Scans directories to detect projects and their types based on config files.

use std::collections::HashSet;
use std::path::Path;
use tracing::{debug, info, warn};

use super::types::{DetectedProject, ScanWorkspaceRequest, WorkspaceScanResult};

/// Config file patterns that indicate different project types
struct LanguageIndicator {
    files: &'static [&'static str],
    language: &'static str,
}

const LANGUAGE_INDICATORS: &[LanguageIndicator] = &[
    // Python
    LanguageIndicator {
        files: &[
            "pyproject.toml",
            "setup.py",
            "requirements.txt",
            "Pipfile",
            "poetry.lock",
        ],
        language: "python",
    },
    // JavaScript/TypeScript
    LanguageIndicator {
        files: &["tsconfig.json", "tsconfig.*.json"],
        language: "typescript",
    },
    LanguageIndicator {
        files: &["package.json"],
        language: "javascript",
    },
    // Rust
    LanguageIndicator {
        files: &["Cargo.toml"],
        language: "rust",
    },
    // Go
    LanguageIndicator {
        files: &["go.mod"],
        language: "go",
    },
    // Java
    LanguageIndicator {
        files: &["pom.xml", "build.gradle", "build.gradle.kts"],
        language: "java",
    },
    // C#/.NET
    LanguageIndicator {
        files: &["*.csproj", "*.sln"],
        language: "csharp",
    },
    // Ruby
    LanguageIndicator {
        files: &["Gemfile"],
        language: "ruby",
    },
    // PHP
    LanguageIndicator {
        files: &["composer.json"],
        language: "php",
    },
];

/// Config files we care about for check configuration
const CONFIG_FILE_PATTERNS: &[&str] = &[
    // Python
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    ".python-version",
    "ruff.toml",
    ".ruff.toml",
    "mypy.ini",
    ".mypy.ini",
    // JavaScript/TypeScript
    "package.json",
    "tsconfig.json",
    ".eslintrc",
    ".eslintrc.js",
    ".eslintrc.json",
    ".eslintrc.yml",
    "eslint.config.js",
    "eslint.config.mjs",
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.yml",
    "prettier.config.js",
    "biome.json",
    // Rust
    "Cargo.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "clippy.toml",
    ".clippy.toml",
    // Generic
    ".editorconfig",
    ".gitignore",
];

/// Scan a workspace for projects
pub fn scan_workspace(request: &ScanWorkspaceRequest) -> Result<WorkspaceScanResult, String> {
    let base_path = Path::new(&request.base_directory);

    if !base_path.exists() {
        return Err(format!(
            "Directory does not exist: {}",
            request.base_directory
        ));
    }

    if !base_path.is_dir() {
        return Err(format!(
            "Path is not a directory: {}",
            request.base_directory
        ));
    }

    info!(
        "Scanning workspace: {} (max_depth: {})",
        request.base_directory, request.max_depth
    );

    let mut projects = Vec::new();
    let mut directories_scanned = 0u32;

    // Check if base directory itself is a project
    if let Some(project) = detect_project(base_path, base_path) {
        projects.push(project);
    }
    directories_scanned += 1;

    // Scan subdirectories
    scan_directory(
        base_path,
        base_path,
        0,
        request.max_depth,
        &mut projects,
        &mut directories_scanned,
    );

    info!(
        "Scan complete: found {} projects in {} directories",
        projects.len(),
        directories_scanned
    );

    Ok(WorkspaceScanResult {
        projects,
        total_directories_scanned: directories_scanned,
        base_directory: request.base_directory.clone(),
    })
}

/// Recursively scan a directory for projects
fn scan_directory(
    dir: &Path,
    base_path: &Path,
    current_depth: u32,
    max_depth: u32,
    projects: &mut Vec<DetectedProject>,
    directories_scanned: &mut u32,
) {
    if current_depth >= max_depth {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Failed to read directory {:?}: {}", dir, e);
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        // Skip common non-project directories
        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if should_skip_directory(dir_name) {
            debug!("Skipping directory: {:?}", path);
            continue;
        }

        *directories_scanned += 1;

        // Check if this directory is a project
        if let Some(project) = detect_project(&path, base_path) {
            projects.push(project);
        }

        // Continue scanning subdirectories
        scan_directory(
            &path,
            base_path,
            current_depth + 1,
            max_depth,
            projects,
            directories_scanned,
        );
    }
}

/// Check if a directory should be skipped
fn should_skip_directory(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | ".hg"
            | ".svn"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".env"
            | "env"
            | "target"
            | "build"
            | "dist"
            | ".next"
            | ".nuxt"
            | "out"
            | ".cache"
            | ".idea"
            | ".vscode"
            | ".vs"
            | "coverage"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".tox"
            | "vendor"
            | "bin"
            | "obj"
    )
}

/// Detect if a directory is a project and extract its info
fn detect_project(dir: &Path, base_path: &Path) -> Option<DetectedProject> {
    let entries = std::fs::read_dir(dir).ok()?;

    let mut found_files: HashSet<String> = HashSet::new();
    let mut config_files: Vec<String> = Vec::new();

    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();

        // Collect all files
        found_files.insert(file_name.clone());

        // Check if it's a config file we care about
        if is_config_file(&file_name) {
            config_files.push(file_name);
        }
    }

    // Detect languages based on indicator files
    let mut languages: Vec<String> = Vec::new();

    for indicator in LANGUAGE_INDICATORS {
        for pattern in indicator.files {
            if pattern.contains('*') {
                // Glob pattern - check if any file matches
                let prefix = pattern.split('*').next().unwrap_or("");
                let suffix = pattern.split('*').last().unwrap_or("");

                if found_files.iter().any(|f| {
                    (prefix.is_empty() || f.starts_with(prefix))
                        && (suffix.is_empty() || f.ends_with(suffix))
                }) {
                    if !languages.contains(&indicator.language.to_string()) {
                        languages.push(indicator.language.to_string());
                    }
                    break;
                }
            } else if found_files.contains(*pattern) {
                if !languages.contains(&indicator.language.to_string()) {
                    languages.push(indicator.language.to_string());
                }
                break;
            }
        }
    }

    // If no languages detected and no config files, not a project
    if languages.is_empty() && config_files.is_empty() {
        return None;
    }

    // If TypeScript detected but also has package.json, keep both but typescript first
    // If only package.json without tsconfig, it's just JavaScript
    if languages.contains(&"javascript".to_string())
        && languages.contains(&"typescript".to_string())
    {
        // TypeScript project with package.json - prioritize TypeScript
        languages.retain(|l| l != "javascript");
        languages.insert(0, "typescript".to_string());
    }

    // Calculate relative path
    let relative_path = dir
        .strip_prefix(base_path)
        .ok()
        .and_then(|p| p.to_str())
        .map(|s| {
            if s.is_empty() {
                ".".to_string()
            } else {
                s.to_string()
            }
        })
        .unwrap_or_else(|| ".".to_string());

    // Get project name from directory
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    Some(DetectedProject {
        path: dir.to_string_lossy().to_string(),
        relative_path,
        languages,
        config_files,
        name,
    })
}

/// Check if a filename is a config file we care about
fn is_config_file(name: &str) -> bool {
    CONFIG_FILE_PATTERNS.iter().any(|pattern| {
        if pattern.contains('*') {
            let prefix = pattern.split('*').next().unwrap_or("");
            let suffix = pattern.split('*').last().unwrap_or("");
            (prefix.is_empty() || name.starts_with(prefix))
                && (suffix.is_empty() || name.ends_with(suffix))
        } else {
            name == *pattern
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_directory() {
        assert!(should_skip_directory("node_modules"));
        assert!(should_skip_directory(".git"));
        assert!(should_skip_directory("__pycache__"));
        assert!(!should_skip_directory("src"));
        assert!(!should_skip_directory("lib"));
    }

    #[test]
    fn test_is_config_file() {
        assert!(is_config_file("package.json"));
        assert!(is_config_file("pyproject.toml"));
        assert!(is_config_file("Cargo.toml"));
        assert!(is_config_file("tsconfig.json"));
        assert!(!is_config_file("main.py"));
        assert!(!is_config_file("index.js"));
    }
}
