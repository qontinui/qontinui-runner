//! Check Executor Module
//!
//! This module provides execution infrastructure for code quality checks.
//! It supports four check types:
//!
//! 1. **Lint** - Code quality and style checks (ruff, eslint, clippy)
//! 2. **Format** - Code formatting checks (black, prettier, rustfmt)
//! 3. **Type Check** - Static type analysis (mypy, tsc)
//! 4. **Custom Command** - User-defined custom check commands
//!
//! # Module Structure
//!
//! - `types` - Core types (CheckDefinition, CheckResult, CheckStatus)
//! - `lint_runner` - Linting tool execution (ruff, eslint, clippy)
//! - `format_runner` - Formatting tool execution (black, prettier, rustfmt)
//! - `type_runner` - Type checker execution (mypy, tsc)
//! - `custom_runner` - Custom command execution
//!
//! # Usage
//!
//! ```rust,ignore
//! use check_executor::{execute_check, CheckDefinition, CheckType, CheckTool};
//!
//! let check = CheckDefinition {
//!     id: "check-1".to_string(),
//!     name: "Ruff Linting".to_string(),
//!     check_type: CheckType::Lint,
//!     tool: CheckTool::Ruff,
//!     ..Default::default()
//! };
//!
//! let result = execute_check(&check);
//! println!("Check passed: {}", result.is_success());
//! ```

mod types;

pub mod analyzers;
pub mod command_builder;
pub mod output_parser;

// Re-export public types
pub use types::{
    CheckDefinition, CheckExecutionResult, CheckLanguage, CheckStatus, CheckSuiteSummary,
    CheckTool, CheckToolInfoSerialized, CheckType, ProjectDetectionResult, SuggestedCheck,
    CHECK_TOOLS, CHECK_TYPE_INFO,
};

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;
use tracing::{debug, error, info, warn};

/// Detected project type based on marker files
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedProjectType {
    Python,
    JavaScript,
    TypeScript,
    Rust,
    Unknown,
}

/// Detect the project type based on marker files in the working directory
fn detect_project_type(working_dir: &str) -> DetectedProjectType {
    let path = Path::new(working_dir);

    // Check for Rust (Cargo.toml)
    if path.join("Cargo.toml").exists() {
        return DetectedProjectType::Rust;
    }

    // Check for Python (pyproject.toml, setup.py, requirements.txt)
    if path.join("pyproject.toml").exists()
        || path.join("setup.py").exists()
        || path.join("requirements.txt").exists()
    {
        return DetectedProjectType::Python;
    }

    // Check for TypeScript (tsconfig.json)
    if path.join("tsconfig.json").exists() {
        return DetectedProjectType::TypeScript;
    }

    // Check for JavaScript (package.json without tsconfig)
    if path.join("package.json").exists() {
        return DetectedProjectType::JavaScript;
    }

    DetectedProjectType::Unknown
}

/// Check if a tool is applicable to the detected project type
fn is_tool_applicable(tool: &CheckTool, project_type: &DetectedProjectType) -> bool {
    // Get tool info to determine required language
    let tool_info = CHECK_TOOLS.iter().find(|t| &t.tool == tool);

    let required_language = match tool_info {
        Some(info) => &info.language,
        None => return true, // Unknown tools are always applicable
    };

    match (required_language, project_type) {
        // Language-specific tools
        (CheckLanguage::Python, DetectedProjectType::Python) => true,
        (CheckLanguage::Javascript, DetectedProjectType::JavaScript) => true,
        (CheckLanguage::Javascript, DetectedProjectType::TypeScript) => true, // JS tools work on TS
        (CheckLanguage::Typescript, DetectedProjectType::TypeScript) => true,
        (CheckLanguage::Rust, DetectedProjectType::Rust) => true,

        // "Other" language tools are always applicable (security scan, quality gate, etc.)
        (CheckLanguage::Other, _) => true,

        // Unknown project type - run all tools (let them fail naturally if needed)
        (_, DetectedProjectType::Unknown) => true,

        // Mismatched language - skip
        _ => false,
    }
}

/// Execute a code quality check based on its type and tool
///
/// This is the main entry point for running any code quality check.
/// For tools with native Rust implementations, runs the native analyzer.
/// For other tools, builds the appropriate command and parses the output.
pub fn execute_check(check_def: &CheckDefinition) -> CheckExecutionResult {
    let started_at = chrono::Utc::now().to_rfc3339();
    let start_time = Instant::now();

    info!(
        "Executing check: {} (tool: {:?}, type: {:?})",
        check_def.name, check_def.tool, check_def.check_type
    );

    // Detect project type and check if tool is applicable
    let working_dir = check_def
        .working_directory
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let project_type = detect_project_type(&working_dir);

    if !is_tool_applicable(&check_def.tool, &project_type) {
        let tool_info = CHECK_TOOLS.iter().find(|t| t.tool == check_def.tool);
        let tool_lang = tool_info
            .map(|t| format!("{:?}", t.language))
            .unwrap_or_else(|| "Unknown".to_string());

        info!(
            "Skipping check {} - tool {:?} ({}) not applicable for {:?} project",
            check_def.name, check_def.tool, tool_lang, project_type
        );

        return CheckExecutionResult {
            check_id: check_def.id.clone(),
            check_name: check_def.name.clone(),
            status: CheckStatus::Passed,
            duration_ms: start_time.elapsed().as_millis() as u64,
            output: format!(
                "Skipped: {:?} tool not applicable for {:?} project (no matching project files found)",
                check_def.tool, project_type
            ),
            error: None,
            issues_found: 0,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: None,
            exit_code: Some(0),
            started_at,
            completed_at: chrono::Utc::now().to_rfc3339(),
        };
    }

    debug!(
        "Project type detected: {:?}, tool {:?} is applicable",
        project_type, check_def.tool
    );

    // Check if this tool has a native analyzer implementation
    if analyzers::has_native_analyzer(&check_def.tool) {
        debug!("Using native analyzer for {:?}", check_def.tool);
        return execute_native_check(check_def, started_at, start_time);
    }

    // Build the command for external tools
    let (program, args) = match command_builder::build_command(check_def) {
        Ok(cmd) => cmd,
        Err(e) => {
            error!("Failed to build command for check {}: {}", check_def.id, e);
            return CheckExecutionResult::error(
                check_def.id.clone(),
                check_def.name.clone(),
                format!("Failed to build command: {}", e),
            );
        }
    };

    debug!("Running command: {} {:?}", program, args);

    // Set up the working directory
    let working_dir = check_def
        .working_directory
        .clone()
        .unwrap_or_else(|| ".".to_string());

    // Execute the command
    // On Windows, we need to run commands through cmd.exe because tools like
    // npx, eslint, prettier are batch files (.cmd) that won't execute directly
    let mut cmd = if cfg!(target_os = "windows") {
        let full_command = if args.is_empty() {
            program.clone()
        } else {
            format!("{} {}", program, args.join(" "))
        };
        let mut c = Command::new("cmd");
        c.args(["/C", &full_command]);
        c
    } else {
        let mut c = Command::new(&program);
        c.args(&args);
        c
    };

    cmd.current_dir(&working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Add environment variables for consistent output
    cmd.env("FORCE_COLOR", "0");
    cmd.env("NO_COLOR", "1");

    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) => {
            let duration_ms = start_time.elapsed().as_millis() as u64;
            error!("Failed to execute check {}: {}", check_def.id, e);
            return CheckExecutionResult {
                check_id: check_def.id.clone(),
                check_name: check_def.name.clone(),
                status: CheckStatus::Error,
                duration_ms,
                output: String::new(),
                error: Some(format!("Failed to execute command: {}", e)),
                issues_found: 0,
                issues_fixed: 0,
                files_checked: 0,
                structured_output: None,
                exit_code: None,
                started_at,
                completed_at: chrono::Utc::now().to_rfc3339(),
            };
        }
    };

    let duration_ms = start_time.elapsed().as_millis() as u64;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined_output = if stderr.is_empty() {
        stdout.clone()
    } else {
        format!("{}\n{}", stdout, stderr)
    };
    let exit_code = output.status.code();

    // Check for timeout only if timeout is specified
    if let Some(timeout_secs) = check_def.timeout_seconds {
        if duration_ms >= (timeout_secs as u64 * 1000) {
            warn!(
                "Check {} timed out after {} seconds",
                check_def.id, timeout_secs
            );
            return CheckExecutionResult::timeout(
                check_def.id.clone(),
                check_def.name.clone(),
                timeout_secs,
            );
        }
    }

    // Parse the output to extract issues
    let parsed = output_parser::parse_output(check_def, &stdout, &stderr, exit_code);

    // Determine the status
    let status = if output.status.success() {
        if check_def.auto_fix && parsed.issues_fixed > 0 {
            CheckStatus::Fixed
        } else {
            CheckStatus::Passed
        }
    } else {
        // Some tools use non-zero exit codes for issues found
        if parsed.issues_found > 0 {
            if check_def.auto_fix
                && parsed.issues_fixed > 0
                && parsed.issues_fixed >= parsed.issues_found
            {
                CheckStatus::Fixed
            } else {
                CheckStatus::Failed
            }
        } else {
            // Non-zero exit with no parsed issues - could be an error
            CheckStatus::Failed
        }
    };

    info!(
        "Check {} completed with status {:?} (issues: {}, fixed: {})",
        check_def.id, status, parsed.issues_found, parsed.issues_fixed
    );

    let error_msg = if status == CheckStatus::Error {
        Some("Check execution failed".to_string())
    } else {
        None
    };

    CheckExecutionResult {
        check_id: check_def.id.clone(),
        check_name: check_def.name.clone(),
        status,
        duration_ms,
        output: combined_output,
        error: error_msg,
        issues_found: parsed.issues_found,
        issues_fixed: parsed.issues_fixed,
        files_checked: parsed.files_checked,
        structured_output: Some(parsed.structured_output),
        exit_code,
        started_at,
        completed_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Execute a check using a native Rust analyzer
fn execute_native_check(
    check_def: &CheckDefinition,
    started_at: String,
    start_time: Instant,
) -> CheckExecutionResult {
    let _working_dir = check_def
        .working_directory
        .clone()
        .unwrap_or_else(|| ".".to_string());

    // Run the native analyzer
    match analyzers::run_native_analyzer(check_def) {
        Ok(parsed) => {
            let duration_ms = start_time.elapsed().as_millis() as u64;

            // Determine status based on issues found
            let status = if parsed.issues_found == 0 {
                CheckStatus::Passed
            } else if parsed.issues_fixed > 0 && parsed.issues_fixed >= parsed.issues_found {
                CheckStatus::Fixed
            } else {
                CheckStatus::Failed
            };

            info!(
                "Native check {} completed with status {:?} (issues: {}, fixed: {})",
                check_def.id, status, parsed.issues_found, parsed.issues_fixed
            );

            // Build output summary for display
            let output = format!(
                "Native analyzer completed.\nFiles checked: {}\nIssues found: {}\nIssues fixed: {}",
                parsed.files_checked, parsed.issues_found, parsed.issues_fixed
            );

            CheckExecutionResult {
                check_id: check_def.id.clone(),
                check_name: check_def.name.clone(),
                status,
                duration_ms,
                output,
                error: None,
                issues_found: parsed.issues_found,
                issues_fixed: parsed.issues_fixed,
                files_checked: parsed.files_checked,
                structured_output: Some(parsed.structured_output),
                exit_code: Some(if parsed.issues_found == 0 { 0 } else { 1 }),
                started_at,
                completed_at: chrono::Utc::now().to_rfc3339(),
            }
        }
        Err(e) => {
            let duration_ms = start_time.elapsed().as_millis() as u64;
            error!("Native analyzer failed for check {}: {}", check_def.id, e);

            CheckExecutionResult {
                check_id: check_def.id.clone(),
                check_name: check_def.name.clone(),
                status: CheckStatus::Error,
                duration_ms,
                output: String::new(),
                error: Some(format!("Native analyzer failed: {}", e)),
                issues_found: 0,
                issues_fixed: 0,
                files_checked: 0,
                structured_output: None,
                exit_code: Some(1),
                started_at,
                completed_at: chrono::Utc::now().to_rfc3339(),
            }
        }
    }
}

/// Execute multiple checks and aggregate results
pub fn execute_check_suite(
    checks: &[CheckDefinition],
    stop_on_failure: bool,
) -> (Vec<CheckExecutionResult>, CheckSuiteSummary) {
    let mut results: Vec<CheckExecutionResult> = Vec::with_capacity(checks.len());
    let mut summary = CheckSuiteSummary::default();

    for check in checks {
        let result = execute_check(check);
        summary.add_result(&result);

        let should_stop = stop_on_failure && !result.is_success() && check.is_critical;
        results.push(result);

        if should_stop {
            info!("Stopping check suite due to critical check failure");
            break;
        }
    }

    (results, summary)
}

/// Detect project type and suggest relevant checks
pub fn detect_project_checks(working_directory: &str) -> ProjectDetectionResult {
    use std::path::Path;

    let path = Path::new(working_directory);
    let mut result = ProjectDetectionResult {
        detected_languages: vec![],
        detected_tools: vec![],
        config_files: vec![],
        suggested_checks: vec![],
    };

    // Check for Python project
    let python_indicators = [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
    ];
    for indicator in &python_indicators {
        if path.join(indicator).exists() {
            if !result.detected_languages.contains(&CheckLanguage::Python) {
                result.detected_languages.push(CheckLanguage::Python);
            }
            result.config_files.push(indicator.to_string());
        }
    }

    // Check for JavaScript/TypeScript project
    let js_indicators = ["package.json", "tsconfig.json"];
    for indicator in &js_indicators {
        if path.join(indicator).exists() {
            let lang = if *indicator == "tsconfig.json" {
                CheckLanguage::Typescript
            } else {
                CheckLanguage::Javascript
            };
            if !result.detected_languages.contains(&lang) {
                result.detected_languages.push(lang);
            }
            result.config_files.push(indicator.to_string());
        }
    }

    // Check for Rust project
    if path.join("Cargo.toml").exists() {
        result.detected_languages.push(CheckLanguage::Rust);
        result.config_files.push("Cargo.toml".to_string());
    }

    // Suggest checks based on detected languages
    for lang in &result.detected_languages {
        match lang {
            CheckLanguage::Python => {
                // Check for ruff config
                if path.join("ruff.toml").exists()
                    || path.join(".ruff.toml").exists()
                    || path.join("pyproject.toml").exists()
                {
                    result.detected_tools.push(CheckTool::Ruff);
                    result.suggested_checks.push(SuggestedCheck {
                        tool: CheckTool::Ruff,
                        name: "Ruff Linting".to_string(),
                        description: "Fast Python linter".to_string(),
                        command: "ruff check .".to_string(),
                        config_path: Some("pyproject.toml".to_string()),
                        auto_fix: true,
                        reason: "Found Python project with ruff configuration".to_string(),
                    });
                }

                // Suggest black
                result.detected_tools.push(CheckTool::Black);
                result.suggested_checks.push(SuggestedCheck {
                    tool: CheckTool::Black,
                    name: "Black Formatting".to_string(),
                    description: "Python code formatter".to_string(),
                    command: "black --check .".to_string(),
                    config_path: Some("pyproject.toml".to_string()),
                    auto_fix: true,
                    reason: "Found Python project".to_string(),
                });

                // Suggest mypy
                result.detected_tools.push(CheckTool::Mypy);
                result.suggested_checks.push(SuggestedCheck {
                    tool: CheckTool::Mypy,
                    name: "Mypy Type Check".to_string(),
                    description: "Python static type checker".to_string(),
                    command: "mypy .".to_string(),
                    config_path: Some("pyproject.toml".to_string()),
                    auto_fix: false,
                    reason: "Found Python project".to_string(),
                });
            }
            CheckLanguage::Javascript | CheckLanguage::Typescript => {
                // Check for eslint config
                let eslint_configs = [
                    ".eslintrc",
                    ".eslintrc.js",
                    ".eslintrc.json",
                    "eslint.config.js",
                ];
                for config in &eslint_configs {
                    if path.join(config).exists() {
                        result.detected_tools.push(CheckTool::Eslint);
                        result.suggested_checks.push(SuggestedCheck {
                            tool: CheckTool::Eslint,
                            name: "ESLint".to_string(),
                            description: "JavaScript/TypeScript linter".to_string(),
                            command: "eslint .".to_string(),
                            config_path: Some(config.to_string()),
                            auto_fix: true,
                            reason: format!("Found ESLint config: {}", config),
                        });
                        break;
                    }
                }

                // Check for prettier
                let prettier_configs = [
                    ".prettierrc",
                    ".prettierrc.js",
                    ".prettierrc.json",
                    "prettier.config.js",
                ];
                for config in &prettier_configs {
                    if path.join(config).exists() {
                        result.detected_tools.push(CheckTool::Prettier);
                        result.suggested_checks.push(SuggestedCheck {
                            tool: CheckTool::Prettier,
                            name: "Prettier".to_string(),
                            description: "Code formatter".to_string(),
                            command: "prettier --check .".to_string(),
                            config_path: Some(config.to_string()),
                            auto_fix: true,
                            reason: format!("Found Prettier config: {}", config),
                        });
                        break;
                    }
                }

                // TypeScript type checking
                if *lang == CheckLanguage::Typescript {
                    result.detected_tools.push(CheckTool::Tsc);
                    result.suggested_checks.push(SuggestedCheck {
                        tool: CheckTool::Tsc,
                        name: "TypeScript Type Check".to_string(),
                        description: "TypeScript compiler type checking".to_string(),
                        command: "tsc --noEmit".to_string(),
                        config_path: Some("tsconfig.json".to_string()),
                        auto_fix: false,
                        reason: "Found tsconfig.json".to_string(),
                    });
                }
            }
            CheckLanguage::Rust => {
                // Clippy
                result.detected_tools.push(CheckTool::Clippy);
                result.suggested_checks.push(SuggestedCheck {
                    tool: CheckTool::Clippy,
                    name: "Clippy Linting".to_string(),
                    description: "Rust linter".to_string(),
                    command: "cargo clippy -- -D warnings".to_string(),
                    config_path: Some("Cargo.toml".to_string()),
                    auto_fix: true,
                    reason: "Found Rust project".to_string(),
                });

                // Rustfmt
                result.detected_tools.push(CheckTool::Rustfmt);
                result.suggested_checks.push(SuggestedCheck {
                    tool: CheckTool::Rustfmt,
                    name: "Rust Formatting".to_string(),
                    description: "Rust code formatter".to_string(),
                    command: "cargo fmt --check".to_string(),
                    config_path: Some("Cargo.toml".to_string()),
                    auto_fix: true,
                    reason: "Found Rust project".to_string(),
                });

                // Cargo check
                result.detected_tools.push(CheckTool::CargoCheck);
                result.suggested_checks.push(SuggestedCheck {
                    tool: CheckTool::CargoCheck,
                    name: "Cargo Check".to_string(),
                    description: "Check Rust code for errors".to_string(),
                    command: "cargo check".to_string(),
                    config_path: Some("Cargo.toml".to_string()),
                    auto_fix: false,
                    reason: "Found Rust project".to_string(),
                });
            }
            CheckLanguage::Other => {}
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_check_suite_empty() {
        let (results, summary) = execute_check_suite(&[], false);
        assert!(results.is_empty());
        assert_eq!(summary.total, 0);
    }
}
