//! Follow-Up Workflow System
//!
//! Automatically picks up unfixed issues from a completed workflow run
//! and generates + executes a new workflow to fix them.
//!
//! Unlike reflection (which analyzes process quality and records systemic fixes),
//! follow-up completes unfinished work items identified during the source run.
//!
//! ## Module structure
//!
//! - `trigger` - Post-workflow trigger with recursion prevention
//! - `workflow` - Programmatic follow-up workflow definition

#![allow(dead_code)]

pub mod trigger;
pub mod workflow;
