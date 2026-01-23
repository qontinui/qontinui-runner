//! Check Generation Module
//!
//! Provides workspace scanning and AI-assisted check configuration generation.
//!
//! # Usage
//!
//! ```rust,ignore
//! use check_generation::{scan_workspace, generate_checks, ScanWorkspaceRequest, GenerateChecksRequest};
//!
//! // Scan workspace for projects
//! let scan_request = ScanWorkspaceRequest {
//!     base_directory: "/path/to/workspace".to_string(),
//!     max_depth: 2,
//! };
//! let scan_result = scan_workspace(&scan_request)?;
//!
//! // Generate check suggestions
//! let generate_request = GenerateChecksRequest {
//!     workspace_scan: scan_result,
//!     user_preferences: None,
//! };
//! let suggestions = generate_checks(&generate_request);
//! ```

mod generator;
mod scanner;
pub mod types;

// Re-export public functions
pub use generator::generate_checks;
pub use scanner::scan_workspace;

// Re-export commonly used types
pub use types::{
    GenerateChecksRequest, GenerateChecksResponse, ScanWorkspaceRequest, WorkspaceScanResult,
};
