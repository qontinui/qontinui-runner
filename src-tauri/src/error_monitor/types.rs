//! Error monitoring types for application log collection.
//!
//! This module provides types for:
//! - Configurable log sources (user-defined application logs)
//! - Error events captured from logs
//! - Error summaries and statistics

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// Log Source Configuration
// =============================================================================

/// Format of the log file
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum LogFormat {
    /// Line-based text logs
    #[default]
    Plaintext,
    /// Structured JSON (one object per line or array)
    Json,
    /// JSON Lines format (one JSON object per line)
    Jsonl,
}

impl LogFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "plaintext" => Some(Self::Plaintext),
            "json" => Some(Self::Json),
            "jsonl" => Some(Self::Jsonl),
            _ => None,
        }
    }
}


/// Parser type for extracting errors from logs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ParserType {
    /// Python tracebacks and exceptions
    Python,
    /// JavaScript/TypeScript errors and stack traces
    #[serde(alias = "java_script", rename = "javascript")]
    JavaScript,
    /// Rust panics and errors
    Rust,
    /// Generic regex-based pattern matching
    #[default]
    Generic,
}

impl ParserType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
            Self::Generic => "generic",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "python" => Some(Self::Python),
            "javascript" => Some(Self::JavaScript),
            "rust" => Some(Self::Rust),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }
}


/// Path type for log source location
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PathType {
    /// Single file path
    #[default]
    File,
    /// Glob pattern (e.g., "logs/*.log")
    Glob,
    /// Directory (watch all files in directory)
    Directory,
}

impl PathType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Glob => "glob",
            Self::Directory => "directory",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "glob" => Some(Self::Glob),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}


/// Configuration for a log source to monitor
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSourceConfig {
    /// Unique identifier
    pub id: Option<i64>,
    /// Display name for this log source
    pub name: String,
    /// Optional description
    pub description: Option<String>,
    /// Path to log file, glob pattern, or directory
    pub path: String,
    /// Type of path
    #[serde(default)]
    pub path_type: PathType,
    /// Format of the log file
    #[serde(default)]
    pub format: LogFormat,
    /// Parser to use for extracting errors
    #[serde(default)]
    pub parser: ParserType,
    /// Regex pattern to extract timestamp from log lines
    pub timestamp_pattern: Option<String>,
    /// Timezone for parsing timestamps
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Custom regex patterns to identify errors (JSON array)
    pub error_patterns: Option<Vec<String>>,
    /// Custom regex patterns to identify warnings (JSON array)
    pub warning_patterns: Option<Vec<String>>,
    /// Patterns to ignore (JSON array)
    pub ignore_patterns: Option<Vec<String>>,
    /// Whether this source is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Polling interval in milliseconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u32,
    /// Creation timestamp
    pub created_at: Option<String>,
    /// Last update timestamp
    pub updated_at: Option<String>,
}

fn default_timezone() -> String {
    "local".to_string()
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u32 {
    5000
}

// =============================================================================
// Error Event Types
// =============================================================================

/// Severity of an error event
///
/// This is the canonical definition of ErrorSeverity used throughout the application.
/// Re-exported from `crate::error` for convenience.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ErrorSeverity {
    /// Application crash, data loss risk
    Critical,
    /// Operation failed
    #[default]
    Error,
    /// Potential issue
    Warning,
    /// Informational message (not an error)
    Info,
}

impl ErrorSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "critical" => Some(Self::Critical),
            "error" => Some(Self::Error),
            "warning" => Some(Self::Warning),
            "info" => Some(Self::Info),
            _ => None,
        }
    }
}


/// Status of an error event
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ErrorStatus {
    /// Newly detected, not yet reviewed
    #[default]
    New,
    /// Acknowledged by user/system
    Acknowledged,
    /// Being worked on (promoted to finding)
    InProgress,
    /// Promoted to a task_run_finding
    Promoted,
    /// Manually marked as ignored
    Ignored,
    /// Fixed/resolved
    Resolved,
    /// Won't fix (documented reason)
    WontFix,
}

impl ErrorStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Acknowledged => "acknowledged",
            Self::InProgress => "in_progress",
            Self::Promoted => "promoted",
            Self::Ignored => "ignored",
            Self::Resolved => "resolved",
            Self::WontFix => "wont_fix",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "new" => Some(Self::New),
            "acknowledged" => Some(Self::Acknowledged),
            "in_progress" => Some(Self::InProgress),
            "promoted" => Some(Self::Promoted),
            "ignored" => Some(Self::Ignored),
            "resolved" => Some(Self::Resolved),
            "wont_fix" => Some(Self::WontFix),
            _ => None,
        }
    }

    /// Check if this is a terminal status
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Resolved | Self::WontFix | Self::Ignored)
    }

    /// Check if this status represents an unresolved error
    pub fn is_unresolved(&self) -> bool {
        matches!(
            self,
            Self::New | Self::Acknowledged | Self::InProgress | Self::Promoted
        )
    }
}


/// Location information for an error
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorLocation {
    /// Source file path where error occurred
    pub file_path: String,
    /// Line number
    pub line_number: Option<u32>,
    /// Column number
    pub column_number: Option<u32>,
    /// Function or method name
    pub function_name: Option<String>,
}

/// An error event captured from application logs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEvent {
    /// Log source name
    pub log_source_name: String,
    /// Severity level
    #[serde(default)]
    pub severity: ErrorSeverity,
    /// Type of error (e.g., "TypeError", "ConnectionError")
    pub error_type: Option<String>,
    /// Error code (HTTP status, exit code, etc.)
    pub error_code: Option<String>,
    /// Error message
    pub message: String,
    /// Full stack trace if available
    pub stack_trace: Option<String>,
    /// Location information
    pub location: Option<ErrorLocation>,
    /// Surrounding log lines for context
    pub context_lines: Option<String>,
    /// Original unparsed log entry
    pub raw_entry: String,
    /// Timestamp from the log entry
    pub log_timestamp: Option<String>,
    /// Trace ID for cross-service correlation
    pub trace_id: Option<String>,
}

impl ErrorEvent {
    /// Compute a signature hash for deduplication
    pub fn compute_signature_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.log_source_name.hash(&mut hasher);
        self.severity.as_str().hash(&mut hasher);
        self.error_type.hash(&mut hasher);
        self.message.hash(&mut hasher);
        if let Some(ref loc) = self.location {
            loc.file_path.hash(&mut hasher);
            loc.line_number.hash(&mut hasher);
        }
        format!("{:x}", hasher.finish())
    }
}

/// A stored error event with database metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredErrorEvent {
    /// Database ID
    pub id: i64,
    /// Log source ID
    pub log_source_id: Option<i64>,
    /// Log source name (denormalized)
    pub log_source_name: String,
    /// Task run ID (if captured during a workflow)
    pub task_run_id: Option<String>,
    /// Workflow name (from joined task_runs table)
    pub workflow_name: Option<String>,
    /// Workflow step ID (if applicable)
    pub workflow_step_id: Option<String>,
    /// Timestamp from the log entry
    pub log_timestamp: Option<String>,
    /// When this error was captured
    pub captured_at: String,
    /// Severity level
    pub severity: ErrorSeverity,
    /// Type of error
    pub error_type: Option<String>,
    /// Error code
    pub error_code: Option<String>,
    /// Error message
    pub message: String,
    /// Full stack trace
    pub stack_trace: Option<String>,
    /// Surrounding context lines
    pub context_lines: Option<String>,
    /// Original raw entry
    pub raw_entry: Option<String>,
    /// Location information
    pub location: Option<ErrorLocation>,
    /// Signature hash for deduplication
    pub signature_hash: String,
    /// Number of times this error has occurred
    pub occurrence_count: u32,
    /// First occurrence timestamp
    pub first_seen_at: String,
    /// Most recent occurrence timestamp
    pub last_seen_at: String,
    /// Current status
    pub status: ErrorStatus,
    /// Linked finding ID (if promoted)
    pub finding_id: Option<i64>,
    /// Task run that resolved this error
    pub resolved_by_task_run_id: Option<String>,
    /// Resolution notes
    pub resolution_notes: Option<String>,
    /// Trace ID for cross-service correlation
    pub trace_id: Option<String>,
    /// When acknowledged
    pub acknowledged_at: Option<String>,
    /// When resolved
    pub resolved_at: Option<String>,
}

/// Summary statistics for error events
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorSummary {
    /// Total number of error events
    pub total: u32,
    /// New (unreviewed) errors
    pub new_count: u32,
    /// Total unresolved errors
    pub unresolved_count: u32,
    /// Critical severity count
    pub critical_count: u32,
    /// Error severity count
    pub error_count: u32,
    /// Warning severity count
    pub warning_count: u32,
    /// Breakdown by log source
    pub by_source: HashMap<String, u32>,
    /// Breakdown by error type
    pub by_error_type: HashMap<String, u32>,
    /// Breakdown by status
    pub by_status: HashMap<String, u32>,
    /// Whether there are actionable errors (critical or error severity, unresolved)
    pub has_actionable_errors: bool,
}

/// Query filters for error events
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorQuery {
    /// Filter by task run ID
    pub task_run_id: Option<String>,
    /// Filter by log source name
    pub log_source_name: Option<String>,
    /// Filter by status
    pub status: Option<Vec<ErrorStatus>>,
    /// Filter by severity
    pub severity: Option<Vec<ErrorSeverity>>,
    /// Filter by error type
    pub error_type: Option<String>,
    /// Only errors captured after this timestamp
    pub captured_after: Option<String>,
    /// Only errors captured before this timestamp
    pub captured_before: Option<String>,
    /// Limit number of results
    pub limit: Option<u32>,
    /// Offset for pagination
    pub offset: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_severity_serialization() {
        let severity = ErrorSeverity::Critical;
        let json = serde_json::to_string(&severity).unwrap();
        assert_eq!(json, "\"critical\"");

        let parsed: ErrorSeverity = serde_json::from_str("\"error\"").unwrap();
        assert_eq!(parsed, ErrorSeverity::Error);
    }

    #[test]
    fn test_error_status_terminal() {
        assert!(ErrorStatus::Resolved.is_terminal());
        assert!(ErrorStatus::WontFix.is_terminal());
        assert!(ErrorStatus::Ignored.is_terminal());
        assert!(!ErrorStatus::New.is_terminal());
        assert!(!ErrorStatus::InProgress.is_terminal());
    }

    #[test]
    fn test_error_event_signature_hash() {
        let event = ErrorEvent {
            log_source_name: "backend".to_string(),
            severity: ErrorSeverity::Error,
            error_type: Some("TypeError".to_string()),
            error_code: None,
            message: "Cannot read property 'foo' of undefined".to_string(),
            stack_trace: None,
            location: Some(ErrorLocation {
                file_path: "src/index.js".to_string(),
                line_number: Some(42),
                column_number: None,
                function_name: None,
            }),
            context_lines: None,
            raw_entry: "error log line".to_string(),
            log_timestamp: None,
            trace_id: None,
        };

        let hash1 = event.compute_signature_hash();
        let hash2 = event.compute_signature_hash();
        assert_eq!(hash1, hash2);

        // Different message should produce different hash
        let mut event2 = event.clone();
        event2.message = "Different error message".to_string();
        let hash3 = event2.compute_signature_hash();
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_log_format_default() {
        let format: LogFormat = Default::default();
        assert_eq!(format, LogFormat::Plaintext);
    }

    #[test]
    fn test_parser_type_roundtrip() {
        for parser in [
            ParserType::Python,
            ParserType::JavaScript,
            ParserType::Rust,
            ParserType::Generic,
        ] {
            let s = parser.as_str();
            let parsed = ParserType::from_str(s).unwrap();
            assert_eq!(parser, parsed);
        }
    }
}
