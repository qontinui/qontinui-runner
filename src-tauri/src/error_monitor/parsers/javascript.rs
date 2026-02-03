//! JavaScript/TypeScript log parser for errors and stack traces.
//!
//! Parses JavaScript-style error output including:
//! - V8/Node.js stack traces
//! - Browser console errors
//! - Next.js/React errors
//! - TypeScript compilation errors
//! - ESLint/build tool errors

#![allow(dead_code)]

use regex::Regex;
use std::sync::LazyLock;

use super::traits::{patterns, ErrorEventBuilder, LogParser};
use crate::error_monitor::types::{ErrorEvent, ErrorLocation, ErrorSeverity};

/// Regex patterns for JavaScript error parsing
mod js_patterns {
    use super::*;

    /// Matches JavaScript error with type: "TypeError: Cannot read property 'x' of undefined"
    pub static ERROR_WITH_TYPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:Uncaught\s+)?([A-Z][a-zA-Z]*Error)\s*:\s*(.+)$").unwrap()
    });

    /// Matches V8 stack trace line: "    at functionName (file:line:col)"
    pub static STACK_FRAME_V8: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s+at\s+(?:(.+?)\s+)?\(?([^:]+):(\d+):(\d+)\)?$").unwrap()
    });

    /// Matches anonymous stack frame: "    at file:line:col"
    pub static STACK_FRAME_ANON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s+at\s+([^:]+):(\d+):(\d+)$").unwrap()
    });

    /// Matches Next.js/webpack error format
    pub static NEXTJS_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:error|Error)\s*[-:]?\s*(.+)$").unwrap()
    });

    /// Matches TypeScript compilation error: "src/file.ts(10,5): error TS2322: ..."
    pub static TS_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(.+?)\((\d+),(\d+)\):\s*(error|warning)\s+(TS\d+):\s*(.+)$").unwrap()
    });

    /// Matches ESLint error format: "/path/file.js:10:5: error Message [rule-name]"
    pub static ESLINT_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(.+?):(\d+):(\d+):\s*(error|warning)\s+(.+?)(?:\s+\[(.+?)\])?$").unwrap()
    });

    /// Matches console.error with prefix: "[ERROR] message" or "ERROR: message"
    pub static CONSOLE_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^\[?(ERROR|WARN(?:ING)?|FATAL)\]?\s*:?\s*(.+)$").unwrap()
    });

    /// Matches React/Next.js component error
    pub static REACT_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(?:Error:\s*)?(?:Unhandled Runtime Error|Hydration failed|React error|Component error)\s*[-:]?\s*(.+)?$",
        )
        .unwrap()
    });

    /// Matches Node.js unhandled rejection/exception
    pub static UNHANDLED_REJECTION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:UnhandledPromiseRejection|unhandled(?:Rejection)?)\s*:?\s*(.+)?$")
            .unwrap()
    });

    /// Matches webpack/build errors
    pub static BUILD_ERROR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:Module )?(?:build|compile|bundle)\s+(?:failed|error)\s*:?\s*(.+)?$")
            .unwrap()
    });

    /// Known JavaScript error types
    pub static KNOWN_ERROR_TYPES: &[&str] = &[
        "Error",
        "TypeError",
        "ReferenceError",
        "SyntaxError",
        "RangeError",
        "URIError",
        "EvalError",
        "AggregateError",
        "InternalError",
        "AssertionError",
        // Custom/framework errors
        "ValidationError",
        "NetworkError",
        "TimeoutError",
        "AbortError",
        "NotFoundError",
        "PermissionError",
        "QuotaExceededError",
        "ReadOnlyError",
        "TransactionInactiveError",
        "ConstraintError",
        "DataError",
        "InvalidStateError",
        "NotSupportedError",
        "SecurityError",
        "UnknownError",
        "OperationError",
        // Node.js specific
        "SystemError",
        "AssertionError",
    ];
}

/// Parser for JavaScript/TypeScript log files and error output.
pub struct JavaScriptParser {
    /// Whether to parse TypeScript-specific errors
    parse_typescript: bool,
    /// Whether to parse ESLint-style errors
    parse_eslint: bool,
}

impl JavaScriptParser {
    /// Create a new JavaScript parser with default settings.
    pub fn new() -> Self {
        Self {
            parse_typescript: true,
            parse_eslint: true,
        }
    }

    /// Enable/disable TypeScript error parsing.
    pub fn with_typescript(mut self, enabled: bool) -> Self {
        self.parse_typescript = enabled;
        self
    }

    /// Enable/disable ESLint error parsing.
    pub fn with_eslint(mut self, enabled: bool) -> Self {
        self.parse_eslint = enabled;
        self
    }

    /// Parse a JavaScript stack trace from lines.
    fn parse_stack_trace(&self, lines: &[&str], source_name: &str) -> Option<ErrorEvent> {
        if lines.is_empty() {
            return None;
        }

        let first_line = lines[0];
        let mut stack_frames: Vec<String> = Vec::new();
        let mut first_location: Option<ErrorLocation> = None;

        // Parse the error line
        let (error_type, message) = if let Some(caps) = js_patterns::ERROR_WITH_TYPE.captures(first_line) {
            (
                Some(caps.get(1).unwrap().as_str().to_string()),
                caps.get(2).unwrap().as_str().to_string(),
            )
        } else {
            (None, first_line.to_string())
        };

        // Parse stack frames
        for line in lines.iter().skip(1) {
            if let Some(caps) = js_patterns::STACK_FRAME_V8.captures(line) {
                let func_name = caps.get(1).map(|m| m.as_str().to_string());
                let file = caps.get(2).unwrap().as_str();
                let line_num: u32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);
                let col_num: u32 = caps.get(4).unwrap().as_str().parse().unwrap_or(0);

                // Capture first location
                if first_location.is_none() {
                    first_location = Some(ErrorLocation {
                        file_path: file.to_string(),
                        line_number: Some(line_num),
                        column_number: Some(col_num),
                        function_name: func_name.clone(),
                    });
                }

                stack_frames.push(line.to_string());
            } else if let Some(caps) = js_patterns::STACK_FRAME_ANON.captures(line) {
                let file = caps.get(1).unwrap().as_str();
                let line_num: u32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
                let col_num: u32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);

                if first_location.is_none() {
                    first_location = Some(ErrorLocation {
                        file_path: file.to_string(),
                        line_number: Some(line_num),
                        column_number: Some(col_num),
                        function_name: None,
                    });
                }

                stack_frames.push(line.to_string());
            } else if line.trim().starts_with("at ") {
                stack_frames.push(line.to_string());
            }
        }

        let severity = self.severity_for_error_type(error_type.as_deref());

        let mut builder = ErrorEventBuilder::new(source_name)
            .severity(severity)
            .message(&message)
            .raw_entry(lines.join("\n"));

        if let Some(ref err_type) = error_type {
            builder = builder.error_type(err_type);
        }

        if !stack_frames.is_empty() {
            builder = builder.stack_trace(stack_frames.join("\n"));
        }

        if let Some(location) = first_location {
            builder = builder.location(location);
        }

        Some(builder.build())
    }

    /// Parse a TypeScript compilation error.
    fn parse_typescript_error(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        if !self.parse_typescript {
            return None;
        }

        if let Some(caps) = js_patterns::TS_ERROR.captures(line) {
            let file = caps.get(1).unwrap().as_str();
            let line_num: u32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            let col_num: u32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);
            let level = caps.get(4).unwrap().as_str();
            let code = caps.get(5).unwrap().as_str();
            let message = caps.get(6).unwrap().as_str();

            let severity = if level == "error" {
                ErrorSeverity::Error
            } else {
                ErrorSeverity::Warning
            };

            return Some(
                ErrorEventBuilder::new(source_name)
                    .severity(severity)
                    .error_type(format!("TypeScript {}", code))
                    .error_code(code)
                    .message(message)
                    .file_location(file, Some(line_num))
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

        None
    }

    /// Parse an ESLint-style error.
    fn parse_eslint_error(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        if !self.parse_eslint {
            return None;
        }

        if let Some(caps) = js_patterns::ESLINT_ERROR.captures(line) {
            let file = caps.get(1).unwrap().as_str();
            let line_num: u32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            let col_num: u32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);
            let level = caps.get(4).unwrap().as_str();
            let message = caps.get(5).unwrap().as_str();
            let rule = caps.get(6).map(|m| m.as_str());

            let severity = if level == "error" {
                ErrorSeverity::Error
            } else {
                ErrorSeverity::Warning
            };

            let error_type = rule
                .map(|r| format!("ESLint/{}", r))
                .unwrap_or_else(|| "ESLint".to_string());

            return Some(
                ErrorEventBuilder::new(source_name)
                    .severity(severity)
                    .error_type(error_type)
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

        None
    }

    /// Determine severity based on error type.
    fn severity_for_error_type(&self, error_type: Option<&str>) -> ErrorSeverity {
        match error_type {
            Some("SyntaxError") | Some("ReferenceError") => ErrorSeverity::Error,
            Some("InternalError") | Some("SystemError") => ErrorSeverity::Critical,
            Some(e) if e.contains("Warning") => ErrorSeverity::Warning,
            _ => ErrorSeverity::Error,
        }
    }

    /// Check if a string is a known JavaScript error type.
    fn is_known_error_type(&self, s: &str) -> bool {
        js_patterns::KNOWN_ERROR_TYPES.contains(&s) || s.ends_with("Error")
    }
}

impl Default for JavaScriptParser {
    fn default() -> Self {
        Self::new()
    }
}

impl LogParser for JavaScriptParser {
    fn parse_content(&self, content: &str, source_name: &str) -> Vec<ErrorEvent> {
        let mut errors = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            // Check for TypeScript error
            if let Some(error) = self.parse_typescript_error(line, source_name) {
                errors.push(error);
                i += 1;
                continue;
            }

            // Check for ESLint error
            if let Some(error) = self.parse_eslint_error(line, source_name) {
                errors.push(error);
                i += 1;
                continue;
            }

            // Check for JavaScript error with potential stack trace
            if js_patterns::ERROR_WITH_TYPE.is_match(line) {
                // Collect stack trace lines
                let mut error_lines = vec![line];
                i += 1;

                while i < lines.len() {
                    let next_line = lines[i];
                    if next_line.trim().starts_with("at ") {
                        error_lines.push(next_line);
                        i += 1;
                    } else {
                        break;
                    }
                }

                if let Some(error) = self.parse_stack_trace(&error_lines, source_name) {
                    errors.push(error);
                }
                continue;
            }

            // Check for React/Next.js specific errors
            if js_patterns::REACT_ERROR.is_match(line) {
                let mut builder = ErrorEventBuilder::new(source_name)
                    .severity(ErrorSeverity::Error)
                    .error_type("ReactError")
                    .raw_entry(line);

                if let Some(caps) = js_patterns::REACT_ERROR.captures(line) {
                    if let Some(msg) = caps.get(1) {
                        builder = builder.message(msg.as_str());
                    } else {
                        builder = builder.message(line);
                    }
                }

                errors.push(builder.build());
                i += 1;
                continue;
            }

            // Check for unhandled rejection
            if js_patterns::UNHANDLED_REJECTION.is_match(line) {
                let mut builder = ErrorEventBuilder::new(source_name)
                    .severity(ErrorSeverity::Error)
                    .error_type("UnhandledPromiseRejection")
                    .raw_entry(line);

                if let Some(caps) = js_patterns::UNHANDLED_REJECTION.captures(line) {
                    if let Some(msg) = caps.get(1) {
                        builder = builder.message(msg.as_str());
                    } else {
                        builder = builder.message("Unhandled Promise Rejection");
                    }
                }

                errors.push(builder.build());
                i += 1;
                continue;
            }

            // Check for console error format
            if let Some(caps) = js_patterns::CONSOLE_ERROR.captures(line) {
                let level = caps.get(1).unwrap().as_str();
                let message = caps.get(2).unwrap().as_str();

                let severity = patterns::severity_from_level(level);

                errors.push(
                    ErrorEventBuilder::new(source_name)
                        .severity(severity)
                        .message(message)
                        .raw_entry(line)
                        .build(),
                );
                i += 1;
                continue;
            }

            // Check for build errors
            if js_patterns::BUILD_ERROR.is_match(line) {
                let mut builder = ErrorEventBuilder::new(source_name)
                    .severity(ErrorSeverity::Error)
                    .error_type("BuildError")
                    .raw_entry(line);

                if let Some(caps) = js_patterns::BUILD_ERROR.captures(line) {
                    if let Some(msg) = caps.get(1) {
                        builder = builder.message(msg.as_str());
                    } else {
                        builder = builder.message(line);
                    }
                }

                errors.push(builder.build());
                i += 1;
                continue;
            }

            i += 1;
        }

        errors
    }

    fn parse_line(&self, line: &str, source_name: &str) -> Option<ErrorEvent> {
        // TypeScript error
        if let Some(error) = self.parse_typescript_error(line, source_name) {
            return Some(error);
        }

        // ESLint error
        if let Some(error) = self.parse_eslint_error(line, source_name) {
            return Some(error);
        }

        // JavaScript error
        if let Some(caps) = js_patterns::ERROR_WITH_TYPE.captures(line) {
            let error_type = caps.get(1).unwrap().as_str();
            let message = caps.get(2).unwrap().as_str();

            return Some(
                ErrorEventBuilder::new(source_name)
                    .severity(self.severity_for_error_type(Some(error_type)))
                    .error_type(error_type)
                    .message(message)
                    .raw_entry(line)
                    .build(),
            );
        }

        // Console error
        if let Some(caps) = js_patterns::CONSOLE_ERROR.captures(line) {
            let level = caps.get(1).unwrap().as_str();
            let message = caps.get(2).unwrap().as_str();

            return Some(
                ErrorEventBuilder::new(source_name)
                    .severity(patterns::severity_from_level(level))
                    .message(message)
                    .raw_entry(line)
                    .build(),
            );
        }

        None
    }

    fn parser_name(&self) -> &'static str {
        "javascript"
    }

    fn is_error_start(&self, line: &str) -> bool {
        js_patterns::ERROR_WITH_TYPE.is_match(line)
            || js_patterns::TS_ERROR.is_match(line)
            || js_patterns::ESLINT_ERROR.is_match(line)
            || js_patterns::CONSOLE_ERROR.is_match(line)
            || js_patterns::REACT_ERROR.is_match(line)
            || js_patterns::UNHANDLED_REJECTION.is_match(line)
            || js_patterns::BUILD_ERROR.is_match(line)
    }

    fn is_error_continuation(&self, line: &str) -> bool {
        line.trim().starts_with("at ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_type_error() {
        let parser = JavaScriptParser::new();
        let content = "TypeError: Cannot read property 'foo' of undefined";

        let errors = parser.parse_content(content, "frontend");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, Some("TypeError".to_string()));
        assert!(errors[0].message.contains("Cannot read property"));
    }

    #[test]
    fn test_parse_error_with_stack() {
        let parser = JavaScriptParser::new();
        let content = r#"TypeError: Cannot read property 'foo' of undefined
    at Object.processData (/app/src/utils.js:42:15)
    at async main (/app/src/index.js:10:5)"#;

        let errors = parser.parse_content(content, "backend");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert_eq!(error.error_type, Some("TypeError".to_string()));
        assert!(error.stack_trace.is_some());

        let location = error.location.as_ref().unwrap();
        assert_eq!(location.file_path, "/app/src/utils.js");
        assert_eq!(location.line_number, Some(42));
        assert_eq!(location.column_number, Some(15));
        assert_eq!(
            location.function_name,
            Some("Object.processData".to_string())
        );
    }

    #[test]
    fn test_parse_typescript_error() {
        let parser = JavaScriptParser::new();
        let content = "src/components/Button.tsx(25,10): error TS2322: Type 'string' is not assignable to type 'number'.";

        let errors = parser.parse_content(content, "build");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert!(error.error_type.as_ref().unwrap().contains("TS2322"));
        assert_eq!(error.error_code, Some("TS2322".to_string()));

        let location = error.location.as_ref().unwrap();
        assert_eq!(location.file_path, "src/components/Button.tsx");
        assert_eq!(location.line_number, Some(25));
        assert_eq!(location.column_number, Some(10));
    }

    #[test]
    fn test_parse_eslint_error() {
        let parser = JavaScriptParser::new();
        let content = "/app/src/index.js:10:5: error Unexpected console statement [no-console]";

        let errors = parser.parse_content(content, "lint");
        assert_eq!(errors.len(), 1);

        let error = &errors[0];
        assert_eq!(error.error_type, Some("ESLint/no-console".to_string()));
    }

    #[test]
    fn test_parse_console_error() {
        let parser = JavaScriptParser::new();
        let content = "[ERROR] Failed to connect to database";

        let errors = parser.parse_content(content, "app");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, ErrorSeverity::Error);
        assert!(errors[0].message.contains("Failed to connect"));
    }

    #[test]
    fn test_parse_uncaught_error() {
        let parser = JavaScriptParser::new();
        let content = "Uncaught ReferenceError: myVar is not defined";

        let errors = parser.parse_content(content, "browser");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error_type, Some("ReferenceError".to_string()));
    }

    #[test]
    fn test_is_error_start() {
        let parser = JavaScriptParser::new();

        assert!(parser.is_error_start("TypeError: something wrong"));
        assert!(parser.is_error_start("[ERROR] Something failed"));
        assert!(parser.is_error_start("src/file.ts(10,5): error TS2322: msg"));

        assert!(!parser.is_error_start("Normal log line"));
        assert!(!parser.is_error_start("    at someFunction (/path:10:5)"));
    }
}
