//! Fixer Workflow System
//!
//! Runs after all reflections and follow-ups complete for a source workflow.
//! Aggregates their findings and implements whatever remains unfixed.
//!
//! ## Module structure
//!
//! - `trigger` - Post-workflow trigger with wait-for-children logic
//! - `workflow` - Programmatic fixer workflow definition

#![allow(dead_code)]

pub mod trigger;
pub mod workflow;
