//! DOM Capture module for capturing and storing browser DOM snapshots.
//!
//! This module provides functionality to:
//! - Capture full HTML from browser pages (via Playwright or browser extension)
//! - Store HTML content in files with JSONL metadata
//! - Retrieve captured DOM for AI analysis

pub mod logger;
pub mod types;

pub use logger::*;
pub use types::*;
