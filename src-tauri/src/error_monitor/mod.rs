//! Error monitoring module for application log collection.
//!
//! This module provides:
//! - Types for log sources and error events
//! - Parsers for different log formats (Python, JavaScript, Rust, generic)
//! - Continuous monitoring service
//!
//! Persistence is handled by `database::pg::{error_monitor, log_sources}`.
//!
//! # Architecture
//!
//! The error monitoring system captures errors from user-configured application
//! logs (the applications being automated) and makes them available to the
//! debug agent for analysis and fixing.
//!
//! ```text
//! Application Logs (user-configured)
//!        ↓
//!    Error Collector (parsers)
//!        ↓
//!    error_events table (persistent store)
//!        ↓
//!    Debug Context Curator (aggregation)
//!        ↓
//!    Debug Agent (AI analysis and fixing)
//! ```
//!
//! # Key Features
//!
//! - **Continuous monitoring**: Watch log files for new errors even outside workflows
//! - **Workflow-scoped collection**: Collect errors during specific task runs
//! - **Deduplication**: Track occurrence counts for repeated errors
//! - **Pattern detection**: Identify recurring error patterns
//! - **Debug agent integration**: Provide error context for AI-assisted debugging
//!
//! # Parsers
//!
//! The module includes specialized parsers for common log formats:
//!
//! - **Python**: Tracebacks, exceptions, logging module output
//! - **JavaScript**: V8 stack traces, TypeScript errors, ESLint output
//! - **Rust**: Panics, backtraces, cargo/rustc errors, tracing output
//! - **Generic**: Regex-based pattern matching for custom formats
//!
//! ```rust,ignore
//! use error_monitor::parsers::{LogParser, PythonParser, create_parser};
//!
//! // Use a specific parser
//! let parser = PythonParser::new();
//! let errors = parser.parse_content(log_content, "backend");
//!
//! // Or create one based on parser type
//! let parser = create_parser(&ParserType::JavaScript);
//! let errors = parser.parse_content(log_content, "frontend");
//! ```

pub mod commands;
pub mod curator;
pub mod parsers;
pub mod pipeline;
pub mod service;
pub mod types;
pub mod workflow;

pub use curator::*;
pub use service::*;
pub use types::*;
pub use workflow::*;
