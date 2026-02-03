//! Generic log parser using regex patterns.
//!
//! This parser uses configurable regex patterns to detect errors and warnings
//! in log files. It's useful for:
//! - Custom log formats
//! - Application-specific error patterns
//! - Log files without a standard format

use regex::Regex;
use std::sync::LazyLock;

use super::traits::{patterns, ErrorEventBuilder, LogParser};
use crate::error_monitor::types::{ErrorEvent, ErrorSeverity};

/// Default error patterns used by the generic parser
mod default_patterns {
    use super::*;

    /// Generic error pattern: matches ERROR, Error, error with various formats
    pub static ERROR_GENERIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:^|\s|\[)(error|fatal|critical|severe|exception)(?:\]|:|\s)(.*)$")
            .unwrap()
    });

    /// Generic warning pattern
    pub static WARNING_GENERIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:^|\s|\[)(warn(?:ing)?|caution)(?:\]|:|\s)(.*)$").unwrap()
    });

    /// HTTP error status pattern (4xx, 5xx)
    pub static HTTP_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:HTTP|status|code)[/\s:]*([45]\d{2})(?:\s|$)"#).unwrap()
    });

    /// Stack trace indicator (common patterns)
    pub static STACK_TRACE_START: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:stack\s*trace|traceback|call\s*stack|backtrace):?").unwrap()
    });

    /// Exception/error with type pattern
    pub static EXCEPTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|\s)([A-Z][a-zA-Z]*(?:Error|Exception|Failure|Fault))\s*:?\s*(.+)?$")
            .unwrap()
    });

    /// Failed/failure pattern
    pub static FAILURE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(fail(?:ed|ure)?|crash(?:ed)?|abort(?:ed)?)\b").unwrap()
    });

    /// Timeout pattern
    pub static TIMEOUT_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(timeout|timed?\s*out)\b").unwrap());

    /// Connection error pattern
    pub static CONNECTION_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(connection\s+(?:refused|reset|closed|failed|error)|ECONNREFUSED|ECONNRESET|ETIMEDOUT)\b")
            .unwrap()
    });

    /// Permission/access denied pattern
    pub static PERMISSION_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(permission\s+denied|access\s+denied|unauthorized|forbidden|EACCES|EPERM)\b")
            .unwrap()
    });

    /// File not found pattern
    pub static NOT_FOUND_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(not\s+found|no\s+such\s+file|does\s*n.t\s+exist|ENOENT|404)\b").unwrap()
    });

    /// Out of memory pattern
    pub static OOM_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(out\s+of\s+memory|memory\s+(?:exhausted|allocation\s+failed)|OOM|ENOMEM)\b")
            .unwrap()
    });
}

/// Configuration for custom patterns
#[derive(Debug, Clone)]
pub struct PatternConfig {
    /// Custom error patterns (regex strings)
    pub error_patterns: Vec<String>,
    /// Custom warning patterns (regex strings)
    pub warning_patterns: Vec<String>,
    /// Patterns to ignore (even if they match error patterns)
    pub ignore_patterns: Vec<String>,
}

impl Default for PatternConfig {
    fn default() -> Self {
        Self {
            error_patterns: Vec::new(),
            warning_patterns: Vec::new(),
            ignore_patterns: Vec::new(),
        }
    }
}

/// Generic log parser using regex patterns.
pub struct GenericParser {
    /// Custom error patterns (compiled regexes)
    error_patterns: Vec<Regex>,
    /// Custom warning patterns (compiled regexes)
    warning_patterns: Vec<Regex>,
    /// Patterns to ignore
    ignore_patterns: Vec<Regex>,
    /// Use default patterns in addition to custom ones
    use_defaults: bool,
}

impl GenericParser {
    /// Create a new generic parser with default patterns.
    pub fn new() -> Self {
        Self {
            error_patterns: Vec::new(),
            warning_patterns: Vec::new(),
            ignore_patterns: Vec::new(),
            use_defaults: true,
        }
    }

    /// Create a parser with custom configuration.
    pub fn with_config(config: PatternConfig) -> Self {
        let error_patterns: Vec<Regex> = config
            .error_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        let warning_patterns: Vec<Regex> = config
            .warning_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        let ignore_patterns: Vec<Regex> = config
            .ignore_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        Self {
            error_patterns,
            warning_patterns,
            ignore_patterns,
            use_defaults: true,
        }
    }

    /// Add a custom error pattern.
    pub fn add_error_pattern(mut self, pattern: &str) -> Self {
        if let Ok(re) = Regex::new(pattern) {
            self.error_patterns.push(re);
        }
        self
    }

    /// Add a custom warning pattern.
    pub fn add_warning_pattern(mut self, pattern: &str) -> Self {
        if let Ok(re) = Regex::new(pattern) {
            self.warning_patterns.push(re);
        }
        self
    }

    /// Add a pattern to ignore.
    pub fn add_ignore_pattern(mut self, pattern: &str) -> Self {
        if let Ok(re) = Regex::new(pattern) {
            self.ignore_patterns.push(re);
        }
        self
    }

    /// Disable default patterns (only use custom ones).
    pub fn without_defaults(mut self) -> Self {
        self.use_defaults = false;
        self
    }

    /// Check if a line should be ignored.
    fn should_ignore(&self, line: &str) -> bool {
        self.ignore_patterns.iter().any(|p| p.is_match(line))
    }

    /// Detect error severity and extract message from a line.
    fn detect_error(&self, line: &str) -> Option<(ErrorSeverity, String, Option<String>)> {
        // Check ignore patterns first
        if self.should_ignore(line) {
            return None;
        }

        // Check custom error patterns
        for pattern in &self.error_patterns {
            if pattern.is_match(line) {
                return Some((ErrorSeverity::Error, line.to_string(), None));
            }
        }

        // Check custom warning patterns
        for pattern in &self.warning_patterns {
            if pattern.is_match(line) {
                return Some((ErrorSeverity::Warning, line.to_string(), None));
            }
        }

        if !self.use_defaults {
            return None;
        }

        // Check default patterns

        // Critical patterns (OOM, etc.)
        if default_patterns::OOM_ERROR.is_match(line) {
            return Some((ErrorSeverity::Critical, line.to_string(), Some("OutOfMemory".to_string())));
        }

        // Exception pattern with type
        if let Some(caps) = default_patterns::EXCEPTION_PATTERN.captures(line) {
            let error_type = caps.get(1).unwrap().as_str().to_string();
            let message = caps
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| line.to_string());
            return Some((ErrorSeverity::Error, message, Some(error_type)));
        }

        // Generic error pattern
        if let Some(caps) = default_patterns::ERROR_GENERIC.captures(line) {
            let level = caps.get(1).unwrap().as_str().to_lowercase();
            let message = caps.get(2).unwrap().as_str().trim().to_string();
            let severity = match level.as_str() {
                "fatal" | "critical" | "severe" => ErrorSeverity::Critical,
                _ => ErrorSeverity::Error,
            };
            return Some((severity, message, None));
        }

        // Generic warning pattern
        if let Some(caps) = default_patterns::WARNING_GENERIC.captures(line) {
            let message = caps.get(2).unwrap().as_str().trim().to_string();
            return Some((ErrorSeverity::Warning, message, None));
        }

        // HTTP error
        if let Some(caps) = default_patterns::HTTP_ERROR.captures(line) {
            let status = caps.get(1).unwrap().as_str();
            let severity = if status.starts_with('5') {
                ErrorSeverity::Error
            } else {
                ErrorSeverity::Warning
            };
            return Some((
                severity,
                line.to_string(),
                Some(format!("HTTP{}", status)),
            ));
        }

        // Connection errors
        if default_patterns::CONNECTION_ERROR.is_match(line) {
            return Some((ErrorSeverity::Error, line.to_string(), Some("ConnectionError".to_string())));
        }

        // Permission errors
        if default_patterns::PERMISSION_ERROR.is_match(line) {
            return Some((ErrorSeverity::Error, line.to_string(), Some("PermissionError".to_string())));
        }

        // Not found errors
        if default_patterns::NOT_FOUND_ERROR.is_match(line) {
            return Some((ErrorSeverity::Error, line.to_string(), Some("NotFoundError".to_string())));
        }

        // Timeout
        if default_patterns::TIMEOUT_PATTERN.is_match(line) {
            return Some((ErrorSeverity::Error, line.to_string(), Some("TimeoutError".to_string())));
        }

        // General failure
        if default_patterns::FAILURE_PATTERN.is_match(line) {
            return Some((ErrorSeverity::Error, line.to_string(), None));
        }

        None
    }
}

impl Default for GenericParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LogParser for GenericParser {
    fn parse_content(&self, content: &str, source_name: &str) -> Vec<ErrorEvent> {
        let mut errors = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            if let Some((severity, message, error_type)) = self.detect_error(line) {
                let timestamp = patterns::extract_timestamp(line);

                // Check if this is the start of a multi-line error (stack trace)
                let mut context_lines = Vec::new();
                let _start_i = i;
                i += 1;

                // Collect potential continuation lines
                if default_patterns::STACK_TRACE_START.is_match(line) {
                    while i < lines.len() {
                        let next_line = lines[i];
                        // Stack trace lines are typically indented or start with "at"
                        if next_line.starts_with("    ")
                            || next_line.starts_with("\t")
                            || next_line.trim().starts_with("at ")
                        {
                            context_lines.push(next_line.to_string());
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }

                let mut builder = ErrorEventBuilder::new(source_name)
                    .severity(severity)
                    .message(&message)
                    .raw_entry(line);

                if let Some(ref et) = error_type {
                    builder = builder.error_type(et);
                }

                if let Some(ts) = timestamp {
                    builder = builder.log_timestamp(ts);
                }

                if !context_lines.is_empty() {
                    builder = builder.stack_trace(context_lines.join("\n"));
                    builder = builder.raw_entry(
                        std::iter::once(line)
                            .chain(context_lines.iter().map(|s| s.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }

                errors.push(builder.build());
                continue;
            }

            i += 1;
        }

        errors
    }

    fn parse_line(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        if let Some((severity, message, error_type)) = self.detect_error(line) {
            let timestamp = patterns::extract_timestamp(line);

            let mut builder = ErrorEventBuilder::new(source_name)
                .severity(severity)
                .message(&message)
                .raw_entry(line);

            if let Some(ref et) = error_type {
                builder = builder.error_type(et);
            }

            if let Some(ts) = timestamp {
                builder = builder.log_timestamp(ts);
            }

            return Some(builder.build());
        }

        None
    }

    fn parser_name(&self) -> &'static str {
        "generic"
    }

    fn is_error_start(&self, line: &str) -> bool {
        self.detect_error(line).is_some()
    }

    fn is_error_continuation(&self, line: &str) -> bool {
        // Indented lines or "at" lines are typically continuation
        line.starts_with("    ")
            || line.starts_with("\t")
            || line.trim().starts_with("at ")
            || line.trim().starts_with("in ")
            || line.trim().starts_with("from ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_generic_error() {
        let parser = GenericParser::new();
        let content = "[ERROR] Something went wrong";

        let errors = parser.parse_content(content, "app");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Error);
    }

    #[test]
    fn test_parse_warning() {
        let parser = GenericParser::new();
        let content = "WARNING: Disk space running low";

        let errors = parser.parse_content(content, "system");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Warning);
    }

    #[test]
    fn test_parse_http_error() {
        let parser = GenericParser::new();
        let content = "Request failed with HTTP status 500";

        let errors = parser.parse_content(content, "api");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, Some("HTTP500".to_string()));
    }

    #[test]
    fn test_parse_connection_error() {
        let parser = GenericParser::new();
        let content = "Connection refused to localhost:5432";

        let errors = parser.parse_content(content, "db");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, Some("ConnectionError".to_string()));
    }

    #[test]
    fn test_parse_exception_with_type() {
        let parser = GenericParser::new();
        let content = "ValidationError: Invalid email format";

        let errors = parser.parse_content(content, "api");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, Some("ValidationError".to_string()));
    }

    #[test]
    fn test_custom_pattern() {
        let parser = GenericParser::new()
            .add_error_pattern(r"CUSTOM_ERROR:\s*(.+)")
            .add_ignore_pattern(r"^DEBUG:");

        let content = "CUSTOM_ERROR: Something specific happened";
        let errors = parser.parse_content(content, "custom");
        assert_eq!(errors.len(), 1);

        // Ignored pattern
        let content2 = "DEBUG: This should be ignored even with ERROR in it";
        let errors2 = parser.parse_content(content2, "custom");
        assert_eq!(errors2.len(), 0);
    }

    #[test]
    fn test_parse_timestamp() {
        let parser = GenericParser::new();
        let content = "2024-01-15T10:30:00Z ERROR Database connection failed";

        let errors = parser.parse_content(content, "app");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].log_timestamp.is_some());
    }

    #[test]
    fn test_critical_severity() {
        let parser = GenericParser::new();
        let content = "[CRITICAL] System out of memory";

        let errors = parser.parse_content(content, "system");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Critical);
    }

    #[test]
    fn test_oom_error() {
        let parser = GenericParser::new();
        let content = "Process killed: Out of memory";

        let errors = parser.parse_content(content, "system");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Critical);
        assert_eq!(errors[0].error_type, Some("OutOfMemory".to_string()));
    }

    #[test]
    fn test_without_defaults() {
        let parser = GenericParser::new()
            .without_defaults()
            .add_error_pattern(r"MY_ERROR");

        // Default patterns should not match
        let content = "[ERROR] Standard error";
        let errors = parser.parse_content(content, "app");
        assert_eq!(errors.len(), 0);

        // Custom pattern should match
        let content2 = "MY_ERROR happened";
        let errors2 = parser.parse_content(content2, "app");
        assert_eq!(errors2.len(), 1);
    }
}
