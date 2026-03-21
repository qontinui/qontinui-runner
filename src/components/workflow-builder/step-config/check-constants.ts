import type { CheckType } from "../../../types/unified-workflow";

export type CheckLanguage = "python" | "javascript" | "typescript" | "rust" | "other";

export type CheckTool =
  | "black"
  | "isort"
  | "ruff"
  | "mypy"
  | "pyright"
  | "eslint"
  | "prettier"
  | "tsc"
  | "biome"
  | "clippy"
  | "rustfmt"
  | "cargo_check"
  | "circular_deps"
  | "god_class"
  | "coupling"
  | "type_coverage_py"
  | "type_coverage_ts"
  | "srp_analyzer"
  | "dead_code_py"
  | "dead_code_ts"
  | "dead_code_rust"
  | "todo_scanner"
  | "dead_ui_state"
  | "unused_api_params"
  | "security_scan"
  | "unsafe_rust"
  | "rust_complexity"
  | "quality_gate"
  | "custom";

export interface CheckToolInfo {
  tool: CheckTool;
  name: string;
  description: string;
  check_type: CheckType;
  language: CheckLanguage;
  supports_auto_fix: boolean;
  default_command: string;
  config_files: string[];
  install_command?: string;
}

export interface CheckTypeInfo {
  type: CheckType;
  name: string;
  description: string;
  icon: string;
  color: string;
}

export const CHECK_TOOLS: CheckToolInfo[] = [
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
  {
    tool: "circular_deps",
    name: "Circular Dependencies",
    description: "Detect circular import dependencies (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "god_class",
    name: "God Class Detector",
    description: "Find classes that are too large (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["pyproject.toml"],
  },
  {
    tool: "coupling",
    name: "Coupling Analyzer",
    description: "Analyze module coupling and cohesion (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "type_coverage_py",
    name: "Python Type Coverage",
    description: "Measure type hint coverage (built-in)",
    check_type: "typecheck",
    language: "python",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["pyproject.toml"],
  },
  {
    tool: "type_coverage_ts",
    name: "TypeScript Type Coverage",
    description: "Measure type coverage (built-in)",
    check_type: "typecheck",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["tsconfig.json"],
  },
  {
    tool: "srp_analyzer",
    name: "SRP Analyzer",
    description: "Detect SRP violations (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "dead_code_py",
    name: "Python Dead Code",
    description: "Detect unused code in Python (built-in)",
    check_type: "analyze",
    language: "python",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "dead_code_ts",
    name: "TypeScript Dead Code",
    description: "Detect unused code in TypeScript (built-in)",
    check_type: "analyze",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["tsconfig.json"],
  },
  {
    tool: "dead_code_rust",
    name: "Rust Dead Code",
    description: "Detect unused code in Rust (built-in)",
    check_type: "analyze",
    language: "rust",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["Cargo.toml"],
  },
  {
    tool: "todo_scanner",
    name: "TODO Scanner",
    description: "Find TODO/FIXME comments (built-in)",
    check_type: "analyze",
    language: "other",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "dead_ui_state",
    name: "Dead UI State",
    description: "Find disconnected React useState hooks (built-in)",
    check_type: "analyze",
    language: "typescript",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["tsconfig.json"],
  },
  {
    tool: "unused_api_params",
    name: "Unused API Params",
    description: "Find unused API endpoint parameters (built-in)",
    check_type: "analyze",
    language: "other",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "security_scan",
    name: "Security Scanner",
    description: "Detect security vulnerabilities (built-in)",
    check_type: "security",
    language: "other",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
  {
    tool: "unsafe_rust",
    name: "Rust Unsafe Analyzer",
    description: "Track and audit unsafe code blocks (built-in)",
    check_type: "security",
    language: "rust",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["Cargo.toml"],
  },
  {
    tool: "rust_complexity",
    name: "Rust Complexity",
    description: "Analyze cyclomatic complexity (built-in)",
    check_type: "analyze",
    language: "rust",
    supports_auto_fix: false,
    default_command: "",
    config_files: ["Cargo.toml"],
  },
  {
    tool: "quality_gate",
    name: "Quality Gates",
    description: "Composite check enforcing quality thresholds (built-in)",
    check_type: "analyze",
    language: "other",
    supports_auto_fix: false,
    default_command: "",
    config_files: [],
  },
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
