//! Native Code Analyzers
//!
//! This module contains native Rust implementations of code analysis tools.
//! These replace the need for external Python tools (qontinui-devtools).

pub mod common;
pub mod concurrency;
pub mod python;
pub mod rust_analyzer;
pub mod security;
// pub mod typescript; // Has pre-existing bugs in dead_code.rs - disabled until fixed

use crate::check_executor::output_parser::ParsedOutput;
use crate::check_executor::types::{CheckDefinition, CheckTool};

/// Run a native analyzer based on the check tool type
pub fn run_native_analyzer(check_def: &CheckDefinition) -> Result<ParsedOutput, String> {
    let working_dir = check_def.working_directory.as_deref().unwrap_or(".");

    match check_def.tool {
        // Python analyzers
        CheckTool::CircularDeps => python::circular_deps::analyze(working_dir),
        CheckTool::GodClass => python::god_class::analyze_with_defaults(working_dir),
        CheckTool::Coupling => python::coupling::analyze_with_defaults(working_dir),
        CheckTool::TypeCoveragePy => python::type_coverage::analyze_with_defaults(working_dir),
        CheckTool::DeadCodePy => python::dead_code::analyze(working_dir),
        CheckTool::SrpAnalyzer => python::srp::analyze_with_defaults(working_dir),

        // TypeScript analyzers (pre-existing bugs in dead_code.rs - kept commented)
        // CheckTool::TypeCoverageTs => typescript::type_coverage::analyze_with_defaults(working_dir),
        // CheckTool::DeadCodeTs => typescript::dead_code::analyze(working_dir),

        // Concurrency analyzers
        CheckTool::RaceDetector => concurrency::analyze_all(working_dir),
        CheckTool::RaceDetectorPy => concurrency::python::analyze(working_dir),
        CheckTool::RaceDetectorRust => concurrency::rust::analyze(working_dir),
        CheckTool::RaceDetectorTs => concurrency::typescript::analyze(working_dir),

        // Rust analyzers
        CheckTool::RustComplexity => rust_analyzer::complexity::analyze_with_defaults(working_dir),
        CheckTool::UnsafeRust => rust_analyzer::unsafe_code::analyze(working_dir),
        CheckTool::DeadCodeRust => rust_analyzer::dead_code::analyze(working_dir),

        // Security scanner
        CheckTool::SecurityScan => security::scan::analyze_with_defaults(working_dir),

        // Code quality tools
        CheckTool::TodoScanner => common::todo_scanner::analyze(working_dir),

        // Quality gate (TODO - requires all dependent analyzers)
        // CheckTool::QualityGate => common::quality_gate::analyze(working_dir, check_def),

        // Not a native analyzer - use command execution
        _ => Err(format!(
            "Tool {:?} is not a native analyzer",
            check_def.tool
        )),
    }
}

/// Check if a tool has a native analyzer implementation
pub fn has_native_analyzer(tool: &CheckTool) -> bool {
    matches!(
        tool,
        CheckTool::CircularDeps
            | CheckTool::GodClass
            | CheckTool::Coupling
            | CheckTool::TypeCoveragePy
            // TypeScript analyzers have pre-existing bugs
            // | CheckTool::TypeCoverageTs
            // | CheckTool::DeadCodeTs
            | CheckTool::DeadCodePy
            | CheckTool::SrpAnalyzer
            | CheckTool::RustComplexity
            | CheckTool::UnsafeRust
            | CheckTool::DeadCodeRust
            | CheckTool::SecurityScan
            | CheckTool::TodoScanner
            | CheckTool::RaceDetector
            | CheckTool::RaceDetectorPy
            | CheckTool::RaceDetectorRust
            | CheckTool::RaceDetectorTs
    )
}
