/**
 * Check Builder Types
 *
 * TypeScript types for the code quality check system (linting, formatting, type checking).
 */

// Check types supported by the system
export type CheckType = "lint" | "format" | "typecheck" | "security" | "analyze" | "custom_command";

// Supported check tools
export type CheckTool =
  // Python
  | "black"
  | "isort"
  | "ruff"
  | "mypy"
  | "pyright"
  // JavaScript/TypeScript
  | "eslint"
  | "prettier"
  | "tsc"
  | "biome"
  // Rust
  | "clippy"
  | "rustfmt"
  | "cargo_check"
  // qontinui-devtools Analysis
  | "circular_deps"
  | "god_class"
  | "coupling"
  | "type_coverage_py"
  | "type_coverage_ts"
  | "srp_analyzer"
  | "dead_code_py"
  | "dead_code_ts"
  | "dead_code_rust"
  // qontinui-devtools Code Quality
  | "todo_scanner"
  | "dead_ui_state"
  | "unused_api_params"
  // qontinui-devtools Security
  | "security_scan"
  | "unsafe_rust"
  // qontinui-devtools Rust Analysis
  | "rust_complexity"
  // qontinui-devtools Meta
  | "quality_gate"
  // Generic
  | "custom";

// Check status for execution results
export type CheckStatus =
  | "pending"
  | "running"
  | "passed"
  | "failed"
  | "fixed"
  | "error"
  | "timeout";

// Language/ecosystem categories
export type CheckLanguage = "python" | "javascript" | "typescript" | "rust" | "other";

// Check definition stored in database
export interface Check {
  id: string;
  name: string;
  description?: string;
  check_type: CheckType;
  tool: CheckTool;
  command?: string; // Custom command override
  working_directory?: string;
  config_path?: string; // e.g., pyproject.toml, .eslintrc
  auto_fix: boolean; // Run with --fix flag
  fail_on_warning: boolean;
  timeout_seconds: number;
  is_critical: boolean;
  enabled: boolean;
  ai_generated: boolean;
  ai_generation_prompt?: string;
  tags: string[];
  created_at: string;
  updated_at: string;
}

// Check definition for execution (executor format)
export interface CheckDefinition {
  id: string;
  name: string;
  check_type: CheckType;
  tool: CheckTool;
  command?: string;
  working_directory?: string;
  config_path?: string;
  auto_fix: boolean;
  fail_on_warning: boolean;
  timeout_seconds: number;
  is_critical: boolean;
}

// Check execution result
export interface CheckExecutionResult {
  check_id: string;
  check_name: string;
  status: CheckStatus;
  duration_ms: number;
  output: string;
  error?: string;
  issues_found: number;
  issues_fixed: number;
  files_checked: number;
  structured_output?: CheckStructuredOutput;
}

// Structured output from check tools
export interface CheckStructuredOutput {
  // Issues/violations found
  issues?: CheckIssue[];
  // Summary statistics
  summary?: {
    total_files: number;
    files_with_issues: number;
    total_issues: number;
    issues_by_severity: Record<string, number>;
  };
  // Tool-specific data
  tool_data?: Record<string, unknown>;
}

// Individual issue/violation from a check
export interface CheckIssue {
  file: string;
  line?: number;
  column?: number;
  end_line?: number;
  end_column?: number;
  code?: string; // Rule code (e.g., "E501", "no-unused-vars")
  message: string;
  severity: "error" | "warning" | "info" | "hint";
  fixable: boolean;
  fix_applied?: boolean;
}

// Check result stored in database
export interface CheckResult {
  id: string;
  check_id: string;
  task_run_id?: string;
  status: CheckStatus;
  started_at?: string;
  completed_at?: string;
  duration_ms?: number;
  output?: string;
  error_message?: string;
  issues_found: number;
  issues_fixed: number;
  files_checked: number;
  structured_output?: CheckStructuredOutput;
  created_at: string;
}

// Input for creating a new check
export interface CreateCheckInput {
  name: string;
  description?: string;
  check_type: CheckType;
  tool: CheckTool;
  command?: string;
  working_directory?: string;
  config_path?: string;
  auto_fix?: boolean;
  fail_on_warning?: boolean;
  timeout_seconds?: number;
  is_critical?: boolean;
  enabled?: boolean;
  ai_generated?: boolean;
  ai_generation_prompt?: string;
  tags?: string[];
}

// Input for updating an existing check
export interface UpdateCheckInput {
  name?: string;
  description?: string;
  check_type?: CheckType;
  tool?: CheckTool;
  command?: string;
  working_directory?: string;
  config_path?: string;
  auto_fix?: boolean;
  fail_on_warning?: boolean;
  timeout_seconds?: number;
  is_critical?: boolean;
  enabled?: boolean;
  tags?: string[];
}

// Command response from Tauri
export interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}

// Tool metadata for UI display
export interface CheckToolInfo {
  tool: CheckTool;
  name: string;
  description: string;
  check_type: CheckType;
  language: CheckLanguage;
  supports_auto_fix: boolean;
  default_command: string;
  config_files: string[]; // Config files this tool looks for
  install_command?: string; // How to install this tool
}

// Check type metadata for UI display
export interface CheckTypeInfo {
  type: CheckType;
  name: string;
  description: string;
  icon: string;
  color: string;
}

// Project detection result
export interface ProjectDetectionResult {
  detected_languages: CheckLanguage[];
  detected_tools: CheckTool[];
  config_files: string[];
  suggested_checks: SuggestedCheck[];
}

// Suggested check from project detection
export interface SuggestedCheck {
  tool: CheckTool;
  name: string;
  description: string;
  command: string;
  config_path?: string;
  auto_fix: boolean;
  reason: string; // Why this check is suggested
}

// AI check generation input
export interface AiCheckGenerationInput {
  prompt: string;
  working_directory?: string;
  detected_project?: ProjectDetectionResult;
}

// AI check generation result
export interface AiCheckGenerationResult {
  checks: CreateCheckInput[];
  explanation: string;
}

// ============================================================================
// Check Group Types
// ============================================================================

// Check group stored in database
export interface CheckGroup {
  id: string;
  name: string;
  description?: string;
  color?: string;
  enabled: boolean;
  run_in_parallel: boolean;
  stop_on_failure: boolean;
  tags: string[];
  created_at: string;
  updated_at: string;
}

// Input for creating a new check group
export interface CreateCheckGroupInput {
  name: string;
  description?: string;
  color?: string;
  enabled?: boolean;
  run_in_parallel?: boolean;
  stop_on_failure?: boolean;
  tags?: string[];
  /** Check IDs to add to the group during creation */
  check_ids?: string[];
}

// Input for updating an existing check group
export interface UpdateCheckGroupInput {
  name?: string;
  description?: string;
  color?: string;
  enabled?: boolean;
  run_in_parallel?: boolean;
  stop_on_failure?: boolean;
  tags?: string[];
}

// Check group with its member checks (for display)
export interface CheckGroupWithChecks extends CheckGroup {
  checks: Check[];
}

// Check group execution result
export interface CheckGroupExecutionResult {
  group: CheckGroup;
  results: CheckExecutionResult[];
  summary: {
    total: number;
    passed: number;
    failed: number;
    fixed: number;
    skipped: number;
    duration_ms: number;
  };
}

// ============================================================================
// Tool Configuration Constants
// ============================================================================

export const CHECK_TOOLS: CheckToolInfo[] = [
  // Python - Formatting
  {
    tool: "black",
    name: "Black",
    description: "The uncompromising Python code formatter",
    check_type: "format",
    language: "python",
    supports_auto_fix: true,
    default_command: "black --check .",
    config_files: ["pyproject.toml", ".black.toml"],
    install_command: "pip install black",
  },
  {
    tool: "isort",
    name: "isort",
    description: "Python import sorter",
    check_type: "format",
    language: "python",
    supports_auto_fix: true,
    default_command: "isort --check-only .",
    config_files: ["pyproject.toml", ".isort.cfg", "setup.cfg"],
    install_command: "pip install isort",
  },
  // Python - Linting
  {
    tool: "ruff",
    name: "Ruff",
    description: "An extremely fast Python linter and formatter",
    check_type: "lint",
    language: "python",
    supports_auto_fix: true,
    default_command: "ruff check .",
    config_files: ["pyproject.toml", "ruff.toml", ".ruff.toml"],
    install_command: "pip install ruff",
  },
  // Python - Type Checking
  {
    tool: "mypy",
    name: "mypy",
    description: "Static type checker for Python",
    check_type: "typecheck",
    language: "python",
    supports_auto_fix: false,
    default_command: "mypy .",
    config_files: ["pyproject.toml", "mypy.ini", ".mypy.ini", "setup.cfg"],
    install_command: "pip install mypy",
  },
  {
    tool: "pyright",
    name: "Pyright",
    description: "Fast type checker for Python",
    check_type: "typecheck",
    language: "python",
    supports_auto_fix: false,
    default_command: "pyright",
    config_files: ["pyrightconfig.json", "pyproject.toml"],
    install_command: "pip install pyright",
  },
  // JavaScript/TypeScript - Linting
  {
    tool: "eslint",
    name: "ESLint",
    description: "Pluggable JavaScript/TypeScript linter",
    check_type: "lint",
    language: "javascript",
    supports_auto_fix: true,
    default_command: "eslint .",
    config_files: [".eslintrc", ".eslintrc.js", ".eslintrc.json", "eslint.config.js"],
    install_command: "npm install eslint",
  },
  // JavaScript/TypeScript - Formatting
  {
    tool: "prettier",
    name: "Prettier",
    description: "Opinionated code formatter",
    check_type: "format",
    language: "javascript",
    supports_auto_fix: true,
    default_command: "prettier --check .",
    config_files: [".prettierrc", ".prettierrc.js", ".prettierrc.json", "prettier.config.js"],
    install_command: "npm install prettier",
  },
  // JavaScript/TypeScript - Type Checking
  {
    tool: "tsc",
    name: "TypeScript Compiler",
    description: "TypeScript type checker",
    check_type: "typecheck",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "tsc --noEmit",
    config_files: ["tsconfig.json"],
    install_command: "npm install typescript",
  },
  // JavaScript/TypeScript - All-in-one
  {
    tool: "biome",
    name: "Biome",
    description: "Fast formatter and linter for JavaScript/TypeScript",
    check_type: "lint",
    language: "javascript",
    supports_auto_fix: true,
    default_command: "biome check .",
    config_files: ["biome.json", "biome.jsonc"],
    install_command: "npm install @biomejs/biome",
  },
  // Rust - Linting
  {
    tool: "clippy",
    name: "Clippy",
    description: "Rust linter",
    check_type: "lint",
    language: "rust",
    supports_auto_fix: true,
    default_command: "cargo clippy -- -D warnings",
    config_files: ["Cargo.toml", "clippy.toml", ".clippy.toml"],
    install_command: "rustup component add clippy",
  },
  // Rust - Formatting
  {
    tool: "rustfmt",
    name: "rustfmt",
    description: "Rust code formatter",
    check_type: "format",
    language: "rust",
    supports_auto_fix: true,
    default_command: "cargo fmt --check",
    config_files: ["Cargo.toml", "rustfmt.toml", ".rustfmt.toml"],
    install_command: "rustup component add rustfmt",
  },
  // Rust - Type/Build Checking
  {
    tool: "cargo_check",
    name: "Cargo Check",
    description: "Check Rust code for errors without building",
    check_type: "typecheck",
    language: "rust",
    supports_auto_fix: false,
    default_command: "cargo check",
    config_files: ["Cargo.toml"],
  },
  // === NATIVE ANALYSIS TOOLS (built-in) ===
  {
    tool: "circular_deps",
    name: "Circular Dependencies",
    description: "Detect circular import dependencies that can cause deadlocks (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  {
    tool: "god_class",
    name: "God Class Detector",
    description: "Find classes that are too large or have too many responsibilities (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["pyproject.toml"],
  },
  {
    tool: "coupling",
    name: "Coupling Analyzer",
    description: "Analyze module coupling and cohesion metrics (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  {
    tool: "type_coverage_py",
    name: "Python Type Coverage",
    description: "Measure type hint coverage in Python code (built-in)",
    check_type: "typecheck",
    language: "python",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["pyproject.toml"],
  },
  {
    tool: "type_coverage_ts",
    name: "TypeScript Type Coverage",
    description: "Measure type coverage in TypeScript code (built-in)",
    check_type: "typecheck",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["tsconfig.json"],
  },
  {
    tool: "srp_analyzer",
    name: "SRP Analyzer",
    description: "Detect Single Responsibility Principle violations (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  {
    tool: "dead_code_py",
    name: "Python Dead Code",
    description: "Detect unused code in Python projects (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  {
    tool: "dead_code_ts",
    name: "TypeScript Dead Code",
    description: "Detect unused code in TypeScript projects (built-in)",
    check_type: "analyze",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["tsconfig.json"],
  },
  {
    tool: "dead_code_rust",
    name: "Rust Dead Code",
    description: "Detect unused code in Rust projects (built-in)",
    check_type: "analyze",
    language: "rust",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["Cargo.toml"],
  },
  // === NATIVE CODE QUALITY TOOLS (built-in) ===
  {
    tool: "todo_scanner",
    name: "TODO Scanner",
    description: "Find TODO, FIXME, HACK, XXX comments and technical debt markers (built-in)",
    check_type: "analyze",
    language: "other",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  {
    tool: "dead_ui_state",
    name: "Dead UI State",
    description: "Find React useState hooks not connected to effects or API calls (built-in)",
    check_type: "analyze",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["tsconfig.json"],
  },
  {
    tool: "unused_api_params",
    name: "Unused API Params",
    description: "Find API endpoint parameters declared but never used in handlers (built-in)",
    check_type: "analyze",
    language: "other",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  // === NATIVE SECURITY TOOLS (built-in) ===
  {
    tool: "security_scan",
    name: "Security Scanner",
    description: "Detect security vulnerabilities (SQL injection, secrets, etc.) (built-in)",
    check_type: "security",
    language: "other", // Scans all languages
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  {
    tool: "unsafe_rust",
    name: "Rust Unsafe Analyzer",
    description: "Track and audit unsafe code blocks in Rust (built-in)",
    check_type: "security",
    language: "rust",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["Cargo.toml"],
  },
  // === NATIVE RUST ANALYSIS (built-in) ===
  {
    tool: "rust_complexity",
    name: "Rust Complexity",
    description: "Analyze cyclomatic complexity of Rust functions (built-in)",
    check_type: "analyze",
    language: "rust",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: ["Cargo.toml"],
  },
  // === META TOOLS (built-in) ===
  {
    tool: "quality_gate",
    name: "Quality Gates",
    description: "Composite check enforcing multiple quality thresholds (built-in)",
    check_type: "analyze",
    language: "other",
    supports_auto_fix: false,
    default_command: "", // Native analyzer - no external command needed
    config_files: [],
  },
  // Custom
  {
    tool: "custom",
    name: "Custom Command",
    description: "Run any custom check command",
    check_type: "custom_command",
    language: "other",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
];

export const CHECK_TYPE_INFO: CheckTypeInfo[] = [
  {
    type: "lint",
    name: "Lint",
    description: "Code quality and style checks",
    icon: "AlertTriangle",
    color: "amber",
  },
  {
    type: "format",
    name: "Format",
    description: "Code formatting checks",
    icon: "AlignLeft",
    color: "blue",
  },
  {
    type: "typecheck",
    name: "Type Check",
    description: "Static type analysis",
    icon: "FileType",
    color: "purple",
  },
  {
    type: "security",
    name: "Security",
    description: "Security vulnerability scanning",
    icon: "Shield",
    color: "red",
  },
  {
    type: "analyze",
    name: "Analyze",
    description: "Architecture and code analysis",
    icon: "Search",
    color: "indigo",
  },
  {
    type: "custom_command",
    name: "Custom",
    description: "Custom check command",
    icon: "Terminal",
    color: "gray",
  },
];

// Helper to get tool info
export function getToolInfo(tool: CheckTool): CheckToolInfo | undefined {
  return CHECK_TOOLS.find((t) => t.tool === tool);
}

// Helper to get check type info
export function getCheckTypeInfo(type: CheckType): CheckTypeInfo | undefined {
  return CHECK_TYPE_INFO.find((t) => t.type === type);
}

// Helper to get tools by language
export function getToolsByLanguage(language: CheckLanguage): CheckToolInfo[] {
  return CHECK_TOOLS.filter((t) => t.language === language);
}

// Helper to get tools by check type
export function getToolsByCheckType(checkType: CheckType): CheckToolInfo[] {
  return CHECK_TOOLS.filter((t) => t.check_type === checkType);
}

// Helper to get auto-fix command for a tool
export function getAutoFixCommand(tool: CheckTool, baseCommand?: string): string {
  const toolInfo = getToolInfo(tool);
  if (!toolInfo || !toolInfo.supports_auto_fix) {
    return baseCommand || toolInfo?.default_command || "";
  }

  const cmd = baseCommand || toolInfo.default_command;

  switch (tool) {
    case "black":
      return cmd.replace("--check ", "");
    case "isort":
      return cmd.replace("--check-only ", "");
    case "ruff":
      return cmd.replace("check", "check --fix");
    case "eslint":
      return `${cmd} --fix`;
    case "prettier":
      return cmd.replace("--check", "--write");
    case "clippy":
      return cmd.replace("clippy", "clippy --fix");
    case "rustfmt":
      return cmd.replace("--check", "");
    case "biome":
      return cmd.replace("check", "check --write");
    default:
      return cmd;
  }
}

// Helper to determine if a status indicates success
export function isSuccessStatus(status: CheckStatus): boolean {
  return status === "passed" || status === "fixed";
}

// Helper to get status display info
export function getStatusInfo(status: CheckStatus): { label: string; color: string; icon: string } {
  switch (status) {
    case "pending":
      return { label: "Pending", color: "gray", icon: "Clock" };
    case "running":
      return { label: "Running", color: "blue", icon: "Loader2" };
    case "passed":
      return { label: "Passed", color: "green", icon: "CheckCircle" };
    case "failed":
      return { label: "Failed", color: "red", icon: "XCircle" };
    case "fixed":
      return { label: "Fixed", color: "emerald", icon: "Wrench" };
    case "error":
      return { label: "Error", color: "red", icon: "AlertCircle" };
    case "timeout":
      return { label: "Timeout", color: "amber", icon: "Clock" };
  }
}
