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

pub mod architecture;
pub mod causal;
pub mod causal_parser;
pub mod context;
pub mod cross_run_learning;
pub mod fuzzy_matching;
pub mod graph_engine;
pub mod graph_engine_pg;
pub mod graph_types;
pub mod parser;
pub mod prediction;
pub mod storage;
pub mod trends;
pub mod trigger;
pub mod types;
pub mod unified_search;
pub mod workflow;
