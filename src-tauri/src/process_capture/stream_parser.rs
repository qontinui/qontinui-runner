//! Stream error parser that bridges stdout/stderr streams to the Error Monitor.
//!
//! Adapts the `LogParser` trait for line-by-line stream processing,
//! accumulating multi-line errors (Python tracebacks, JS stack traces)
//! before emitting complete error events.

use crate::error_monitor::parsers::{create_parser, LogParser};
use crate::error_monitor::types::{ErrorEvent, ParserType};

/// Parses individual stream lines and produces `ErrorEvent`s
/// suitable for ingestion by the Error Monitor service.
pub struct StreamErrorParser {
    parser: Box<dyn LogParser>,
    source_name: String,
    accumulator: Vec<String>,
    in_error_block: bool,
}

impl StreamErrorParser {
    pub fn new(parser_type: &ParserType, source_name: &str) -> Self {
        Self {
            parser: create_parser(parser_type),
            source_name: source_name.to_string(),
            accumulator: Vec::new(),
            in_error_block: false,
        }
    }

    /// Process a single line of output.
    /// Returns any complete error events detected.
    pub fn process_line(&mut self, line: &str) -> Vec<ErrorEvent> {
        let mut events = Vec::new();

        if self.parser.is_error_start(line) {
            // If we were already accumulating an error, flush it first
            if self.in_error_block && !self.accumulator.is_empty() {
                events.extend(self.flush_accumulator());
            }
            self.in_error_block = true;
            self.accumulator.push(line.to_string());
        } else if self.in_error_block && self.parser.is_error_continuation(line) {
            self.accumulator.push(line.to_string());
        } else {
            // Not an error continuation — flush any accumulated error
            if self.in_error_block && !self.accumulator.is_empty() {
                events.extend(self.flush_accumulator());
            }
            self.in_error_block = false;

            // Try parsing as a single-line error
            if let Some(event) = self.parser.parse_line(line, &self.source_name) {
                events.push(event);
            }
        }

        events
    }

    /// Flush any accumulated multi-line error block.
    /// Call this when the process exits to emit any trailing error.
    pub fn flush(&mut self) -> Vec<ErrorEvent> {
        if self.accumulator.is_empty() {
            return Vec::new();
        }
        let events = self.flush_accumulator();
        self.in_error_block = false;
        events
    }

    fn flush_accumulator(&mut self) -> Vec<ErrorEvent> {
        let content = self.accumulator.join("\n");
        self.accumulator.clear();
        self.parser.parse_content(&content, &self.source_name)
    }
}
