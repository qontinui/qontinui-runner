//! Findings module for AI-detected issues.
//!
//! Findings are parsed from `[FINDING:...]` markers in AI output by
//! [`parser`] and persisted to PostgreSQL via `database::pg::findings`.

pub mod parser;
pub mod types;

pub use parser::*;
pub use types::*;

// Re-export the extension traits at the module root so call sites that do
// `use crate::findings::*;` automatically pick up the `.as_str()` /
// `.from_str()` / `.is_terminal()` methods we layered on top of the
// schemas-crate enums. Without the `Ext` traits in scope, the methods are
// invisible.
pub use types::{
    FindingActionTypeExt, FindingCategoryExt, FindingExt, FindingSeverityExt, FindingStatusExt,
};
