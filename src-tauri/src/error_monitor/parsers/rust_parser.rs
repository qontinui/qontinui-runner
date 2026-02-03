//! Rust log parser for panics, errors, and tracing output.
//!
//! Parses Rust-style error output including:
//! - Panic messages with backtraces
//! - Tracing/log crate output
//! - Cargo build errors
//! - Compilation errors

#![allow(dead_code)]

use regex::Regex;
use std::sync::LazyLock;

use super::traits::{patterns, ErrorEventBuilder, LogParser};
use crate::error_monitor::types::{ErrorEvent, ErrorLocation, ErrorSeverity};

/// Regex patterns for Rust error parsing
mod rust_patterns {
    use super::*;

    /// Matches panic message: "thread 'name' panicked at 'message', file:line:col"
    pub static PANIC_MESSAGE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^thread '([^']+)' panicked at '(.+)', ([^:]+):(\d+):(\d+)$").unwrap()
    });

    /// Matches newer panic format: "thread 'name' panicked at file:line:col:"
    pub static PANIC_MESSAGE_NEW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^thread '([^']+)' panicked at ([^:]+):(\d+):(\d+):?$").unwrap()
    });

    /// Matches panic message on following line
    pub static PANIC_REASON: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(.+)$").unwrap());

    /// Matches backtrace frame: "   N: function_name"
    pub static BACKTRACE_FRAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s+(\d+):\s+(.+)$").unwrap());

    /// Matches backtrace location: "             at path/to/file.rs:line:col"
    pub static BACKTRACE_LOCATION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s+at\s+([^:]+):(\d+)(?::(\d+))?$").unwrap());

    /// Matches tracing/log output: "2024-01-15T10:30:00Z ERROR module: message" or "2024-01-15 ERROR module: message"
    pub static TRACING_LOG: LazyLock<Regex> = LazyLock::new(|| {
        // Non-verbose version for reliability
        // Timestamp can be full datetime or just date, and is optional
        Regex::new(
            r"^(?:\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?)?\s*(ERROR|WARN|INFO|DEBUG|TRACE)\s+(?:([a-zA-Z_][a-zA-Z0-9_:]*):?\s+)?(.+)$"
        ).unwrap()
    });

    /// Matches cargo/rustc error: "error[E0382]: borrow of moved value"
    pub static COMPILER_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(error|warning)(?:\[([A-Z]\d+)\])?: (.+)$").unwrap()
    });

    /// Matches compiler error location: " --> path/to/file.rs:line:col"
    pub static COMPILER_LOCATION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*-->\s*([^:]+):(\d+):(\d+)$").unwrap());

    /// Matches error chain: "Caused by:"
    pub static CAUSED_BY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^Caused by:$").unwrap());

    /// Matches anyhow/eyre context: "Context:"
    pub static ERROR_CONTEXT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(?:Context|Error|Note):?\s*(.*)$").unwrap());

    /// Matches assertion failure
    pub static ASSERTION_FAILED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"assertion (?:failed|`[^`]+` failed): (.+)$").unwrap()
    });

    /// Matches unwrap/expect failure
    pub static UNWRAP_FAILED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"called `(?:Option|Result)::(?:unwrap|expect)\(\)` on (?:a `None`|an `Err`) value(?:: (.+))?$")
            .unwrap()
    });
}

/// Parser for Rust log files and error output.
pub struct RustParser {
    /// Whether to parse cargo/rustc compiler errors
    parse_compiler_errors: bool,
}

impl RustParser {
    /// Create a new Rust parser with default settings.
    pub fn new() -> Self {
        Self {
            parse_compiler_errors: true,
        }
    }

    /// Enable/disable compiler error parsing.
    pub fn with_compiler_errors(mut self, enabled: bool) -> Self {
        self.parse_compiler_errors = enabled;
        self
    }

    /// Parse a panic with potential backtrace.
    fn parse_panic(&self, lines: &[&str], source_name: &str) -> Option<ErrorEvent> {
        if lines.is_empty() {
            return None;
        }

        let first_line = lines[0];
        let mut backtrace_frames: Vec<String> = Vec::new();

        // Try new panic format first, then old format
        let (thread_name, file_path, line_number, column_number, panic_message) =
            if let Some(caps) = rust_patterns::PANIC_MESSAGE_NEW.captures(first_line) {
                let msg = if lines.len() > 1 {
                    lines[1].trim().to_string()
                } else {
                    String::new()
                };
                (
                    Some(caps.get(1).unwrap().as_str().to_string()),
                    Some(caps.get(2).unwrap().as_str().to_string()),
                    caps.get(3).unwrap().as_str().parse().ok(),
                    caps.get(4).unwrap().as_str().parse().ok(),
                    msg,
                )
            } else if let Some(caps) = rust_patterns::PANIC_MESSAGE.captures(first_line) {
                (
                    Some(caps.get(1).unwrap().as_str().to_string()),
                    Some(caps.get(3).unwrap().as_str().to_string()),
                    caps.get(4).unwrap().as_str().parse().ok(),
                    caps.get(5).unwrap().as_str().parse().ok(),
                    caps.get(2).unwrap().as_str().to_string(),
                )
            } else {
                return None;
            };

        // Parse backtrace
        let mut in_backtrace = false;
        for line in lines.iter().skip(1) {
            if line.contains("stack backtrace:") {
                in_backtrace = true;
                continue;
            }

            if in_backtrace {
                if rust_patterns::BACKTRACE_FRAME.is_match(line)
                    || rust_patterns::BACKTRACE_LOCATION.is_match(line)
                {
                    backtrace_frames.push(line.to_string());
                } else if line.trim().is_empty() || line.starts_with("note:") {
                    break;
                }
            }
        }

        // Determine error type from panic message
        let error_type = if rust_patterns::ASSERTION_FAILED.is_match(&panic_message) {
            "AssertionError"
        } else if rust_patterns::UNWRAP_FAILED.is_match(&panic_message) {
            "UnwrapError"
        } else {
            "Panic"
        };

        let mut builder = ErrorEventBuilder::new(source_name)
            .severity(ErrorSeverity::Critical)
            .error_type(error_type)
            .message(&panic_message)
            .raw_entry(lines.join("\n"));

        if let Some(ref path) = file_path {
            builder = builder.location(ErrorLocation {
                file_path: path.clone(),
                line_number,
                column_number,
                function_name: thread_name,
            });
        }

        if !backtrace_frames.is_empty() {
            builder = builder.stack_trace(backtrace_frames.join("\n"));
        }

        Some(builder.build())
    }

    /// Parse a compiler error.
    fn parse_compiler_error(
        &self,
        lines: &[&str],
        source_name: &str,
    ) -> Option<(ErrorEvent, usize)> {
        if !self.parse_compiler_errors || lines.is_empty() {
            return None;
        }

        let first_line = lines[0];
        if let Some(caps) = rust_patterns::COMPILER_ERROR.captures(first_line) {
            let level = caps.get(1).unwrap().as_str();
            let code = caps.get(2).map(|m| m.as_str().to_string());
            let message = caps.get(3).unwrap().as_str();

            let severity = if level == "error" {
                ErrorSeverity::Error
            } else {
                ErrorSeverity::Warning
            };

            let mut builder = ErrorEventBuilder::new(source_name)
                .severity(severity)
                .message(message);

            if let Some(ref c) = code {
                builder = builder.error_type(format!("Rustc/{}", c)).error_code(c);
            } else {
                builder = builder.error_type("RustcError");
            }

            // Look for location on next line
            let mut lines_consumed = 1;
            if lines.len() > 1 {
                if let Some(loc_caps) = rust_patterns::COMPILER_LOCATION.captures(lines[1]) {
                    let file = loc_caps.get(1).unwrap().as_str();
                    let line_num: u32 = loc_caps.get(2).unwrap().as_str().parse().unwrap_or(0);
                    let col_num: u32 = loc_caps.get(3).unwrap().as_str().parse().unwrap_or(0);

                    builder = builder.location(ErrorLocation {
                        file_path: file.to_string(),
                        line_number: Some(line_num),
                        column_number: Some(col_num),
                        function_name: None,
                    });
                    lines_consumed = 2;
                }
            }

            // Collect context lines (code snippets, notes)
            let mut context = Vec::new();
            for i in lines_consumed..lines.len().min(lines_consumed + 10) {
                let line = lines[i];
                if line.starts_with("error") || line.starts_with("warning") {
                    break;
                }
                if !line.trim().is_empty() {
                    context.push(line.to_string());
                    lines_consumed = i + 1;
                }
            }

            if !context.is_empty() {
                builder = builder.context_lines(context.join("\n"));
            }

            builder = builder.raw_entry(lines[..lines_consumed].join("\n"));

            return Some((builder.build(), lines_consumed));
        }

        None
    }

    /// Parse tracing/log crate output.
    fn parse_tracing_log(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        if let Some(caps) = rust_patterns::TRACING_LOG.captures(line) {
            let level = caps.get(1).unwrap().as_str();
            let _target = caps.get(2).map(|m| m.as_str());
            let message = caps.get(3).unwrap().as_str();

            // Only capture ERROR and WARN
            let severity = match level {
                "ERROR" => ErrorSeverity::Error,
                "WARN" => ErrorSeverity::Warning,
                _ => return None,
            };

            let timestamp = patterns::extract_timestamp(line);

            let mut builder = ErrorEventBuilder::new(source_name)
                .severity(severity)
                .message(message)
                .raw_entry(line);

            if let Some(ts) = timestamp {
                builder = builder.log_timestamp(ts);
            }

            return Some(builder.build());
        }

        None
    }
}

impl Default for RustParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LogParser for RustParser {
    fn parse_content(&self, content: &str, source_name: &str) -> Vec<ErrorEvent> {
        let mut errors = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            // Check for panic
            if line.contains("panicked at") {
                // Collect panic and backtrace
                let mut panic_lines = vec![line];
                i += 1;

                while i < lines.len() {
                    let next_line = lines[i];
                    if next_line.contains("stack backtrace:")
                        || rust_patterns::BACKTRACE_FRAME.is_match(next_line)
                        || rust_patterns::BACKTRACE_LOCATION.is_match(next_line)
                        || (panic_lines.len() == 1 && !next_line.starts_with("thread"))
                    {
                        panic_lines.push(next_line);
                        i += 1;
                    } else if next_line.trim().is_empty() {
                        i += 1;
                        continue;
                    } else {
                        break;
                    }
                }

                if let Some(error) = self.parse_panic(&panic_lines, source_name) {
                    errors.push(error);
                }
                continue;
            }

            // Check for compiler error
            if let Some((error, consumed)) =
                self.parse_compiler_error(&lines[i..], source_name)
            {
                errors.push(error);
                i += consumed;
                continue;
            }

            // Check for tracing/log output
            if let Some(error) = self.parse_tracing_log(line, source_name) {
                errors.push(error);
                i += 1;
                continue;
            }

            i += 1;
        }

        errors
    }

    fn parse_line(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        // Check for tracing log
        if let Some(error) = self.parse_tracing_log(line, source_name) {
            return Some(error);
        }

        // Check for simple panic (single line)
        if line.contains("panicked at") {
            if let Some(caps) = rust_patterns::PANIC_MESSAGE.captures(line) {
                let message = caps.get(2).unwrap().as_str();
                let file = caps.get(3).unwrap().as_str();
                let line_num: u32 = caps.get(4).unwrap().as_str().parse().unwrap_or(0);
                let col_num: u32 = caps.get(5).unwrap().as_str().parse().unwrap_or(0);

                return Some(
                    ErrorEventBuilder::new(source_name)
                        .severity(ErrorSeverity::Critical)
                        .error_type("Panic")
                        .message(message)
                        .location(ErrorLocation {
                            file_path: file.to_string(),
                            line_number: Some(line_num),
                            column_number: Some(col_num),
                            function_name: None,
                        })
                        .raw_entry(line)
                        .build(),
                );
            }
        }

        // Check for compiler error (single line)
        if let Some(caps) = rust_patterns::COMPILER_ERROR.captures(line) {
            let level = caps.get(1).unwrap().as_str();
            let code = caps.get(2).map(|m| m.as_str().to_string());
            let message = caps.get(3).unwrap().as_str();

            let severity = if level == "error" {
                ErrorSeverity::Error
            } else {
                ErrorSeverity::Warning
            };

            let mut builder = ErrorEventBuilder::new(source_name)
                .severity(severity)
                .message(message)
                .raw_entry(line);

            if let Some(ref c) = code {
                builder = builder.error_type(format!("Rustc/{}", c)).error_code(c);
            }

            return Some(builder.build());
        }

        None
    }

    fn parser_name(&self) -> &'static str {
        "rust"
    }

    fn is_error_start(&self, line: &str) -> bool {
        line.contains("panicked at")
            || rust_patterns::COMPILER_ERROR.is_match(line)
            || (rust_patterns::TRACING_LOG.is_match(line)
                && (line.contains(" ERROR ") || line.contains(" WARN ")))
    }

    fn is_error_continuation(&self, line: &str) -> bool {
        rust_patterns::BACKTRACE_FRAME.is_match(line)
            || rust_patterns::BACKTRACE_LOCATION.is_match(line)
            || rust_patterns::COMPILER_LOCATION.is_match(line)
            || line.starts_with("   |")
            || line.starts_with("   =")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_panic() {
        let parser = RustParser::new();
        let content = "thread 'main' panicked at 'called `Option::unwrap()` on a `None` value', src/main.rs:42:10";

        let errors = parser.parse_content(content, "runner");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert_eq!(error.severity, ErrorSeverity::Critical);
        assert!(error.message.contains("unwrap"));

        let location = error.location.as_ref().unwrap();
        assert_eq!(location.file_path, "src/main.rs");
        assert_eq!(location.line_number, Some(42));
    }

    #[test]
    fn test_parse_panic_with_backtrace() {
        let parser = RustParser::new();
        let content = r#"thread 'main' panicked at 'assertion failed', src/lib.rs:10:5
stack backtrace:
   0: std::panicking::begin_panic
   1: my_crate::my_function
             at ./src/lib.rs:10:5
   2: main
             at ./src/main.rs:5:5"#;

        let errors = parser.parse_content(content, "test");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert!(error.stack_trace.is_some());
        assert!(error.stack_trace.as_ref().unwrap().contains("my_crate::my_function"));
    }

    #[test]
    fn test_parse_compiler_error() {
        let parser = RustParser::new();
        let content = r#"error[E0382]: borrow of moved value: `x`
 --> src/main.rs:10:5
  |
9 |     let y = x;
  |             - value moved here
10|     println!("{}", x);
  |                    ^ value borrowed here after move"#;

        let errors = parser.parse_content(content, "cargo");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert_eq!(error.error_code, Some("E0382".to_string()));
        assert!(error.error_type.as_ref().unwrap().contains("E0382"));

        let location = error.location.as_ref().unwrap();
        assert_eq!(location.file_path, "src/main.rs");
        assert_eq!(location.line_number, Some(10));
    }

    #[test]
    fn test_parse_tracing_log() {
        let parser = RustParser::new();
        let content = "2024-01-15T10:30:00Z ERROR my_module: Database connection failed";

        let errors = parser.parse_content(content, "app");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Error);
        assert!(errors[0].message.contains("Database connection failed"));
    }

    #[test]
    fn test_parse_warning() {
        let parser = RustParser::new();
        let content = "warning: unused variable: `x`";

        let errors = parser.parse_content(content, "cargo");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Warning);
    }

    #[test]
    fn test_is_error_start() {
        let parser = RustParser::new();

        assert!(parser.is_error_start("thread 'main' panicked at 'msg', file:1:1"));
        assert!(parser.is_error_start("error[E0382]: borrow of moved value"));
        assert!(parser.is_error_start("2024-01-15 ERROR module: message"));

        assert!(!parser.is_error_start("Normal log line"));
        assert!(!parser.is_error_start("   0: std::panicking::begin_panic"));
    }
}
