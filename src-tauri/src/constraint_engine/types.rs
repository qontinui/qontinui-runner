//! Types for the constraint engine.

use serde::{Deserialize, Serialize};

/// Severity of a constraint violation.
///
/// - `Block`: Reject the fix, inject violation context, re-run agentic phase
///   without consuming an iteration. After `max_retries`, consume the iteration.
/// - `Warn`: Apply the fix, but inject violation context for the next iteration.
/// - `Log`: Record only, don't affect execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSeverity {
    Block,
    Warn,
    Log,
}

/// What to check and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstraintCheck {
    /// Grep modified files for a regex pattern.
    /// Violation if the pattern IS found (use for secrets, debug statements, etc.)
    GrepForbidden {
        /// Regex pattern to search for.
        pattern: String,
        /// Optional glob to limit which modified files are checked.
        /// Default: all modified files.
        #[serde(default)]
        file_glob: Option<String>,
    },

    /// Grep modified files for a regex pattern.
    /// Violation if the pattern is NOT found (use for required headers, licenses, etc.)
    GrepRequired {
        /// Regex pattern that must be present.
        pattern: String,
        /// Optional glob to limit which modified files are checked.
        #[serde(default)]
        file_glob: Option<String>,
    },

    /// Check that all modified files are within allowed paths.
    /// Violation if any modified file is outside the allowed directories.
    FileScope {
        /// Allowed directory prefixes (relative to project root).
        /// e.g., ["src/", "tests/", "config/"]
        allowed_paths: Vec<String>,
    },

    /// Run a shell command. Violation if exit code is non-zero.
    /// Useful for quick compilation checks, linting, etc.
    Command {
        /// The command to run.
        cmd: String,
        /// Working directory (relative to project root). Default: project root.
        #[serde(default)]
        cwd: Option<String>,
        /// Timeout in seconds. Default: 30.
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
}

fn default_timeout() -> u64 {
    30
}

/// A constraint definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    /// Unique identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Why this constraint exists (shown to the AI on violation).
    pub description: String,
    /// What to check.
    pub check: ConstraintCheck,
    /// How severe a violation is.
    pub severity: ConstraintSeverity,
    /// Whether this constraint is enabled. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Result of evaluating a single constraint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintResult {
    /// The constraint that was evaluated.
    pub constraint_id: String,
    pub constraint_name: String,
    /// Whether the constraint passed.
    pub passed: bool,
    /// Severity of the constraint (for quick filtering).
    pub severity: ConstraintSeverity,
    /// Details about the violation (empty if passed).
    pub violations: Vec<ConstraintViolation>,
}

/// A specific violation found during constraint evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstraintViolation {
    /// File where the violation was found (if applicable).
    pub file: Option<String>,
    /// Line number (if applicable).
    pub line: Option<u32>,
    /// What was found / what went wrong.
    pub detail: String,
}
