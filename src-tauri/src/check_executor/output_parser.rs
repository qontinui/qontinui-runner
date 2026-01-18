//! Output Parser
//!
//! Parses output from different check tools to extract structured issue information.

use super::types::{
    CheckDefinition, CheckIssue, CheckStructuredOutput, CheckSummary, CheckTool, IssueSeverity,
};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Parsed output from a check tool
pub struct ParsedOutput {
    pub issues_found: u32,
    pub issues_fixed: u32,
    pub files_checked: u32,
    pub structured_output: CheckStructuredOutput,
}

/// Parse the output from a check tool
pub fn parse_output(
    check_def: &CheckDefinition,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> ParsedOutput {
    match check_def.tool {
        CheckTool::Ruff => parse_ruff_output(stdout, stderr, check_def.auto_fix),
        CheckTool::Eslint => parse_eslint_output(stdout, stderr),
        CheckTool::Mypy => parse_mypy_output(stdout, stderr),
        CheckTool::Pyright => parse_pyright_output(stdout, stderr),
        CheckTool::Clippy | CheckTool::CargoCheck => parse_cargo_output(stdout, stderr),
        CheckTool::Tsc => parse_tsc_output(stdout, stderr),
        CheckTool::Black => parse_black_output(stdout, stderr, check_def.auto_fix),
        CheckTool::Isort => parse_isort_output(stdout, stderr, check_def.auto_fix),
        CheckTool::Prettier => parse_prettier_output(stdout, stderr, check_def.auto_fix),
        CheckTool::Rustfmt => parse_rustfmt_output(stdout, stderr, check_def.auto_fix),
        CheckTool::Biome => parse_biome_output(stdout, stderr),
        // qontinui-devtools tools - use devtools JSON parser
        CheckTool::CircularDeps
        | CheckTool::GodClass
        | CheckTool::Coupling
        | CheckTool::TypeCoveragePy
        | CheckTool::TypeCoverageTs
        | CheckTool::SrpAnalyzer
        | CheckTool::DeadCodePy
        | CheckTool::DeadCodeTs
        | CheckTool::DeadCodeRust
        | CheckTool::TodoScanner
        | CheckTool::DeadUiState
        | CheckTool::UnusedApiParams
        | CheckTool::SecurityScan
        | CheckTool::UnsafeRust
        | CheckTool::RustComplexity
        | CheckTool::QualityGate
        | CheckTool::RaceDetector
        | CheckTool::RaceDetectorPy
        | CheckTool::RaceDetectorRust
        | CheckTool::RaceDetectorTs => parse_devtools_output(stdout, stderr, exit_code),
        CheckTool::Custom => parse_generic_output(stdout, stderr, exit_code),
    }
}

// ============================================================================
// Ruff Parser
// ============================================================================

#[derive(Deserialize, Debug)]
struct RuffIssue {
    code: Option<String>,
    message: String,
    filename: String,
    location: RuffLocation,
    end_location: Option<RuffLocation>,
    fix: Option<RuffFix>,
}

#[derive(Deserialize, Debug)]
struct RuffLocation {
    row: u32,
    column: u32,
}

#[derive(Deserialize, Debug)]
struct RuffFix {
    applicability: Option<String>,
}

fn parse_ruff_output(stdout: &str, _stderr: &str, auto_fix: bool) -> ParsedOutput {
    let mut issues = vec![];
    let mut files_with_issues = std::collections::HashSet::new();
    let mut issues_fixed = 0;

    // Try to parse as JSON array
    if let Ok(ruff_issues) = serde_json::from_str::<Vec<RuffIssue>>(stdout) {
        for issue in ruff_issues {
            files_with_issues.insert(issue.filename.clone());
            let fixable = issue.fix.is_some();
            let fix_applied = auto_fix && fixable;
            if fix_applied {
                issues_fixed += 1;
            }

            issues.push(CheckIssue {
                file: issue.filename,
                line: Some(issue.location.row),
                column: Some(issue.location.column),
                end_line: issue.end_location.as_ref().map(|l| l.row),
                end_column: issue.end_location.as_ref().map(|l| l.column),
                code: issue.code,
                message: issue.message,
                severity: IssueSeverity::Error,
                fixable,
                fix_applied: Some(fix_applied),
            });
        }
    } else {
        debug!("Failed to parse ruff output as JSON, counting lines");
        // Fallback: count non-empty lines as issues
        for line in stdout.lines() {
            if !line.trim().is_empty() && line.contains(':') {
                issues.push(CheckIssue {
                    file: "unknown".to_string(),
                    line: None,
                    column: None,
                    end_line: None,
                    end_column: None,
                    code: None,
                    message: line.to_string(),
                    severity: IssueSeverity::Error,
                    fixable: false,
                    fix_applied: None,
                });
            }
        }
    }

    let issues_found = issues.len() as u32;

    ParsedOutput {
        issues_found,
        issues_fixed,
        files_checked: files_with_issues.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([("error".to_string(), issues_found)]),
            }),
            tool_data: HashMap::new(),
        },
    }
}

// ============================================================================
// ESLint Parser
// ============================================================================

#[derive(Deserialize, Debug)]
struct EslintResult {
    #[serde(rename = "filePath")]
    file_path: String,
    messages: Vec<EslintMessage>,
    #[serde(rename = "errorCount")]
    error_count: u32,
    #[serde(rename = "warningCount")]
    warning_count: u32,
    #[serde(rename = "fixableErrorCount")]
    fixable_error_count: Option<u32>,
    #[serde(rename = "fixableWarningCount")]
    fixable_warning_count: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct EslintMessage {
    #[serde(rename = "ruleId")]
    rule_id: Option<String>,
    severity: u8,
    message: String,
    line: Option<u32>,
    column: Option<u32>,
    #[serde(rename = "endLine")]
    end_line: Option<u32>,
    #[serde(rename = "endColumn")]
    end_column: Option<u32>,
    fix: Option<serde_json::Value>,
}

fn parse_eslint_output(stdout: &str, _stderr: &str) -> ParsedOutput {
    let mut issues = vec![];
    let mut files_with_issues = 0u32;
    let mut total_errors = 0u32;
    let mut total_warnings = 0u32;

    if let Ok(results) = serde_json::from_str::<Vec<EslintResult>>(stdout) {
        for result in results {
            if !result.messages.is_empty() {
                files_with_issues += 1;
            }
            total_errors += result.error_count;
            total_warnings += result.warning_count;

            for msg in result.messages {
                let severity = if msg.severity >= 2 {
                    IssueSeverity::Error
                } else {
                    IssueSeverity::Warning
                };

                issues.push(CheckIssue {
                    file: result.file_path.clone(),
                    line: msg.line,
                    column: msg.column,
                    end_line: msg.end_line,
                    end_column: msg.end_column,
                    code: msg.rule_id,
                    message: msg.message,
                    severity,
                    fixable: msg.fix.is_some(),
                    fix_applied: None,
                });
            }
        }
    }

    let issues_found = issues.len() as u32;

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_with_issues,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), total_errors),
                    ("warning".to_string(), total_warnings),
                ]),
            }),
            tool_data: HashMap::new(),
        },
    }
}

// ============================================================================
// Mypy Parser
// ============================================================================

fn parse_mypy_output(stdout: &str, stderr: &str) -> ParsedOutput {
    let mut issues = vec![];
    let mut files_with_issues = std::collections::HashSet::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // Mypy format: file.py:line:column: error: message
    for line in combined.lines() {
        if let Some(parsed) = parse_mypy_line(line) {
            files_with_issues.insert(parsed.file.clone());
            issues.push(parsed);
        }
    }

    let issues_found = issues.len() as u32;
    let error_count = issues.iter().filter(|i| i.severity == IssueSeverity::Error).count() as u32;
    let warning_count = issues_found - error_count;

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_with_issues.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                ]),
            }),
            tool_data: HashMap::new(),
        },
    }
}

fn parse_mypy_line(line: &str) -> Option<CheckIssue> {
    // Format: file.py:line:column: severity: message
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }

    let file = parts[0].trim().to_string();
    let line_num = parts[1].trim().parse().ok();
    let column = parts[2].trim().parse().ok();
    let rest = parts[3..].join(":");
    let rest = rest.trim();

    let (severity, message) = if rest.starts_with(" error:") {
        (IssueSeverity::Error, rest[7..].trim().to_string())
    } else if rest.starts_with(" warning:") {
        (IssueSeverity::Warning, rest[9..].trim().to_string())
    } else if rest.starts_with(" note:") {
        (IssueSeverity::Info, rest[6..].trim().to_string())
    } else {
        (IssueSeverity::Error, rest.to_string())
    };

    Some(CheckIssue {
        file,
        line: line_num,
        column,
        end_line: None,
        end_column: None,
        code: None,
        message,
        severity,
        fixable: false,
        fix_applied: None,
    })
}

// ============================================================================
// Pyright Parser
// ============================================================================

#[derive(Deserialize, Debug)]
struct PyrightOutput {
    #[serde(rename = "generalDiagnostics")]
    general_diagnostics: Vec<PyrightDiagnostic>,
}

#[derive(Deserialize, Debug)]
struct PyrightDiagnostic {
    file: String,
    severity: String,
    message: String,
    rule: Option<String>,
    range: Option<PyrightRange>,
}

#[derive(Deserialize, Debug)]
struct PyrightRange {
    start: PyrightPosition,
    end: PyrightPosition,
}

#[derive(Deserialize, Debug)]
struct PyrightPosition {
    line: u32,
    character: u32,
}

fn parse_pyright_output(stdout: &str, _stderr: &str) -> ParsedOutput {
    let mut issues = vec![];
    let mut files_with_issues = std::collections::HashSet::new();

    if let Ok(output) = serde_json::from_str::<PyrightOutput>(stdout) {
        for diag in output.general_diagnostics {
            files_with_issues.insert(diag.file.clone());

            let severity = match diag.severity.as_str() {
                "error" => IssueSeverity::Error,
                "warning" => IssueSeverity::Warning,
                "information" => IssueSeverity::Info,
                _ => IssueSeverity::Error,
            };

            issues.push(CheckIssue {
                file: diag.file,
                line: diag.range.as_ref().map(|r| r.start.line + 1),
                column: diag.range.as_ref().map(|r| r.start.character + 1),
                end_line: diag.range.as_ref().map(|r| r.end.line + 1),
                end_column: diag.range.as_ref().map(|r| r.end.character + 1),
                code: diag.rule,
                message: diag.message,
                severity,
                fixable: false,
                fix_applied: None,
            });
        }
    }

    let issues_found = issues.len() as u32;

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_with_issues.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::new(),
            }),
            tool_data: HashMap::new(),
        },
    }
}

// ============================================================================
// Cargo (Clippy/Check) Parser
// ============================================================================

#[derive(Deserialize, Debug)]
struct CargoMessage {
    reason: Option<String>,
    message: Option<CargoCompilerMessage>,
}

#[derive(Deserialize, Debug)]
struct CargoCompilerMessage {
    code: Option<CargoCode>,
    level: String,
    message: String,
    spans: Vec<CargoSpan>,
}

#[derive(Deserialize, Debug)]
struct CargoCode {
    code: String,
}

#[derive(Deserialize, Debug)]
struct CargoSpan {
    file_name: String,
    line_start: u32,
    line_end: u32,
    column_start: u32,
    column_end: u32,
    is_primary: bool,
}

fn parse_cargo_output(stdout: &str, _stderr: &str) -> ParsedOutput {
    let mut issues = vec![];
    let mut files_with_issues = std::collections::HashSet::new();

    for line in stdout.lines() {
        if let Ok(msg) = serde_json::from_str::<CargoMessage>(line) {
            if msg.reason.as_deref() == Some("compiler-message") {
                if let Some(compiler_msg) = msg.message {
                    let severity = match compiler_msg.level.as_str() {
                        "error" => IssueSeverity::Error,
                        "warning" => IssueSeverity::Warning,
                        "note" | "help" => IssueSeverity::Info,
                        _ => IssueSeverity::Error,
                    };

                    // Use the primary span if available
                    let primary_span = compiler_msg.spans.iter().find(|s| s.is_primary);

                    if let Some(span) = primary_span {
                        files_with_issues.insert(span.file_name.clone());
                        issues.push(CheckIssue {
                            file: span.file_name.clone(),
                            line: Some(span.line_start),
                            column: Some(span.column_start),
                            end_line: Some(span.line_end),
                            end_column: Some(span.column_end),
                            code: compiler_msg.code.map(|c| c.code),
                            message: compiler_msg.message,
                            severity,
                            fixable: false,
                            fix_applied: None,
                        });
                    } else if !compiler_msg.spans.is_empty() {
                        let span = &compiler_msg.spans[0];
                        files_with_issues.insert(span.file_name.clone());
                        issues.push(CheckIssue {
                            file: span.file_name.clone(),
                            line: Some(span.line_start),
                            column: Some(span.column_start),
                            end_line: Some(span.line_end),
                            end_column: Some(span.column_end),
                            code: compiler_msg.code.map(|c| c.code),
                            message: compiler_msg.message,
                            severity,
                            fixable: false,
                            fix_applied: None,
                        });
                    }
                }
            }
        }
    }

    let issues_found = issues.len() as u32;
    let error_count = issues.iter().filter(|i| i.severity == IssueSeverity::Error).count() as u32;
    let warning_count = issues_found - error_count;

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_with_issues.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                ]),
            }),
            tool_data: HashMap::new(),
        },
    }
}

// ============================================================================
// TypeScript Compiler Parser
// ============================================================================

fn parse_tsc_output(stdout: &str, stderr: &str) -> ParsedOutput {
    let mut issues = vec![];
    let mut files_with_issues = std::collections::HashSet::new();
    let combined = format!("{}\n{}", stdout, stderr);

    // TSC format: file.ts(line,col): error TSxxxx: message
    for line in combined.lines() {
        if let Some(parsed) = parse_tsc_line(line) {
            files_with_issues.insert(parsed.file.clone());
            issues.push(parsed);
        }
    }

    let issues_found = issues.len() as u32;

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_with_issues.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([("error".to_string(), issues_found)]),
            }),
            tool_data: HashMap::new(),
        },
    }
}

fn parse_tsc_line(line: &str) -> Option<CheckIssue> {
    // Format: file.ts(line,col): error TSxxxx: message
    // Or: file.ts:line:col - error TSxxxx: message
    let line = line.trim();

    // Try parentheses format first
    if let Some(paren_pos) = line.find('(') {
        if let Some(close_paren) = line.find(')') {
            let file = line[..paren_pos].to_string();
            let coords = &line[paren_pos + 1..close_paren];
            let rest = &line[close_paren + 1..];

            let coords: Vec<&str> = coords.split(',').collect();
            let line_num = coords.first().and_then(|s| s.trim().parse().ok());
            let column = coords.get(1).and_then(|s| s.trim().parse().ok());

            // Extract error code and message
            let (code, message) = if let Some(colon_pos) = rest.find(':') {
                let after_colon = &rest[colon_pos + 1..].trim();
                if let Some(second_colon) = after_colon.find(':') {
                    let code = after_colon[..second_colon].trim();
                    let message = after_colon[second_colon + 1..].trim();
                    (
                        Some(code.to_string()),
                        message.to_string(),
                    )
                } else {
                    (None, after_colon.to_string())
                }
            } else {
                (None, rest.trim().to_string())
            };

            return Some(CheckIssue {
                file,
                line: line_num,
                column,
                end_line: None,
                end_column: None,
                code,
                message,
                severity: IssueSeverity::Error,
                fixable: false,
                fix_applied: None,
            });
        }
    }

    None
}

// ============================================================================
// Formatter Parsers (Black, Isort, Prettier, Rustfmt)
// ============================================================================

fn parse_black_output(stdout: &str, stderr: &str, auto_fix: bool) -> ParsedOutput {
    let combined = format!("{}\n{}", stdout, stderr);
    let mut would_reformat = 0u32;
    let mut reformatted = 0u32;

    for line in combined.lines() {
        if line.contains("would reformat") || line.contains("would be reformatted") {
            would_reformat += 1;
        } else if line.contains("reformatted") && !line.contains("would") {
            reformatted += 1;
        }
    }

    let issues_found = if auto_fix { reformatted } else { would_reformat };
    let issues_fixed = if auto_fix { reformatted } else { 0 };

    ParsedOutput {
        issues_found,
        issues_fixed,
        files_checked: 0,
        structured_output: CheckStructuredOutput::default(),
    }
}

fn parse_isort_output(stdout: &str, stderr: &str, auto_fix: bool) -> ParsedOutput {
    let combined = format!("{}\n{}", stdout, stderr);
    let mut would_sort = 0u32;
    let mut sorted = 0u32;

    for line in combined.lines() {
        if line.contains("Imports are incorrectly sorted") || line.contains("would sort") {
            would_sort += 1;
        } else if line.contains("Fixing") || line.contains("sorted") {
            sorted += 1;
        }
    }

    let issues_found = if auto_fix { sorted } else { would_sort };
    let issues_fixed = if auto_fix { sorted } else { 0 };

    ParsedOutput {
        issues_found,
        issues_fixed,
        files_checked: 0,
        structured_output: CheckStructuredOutput::default(),
    }
}

fn parse_prettier_output(stdout: &str, stderr: &str, auto_fix: bool) -> ParsedOutput {
    let combined = format!("{}\n{}", stdout, stderr);
    let mut issues_found = 0u32;

    // In check mode, prettier lists files that would be changed
    for line in combined.lines() {
        let line = line.trim();
        // Prettier outputs file paths when they would be changed
        if !line.is_empty()
            && !line.starts_with("Checking")
            && !line.starts_with("All")
            && !line.contains("files")
        {
            // Likely a file path
            if line.contains('.') {
                issues_found += 1;
            }
        }
    }

    let issues_fixed = if auto_fix { issues_found } else { 0 };

    ParsedOutput {
        issues_found,
        issues_fixed,
        files_checked: 0,
        structured_output: CheckStructuredOutput::default(),
    }
}

fn parse_rustfmt_output(stdout: &str, stderr: &str, auto_fix: bool) -> ParsedOutput {
    let combined = format!("{}\n{}", stdout, stderr);
    let mut diff_count = 0u32;

    // In check mode, rustfmt outputs diff
    for line in combined.lines() {
        if line.starts_with("Diff in") {
            diff_count += 1;
        }
    }

    let issues_found = diff_count;
    let issues_fixed = if auto_fix { diff_count } else { 0 };

    ParsedOutput {
        issues_found,
        issues_fixed,
        files_checked: 0,
        structured_output: CheckStructuredOutput::default(),
    }
}

// ============================================================================
// Biome Parser
// ============================================================================

fn parse_biome_output(stdout: &str, _stderr: &str) -> ParsedOutput {
    // Biome JSON output is complex, for now just count issues
    let mut issues_found = 0u32;

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(diagnostics) = value.get("diagnostics").and_then(|d| d.as_array()) {
            issues_found = diagnostics.len() as u32;
        }
    }

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: 0,
        structured_output: CheckStructuredOutput::default(),
    }
}

// ============================================================================
// qontinui-devtools Parser
// ============================================================================

fn parse_devtools_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> ParsedOutput {
    // qontinui-devtools outputs JSON with issues array
    // Format: { "issues": [...], "summary": {...} }
    let mut issues = vec![];
    let mut files_with_issues = std::collections::HashSet::new();

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        // Try to parse issues array
        if let Some(issues_arr) = value.get("issues").and_then(|i| i.as_array()) {
            for issue_val in issues_arr {
                let file = issue_val
                    .get("file")
                    .and_then(|f| f.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                files_with_issues.insert(file.clone());

                let severity = match issue_val
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("error")
                {
                    "warning" => IssueSeverity::Warning,
                    "info" => IssueSeverity::Info,
                    "hint" => IssueSeverity::Hint,
                    _ => IssueSeverity::Error,
                };

                issues.push(CheckIssue {
                    file,
                    line: issue_val.get("line").and_then(|l| l.as_u64()).map(|l| l as u32),
                    column: issue_val.get("column").and_then(|c| c.as_u64()).map(|c| c as u32),
                    end_line: issue_val.get("end_line").and_then(|l| l.as_u64()).map(|l| l as u32),
                    end_column: issue_val.get("end_column").and_then(|c| c.as_u64()).map(|c| c as u32),
                    code: issue_val.get("code").and_then(|c| c.as_str()).map(|s| s.to_string()),
                    message: issue_val
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string(),
                    severity,
                    fixable: issue_val.get("fixable").and_then(|f| f.as_bool()).unwrap_or(false),
                    fix_applied: None,
                });
            }
        }
    } else {
        // Fallback to generic parsing if JSON parsing fails
        debug!("Failed to parse devtools output as JSON, falling back to generic parser");
        return parse_generic_output(stdout, stderr, exit_code);
    }

    let issues_found = issues.len() as u32;
    let error_count = issues.iter().filter(|i| i.severity == IssueSeverity::Error).count() as u32;
    let warning_count = issues.iter().filter(|i| i.severity == IssueSeverity::Warning).count() as u32;

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: files_with_issues.len() as u32,
        structured_output: CheckStructuredOutput {
            issues,
            summary: Some(CheckSummary {
                total_files: 0,
                files_with_issues: files_with_issues.len() as u32,
                total_issues: issues_found,
                issues_by_severity: HashMap::from([
                    ("error".to_string(), error_count),
                    ("warning".to_string(), warning_count),
                ]),
            }),
            tool_data: HashMap::new(),
        },
    }
}

// ============================================================================
// Generic Parser
// ============================================================================

fn parse_generic_output(stdout: &str, stderr: &str, exit_code: Option<i32>) -> ParsedOutput {
    // For custom commands, we just count non-empty lines as potential issues
    // and rely on exit code for pass/fail
    let combined = format!("{}\n{}", stdout, stderr);
    let mut line_count = 0u32;

    for line in combined.lines() {
        let line = line.trim();
        if !line.is_empty() {
            line_count += 1;
        }
    }

    // If exit code is 0, no issues; otherwise, treat line count as issues
    let issues_found = if exit_code == Some(0) { 0 } else { line_count };

    ParsedOutput {
        issues_found,
        issues_fixed: 0,
        files_checked: 0,
        structured_output: CheckStructuredOutput::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_executor::CheckType;

    fn make_check_def(tool: CheckTool) -> CheckDefinition {
        CheckDefinition {
            id: "test".to_string(),
            name: "Test".to_string(),
            check_type: CheckType::Lint,
            tool,
            command: None,
            working_directory: None,
            config_path: None,
            auto_fix: false,
            fail_on_warning: false,
            timeout_seconds: 60,
            is_critical: false,
        }
    }

    #[test]
    fn test_parse_mypy_line() {
        let line = "src/main.py:10:5: error: Incompatible types";
        let issue = parse_mypy_line(line).unwrap();
        assert_eq!(issue.file, "src/main.py");
        assert_eq!(issue.line, Some(10));
        assert_eq!(issue.column, Some(5));
        assert_eq!(issue.severity, IssueSeverity::Error);
    }

    #[test]
    fn test_parse_tsc_line() {
        let line = "src/index.ts(5,10): error TS2304: Cannot find name 'foo'";
        let issue = parse_tsc_line(line).unwrap();
        assert_eq!(issue.file, "src/index.ts");
        assert_eq!(issue.line, Some(5));
        assert_eq!(issue.column, Some(10));
    }

    #[test]
    fn test_parse_ruff_json() {
        let json = r#"[
            {
                "code": "E501",
                "message": "Line too long",
                "filename": "src/main.py",
                "location": {"row": 10, "column": 80},
                "end_location": {"row": 10, "column": 100}
            }
        ]"#;

        let check = make_check_def(CheckTool::Ruff);
        let result = parse_output(&check, json, "", Some(1));

        assert_eq!(result.issues_found, 1);
        assert_eq!(result.structured_output.issues.len(), 1);
        assert_eq!(result.structured_output.issues[0].code, Some("E501".to_string()));
    }
}
