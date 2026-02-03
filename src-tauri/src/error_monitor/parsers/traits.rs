//! Parser trait definitions.

#![allow(dead_code)]

use crate::error_monitor::types::{ErrorEvent, ErrorLocation, ErrorSeverity};

/// Trait for log parsers that extract errors from log content.
pub trait LogParser: Send + Sync {
    /// Parse log content and extract error events.
    ///
    /// # Arguments
    /// * `content` - The raw log content to parse
    /// * `source_name` - The name of the log source (for attribution)
    ///
    /// # Returns
    /// A vector of extracted error events
    fn parse_content(&self, content: &str, source_name: &str) -> Vec<ErrorEvent>;

    /// Parse a single log line and determine if it's an error.
    ///
    /// # Returns
    /// Some(ErrorEvent) if the line contains an error, None otherwise
    fn parse_line(&self, line: &str, source_name: &str) -> Option<ErrorEvent>;

    /// Get the name of this parser type.
    fn parser_name(&self) -> &'static str;

    /// Check if a line looks like the start of a multi-line error (e.g., stack trace).
    fn is_error_start(&self, line: &str) -> bool;

    /// Check if a line is a continuation of a multi-line error.
    fn is_error_continuation(&self, line: &str) -> bool;
}

/// Helper struct for building error events
#[derive(Debug, Default)]
pub struct ErrorEventBuilder {
    pub source_name: String,
    pub severity: ErrorSeverity,
    pub error_type: Option<String>,
    pub error_code: Option<String>,
    pub message: String,
    pub stack_trace: Option<String>,
    pub location: Option<ErrorLocation>,
    pub context_lines: Option<String>,
    pub raw_entry: String,
    pub log_timestamp: Option<String>,
}

impl ErrorEventBuilder {
    pub fn new(source_name: &str) -> Self {
        Self {
            source_name: source_name.to_string(),
            severity: ErrorSeverity::Error,
            ..Default::default()
        }
    }

    pub fn severity(mut self, severity: ErrorSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn error_type(mut self, error_type: impl Into<String>) -> Self {
        self.error_type = Some(error_type.into());
        self
    }

    pub fn error_code(mut self, error_code: impl Into<String>) -> Self {
        self.error_code = Some(error_code.into());
        self
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn stack_trace(mut self, stack_trace: impl Into<String>) -> Self {
        self.stack_trace = Some(stack_trace.into());
        self
    }

    pub fn location(mut self, location: ErrorLocation) -> Self {
        self.location = Some(location);
        self
    }

    pub fn file_location(mut self, file_path: &str, line_number: Option<u32>) -> Self {
        self.location = Some(ErrorLocation {
            file_path: file_path.to_string(),
            line_number,
            column_number: None,
            function_name: None,
        });
        self
    }

    pub fn context_lines(mut self, context: impl Into<String>) -> Self {
        self.context_lines = Some(context.into());
        self
    }

    pub fn raw_entry(mut self, raw: impl Into<String>) -> Self {
        self.raw_entry = raw.into();
        self
    }

    pub fn log_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.log_timestamp = Some(timestamp.into());
        self
    }

    pub fn build(self) -> ErrorEvent {
        ErrorEvent {
            log_source_name: self.source_name,
            severity: self.severity,
            error_type: self.error_type,
            error_code: self.error_code,
            message: self.message,
            stack_trace: self.stack_trace,
            location: self.location,
            context_lines: self.context_lines,
            raw_entry: self.raw_entry,
            log_timestamp: self.log_timestamp,
        }
    }
}

/// Common patterns used across parsers
pub mod patterns {
    use regex::Regex;
    use std::sync::LazyLock;

    /// ISO 8601 timestamp pattern
    pub static TIMESTAMP_ISO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)")
            .unwrap()
    });

    /// Common log timestamp pattern (various formats)
    pub static TIMESTAMP_COMMON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d{4}[-/]\d{2}[-/]\d{2}[T ]?\d{2}:\d{2}:\d{2}(?:\.\d+)?)").unwrap()
    });

    /// Generic error/warning level pattern
    pub static LOG_LEVEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(ERROR|WARN(?:ING)?|CRITICAL|FATAL|SEVERE|EXCEPTION)\b").unwrap()
    });

    /// HTTP status code pattern (4xx, 5xx errors)
    pub static HTTP_ERROR_STATUS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b([45]\d{2})\b").unwrap());

    /// Extract timestamp from a log line
    pub fn extract_timestamp(line: &str) -> Option<String> {
        TIMESTAMP_ISO
            .captures(line)
            .or_else(|| TIMESTAMP_COMMON.captures(line))
            .map(|c| c.get(1).unwrap().as_str().to_string())
    }

    /// Determine severity from log level keywords
    pub fn severity_from_level(level: &str) -> crate::error_monitor::types::ErrorSeverity {
        use crate::error_monitor::types::ErrorSeverity;
        match level.to_uppercase().as_str() {
            "CRITICAL" | "FATAL" | "SEVERE" => ErrorSeverity::Critical,
            "ERROR" | "EXCEPTION" => ErrorSeverity::Error,
            "WARN" | "WARNING" => ErrorSeverity::Warning,
            _ => ErrorSeverity::Error,
        }
    }
}
