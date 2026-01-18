//! Python code analyzers
//!
//! Native Rust implementations of Python code analysis tools.
//! Uses tree-sitter-python for parsing.

pub mod circular_deps;
pub mod coupling;
pub mod dead_code;
pub mod god_class;
pub mod srp;
pub mod type_coverage;
