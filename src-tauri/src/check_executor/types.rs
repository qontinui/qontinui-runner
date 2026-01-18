//! Core types for code quality check execution
//!
//! Defines the data structures used across all check executors (linting, formatting, type checking).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Check type enumeration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    /// Linting checks (ruff, eslint, clippy)
    Lint,
    /// Formatting checks (black, prettier, rustfmt)
    Format,
    /// Type checking (mypy, tsc)
    Typecheck,
    /// Code analysis (qontinui-devtools analyzers)
    Analyze,
    /// Security checks (qontinui-devtools security scans)
    Security,
    /// Custom command
    CustomCommand,
}

/// Supported check tools
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckTool {
    // Python
    Black,
    Isort,
    Ruff,
    Mypy,
    Pyright,
    // JavaScript/TypeScript
    Eslint,
    Prettier,
    Tsc,
    Biome,
    // Rust
    Clippy,
    Rustfmt,
    CargoCheck,
    // qontinui-devtools Analysis
    #[serde(rename = "circular_deps")]
    CircularDeps,
    #[serde(rename = "god_class")]
    GodClass,
    Coupling,
    #[serde(rename = "type_coverage_py")]
    TypeCoveragePy,
    #[serde(rename = "type_coverage_ts")]
    TypeCoverageTs,
    #[serde(rename = "srp_analyzer")]
    SrpAnalyzer,
    #[serde(rename = "dead_code_py")]
    DeadCodePy,
    #[serde(rename = "dead_code_ts")]
    DeadCodeTs,
    #[serde(rename = "dead_code_rust")]
    DeadCodeRust,
    // qontinui-devtools Code Quality
    #[serde(rename = "todo_scanner")]
    TodoScanner,
    #[serde(rename = "dead_ui_state")]
    DeadUiState,
    #[serde(rename = "unused_api_params")]
    UnusedApiParams,
    // qontinui-devtools Security
    #[serde(rename = "security_scan")]
    SecurityScan,
    #[serde(rename = "unsafe_rust")]
    UnsafeRust,
    // qontinui-devtools Rust Analysis
    #[serde(rename = "rust_complexity")]
    RustComplexity,
    // qontinui-devtools Concurrency Analysis
    #[serde(rename = "race_detector")]
    RaceDetector,
    #[serde(rename = "race_detector_py")]
    RaceDetectorPy,
    #[serde(rename = "race_detector_rust")]
    RaceDetectorRust,
    #[serde(rename = "race_detector_ts")]
    RaceDetectorTs,
    // qontinui-devtools Meta
    #[serde(rename = "quality_gate")]
    QualityGate,
    // Generic
    Custom,
}

/// Check execution status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Fixed,
    Error,
    Timeout,
}

/// Language/ecosystem categories
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckLanguage {
    Python,
    Javascript,
    Typescript,
    Rust,
    Other,
}

/// Issue severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

/// Individual issue/violation from a check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckIssue {
    /// File path
    pub file: String,
    /// Line number (1-based)
    pub line: Option<u32>,
    /// Column number (1-based)
    pub column: Option<u32>,
    /// End line number
    pub end_line: Option<u32>,
    /// End column number
    pub end_column: Option<u32>,
    /// Rule code (e.g., "E501", "no-unused-vars")
    pub code: Option<String>,
    /// Issue message
    pub message: String,
    /// Severity level
    pub severity: IssueSeverity,
    /// Whether this issue is fixable
    pub fixable: bool,
    /// Whether the fix was applied
    pub fix_applied: Option<bool>,
}

/// Structured output from check tools
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckStructuredOutput {
    /// Issues/violations found
    #[serde(default)]
    pub issues: Vec<CheckIssue>,
    /// Summary statistics
    pub summary: Option<CheckSummary>,
    /// Tool-specific data
    #[serde(default)]
    pub tool_data: HashMap<String, serde_json::Value>,
}

/// Summary statistics for a check
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckSummary {
    pub total_files: u32,
    pub files_with_issues: u32,
    pub total_issues: u32,
    #[serde(default)]
    pub issues_by_severity: HashMap<String, u32>,
}

/// Check definition from the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckDefinition {
    /// Unique check ID (UUID)
    pub id: String,
    /// Check name
    pub name: String,
    /// Check type
    pub check_type: CheckType,
    /// Tool to use
    pub tool: CheckTool,
    /// Custom command override
    pub command: Option<String>,
    /// Working directory
    pub working_directory: Option<String>,
    /// Path to config file (e.g., pyproject.toml)
    pub config_path: Option<String>,
    /// Whether to run in auto-fix mode
    #[serde(default)]
    pub auto_fix: bool,
    /// Whether to fail on warnings
    #[serde(default)]
    pub fail_on_warning: bool,
    /// Timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    /// Whether check failure should fail the entire workflow
    #[serde(default)]
    pub is_critical: bool,
}

fn default_timeout() -> u32 {
    60
}

/// Full check record from database (includes metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// Unique check ID (UUID)
    pub id: String,
    /// Check name
    pub name: String,
    /// Description
    pub description: Option<String>,
    /// Check type
    pub check_type: CheckType,
    /// Tool to use
    pub tool: CheckTool,
    /// Custom command override
    pub command: Option<String>,
    /// Working directory
    pub working_directory: Option<String>,
    /// Path to config file
    pub config_path: Option<String>,
    /// Whether to run in auto-fix mode
    pub auto_fix: bool,
    /// Whether to fail on warnings
    pub fail_on_warning: bool,
    /// Timeout in seconds
    pub timeout_seconds: u32,
    /// Whether check failure should fail the entire workflow
    pub is_critical: bool,
    /// Whether check is enabled
    pub enabled: bool,
    /// Whether AI generated this check
    pub ai_generated: bool,
    /// AI generation prompt (if AI generated)
    pub ai_generation_prompt: Option<String>,
    /// Tags for organization
    #[serde(default)]
    pub tags: Vec<String>,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
}

/// Check execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckExecutionResult {
    /// Check ID
    pub check_id: String,
    /// Check name
    pub check_name: String,
    /// Execution status
    pub status: CheckStatus,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Check output (stdout/stderr)
    pub output: String,
    /// Error message if failed
    pub error: Option<String>,
    /// Number of issues found
    pub issues_found: u32,
    /// Number of issues fixed (if auto-fix)
    pub issues_fixed: u32,
    /// Number of files checked
    pub files_checked: u32,
    /// Structured output with parsed issues
    pub structured_output: Option<CheckStructuredOutput>,
    /// Process exit code
    pub exit_code: Option<i32>,
    /// Timestamp when execution started
    pub started_at: String,
    /// Timestamp when execution completed
    pub completed_at: String,
}

impl CheckExecutionResult {
    /// Create a new result for a passed check
    pub fn passed(check_id: String, check_name: String, duration_ms: u64, output: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            check_id,
            check_name,
            status: CheckStatus::Passed,
            duration_ms,
            output,
            error: None,
            issues_found: 0,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: None,
            exit_code: Some(0),
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for a failed check
    pub fn failed(
        check_id: String,
        check_name: String,
        duration_ms: u64,
        output: String,
        issues_found: u32,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            check_id,
            check_name,
            status: CheckStatus::Failed,
            duration_ms,
            output,
            error: None,
            issues_found,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: None,
            exit_code: Some(1),
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for a fixed check (auto-fix applied)
    pub fn fixed(
        check_id: String,
        check_name: String,
        duration_ms: u64,
        output: String,
        issues_fixed: u32,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            check_id,
            check_name,
            status: CheckStatus::Fixed,
            duration_ms,
            output,
            error: None,
            issues_found: issues_fixed,
            issues_fixed,
            files_checked: 0,
            structured_output: None,
            exit_code: Some(0),
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for an errored check
    pub fn error(check_id: String, check_name: String, error: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            check_id,
            check_name,
            status: CheckStatus::Error,
            duration_ms: 0,
            output: String::new(),
            error: Some(error),
            issues_found: 0,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: None,
            exit_code: None,
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Create a new result for a timed out check
    pub fn timeout(check_id: String, check_name: String, timeout_seconds: u32) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            check_id,
            check_name,
            status: CheckStatus::Timeout,
            duration_ms: (timeout_seconds * 1000) as u64,
            output: String::new(),
            error: Some(format!("Check timed out after {} seconds", timeout_seconds)),
            issues_found: 0,
            issues_fixed: 0,
            files_checked: 0,
            structured_output: None,
            exit_code: None,
            started_at: now.clone(),
            completed_at: now,
        }
    }

    /// Check if the result indicates success
    pub fn is_success(&self) -> bool {
        matches!(self.status, CheckStatus::Passed | CheckStatus::Fixed)
    }
}

/// Summary statistics for a check suite execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckSuiteSummary {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub fixed: u32,
    pub error: u32,
    pub timeout: u32,
    pub total_issues_found: u32,
    pub total_issues_fixed: u32,
    pub duration_ms: u64,
}

impl CheckSuiteSummary {
    /// Calculate pass rate as percentage
    pub fn pass_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            ((self.passed + self.fixed) as f64 / self.total as f64) * 100.0
        }
    }

    /// Check if all checks passed or were fixed
    pub fn all_passed(&self) -> bool {
        self.failed == 0 && self.error == 0 && self.timeout == 0
    }

    /// Update summary from a check result
    pub fn add_result(&mut self, result: &CheckExecutionResult) {
        self.total += 1;
        self.duration_ms += result.duration_ms;
        self.total_issues_found += result.issues_found;
        self.total_issues_fixed += result.issues_fixed;
        match result.status {
            CheckStatus::Passed => self.passed += 1,
            CheckStatus::Failed => self.failed += 1,
            CheckStatus::Fixed => self.fixed += 1,
            CheckStatus::Error => self.error += 1,
            CheckStatus::Timeout => self.timeout += 1,
            CheckStatus::Pending | CheckStatus::Running => {}
        }
    }
}

/// Input for creating a new check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCheckInput {
    pub name: String,
    pub description: Option<String>,
    pub check_type: CheckType,
    pub tool: CheckTool,
    pub command: Option<String>,
    pub working_directory: Option<String>,
    pub config_path: Option<String>,
    #[serde(default)]
    pub auto_fix: bool,
    #[serde(default)]
    pub fail_on_warning: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u32,
    #[serde(default)]
    pub is_critical: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub ai_generated: bool,
    pub ai_generation_prompt: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

/// Input for updating an existing check
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCheckInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub check_type: Option<CheckType>,
    pub tool: Option<CheckTool>,
    pub command: Option<String>,
    pub working_directory: Option<String>,
    pub config_path: Option<String>,
    pub auto_fix: Option<bool>,
    pub fail_on_warning: Option<bool>,
    pub timeout_seconds: Option<u32>,
    pub is_critical: Option<bool>,
    pub enabled: Option<bool>,
    pub tags: Option<Vec<String>>,
}

/// Check result stored in database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub check_id: String,
    pub task_run_id: Option<String>,
    pub status: CheckStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub output: Option<String>,
    pub error_message: Option<String>,
    pub issues_found: u32,
    pub issues_fixed: u32,
    pub files_checked: u32,
    pub structured_output: Option<CheckStructuredOutput>,
    pub created_at: String,
}

/// Tool metadata for UI and detection (static version for constants)
#[derive(Debug, Clone)]
pub struct CheckToolInfo {
    pub tool: CheckTool,
    pub name: &'static str,
    pub description: &'static str,
    pub check_type: CheckType,
    pub language: CheckLanguage,
    pub supports_auto_fix: bool,
    pub default_command: &'static str,
    pub config_files: &'static [&'static str],
    pub install_command: Option<&'static str>,
}

/// Serializable version of CheckToolInfo for API responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckToolInfoSerialized {
    pub tool: CheckTool,
    pub name: String,
    pub description: String,
    pub check_type: CheckType,
    pub language: CheckLanguage,
    pub supports_auto_fix: bool,
    pub default_command: String,
    pub config_files: Vec<String>,
    pub install_command: Option<String>,
}

impl From<&CheckToolInfo> for CheckToolInfoSerialized {
    fn from(info: &CheckToolInfo) -> Self {
        CheckToolInfoSerialized {
            tool: info.tool.clone(),
            name: info.name.to_string(),
            description: info.description.to_string(),
            check_type: info.check_type.clone(),
            language: info.language.clone(),
            supports_auto_fix: info.supports_auto_fix,
            default_command: info.default_command.to_string(),
            config_files: info.config_files.iter().map(|s| s.to_string()).collect(),
            install_command: info.install_command.map(|s| s.to_string()),
        }
    }
}

/// Project detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDetectionResult {
    pub detected_languages: Vec<CheckLanguage>,
    pub detected_tools: Vec<CheckTool>,
    pub config_files: Vec<String>,
    pub suggested_checks: Vec<SuggestedCheck>,
}

/// Suggested check from project detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedCheck {
    pub tool: CheckTool,
    pub name: String,
    pub description: String,
    pub command: String,
    pub config_path: Option<String>,
    pub auto_fix: bool,
    pub reason: String,
}

// ============================================================================
// Tool Configuration Constants
// ============================================================================

/// Tool metadata for UI and detection
pub const CHECK_TOOLS: &[CheckToolInfo] = &[
    // Python - Formatting
    CheckToolInfo {
        tool: CheckTool::Black,
        name: "Black",
        description: "The uncompromising Python code formatter",
        check_type: CheckType::Format,
        language: CheckLanguage::Python,
        supports_auto_fix: true,
        default_command: "black --check .",
        config_files: &["pyproject.toml", ".black.toml"],
        install_command: Some("pip install black"),
    },
    CheckToolInfo {
        tool: CheckTool::Isort,
        name: "isort",
        description: "Python import sorter",
        check_type: CheckType::Format,
        language: CheckLanguage::Python,
        supports_auto_fix: true,
        default_command: "isort --check-only .",
        config_files: &["pyproject.toml", ".isort.cfg", "setup.cfg"],
        install_command: Some("pip install isort"),
    },
    // Python - Linting
    CheckToolInfo {
        tool: CheckTool::Ruff,
        name: "Ruff",
        description: "An extremely fast Python linter and formatter",
        check_type: CheckType::Lint,
        language: CheckLanguage::Python,
        supports_auto_fix: true,
        default_command: "ruff check .",
        config_files: &["pyproject.toml", "ruff.toml", ".ruff.toml"],
        install_command: Some("pip install ruff"),
    },
    // Python - Type Checking
    CheckToolInfo {
        tool: CheckTool::Mypy,
        name: "mypy",
        description: "Static type checker for Python",
        check_type: CheckType::Typecheck,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "mypy .",
        config_files: &["pyproject.toml", "mypy.ini", ".mypy.ini", "setup.cfg"],
        install_command: Some("pip install mypy"),
    },
    CheckToolInfo {
        tool: CheckTool::Pyright,
        name: "Pyright",
        description: "Fast type checker for Python",
        check_type: CheckType::Typecheck,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "pyright",
        config_files: &["pyrightconfig.json", "pyproject.toml"],
        install_command: Some("pip install pyright"),
    },
    // JavaScript/TypeScript - Linting
    CheckToolInfo {
        tool: CheckTool::Eslint,
        name: "ESLint",
        description: "Pluggable JavaScript/TypeScript linter",
        check_type: CheckType::Lint,
        language: CheckLanguage::Javascript,
        supports_auto_fix: true,
        default_command: "eslint .",
        config_files: &[
            ".eslintrc",
            ".eslintrc.js",
            ".eslintrc.json",
            "eslint.config.js",
        ],
        install_command: Some("npm install eslint"),
    },
    // JavaScript/TypeScript - Formatting
    CheckToolInfo {
        tool: CheckTool::Prettier,
        name: "Prettier",
        description: "Opinionated code formatter",
        check_type: CheckType::Format,
        language: CheckLanguage::Javascript,
        supports_auto_fix: true,
        default_command: "prettier --check .",
        config_files: &[
            ".prettierrc",
            ".prettierrc.js",
            ".prettierrc.json",
            "prettier.config.js",
        ],
        install_command: Some("npm install prettier"),
    },
    // JavaScript/TypeScript - Type Checking
    CheckToolInfo {
        tool: CheckTool::Tsc,
        name: "TypeScript Compiler",
        description: "TypeScript type checker",
        check_type: CheckType::Typecheck,
        language: CheckLanguage::Typescript,
        supports_auto_fix: false,
        default_command: "tsc --noEmit",
        config_files: &["tsconfig.json"],
        install_command: Some("npm install typescript"),
    },
    // JavaScript/TypeScript - All-in-one
    CheckToolInfo {
        tool: CheckTool::Biome,
        name: "Biome",
        description: "Fast formatter and linter for JavaScript/TypeScript",
        check_type: CheckType::Lint,
        language: CheckLanguage::Javascript,
        supports_auto_fix: true,
        default_command: "biome check .",
        config_files: &["biome.json", "biome.jsonc"],
        install_command: Some("npm install @biomejs/biome"),
    },
    // Rust - Linting
    CheckToolInfo {
        tool: CheckTool::Clippy,
        name: "Clippy",
        description: "Rust linter",
        check_type: CheckType::Lint,
        language: CheckLanguage::Rust,
        supports_auto_fix: true,
        default_command: "cargo clippy -- -D warnings",
        config_files: &["Cargo.toml", "clippy.toml", ".clippy.toml"],
        install_command: Some("rustup component add clippy"),
    },
    // Rust - Formatting
    CheckToolInfo {
        tool: CheckTool::Rustfmt,
        name: "rustfmt",
        description: "Rust code formatter",
        check_type: CheckType::Format,
        language: CheckLanguage::Rust,
        supports_auto_fix: true,
        default_command: "cargo fmt --check",
        config_files: &["Cargo.toml", "rustfmt.toml", ".rustfmt.toml"],
        install_command: Some("rustup component add rustfmt"),
    },
    // Rust - Type/Build Checking
    CheckToolInfo {
        tool: CheckTool::CargoCheck,
        name: "Cargo Check",
        description: "Check Rust code for errors without building",
        check_type: CheckType::Typecheck,
        language: CheckLanguage::Rust,
        supports_auto_fix: false,
        default_command: "cargo check",
        config_files: &["Cargo.toml"],
        install_command: None,
    },
    // === Native Analysis Tools (built-in) ===
    CheckToolInfo {
        tool: CheckTool::CircularDeps,
        name: "Circular Dependencies",
        description: "Detect circular import dependencies that can cause deadlocks",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::GodClass,
        name: "God Class Detector",
        description: "Find classes that are too large or have too many responsibilities",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["pyproject.toml"],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::Coupling,
        name: "Coupling Analyzer",
        description: "Analyze module coupling and cohesion metrics",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::TypeCoveragePy,
        name: "Python Type Coverage",
        description: "Measure type hint coverage in Python code",
        check_type: CheckType::Typecheck,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["pyproject.toml"],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::TypeCoverageTs,
        name: "TypeScript Type Coverage",
        description: "Measure type coverage in TypeScript code",
        check_type: CheckType::Typecheck,
        language: CheckLanguage::Typescript,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["tsconfig.json"],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::SrpAnalyzer,
        name: "SRP Analyzer",
        description: "Detect Single Responsibility Principle violations",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::DeadCodePy,
        name: "Python Dead Code",
        description: "Detect unused code in Python projects",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::DeadCodeTs,
        name: "TypeScript Dead Code",
        description: "Detect unused code in TypeScript projects",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Typescript,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["tsconfig.json"],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::DeadCodeRust,
        name: "Rust Dead Code",
        description: "Detect unused code in Rust projects",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Rust,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["Cargo.toml"],
        install_command: None,
    },
    // === Native Code Quality Tools (built-in) ===
    CheckToolInfo {
        tool: CheckTool::TodoScanner,
        name: "TODO Scanner",
        description: "Find TODO, FIXME, HACK, XXX comments and technical debt markers",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Other,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::DeadUiState,
        name: "Dead UI State",
        description: "Find React useState hooks not connected to effects or API calls",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Typescript,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["tsconfig.json"],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::UnusedApiParams,
        name: "Unused API Params",
        description: "Find API endpoint parameters declared but never used in handlers",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Other,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    // === Native Security Tools (built-in) ===
    CheckToolInfo {
        tool: CheckTool::SecurityScan,
        name: "Security Scanner",
        description: "Detect security vulnerabilities (SQL injection, secrets, etc.)",
        check_type: CheckType::Security,
        language: CheckLanguage::Other,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::UnsafeRust,
        name: "Rust Unsafe Analyzer",
        description: "Track and audit unsafe code blocks in Rust",
        check_type: CheckType::Security,
        language: CheckLanguage::Rust,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["Cargo.toml"],
        install_command: None,
    },
    // === Native Rust Analysis (built-in) ===
    CheckToolInfo {
        tool: CheckTool::RustComplexity,
        name: "Rust Complexity",
        description: "Analyze cyclomatic complexity of Rust functions",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Rust,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["Cargo.toml"],
        install_command: None,
    },
    // === Native Concurrency Analysis (built-in) ===
    CheckToolInfo {
        tool: CheckTool::RaceDetector,
        name: "Race Condition Detector",
        description: "Detect potential race conditions across Python, Rust, and TypeScript",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Other,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::RaceDetectorPy,
        name: "Python Race Detector",
        description: "Detect potential race conditions in Python code (threading, asyncio)",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Python,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::RaceDetectorRust,
        name: "Rust Race Detector",
        description: "Detect potential race conditions in Rust code (static mut, Arc<Mutex>, etc.)",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Rust,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["Cargo.toml"],
        install_command: None,
    },
    CheckToolInfo {
        tool: CheckTool::RaceDetectorTs,
        name: "TypeScript Race Detector",
        description: "Detect potential race conditions in TypeScript (async/await, shared state)",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Typescript,
        supports_auto_fix: false,
        default_command: "",
        config_files: &["tsconfig.json"],
        install_command: None,
    },
    // === Meta Tools (built-in) ===
    CheckToolInfo {
        tool: CheckTool::QualityGate,
        name: "Quality Gates",
        description: "Composite check enforcing multiple quality thresholds",
        check_type: CheckType::Analyze,
        language: CheckLanguage::Other,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
    // Custom
    CheckToolInfo {
        tool: CheckTool::Custom,
        name: "Custom Command",
        description: "Run any custom check command",
        check_type: CheckType::CustomCommand,
        language: CheckLanguage::Other,
        supports_auto_fix: false,
        default_command: "",
        config_files: &[],
        install_command: None,
    },
];

/// Check type metadata for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckTypeInfo {
    pub check_type: CheckType,
    pub name: &'static str,
    pub description: &'static str,
    pub icon: &'static str,
    pub color: &'static str,
}

pub const CHECK_TYPE_INFO: &[CheckTypeInfo] = &[
    CheckTypeInfo {
        check_type: CheckType::Lint,
        name: "Lint",
        description: "Code quality and style checks",
        icon: "AlertTriangle",
        color: "amber",
    },
    CheckTypeInfo {
        check_type: CheckType::Format,
        name: "Format",
        description: "Code formatting checks",
        icon: "AlignLeft",
        color: "blue",
    },
    CheckTypeInfo {
        check_type: CheckType::Typecheck,
        name: "Type Check",
        description: "Static type analysis",
        icon: "FileType",
        color: "purple",
    },
    CheckTypeInfo {
        check_type: CheckType::Analyze,
        name: "Analyze",
        description: "Code analysis and metrics",
        icon: "Search",
        color: "cyan",
    },
    CheckTypeInfo {
        check_type: CheckType::Security,
        name: "Security",
        description: "Security vulnerability checks",
        icon: "Shield",
        color: "red",
    },
    CheckTypeInfo {
        check_type: CheckType::CustomCommand,
        name: "Custom",
        description: "Custom check command",
        icon: "Terminal",
        color: "gray",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_execution_result_passed() {
        let result = CheckExecutionResult::passed(
            "check-1".to_string(),
            "Ruff Linting".to_string(),
            1000,
            "All checks passed".to_string(),
        );
        assert_eq!(result.status, CheckStatus::Passed);
        assert!(result.error.is_none());
        assert!(result.is_success());
    }

    #[test]
    fn test_check_execution_result_fixed() {
        let result = CheckExecutionResult::fixed(
            "check-1".to_string(),
            "Black Formatting".to_string(),
            500,
            "Reformatted 5 files".to_string(),
            5,
        );
        assert_eq!(result.status, CheckStatus::Fixed);
        assert_eq!(result.issues_fixed, 5);
        assert!(result.is_success());
    }

    #[test]
    fn test_check_execution_result_failed() {
        let result = CheckExecutionResult::failed(
            "check-1".to_string(),
            "Mypy Type Check".to_string(),
            2000,
            "Found 10 type errors".to_string(),
            10,
        );
        assert_eq!(result.status, CheckStatus::Failed);
        assert_eq!(result.issues_found, 10);
        assert!(!result.is_success());
    }

    #[test]
    fn test_check_suite_summary() {
        let mut summary = CheckSuiteSummary::default();

        let passed = CheckExecutionResult::passed(
            "c1".to_string(),
            "Check 1".to_string(),
            100,
            "".to_string(),
        );
        summary.add_result(&passed);

        let fixed = CheckExecutionResult::fixed(
            "c2".to_string(),
            "Check 2".to_string(),
            200,
            "".to_string(),
            3,
        );
        summary.add_result(&fixed);

        let failed = CheckExecutionResult::failed(
            "c3".to_string(),
            "Check 3".to_string(),
            300,
            "".to_string(),
            5,
        );
        summary.add_result(&failed);

        assert_eq!(summary.total, 3);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.fixed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.total_issues_found, 8);
        assert_eq!(summary.total_issues_fixed, 3);
        assert_eq!(summary.duration_ms, 600);
        // 2 successful out of 3 = 66.67%
        assert!((summary.pass_rate() - 66.67).abs() < 0.1);
        assert!(!summary.all_passed());
    }
}
