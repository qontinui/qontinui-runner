//! Project Constitution Loader
//!
//! Supports `.qontinui/constitution.md` files that define project-wide principles
//! and constraints. The constitution is injected into all generation phase prompts
//! to ensure generated workflows respect project standards.

use std::path::Path;
use tracing::{debug, info};

/// Load the project constitution from `.qontinui/constitution.md`.
/// Returns None if the file doesn't exist or can't be read.
pub fn load_constitution(project_path: &str) -> Option<String> {
    let paths_to_try = [
        Path::new(project_path).join(".qontinui/constitution.md"),
        Path::new(project_path).join(".qontinui/constitution.txt"),
    ];

    for path in &paths_to_try {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) if !content.trim().is_empty() => {
                    info!("Loaded project constitution from {}", path.display());
                    return Some(content);
                }
                Ok(_) => {
                    debug!("Constitution file at {} is empty, skipping", path.display());
                }
                Err(e) => {
                    debug!("Failed to read constitution at {}: {}", path.display(), e);
                }
            }
        }
    }
    None
}

/// Format the constitution for injection into AI prompts.
pub fn format_constitution_for_prompt(constitution: &str) -> String {
    format!(
        "## Project Constitution (MUST be respected)\n\n\
         The following project-wide principles and constraints MUST be followed.\n\
         Any generated workflow steps that violate these principles are invalid.\n\n\
         {}\n\n\
         ---\n",
        constitution.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_constitution() {
        let constitution = "## Principles\n- Use TypeScript strict mode\n- No any types";
        let formatted = format_constitution_for_prompt(constitution);
        assert!(formatted.contains("Project Constitution"));
        assert!(formatted.contains("TypeScript strict mode"));
        assert!(formatted.contains("MUST be respected"));
    }

    #[test]
    fn test_load_nonexistent() {
        let result = load_constitution("/nonexistent/path");
        assert!(result.is_none());
    }
}
