//! Findings module for AI-detected issues.
//!
//! Findings are parsed from `[FINDING:...]` markers in AI output by
//! [`parser`] and persisted to PostgreSQL via `database::pg::findings`.

pub mod parser;
pub mod types;

pub use parser::*;
pub use types::*;
