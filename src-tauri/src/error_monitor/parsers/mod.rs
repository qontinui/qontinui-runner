//! Log parsers for extracting errors from different log formats.
//!
//! This module provides parsers for various log formats:
//! - Generic regex-based parser for custom patterns
//! - Python parser for tracebacks and exceptions
//! - JavaScript parser for errors and stack traces
//! - Rust parser for panics and errors
//!
//! # Usage
//!
//! ```rust,ignore
//! use error_monitor::parsers::{LogParser, PythonParser};
//!
//! let parser = PythonParser::new();
//! let errors = parser.parse_content(log_content, "backend");
//! ```

mod generic;
mod javascript;
mod python;
mod rust_parser;
mod traits;

pub use generic::*;
pub use javascript::*;
pub use python::*;
pub use rust_parser::*;
pub use traits::*;

use super::types::{ErrorEvent, ParserType};

/// Create a parser based on the parser type
pub fn create_parser(parser_type: &ParserType) -> Box<dyn LogParser> {
    match parser_type {
        ParserType::Python => Box::new(PythonParser::new()),
        ParserType::JavaScript => Box::new(JavaScriptParser::new()),
        ParserType::Rust => Box::new(RustParser::new()),
        ParserType::Generic => Box::new(GenericParser::new()),
    }
}

/// Parse log content using the specified parser type
pub fn parse_log_content(
    content: &str,
    parser_type: &ParserType,
    source_name: &str,
) -> Vec<ErrorEvent> {
    let parser = create_parser(parser_type);
    parser.parse_content(content, source_name)
}
