//! Findings module for AI-detected issues.
//!
//! This module provides:
//! - Types for findings (matching qontinui-schemas)
//! - Parser for [FINDING:...] markers in AI output
//! - Storage operations for SQLite persistence

pub mod parser;
pub mod storage;
pub mod types;

pub use parser::*;
pub use types::*;
