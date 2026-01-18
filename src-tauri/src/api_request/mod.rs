//! API Request module for executing HTTP requests as workflow steps.
//!
//! This module provides:
//! - HTTP request execution with variable substitution
//! - cURL command parsing for easy request import
//! - Response handling with JSON path extraction
//! - Assertion evaluation for verification steps
//! - JSONL logging integration

pub mod curl_parser;
pub mod executor;
pub mod types;
pub mod variable_resolver;

pub use curl_parser::parse_curl;
pub use executor::ApiRequestExecutor;
pub use types::*;
#[allow(unused_imports)]
pub use variable_resolver::VariableResolver;
