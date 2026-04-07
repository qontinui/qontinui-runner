//! Known issues module for persistent, cross-run issue tracking.
//!
//! Issues and pattern templates are stored in PostgreSQL (see
//! `database/pg/known_issues.rs`). This module provides the shared
//! type definitions and in-memory ranking helpers used by both the
//! PG layer and the workflow generator.

pub mod storage;
pub mod types;

pub use types::*;
