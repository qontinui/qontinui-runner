//! Command Builder
//!
//! Builds shell commands for different check tools.

use super::types::{CheckDefinition, CheckTool, CheckType};
use std::error::Error;

/// Build the command and arguments for a check definition
///
/// Returns (program, args) tuple
pub fn build_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    // If custom command is provided, parse and use it
    if let Some(ref custom_cmd) = check_def.command {
        return parse_command_string(custom_cmd);
    }

    // Otherwise, build command based on tool
    match check_def.tool {
        // Python tools
        CheckTool::Black => build_black_command(check_def),
        CheckTool::Isort => build_isort_command(check_def),
        CheckTool::Ruff => build_ruff_command(check_def),
        CheckTool::Mypy => build_mypy_command(check_def),
        CheckTool::Pyright => build_pyright_command(check_def),

        // JavaScript/TypeScript tools
        CheckTool::Eslint => build_eslint_command(check_def),
        CheckTool::Prettier => build_prettier_command(check_def),
        CheckTool::Tsc => build_tsc_command(check_def),
        CheckTool::Biome => build_biome_command(check_def),

        // Rust tools
        CheckTool::Clippy => build_clippy_command(check_def),
        CheckTool::Rustfmt => build_rustfmt_command(check_def),
        CheckTool::CargoCheck => build_cargo_check_command(check_def),

        // qontinui-devtools Analysis tools
        CheckTool::CircularDeps => build_devtools_command("circular-deps", check_def),
        CheckTool::GodClass => build_devtools_command("god-class", check_def),
        CheckTool::Coupling => build_devtools_command("coupling", check_def),
        CheckTool::TypeCoveragePy => build_devtools_command("type-coverage-py", check_def),
        CheckTool::TypeCoverageTs => build_devtools_command("type-coverage-ts", check_def),
        CheckTool::SrpAnalyzer => build_devtools_command("srp-analyzer", check_def),
        CheckTool::DeadCodePy => build_devtools_command("dead-code-py", check_def),
        CheckTool::DeadCodeTs => build_devtools_command("dead-code-ts", check_def),
        CheckTool::DeadCodeRust => build_devtools_command("dead-code-rust", check_def),

        // qontinui-devtools Code Quality tools
        CheckTool::TodoScanner => build_devtools_command("todo-scanner", check_def),
        CheckTool::DeadUiState => build_devtools_command("dead-ui-state", check_def),
        CheckTool::UnusedApiParams => build_devtools_command("unused-api-params", check_def),

        // qontinui-devtools Security tools
        CheckTool::SecurityScan => build_devtools_command("security-scan", check_def),
        CheckTool::UnsafeRust => build_devtools_command("unsafe-rust", check_def),

        // qontinui-devtools Rust Analysis
        CheckTool::RustComplexity => build_devtools_command("rust-complexity", check_def),

        // qontinui-devtools Meta
        CheckTool::QualityGate => build_devtools_command("quality-gate", check_def),

        // qontinui-devtools Race Detectors
        CheckTool::RaceDetector => build_devtools_command("race-detector", check_def),
        CheckTool::RaceDetectorPy => build_devtools_command("race-detector-py", check_def),
        CheckTool::RaceDetectorRust => build_devtools_command("race-detector-rust", check_def),
        CheckTool::RaceDetectorTs => build_devtools_command("race-detector-ts", check_def),

        // Custom - should have a command specified
        CheckTool::Custom => Err("Custom tool requires a command to be specified".into()),
    }
}

/// Parse a command string into program and arguments
fn parse_command_string(cmd: &str) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return Err("Empty command".into());
    }

    let program = parts[0].to_string();
    let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

    Ok((program, args))
}

// ============================================================================
// Python Tools
// ============================================================================

fn build_black_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    if !check_def.auto_fix {
        args.push("--check".to_string());
    }

    if let Some(ref config) = check_def.config_path {
        args.push("--config".to_string());
        args.push(config.clone());
    }

    args.push(".".to_string());

    Ok(("black".to_string(), args))
}

fn build_isort_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    if !check_def.auto_fix {
        args.push("--check-only".to_string());
    }

    if let Some(ref config) = check_def.config_path {
        args.push("--settings-path".to_string());
        args.push(config.clone());
    }

    args.push(".".to_string());

    Ok(("isort".to_string(), args))
}

fn build_ruff_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec!["check".to_string()];

    if check_def.auto_fix {
        args.push("--fix".to_string());
    }

    if let Some(ref config) = check_def.config_path {
        args.push("--config".to_string());
        args.push(config.clone());
    }

    // Output format for parsing
    args.push("--output-format".to_string());
    args.push("json".to_string());

    args.push(".".to_string());

    Ok(("ruff".to_string(), args))
}

fn build_mypy_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    if let Some(ref config) = check_def.config_path {
        args.push("--config-file".to_string());
        args.push(config.clone());
    }

    // Output format for parsing (note: mypy doesn't have JSON output by default)
    args.push("--show-column-numbers".to_string());

    args.push(".".to_string());

    Ok(("mypy".to_string(), args))
}

fn build_pyright_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    // Pyright outputs JSON by default with this flag
    args.push("--outputjson".to_string());

    if let Some(ref config) = check_def.config_path {
        args.push("--project".to_string());
        args.push(config.clone());
    }

    Ok(("pyright".to_string(), args))
}

// ============================================================================
// JavaScript/TypeScript Tools
// ============================================================================

fn build_eslint_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    if check_def.auto_fix {
        args.push("--fix".to_string());
    }

    if let Some(ref config) = check_def.config_path {
        args.push("--config".to_string());
        args.push(config.clone());
    }

    // Output format for parsing
    args.push("--format".to_string());
    args.push("json".to_string());

    args.push(".".to_string());

    Ok(("eslint".to_string(), args))
}

fn build_prettier_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    if check_def.auto_fix {
        args.push("--write".to_string());
    } else {
        args.push("--check".to_string());
    }

    if let Some(ref config) = check_def.config_path {
        args.push("--config".to_string());
        args.push(config.clone());
    }

    args.push(".".to_string());

    Ok(("prettier".to_string(), args))
}

fn build_tsc_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec!["--noEmit".to_string()];

    if let Some(ref config) = check_def.config_path {
        args.push("--project".to_string());
        args.push(config.clone());
    }

    Ok(("tsc".to_string(), args))
}

fn build_biome_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec![];

    if check_def.auto_fix {
        args.push("check".to_string());
        args.push("--write".to_string());
    } else {
        args.push("check".to_string());
    }

    if let Some(ref config) = check_def.config_path {
        args.push("--config-path".to_string());
        args.push(config.clone());
    }

    // Output format for parsing
    args.push("--reporter".to_string());
    args.push("json".to_string());

    args.push(".".to_string());

    Ok(("biome".to_string(), args))
}

// ============================================================================
// Rust Tools
// ============================================================================

fn build_clippy_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec!["clippy".to_string()];

    if check_def.auto_fix {
        args.push("--fix".to_string());
        args.push("--allow-dirty".to_string());
        args.push("--allow-staged".to_string());
    }

    // Message format for parsing
    args.push("--message-format".to_string());
    args.push("json".to_string());

    // Treat warnings as errors if fail_on_warning is set
    if check_def.fail_on_warning {
        args.push("--".to_string());
        args.push("-D".to_string());
        args.push("warnings".to_string());
    }

    Ok(("cargo".to_string(), args))
}

fn build_rustfmt_command(check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec!["fmt".to_string()];

    if !check_def.auto_fix {
        args.push("--check".to_string());
    }

    Ok(("cargo".to_string(), args))
}

fn build_cargo_check_command(_check_def: &CheckDefinition) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec!["check".to_string()];

    // Message format for parsing
    args.push("--message-format".to_string());
    args.push("json".to_string());

    Ok(("cargo".to_string(), args))
}

// ============================================================================
// qontinui-devtools Tools
// ============================================================================

fn build_devtools_command(
    analyzer: &str,
    check_def: &CheckDefinition,
) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let mut args = vec!["run".to_string(), analyzer.to_string()];

    // Add working directory as the target path
    if let Some(ref working_dir) = check_def.working_directory {
        args.push("--path".to_string());
        args.push(working_dir.clone());
    } else {
        args.push("--path".to_string());
        args.push(".".to_string());
    }

    // Add config path if provided
    if let Some(ref config) = check_def.config_path {
        args.push("--config".to_string());
        args.push(config.clone());
    }

    // Output format for parsing
    args.push("--output".to_string());
    args.push("json".to_string());

    Ok(("qontinui-devtools".to_string(), args))
}

/// Get the auto-fix variant of a command
pub fn get_auto_fix_command(tool: &CheckTool, base_args: &[String]) -> Vec<String> {
    let mut args = base_args.to_vec();

    match tool {
        CheckTool::Black => {
            // Remove --check if present
            args.retain(|a| a != "--check");
        }
        CheckTool::Isort => {
            // Remove --check-only if present
            args.retain(|a| a != "--check-only");
        }
        CheckTool::Ruff => {
            // Add --fix if not present
            if !args.contains(&"--fix".to_string()) {
                // Insert after "check"
                if let Some(pos) = args.iter().position(|a| a == "check") {
                    args.insert(pos + 1, "--fix".to_string());
                }
            }
        }
        CheckTool::Eslint => {
            // Add --fix if not present
            if !args.contains(&"--fix".to_string()) {
                args.insert(0, "--fix".to_string());
            }
        }
        CheckTool::Prettier => {
            // Replace --check with --write
            for arg in &mut args {
                if arg == "--check" {
                    *arg = "--write".to_string();
                }
            }
        }
        CheckTool::Clippy => {
            // Add --fix if not present
            if !args.contains(&"--fix".to_string()) {
                // Insert after "clippy"
                if let Some(pos) = args.iter().position(|a| a == "clippy") {
                    args.insert(pos + 1, "--fix".to_string());
                    args.insert(pos + 2, "--allow-dirty".to_string());
                    args.insert(pos + 3, "--allow-staged".to_string());
                }
            }
        }
        CheckTool::Rustfmt => {
            // Remove --check if present
            args.retain(|a| a != "--check");
        }
        CheckTool::Biome => {
            // Add --write if not present
            if !args.contains(&"--write".to_string()) {
                // Insert after "check"
                if let Some(pos) = args.iter().position(|a| a == "check") {
                    args.insert(pos + 1, "--write".to_string());
                }
            }
        }
        // Tools that don't support auto-fix
        CheckTool::Mypy
        | CheckTool::Pyright
        | CheckTool::Tsc
        | CheckTool::CargoCheck
        | CheckTool::Custom
        // qontinui-devtools tools (analysis only, no auto-fix)
        | CheckTool::CircularDeps
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
        | CheckTool::RaceDetectorTs => {}
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_check_def(tool: CheckTool, auto_fix: bool) -> CheckDefinition {
        CheckDefinition {
            id: "test".to_string(),
            name: "Test Check".to_string(),
            check_type: CheckType::Lint,
            tool,
            command: None,
            working_directory: None,
            config_path: None,
            auto_fix,
            fail_on_warning: false,
            timeout_seconds: 60,
            is_critical: false,
        }
    }

    #[test]
    fn test_black_check_mode() {
        let check = make_check_def(CheckTool::Black, false);
        let (program, args) = build_command(&check).unwrap();
        assert_eq!(program, "black");
        assert!(args.contains(&"--check".to_string()));
    }

    #[test]
    fn test_black_fix_mode() {
        let check = make_check_def(CheckTool::Black, true);
        let (program, args) = build_command(&check).unwrap();
        assert_eq!(program, "black");
        assert!(!args.contains(&"--check".to_string()));
    }

    #[test]
    fn test_ruff_check_mode() {
        let check = make_check_def(CheckTool::Ruff, false);
        let (program, args) = build_command(&check).unwrap();
        assert_eq!(program, "ruff");
        assert!(args.contains(&"check".to_string()));
        assert!(!args.contains(&"--fix".to_string()));
    }

    #[test]
    fn test_ruff_fix_mode() {
        let check = make_check_def(CheckTool::Ruff, true);
        let (program, args) = build_command(&check).unwrap();
        assert_eq!(program, "ruff");
        assert!(args.contains(&"--fix".to_string()));
    }

    #[test]
    fn test_custom_command() {
        let mut check = make_check_def(CheckTool::Custom, false);
        check.command = Some("my-linter --strict ./src".to_string());
        let (program, args) = build_command(&check).unwrap();
        assert_eq!(program, "my-linter");
        assert_eq!(args, vec!["--strict", "./src"]);
    }

    #[test]
    fn test_cargo_clippy() {
        let check = make_check_def(CheckTool::Clippy, false);
        let (program, args) = build_command(&check).unwrap();
        assert_eq!(program, "cargo");
        assert!(args.contains(&"clippy".to_string()));
    }
}
