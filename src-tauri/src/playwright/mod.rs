//! Playwright Script Library
//!
//! This module provides persistent storage for Playwright test scripts that users can
//! save, organize, and run from the runner UI.
//!
//! Features:
//! - Store natural language descriptions alongside full .spec.ts code
//! - Execute tests via Node.js subprocess with JSON reporter
//! - Optional cloud sync with qontinui-web
//!
//! # Module Structure
//!
//! - `types` - Enums (SyncStatus, DisplayMode) and constants
//! - `results` - Result types for test execution output
//! - `script_storage` - PlaywrightScript, PlaywrightLibrary, file I/O, CRUD operations
//! - `parser` - JSON parsing, ANSI stripping, file collection utilities
//! - `executor` - Script execution via Node.js subprocess

mod executor;
mod parser;
mod results;
mod script_storage;
mod types;

// Re-export public types from types module
// These are public API exports - may not be used within this crate
#[allow(unused_imports)]
pub use types::{DisplayMode, SyncStatus};

// Re-export public types from results module
pub use results::{
    CriteriaResult, NetworkRequest, PlaywrightResult, StructuredTestOutput, TestSpec,
    WorkflowStatus,
};

// Re-export public types and functions from script_storage module
pub use script_storage::{
    create_script, delete_script, duplicate_script, export_scripts, get_all_scripts, get_all_tags,
    get_categories, get_results_dir, get_script, import_scripts, load_script_library,
    save_script_library, search_scripts, update_script, PlaywrightLibrary, PlaywrightScript,
};

// Re-export execution function from executor module
pub use executor::run_script;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_api_accessible() {
        // Verify all public types are accessible
        let _status = SyncStatus::default();
        let _mode = DisplayMode::default();

        // Verify script creation works
        let script = PlaywrightScript::with_details(
            "Test".to_string(),
            "Description".to_string(),
            None,
            "http://localhost".to_string(),
            "content".to_string(),
            "Category".to_string(),
            vec![],
            60,
            DisplayMode::Headless,
            "chromium".to_string(),
        );
        assert!(!script.id.is_empty());
    }
}
