//! Prompt pattern analysis types.
//!
//! The SQLite-backed analyzers were removed and have no PG replacement yet;
//! this module retains only the `PromptInsight` type which the MCP
//! generator_eval endpoint still references (it currently returns an empty
//! list until the PG port lands).

use serde::{Deserialize, Serialize};

/// An actionable insight derived from analyzing generation data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInsight {
    /// Which agent this insight applies to
    pub agent: String,
    /// Type of insight
    pub insight_type: String,
    /// Human-readable description
    pub description: String,
    /// How many examples support this insight
    pub evidence_count: u32,
    /// Confidence level (0.0 - 1.0)
    pub confidence: f64,
    /// Suggested rule content if this insight should become a rule
    pub suggested_rule: Option<String>,
}
