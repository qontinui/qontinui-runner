//! Structured output types for the agentic phase.
//!
//! Provides a canonical `AgenticPhaseOutput` that replaces fragile string-marker
//! parsing with structured data. The output parser (`output_parser.rs`) attempts
//! to extract a JSON summary block first, then falls back to the existing marker
//! parsers for backward compatibility.

use serde::{Deserialize, Serialize};

/// Status of the agentic phase execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgenticStatus {
    /// All issues addressed successfully
    Success,
    /// Some issues addressed, others remain
    PartialSuccess,
    /// Failed to address any issues
    Failed,
    /// Issues are unfixable (infrastructure, external dependencies, etc.)
    Unfixable,
}

/// A file change made during the agentic phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub action: String, // "modified", "created", "deleted"
}

/// A finding reported by the AI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingOutput {
    pub category: String,
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub needs_input: bool,
    #[serde(default)]
    pub resolved: bool,
}

/// A reflection fix reported by the AI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionFixOutput {
    pub error_id: String,
    pub description: String,
}

/// Canonical structured output from the agentic phase.
///
/// This type consolidates all the information previously extracted via fragile
/// string markers ([UNFIXABLE_ERRORS], [INJECT_STEP], [REFLECTION_FIX], [FINDING])
/// into a single structured type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticPhaseOutput {
    /// Overall status of the agentic phase
    pub status: AgenticStatus,
    /// Human-readable summary of what was done
    pub summary: String,
    /// AI's confidence that the fixes will pass verification (0.0-1.0)
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Whether the AI determined errors are unfixable
    #[serde(default)]
    pub unfixable: bool,
    /// Reason why errors are unfixable (if unfixable is true)
    #[serde(default)]
    pub unfixable_reason: Option<String>,
    /// Files modified during this agentic phase
    #[serde(default)]
    pub files_modified: Vec<FileChange>,
    /// Dynamically injected verification steps.
    ///
    /// Only populated from structured JSON output (when the AI emits a ````json`
    /// summary block). Marker-based `[INJECT_STEP]` parsing is handled by the
    /// streaming parser in `claude_session/runner.rs` during live execution;
    /// post-hoc parsing in `output_parser.rs` does not extract these.
    #[serde(default)]
    pub injected_steps: Vec<serde_json::Value>,
    /// Reflection fixes (when reflection_mode is enabled).
    ///
    /// Only populated from structured JSON output (when the AI emits a ````json`
    /// summary block). Marker-based `[REFLECTION_FIX]` parsing is handled by the
    /// streaming parser in `claude_session/runner.rs` during live execution;
    /// post-hoc parsing in `output_parser.rs` does not extract these.
    #[serde(default)]
    pub reflection_fixes: Vec<ReflectionFixOutput>,
    /// Findings reported by the AI
    #[serde(default)]
    pub findings: Vec<FindingOutput>,
    /// The raw AI output text (always preserved for debugging)
    #[serde(skip_serializing, default)]
    pub raw_output: String,
}

impl AgenticPhaseOutput {
    /// Create a minimal output from raw text when no structured data is available.
    pub fn from_raw(raw_output: String) -> Self {
        Self {
            status: AgenticStatus::PartialSuccess,
            summary: String::new(),
            confidence: None,
            unfixable: false,
            unfixable_reason: None,
            files_modified: Vec::new(),
            injected_steps: Vec::new(),
            reflection_fixes: Vec::new(),
            findings: Vec::new(),
            raw_output,
        }
    }
}
