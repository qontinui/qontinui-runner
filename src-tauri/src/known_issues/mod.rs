//! Known issues module for persistent, cross-run issue tracking.
//!
//! This module provides:
//! - Types for known issues and pattern templates
//! - Storage operations for SQLite persistence
//! - Built-in pattern templates for common issue categories

pub mod auto_detect;
pub mod storage;
pub mod templates;
pub mod types;

pub use types::*;
