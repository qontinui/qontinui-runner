//! Log Watch Types and Helper Functions
//!
//! Extracted from `executor.rs` — types and utilities for the `log_watch` step type.
//! These detect errors in dev-log files within a configurable time window and
//! format them for AI consumption.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ============================================================================
// Log Watch Types
// ============================================================================

/// An error detected in a log file during log_watch step execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogError {
    /// Source log file name (e.g., "backend.log")
    pub source: String,
    /// Line number in the log file (1-indexed)
    pub line_number: usize,
    /// Timestamp extracted from the log line (if available)
    pub timestamp: Option<String>,
    /// The error message/line
    pub message: String,
    /// Context lines before the error (typically 2-3 lines)
    pub context_before: Vec<String>,
    /// Context lines after the error (typically 2-3 lines)
    pub context_after: Vec<String>,
    /// Type of error: "error", "exception", "traceback", "warning", "fatal", "panic"
    pub error_type: String,
}

/// Default error patterns used for log_watch if none specified
pub(crate) const DEFAULT_ERROR_PATTERNS: &[&str] = &[
    "ERROR",
    "Error:",
    "error:",
    "Exception",
    "exception",
    "Traceback",
    "traceback",
    "TypeError",
    "SyntaxError",
    "ReferenceError",
    "ValueError",
    "KeyError",
    "AttributeError",
    "ImportError",
    "RuntimeError",
    "FATAL",
    "fatal",
    "panic",
    "PANIC",
    "FAILED",
    "Failed:",
];

/// Get default log source filenames from global settings.
/// Falls back to ["backend.log", "frontend.log"] if no sources are configured.
pub(crate) fn get_default_log_source_names() -> Vec<String> {
    let settings = crate::settings::get_global_log_source_settings();
    let names: Vec<String> = settings
        .sources
        .iter()
        .filter(|s| s.enabled)
        .map(|s| {
            // If path is absolute, extract the filename; otherwise use as-is
            let path = std::path::Path::new(&s.path);
            if path.is_absolute() {
                path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| s.path.clone())
            } else {
                s.path.clone()
            }
        })
        .collect();
    if names.is_empty() {
        vec!["backend.log".to_string(), "frontend.log".to_string()]
    } else {
        names
    }
}

/// Default time window in seconds
pub(crate) const DEFAULT_TIME_WINDOW_SECONDS: u64 = 60;

/// Number of context lines before/after an error
pub(crate) const CONTEXT_LINES: usize = 3;

// ============================================================================
// Log Watch Helper Functions (outside impl block for reusability)
// ============================================================================

/// Collect recent errors from log files
///
/// Reads the tail of each log file, parses timestamps, and extracts
/// error lines within the specified time window.
pub(crate) async fn collect_recent_log_errors(
    log_sources: &[String],
    time_window_seconds: u64,
    custom_patterns: Option<&[String]>,
) -> Vec<LogError> {
    use chrono::Utc;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let dev_logs_dir = crate::paths::get_dev_logs_dir();
    let cutoff_time = Utc::now() - chrono::Duration::seconds(time_window_seconds as i64);
    let mut all_errors = Vec::new();

    // Build pattern list: defaults + custom
    let mut patterns: Vec<&str> = DEFAULT_ERROR_PATTERNS.to_vec();
    if let Some(custom) = custom_patterns {
        for p in custom {
            patterns.push(p.as_str());
        }
    }

    for source_name in log_sources {
        // If the source name is an absolute path, use it directly;
        // otherwise join with dev_logs_dir (backward compat for workflow configs)
        let source_path = std::path::Path::new(source_name);
        let log_path = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            dev_logs_dir.join(source_name)
        };

        if !log_path.exists() {
            // Log file doesn't exist - this is OK, just skip it
            info!("Log file not found, skipping: {:?}", log_path);
            continue;
        }

        // Read the file
        let file = match File::open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to open log file {:?}: {}", log_path, e);
                continue;
            }
        };

        let reader = BufReader::new(file);
        let lines: Vec<String> = reader.lines().map_while(|l| l.ok()).collect();
        let total_lines = lines.len();

        // Process lines, looking for errors
        for (line_idx, line) in lines.iter().enumerate() {
            // Check if this line matches any error pattern
            let error_type = find_error_type(line, &patterns);
            if error_type.is_none() {
                continue;
            }
            let error_type = error_type.unwrap();

            // Try to parse timestamp from the line
            let timestamp = extract_timestamp(line);

            // If we have a timestamp, check if it's within the time window
            // If no timestamp can be parsed, skip the error to avoid including stale entries
            match &timestamp {
                Some(ts) => {
                    if let Some(parsed) = parse_log_timestamp(ts) {
                        if parsed < cutoff_time {
                            // This error is older than our time window, skip it
                            continue;
                        }
                    } else {
                        // Timestamp found but couldn't be parsed - skip to avoid stale errors
                        continue;
                    }
                }
                None => {
                    // No timestamp in the line - skip to avoid including errors of unknown age
                    // This prevents old errors from files that weren't cleared from being included
                    continue;
                }
            }

            // Collect context lines
            let context_before: Vec<String> =
                lines[line_idx.saturating_sub(CONTEXT_LINES)..line_idx].to_vec();

            let context_after: Vec<String> = lines
                [(line_idx + 1).min(total_lines)..(line_idx + 1 + CONTEXT_LINES).min(total_lines)]
                .to_vec();

            all_errors.push(LogError {
                source: source_name.clone(),
                line_number: line_idx + 1, // 1-indexed
                timestamp,
                message: line.clone(),
                context_before,
                context_after,
                error_type,
            });
        }
    }

    // Limit to avoid overwhelming output (keep most recent 50 errors)
    if all_errors.len() > 50 {
        all_errors = all_errors.into_iter().rev().take(50).rev().collect();
    }

    all_errors
}

/// Find what type of error a line represents, if any
pub(crate) fn find_error_type(line: &str, patterns: &[&str]) -> Option<String> {
    let line_lower = line.to_lowercase();

    // Check each pattern
    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();
        if line_lower.contains(&pattern_lower) {
            // Categorize the error type
            if pattern_lower.contains("traceback") {
                return Some("traceback".to_string());
            } else if pattern_lower.contains("exception") {
                return Some("exception".to_string());
            } else if pattern_lower.contains("panic") {
                return Some("panic".to_string());
            } else if pattern_lower.contains("fatal") {
                return Some("fatal".to_string());
            } else if pattern_lower.contains("error") {
                return Some("error".to_string());
            } else if pattern_lower.contains("failed") {
                return Some("failed".to_string());
            } else {
                return Some("error".to_string());
            }
        }
    }

    None
}

/// Extract timestamp from a log line (handles multiple formats)
pub(crate) fn extract_timestamp(line: &str) -> Option<String> {
    // Common timestamp patterns
    // Pattern 1: 2026-01-26 10:30:45 or 2026-01-26T10:30:45
    // Pattern 2: [2026-01-26T10:30:45Z] or [2026-01-26 10:30:45]
    // Pattern 3: ISO 8601 with milliseconds

    // Try to match ISO 8601 format
    static ISO_TIMESTAMP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)")
            .unwrap()
    });
    static BRACKETED_TIMESTAMP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\[(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)\]")
            .unwrap()
    });

    // Try bracketed format first
    if let Some(caps) = BRACKETED_TIMESTAMP.captures(line) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }

    // Try ISO format
    if let Some(caps) = ISO_TIMESTAMP.captures(line) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }

    None
}

/// Parse a timestamp string into a DateTime
pub(crate) fn parse_log_timestamp(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

    // Try RFC3339 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try common formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ];

    for fmt in formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(ts, fmt) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }

    None
}

/// Format log errors into a markdown report for AI consumption
pub(crate) fn format_log_errors_for_ai(errors: &[LogError]) -> String {
    let mut report = String::new();

    report.push_str("## Log Errors Detected\n\n");
    report.push_str(&format!("**Total errors found:** {}\n\n", errors.len()));

    // Group errors by source
    let mut by_source: std::collections::HashMap<String, Vec<&LogError>> =
        std::collections::HashMap::new();
    for error in errors {
        by_source
            .entry(error.source.clone())
            .or_default()
            .push(error);
    }

    for (source, source_errors) in by_source {
        report.push_str(&format!(
            "### {} ({} errors)\n\n",
            source,
            source_errors.len()
        ));

        for error in source_errors {
            report.push_str(&format!(
                "#### Line {} ({})\n",
                error.line_number, error.error_type
            ));

            if let Some(ref ts) = error.timestamp {
                report.push_str(&format!("**Timestamp:** {}\n", ts));
            }

            report.push_str("\n**Context:**\n```\n");

            // Context before
            for line in &error.context_before {
                report.push_str(&format!("  {}\n", line));
            }

            // Error line (highlighted)
            report.push_str(&format!("> {}\n", error.message));

            // Context after
            for line in &error.context_after {
                report.push_str(&format!("  {}\n", line));
            }

            report.push_str("```\n\n");
        }
    }

    report
}
