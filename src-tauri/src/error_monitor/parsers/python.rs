//! Python log parser for tracebacks and exceptions.
//!
//! Parses Python-style error output including:
//! - Full tracebacks with "Traceback (most recent call last):"
//! - Exception lines like "ValueError: invalid literal"
//! - Logging module errors with level prefixes
//! - Django/Flask framework errors

#![allow(dead_code)]

use regex::Regex;
use std::sync::LazyLock;

use super::traits::{patterns, ErrorEventBuilder, LogParser};
use crate::error_monitor::types::{ErrorEvent, ErrorLocation, ErrorSeverity};

/// Regex patterns for Python error parsing
mod python_patterns {
    use super::*;

    /// Matches the start of a Python traceback
    pub static TRACEBACK_START: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^Traceback \(most recent call last\):").unwrap());

    /// Matches a traceback file line: '  File "path", line N, in func'
    pub static TRACEBACK_FILE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"^\s+File "([^"]+)", line (\d+)(?:, in (.+))?$"#).unwrap());

    /// Matches an exception line: 'ExceptionType: message' or 'ExceptionType'
    pub static EXCEPTION_LINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([A-Z][a-zA-Z0-9_]*(?:Error|Exception|Warning|Interrupt|Exit|Failure|Fault)?)\s*(?::\s*(.*))?$")
            .unwrap()
    });

    /// Matches Python logging format: '2024-01-01 12:00:00 ERROR message' or '2024-01-01 ERROR message'
    pub static LOGGING_LINE: LazyLock<Regex> = LazyLock::new(|| {
        // Non-verbose version for reliability
        // Format: [optional timestamp (date or date+time)] [optional logger in brackets] LEVEL [: or -] message
        Regex::new(
            r"^(?:\d{4}[-/]\d{2}[-/]\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?)?)?\s*(?:\[([a-zA-Z_][\w.]*)\]\s+)?(ERROR|WARNING|WARN|CRITICAL|FATAL|EXCEPTION)\s*[:\-]?\s*(.+)$"
        ).unwrap()
    });

    /// Matches Django/WSGI error format
    pub static DJANGO_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:Internal Server Error|(?:Django|WSGI)\s+Error):\s*(.+)$").unwrap()
    });

    /// Matches assertion errors
    pub static ASSERTION_ERROR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^AssertionError(?::\s*(.+))?$").unwrap());

    /// Matches import errors
    pub static IMPORT_ERROR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:ModuleNotFoundError|ImportError):\s*(.+)$").unwrap());

    /// Matches syntax errors (special format with caret)
    pub static SYNTAX_ERROR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^SyntaxError:\s*(.+)$").unwrap());

    /// Common Python exception types
    pub static KNOWN_EXCEPTIONS: &[&str] = &[
        "Exception",
        "BaseException",
        "TypeError",
        "ValueError",
        "KeyError",
        "IndexError",
        "AttributeError",
        "NameError",
        "ImportError",
        "ModuleNotFoundError",
        "FileNotFoundError",
        "IOError",
        "OSError",
        "RuntimeError",
        "StopIteration",
        "GeneratorExit",
        "AssertionError",
        "ArithmeticError",
        "ZeroDivisionError",
        "OverflowError",
        "FloatingPointError",
        "LookupError",
        "SyntaxError",
        "IndentationError",
        "TabError",
        "SystemError",
        "MemoryError",
        "RecursionError",
        "ConnectionError",
        "BrokenPipeError",
        "ConnectionAbortedError",
        "ConnectionRefusedError",
        "ConnectionResetError",
        "TimeoutError",
        "PermissionError",
        "NotImplementedError",
        "UnicodeError",
        "UnicodeDecodeError",
        "UnicodeEncodeError",
        "JSONDecodeError",
        "HTTPError",
        "URLError",
        "SSLError",
        "DatabaseError",
        "IntegrityError",
        "OperationalError",
        "ProgrammingError",
        "InterfaceError",
        "DataError",
        "NotSupportedError",
        "ValidationError",
        "DoesNotExist",
        "MultipleObjectsReturned",
    ];
}

/// Parser for Python log files and error output.
pub struct PythonParser {
    /// Custom exception patterns to recognize
    custom_exception_patterns: Vec<Regex>,
}

impl PythonParser {
    /// Create a new Python parser with default settings.
    pub fn new() -> Self {
        Self {
            custom_exception_patterns: Vec::new(),
        }
    }

    /// Add a custom exception pattern.
    pub fn with_custom_pattern(mut self, pattern: &str) -> Self {
        if let Ok(re) = Regex::new(pattern) {
            self.custom_exception_patterns.push(re);
        }
        self
    }

    /// Parse a complete traceback from lines.
    fn parse_traceback(&self, lines: &[&str], source_name: &str) -> Option<ErrorEvent> {
        if lines.is_empty() {
            return None;
        }

        let mut stack_frames: Vec<String> = Vec::new();
        let mut last_file: Option<String> = None;
        let mut last_line: Option<u32> = None;
        let mut last_function: Option<String> = None;
        let mut exception_type: Option<String> = None;
        let mut exception_message: Option<String> = None;

        for line in lines {
            // Check for file reference in traceback
            if let Some(caps) = python_patterns::TRACEBACK_FILE.captures(line) {
                last_file = Some(caps.get(1).unwrap().as_str().to_string());
                last_line = caps.get(2).and_then(|m| m.as_str().parse().ok());
                last_function = caps.get(3).map(|m| m.as_str().to_string());
                stack_frames.push(line.to_string());
            }
            // Check for exception line (usually the last line)
            else if let Some(caps) = python_patterns::EXCEPTION_LINE.captures(line) {
                let exc_type = caps.get(1).unwrap().as_str();
                if self.is_known_exception(exc_type) {
                    exception_type = Some(exc_type.to_string());
                    exception_message = caps.get(2).map(|m| m.as_str().to_string());
                }
            }
            // Code snippet lines (indented with spaces, not "File")
            else if line.starts_with("    ") && !line.trim().is_empty() {
                stack_frames.push(line.to_string());
            }
        }

        // Build error event
        let message = exception_message.clone().unwrap_or_else(|| {
            exception_type
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string())
        });

        let severity = self.severity_for_exception(exception_type.as_deref());

        let mut builder = ErrorEventBuilder::new(source_name)
            .severity(severity)
            .message(&message)
            .raw_entry(lines.join("\n"));

        if let Some(ref exc_type) = exception_type {
            builder = builder.error_type(exc_type);
        }

        if !stack_frames.is_empty() {
            builder = builder.stack_trace(stack_frames.join("\n"));
        }

        if let Some(ref file) = last_file {
            builder = builder.location(ErrorLocation {
                file_path: file.clone(),
                line_number: last_line,
                column_number: None,
                function_name: last_function,
            });
        }

        Some(builder.build())
    }

    /// Check if a string is a known Python exception type.
    fn is_known_exception(&self, s: &str) -> bool {
        python_patterns::KNOWN_EXCEPTIONS.contains(&s)
            || s.ends_with("Error")
            || s.ends_with("Exception")
            || s.ends_with("Warning")
            || s.ends_with("Failure")
    }

    /// Determine severity based on exception type.
    fn severity_for_exception(&self, exception_type: Option<&str>) -> ErrorSeverity {
        match exception_type {
            Some("SystemExit") | Some("KeyboardInterrupt") => ErrorSeverity::Warning,
            Some(e) if e.contains("Warning") => ErrorSeverity::Warning,
            Some("MemoryError") | Some("RecursionError") | Some("SystemError") => {
                ErrorSeverity::Critical
            }
            _ => ErrorSeverity::Error,
        }
    }

    /// Parse a Python logging-style line.
    fn parse_logging_line(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        if let Some(caps) = python_patterns::LOGGING_LINE.captures(line) {
            let level = caps.get(2).unwrap().as_str();
            let message = caps.get(3).unwrap().as_str();
            let timestamp = patterns::extract_timestamp(line);

            let severity = patterns::severity_from_level(level);

            let mut builder = ErrorEventBuilder::new(source_name)
                .severity(severity)
                .message(message)
                .raw_entry(line);

            if let Some(ts) = timestamp {
                builder = builder.log_timestamp(ts);
            }

            // Try to extract exception type from message
            if let Some(exc_caps) = python_patterns::EXCEPTION_LINE.captures(message) {
                let exc_type = exc_caps.get(1).unwrap().as_str();
                if self.is_known_exception(exc_type) {
                    builder = builder.error_type(exc_type);
                }
            }

            return Some(builder.build());
        }
        None
    }
}

impl Default for PythonParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LogParser for PythonParser {
    fn parse_content(&self, content: &str, source_name: &str) -> Vec<ErrorEvent> {
        let mut errors = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            // Check for traceback start
            if python_patterns::TRACEBACK_START.is_match(line) {
                // Collect all traceback lines
                let mut traceback_lines = vec![line];
                i += 1;

                while i < lines.len() {
                    let next_line = lines[i];
                    // Traceback continues with indented lines or exception line
                    if next_line.starts_with("  ")
                        || python_patterns::EXCEPTION_LINE.is_match(next_line)
                    {
                        traceback_lines.push(next_line);
                        // Stop after exception line
                        if python_patterns::EXCEPTION_LINE.is_match(next_line)
                            && !next_line.starts_with("  ")
                        {
                            i += 1;
                            break;
                        }
                    } else {
                        break;
                    }
                    i += 1;
                }

                if let Some(error) = self.parse_traceback(&traceback_lines, source_name) {
                    errors.push(error);
                }
                continue;
            }

            // Check for standalone exception line
            if let Some(caps) = python_patterns::EXCEPTION_LINE.captures(line) {
                let exc_type = caps.get(1).unwrap().as_str();
                if self.is_known_exception(exc_type) {
                    let message = caps.get(2).map(|m| m.as_str()).unwrap_or(exc_type);

                    let error = ErrorEventBuilder::new(source_name)
                        .severity(self.severity_for_exception(Some(exc_type)))
                        .error_type(exc_type)
                        .message(message)
                        .raw_entry(line)
                        .build();

                    errors.push(error);
                    i += 1;
                    continue;
                }
            }

            // Check for logging-style error
            if let Some(error) = self.parse_logging_line(line, source_name) {
                errors.push(error);
            }

            i += 1;
        }

        errors
    }

    fn parse_line(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        // Check for exception line
        if let Some(caps) = python_patterns::EXCEPTION_LINE.captures(line) {
            let exc_type = caps.get(1).unwrap().as_str();
            if self.is_known_exception(exc_type) {
                let message = caps.get(2).map(|m| m.as_str()).unwrap_or(exc_type);

                return Some(
                    ErrorEventBuilder::new(source_name)
                        .severity(self.severity_for_exception(Some(exc_type)))
                        .error_type(exc_type)
                        .message(message)
                        .raw_entry(line)
                        .build(),
                );
            }
        }

        // Check for logging-style error
        self.parse_logging_line(line, source_name)
    }

    fn parser_name(&self) -> &'static str {
        "python"
    }

    fn is_error_start(&self, line: &str) -> bool {
        python_patterns::TRACEBACK_START.is_match(line)
            || python_patterns::LOGGING_LINE.is_match(line)
            || (python_patterns::EXCEPTION_LINE.is_match(line) && {
                if let Some(caps) = python_patterns::EXCEPTION_LINE.captures(line) {
                    self.is_known_exception(caps.get(1).unwrap().as_str())
                } else {
                    false
                }
            })
    }

    fn is_error_continuation(&self, line: &str) -> bool {
        // Traceback continuation lines are indented
        line.starts_with("  ") || python_patterns::TRACEBACK_FILE.is_match(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_exception() {
        let parser = PythonParser::new();
        let content = "ValueError: invalid literal for int() with base 10: 'abc'";

        let errors = parser.parse_content(content, "test");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, Some("ValueError".to_string()));
        assert!(errors[0].message.contains("invalid literal"));
    }

    #[test]
    fn test_parse_traceback() {
        let parser = PythonParser::new();
        let content = r#"Traceback (most recent call last):
  File "/app/main.py", line 42, in process_data
    result = int(value)
ValueError: invalid literal for int() with base 10: 'abc'"#;

        let errors = parser.parse_content(content, "backend");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert_eq!(error.error_type, Some("ValueError".to_string()));
        assert!(error.stack_trace.is_some());

        let location = error.location.as_ref().unwrap();
        assert_eq!(location.file_path, "/app/main.py");
        assert_eq!(location.line_number, Some(42));
        assert_eq!(location.function_name, Some("process_data".to_string()));
    }

    #[test]
    fn test_parse_logging_error() {
        let parser = PythonParser::new();
        let content = "2024-01-15 10:30:00 ERROR Database connection failed";

        let errors = parser.parse_content(content, "api");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Error);
        assert!(errors[0].message.contains("Database connection failed"));
    }

    #[test]
    fn test_parse_warning() {
        let parser = PythonParser::new();
        let content = "2024-01-15 10:30:00 WARNING Deprecated function called";

        let errors = parser.parse_content(content, "api");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Warning);
    }

    #[test]
    fn test_multiple_errors() {
        let parser = PythonParser::new();
        let content = r#"2024-01-15 10:30:00 ERROR First error
2024-01-15 10:30:01 ERROR Second error
TypeError: cannot add str and int"#;

        let errors = parser.parse_content(content, "test");
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn test_is_error_start() {
        let parser = PythonParser::new();

        assert!(parser.is_error_start("Traceback (most recent call last):"));
        assert!(parser.is_error_start("2024-01-15 ERROR Something failed"));
        assert!(parser.is_error_start("ValueError: something wrong"));

        assert!(!parser.is_error_start("Normal log line"));
        assert!(!parser.is_error_start("  File \"/path/to/file.py\", line 10"));
    }
}
