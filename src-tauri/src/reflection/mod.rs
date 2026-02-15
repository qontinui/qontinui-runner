//! Reflection Workflow System
//!
//! Automatically analyzes completed workflow runs to identify systemic issues,
//! apply fixes, and track fix effectiveness across subsequent runs.
//!
//! ## Module structure
//!
//! - `types` - ReflectionFix, FixType, FixStatus, FixEffectiveness
//! - `storage` - Database CRUD operations for reflection_fixes table
//! - `effectiveness` - Timestamp-based effectiveness evaluation engine
//! - `trigger` - Post-workflow trigger with recursion prevention
//! - `workflow` - Programmatic reflection workflow definition

#![allow(dead_code)]

pub mod effectiveness;
pub mod parser;
pub mod storage;
pub mod trigger;
pub mod types;
pub mod workflow;
