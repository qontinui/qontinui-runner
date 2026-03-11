//! Build error extraction from process stderr.
//!
//! Parses common build tool output patterns to extract structured errors
//! from managed dev server processes. This replaces the raw stderr dump
//! in the verification failure context with actionable, structured errors.
//!
//! Supported tools:
//! - TypeScript (`error TS####`)
//! - ESLint/Prettier (`line:col  error  message`)
//! - Next.js (`./path/to/file.tsx:line:col`)
//! - Vite/Rollup (`ERROR ...`, `[vite]`)
//! - Webpack (`ERROR in ./path`)
//! - Rust/Cargo (`error[E####]`, `-->`)
//! - Generic error patterns (`Error:`, `FATAL`, `FAILED`)

use regex::Regex;
use std::sync::LazyLock;

use super::types::{OutputLine, OutputStream, ProcessState, ProcessStatus};

/// A structured build error extracted from process output.
#[derive(Debug, Clone)]
pub struct BuildError {
    /// Source file path, if available
    pub file: Option<String>,
    /// Line number, if available
    pub line: Option<u32>,
    /// Column number, if available
    pub column: Option<u32>,
    /// Error code (e.g., "TS2345", "E0308")
    pub code: Option<String>,
    /// Error message
    pub message: String,
    /// Severity: "error" or "warning"
    pub severity: &'static str,
    /// Which tool produced this error
    pub tool: &'static str,
}

/// Result of analyzing a process's stderr output.
#[derive(Debug)]
pub struct ProcessBuildAnalysis {
    /// Process name
    pub name: String,
    /// Whether the build appears to be broken
    pub build_broken: bool,
    /// Extracted structured errors
    pub errors: Vec<BuildError>,
    /// Raw stderr lines that weren't parsed into errors (fallback)
    pub unparsed_lines: Vec<String>,
}

// Regex patterns (compiled once via LazyLock)

// TypeScript: src/foo.ts(10,5): error TS2345: ...
// or: src/foo.ts:10:5 - error TS2345: ...
static TS_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(.+?)[\(:](\d+)[,:](\d+)\)?[\s:-]+(?:error|warning)\s+(TS\d+)\s*:\s*(.+)$")
        .unwrap()
});

// ESLint/Prettier: 10:5  error  Missing semicolon  semi
// or: /path/to/file.tsx
//       1:1  error  ...
static ESLINT_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(\d+):(\d+)\s+(error|warning)\s+(.+?)(?:\s{2,}\S+)?$").unwrap()
});

// ESLint file header: /absolute/path/to/file.tsx
static ESLINT_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(/[^\s]+\.\w+|[A-Z]:\\[^\s]+\.\w+)$").unwrap());

// Next.js build error: ./src/components/Foo.tsx:10:5
// or: ./src/components/Foo.tsx
// Type error: ...
static NEXTJS_FILE_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\./(.+?)(?::(\d+):(\d+))?$").unwrap());

static NEXTJS_TYPE_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(?:Type error|Error):\s*(.+)$").unwrap());

// Vite/Rollup: [vite] Internal server error: ...
// or: ERROR  ...
static VITE_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\[(?:vite|rollup)\]\s*(?:Internal server error:\s*)?(.+)$").unwrap()
});

// Webpack: ERROR in ./src/foo.tsx 10:5-20
static WEBPACK_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^ERROR\s+in\s+\./(.+?)(?:\s+(\d+):(\d+))?").unwrap());

// Rust/Cargo: error[E0308]: mismatched types
//   --> src/main.rs:10:5
static RUST_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(error|warning)(?:\[(E\d+)\])?\s*:\s*(.+)$").unwrap());

static RUST_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*-->\s*(.+?):(\d+):(\d+)$").unwrap());

// Generic error patterns (fallback)
static GENERIC_ERROR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?mi)^(?:Error|FATAL|FAILED|error|panic|Traceback)[\s:]+(.+)$").unwrap()
});

/// Analyze stderr lines from a managed process and extract structured build errors.
pub fn analyze_process_output(
    name: &str,
    lines: &[OutputLine],
    status: &ProcessStatus,
) -> ProcessBuildAnalysis {
    let stderr_lines: Vec<&str> = lines
        .iter()
        .filter(|l| l.stream == OutputStream::Stderr)
        .map(|l| l.line.as_str())
        .collect();

    if stderr_lines.is_empty() {
        return ProcessBuildAnalysis {
            name: name.to_string(),
            build_broken: false,
            errors: Vec::new(),
            unparsed_lines: Vec::new(),
        };
    }

    // Join all stderr lines for multi-line pattern matching
    let stderr_text = stderr_lines.join("\n");

    let mut errors = Vec::new();

    // Try each parser in order of specificity

    // 1. TypeScript errors
    for caps in TS_ERROR.captures_iter(&stderr_text) {
        errors.push(BuildError {
            file: Some(caps[1].to_string()),
            line: caps[2].parse().ok(),
            column: caps[3].parse().ok(),
            code: Some(caps[4].to_string()),
            message: caps[5].trim().to_string(),
            severity: if caps[0].contains("warning") {
                "warning"
            } else {
                "error"
            },
            tool: "TypeScript",
        });
    }

    // 2. ESLint errors (need to track the current file header)
    {
        let mut current_file: Option<String> = None;
        for line in &stderr_lines {
            if let Some(caps) = ESLINT_FILE.captures(line) {
                current_file = Some(caps[1].to_string());
            } else if let Some(caps) = ESLINT_ERROR.captures(line) {
                let severity = if &caps[3] == "warning" {
                    "warning"
                } else {
                    "error"
                };
                errors.push(BuildError {
                    file: current_file.clone(),
                    line: caps[1].parse().ok(),
                    column: caps[2].parse().ok(),
                    code: None,
                    message: caps[4].trim().to_string(),
                    severity,
                    tool: "ESLint",
                });
            }
        }
    }

    // 3. Next.js errors (file path on one line, error on next)
    {
        let mut nextjs_file: Option<(String, Option<u32>, Option<u32>)> = None;
        for line in &stderr_lines {
            if let Some(caps) = NEXTJS_FILE_ERROR.captures(line) {
                nextjs_file = Some((
                    caps[1].to_string(),
                    caps.get(2).and_then(|m| m.as_str().parse().ok()),
                    caps.get(3).and_then(|m| m.as_str().parse().ok()),
                ));
            } else if let Some(caps) = NEXTJS_TYPE_ERROR.captures(line) {
                if let Some((ref file, ref ln, ref col)) = nextjs_file {
                    errors.push(BuildError {
                        file: Some(file.clone()),
                        line: *ln,
                        column: *col,
                        code: None,
                        message: caps[1].trim().to_string(),
                        severity: "error",
                        tool: "Next.js",
                    });
                    nextjs_file = None;
                }
            }
        }
    }

    // 4. Vite/Rollup errors
    for caps in VITE_ERROR.captures_iter(&stderr_text) {
        errors.push(BuildError {
            file: None,
            line: None,
            column: None,
            code: None,
            message: caps[1].trim().to_string(),
            severity: "error",
            tool: "Vite",
        });
    }

    // 5. Webpack errors
    for caps in WEBPACK_ERROR.captures_iter(&stderr_text) {
        errors.push(BuildError {
            file: Some(caps[1].to_string()),
            line: caps.get(2).and_then(|m| m.as_str().parse().ok()),
            column: caps.get(3).and_then(|m| m.as_str().parse().ok()),
            code: None,
            message: format!("Build error in {}", &caps[1]),
            severity: "error",
            tool: "Webpack",
        });
    }

    // 6. Rust/Cargo errors
    {
        let mut rust_error: Option<BuildError> = None;
        for line in &stderr_lines {
            if let Some(caps) = RUST_ERROR.captures(line) {
                // Save previous error if any
                if let Some(prev) = rust_error.take() {
                    errors.push(prev);
                }
                rust_error = Some(BuildError {
                    file: None,
                    line: None,
                    column: None,
                    code: caps.get(2).map(|m| m.as_str().to_string()),
                    message: caps[3].trim().to_string(),
                    severity: if &caps[1] == "warning" {
                        "warning"
                    } else {
                        "error"
                    },
                    tool: "Rust",
                });
            } else if let Some(caps) = RUST_LOCATION.captures(line) {
                if let Some(ref mut err) = rust_error {
                    err.file = Some(caps[1].to_string());
                    err.line = caps[2].parse().ok();
                    err.column = caps[3].parse().ok();
                }
            }
        }
        if let Some(err) = rust_error {
            errors.push(err);
        }
    }

    // 7. Generic errors (only if we didn't find specific ones)
    if errors.is_empty() {
        for caps in GENERIC_ERROR.captures_iter(&stderr_text) {
            errors.push(BuildError {
                file: None,
                line: None,
                column: None,
                code: None,
                message: caps[1].trim().to_string(),
                severity: "error",
                tool: "generic",
            });
        }
    }

    // Deduplicate by message — sort first so dedup_by catches all duplicates
    errors.sort_by(|a, b| (&a.file, &a.message).cmp(&(&b.file, &b.message)));
    errors.dedup_by(|a, b| a.message == b.message && a.file == b.file);

    // Determine if the build is broken: health port down + errors present
    let build_broken = errors.iter().any(|e| e.severity == "error")
        && (status.port_healthy == Some(false) || status.state == ProcessState::Failed);

    // Collect unparsed lines (only if we had few parsed errors)
    // If we extracted good errors, don't dump raw stderr
    let unparsed_lines = if errors.is_empty() {
        // No errors parsed — include raw stderr tail as fallback
        let tail_start = stderr_lines.len().saturating_sub(30);
        stderr_lines[tail_start..]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    } else {
        Vec::new()
    };

    ProcessBuildAnalysis {
        name: name.to_string(),
        build_broken,
        errors,
        unparsed_lines,
    }
}

/// Format build analysis results for the failure context string.
pub fn format_build_analysis(analyses: &[ProcessBuildAnalysis]) -> Option<String> {
    // Check if there's anything to report
    let has_errors = analyses.iter().any(|a| !a.errors.is_empty());
    let has_unparsed = analyses.iter().any(|a| !a.unparsed_lines.is_empty());
    let has_broken = analyses.iter().any(|a| a.build_broken);

    if !has_errors && !has_unparsed {
        return None;
    }

    let mut output = String::from("## Build Errors from Dev Processes\n\n");

    if has_broken {
        let broken_names: Vec<&str> = analyses
            .iter()
            .filter(|a| a.build_broken)
            .map(|a| a.name.as_str())
            .collect();
        output.push_str(&format!(
            "**BUILD BROKEN:** {} — the dev server is down with compilation errors. Fix these first.\n\n",
            broken_names.join(", ")
        ));
    }

    for analysis in analyses {
        if analysis.errors.is_empty() && analysis.unparsed_lines.is_empty() {
            continue;
        }

        if !analysis.errors.is_empty() {
            output.push_str(&format!(
                "**{} ({} error{}):**\n",
                analysis.name,
                analysis.errors.len(),
                if analysis.errors.len() == 1 { "" } else { "s" }
            ));

            for (i, err) in analysis.errors.iter().enumerate() {
                if i >= 20 {
                    output.push_str(&format!(
                        "  ... and {} more errors\n",
                        analysis.errors.len() - 20
                    ));
                    break;
                }

                // Format: - [tool] file:line:col - message
                let location = match (&err.file, err.line, err.column) {
                    (Some(f), Some(l), Some(c)) => format!("{}:{}:{}", f, l, c),
                    (Some(f), Some(l), None) => format!("{}:{}", f, l),
                    (Some(f), None, None) => f.clone(),
                    _ => String::new(),
                };

                let code_str = err
                    .code
                    .as_ref()
                    .map(|c| format!(" [{}]", c))
                    .unwrap_or_default();

                if location.is_empty() {
                    output.push_str(&format!(
                        "- [{}{}] {}\n",
                        err.severity, code_str, err.message
                    ));
                } else {
                    output.push_str(&format!(
                        "- {} [{}{}] {}\n",
                        location, err.severity, code_str, err.message
                    ));
                }
            }
            output.push('\n');
        } else if !analysis.unparsed_lines.is_empty() {
            // Fallback: raw stderr for processes where we couldn't parse errors
            output.push_str(&format!("**{} (stderr):**\n```\n", analysis.name));
            for line in &analysis.unparsed_lines {
                output.push_str(line);
                output.push('\n');
            }
            output.push_str("```\n\n");
        }
    }

    Some(output)
}
