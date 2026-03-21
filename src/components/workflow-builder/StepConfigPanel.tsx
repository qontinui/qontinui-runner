/**
 * StepConfigPanel.tsx
 *
 * Configuration panel for the selected workflow step.
 * Shows different options based on step type.
 *
 * Core step types: command, ui_bridge, prompt, workflow
 */

import { useState } from "react";
import { X, AlertCircle, ChevronDown, ChevronRight, Plus, Trash2, Workflow } from "lucide-react";
import type { UnifiedStep, WorkflowPhase } from "../../types";
import type {
  TestType,
  PlaywrightExecutionMode,
  CheckType,
  BaseStep,
  CommandStep,
  WorkflowStep,
} from "../../types/unified-workflow";
import { useWorkflowBuilder } from "./WorkflowBuilderContext";
// =============================================================================
// Check tool/type constants (previously from check-builder/types)
// =============================================================================

type CheckLanguage = "python" | "javascript" | "typescript" | "rust" | "other";

type CheckTool =
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

interface CheckToolInfo {
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

interface CheckTypeInfo {
  type: CheckType;
  name: string;
  description: string;
  icon: string;
  color: string;
}

const CHECK_TOOLS: CheckToolInfo[] = [
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

const CHECK_TYPE_INFO: CheckTypeInfo[] = [
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

// =============================================================================
// Helper to find step phase
// =============================================================================

function findStepPhase(
  stepId: string,
  workflow: {
    setup_steps: { id: string }[];
    verification_steps: { id: string }[];
    agentic_steps: { id: string }[];
    completion_steps?: { id: string }[];
  },
): WorkflowPhase | null {
  if (workflow.setup_steps.some((s) => s.id === stepId)) return "setup";
  if (workflow.verification_steps.some((s) => s.id === stepId)) return "verification";
  if (workflow.agentic_steps.some((s) => s.id === stepId)) return "agentic";
  if (workflow.completion_steps?.some((s) => s.id === stepId)) return "completion";
  return null;
}

// =============================================================================
// Data Flow Section (Universal for all step types)
// =============================================================================

function DataFlowSection({
  step,
  onUpdate,
  allSteps,
}: {
  step: UnifiedStep;
  onUpdate: (updates: Partial<UnifiedStep>) => void;
  allSteps: { id: string; name: string }[];
}) {
  const [isOpen, setIsOpen] = useState(false);

  // Type-safe access to data flow fields (on BaseStep)
  const baseStep = step as BaseStep;
  const inputs = (baseStep as unknown as Record<string, unknown>).inputs as
    | Record<string, string>
    | undefined;
  const extract = (baseStep as unknown as Record<string, unknown>).extract as
    | Record<string, string>
    | undefined;
  const dependsOn = (baseStep as unknown as Record<string, unknown>).depends_on as
    | string[]
    | undefined;
  const required = (baseStep as unknown as Record<string, unknown>).required as boolean | undefined;

  const inputEntries = inputs ? Object.entries(inputs) : [];
  const extractEntries = extract ? Object.entries(extract) : [];
  const dependsOnList = dependsOn || [];

  const handleAddInput = () => {
    const newInputs = { ...(inputs || {}), "": "" };
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleRemoveInput = (key: string) => {
    const newInputs = { ...(inputs || {}) };
    delete newInputs[key];
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleUpdateInputKey = (oldKey: string, newKey: string) => {
    const newInputs: Record<string, string> = {};
    for (const [k, v] of Object.entries(inputs || {})) {
      if (k === oldKey) {
        newInputs[newKey] = v;
      } else {
        newInputs[k] = v;
      }
    }
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleUpdateInputValue = (key: string, value: string) => {
    const newInputs = { ...(inputs || {}), [key]: value };
    onUpdate({ inputs: newInputs } as Partial<UnifiedStep>);
  };

  const handleAddExtract = () => {
    const newExtract = { ...(extract || {}), "": "" };
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleRemoveExtract = (key: string) => {
    const newExtract = { ...(extract || {}) };
    delete newExtract[key];
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleUpdateExtractKey = (oldKey: string, newKey: string) => {
    const newExtract: Record<string, string> = {};
    for (const [k, v] of Object.entries(extract || {})) {
      if (k === oldKey) {
        newExtract[newKey] = v;
      } else {
        newExtract[k] = v;
      }
    }
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleUpdateExtractValue = (key: string, value: string) => {
    const newExtract = { ...(extract || {}), [key]: value };
    onUpdate({ extract: newExtract } as Partial<UnifiedStep>);
  };

  const handleToggleDependency = (stepId: string) => {
    const updated = dependsOnList.includes(stepId)
      ? dependsOnList.filter((id) => id !== stepId)
      : [...dependsOnList, stepId];
    onUpdate({ depends_on: updated } as Partial<UnifiedStep>);
  };

  // Other steps that could be dependencies (exclude self)
  const otherSteps = allSteps.filter((s) => s.id !== step.id);

  return (
    <div className="mt-4 pt-4 border-t border-zinc-700">
      <button
        type="button"
        onClick={() => setIsOpen(!isOpen)}
        className="flex items-center gap-2 text-xs font-medium text-zinc-500 uppercase tracking-wider hover:text-zinc-400 transition-colors"
      >
        {isOpen ? <ChevronDown className="w-3 h-3" /> : <ChevronRight className="w-3 h-3" />}
        Data Flow
      </button>

      {isOpen && (
        <div className="mt-3 space-y-4">
          {/* Inputs (from other steps) */}
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">Inputs (from other steps)</h4>
            {inputEntries.map(([key, value], idx) => (
              <div key={`input-${key}-${idx}`} className="flex gap-2 mb-2">
                <input
                  type="text"
                  value={key}
                  onChange={(e) => handleUpdateInputKey(key, e.target.value)}
                  placeholder="Variable name"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <input
                  type="text"
                  value={value}
                  onChange={(e) => handleUpdateInputValue(key, e.target.value)}
                  placeholder="step_id.output_key"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm font-mono focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <button
                  onClick={() => handleRemoveInput(key)}
                  className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
            <button
              onClick={handleAddInput}
              className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add input
            </button>
          </div>

          {/* Extract (from this step's output) */}
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">
              Extract (from this step's output)
            </h4>
            {extractEntries.map(([key, value], idx) => (
              <div key={`extract-${key}-${idx}`} className="flex gap-2 mb-2">
                <input
                  type="text"
                  value={key}
                  onChange={(e) => handleUpdateExtractKey(key, e.target.value)}
                  placeholder="Output name"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <input
                  type="text"
                  value={value}
                  onChange={(e) => handleUpdateExtractValue(key, e.target.value)}
                  placeholder="$.data.result"
                  className="flex-1 px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-500 text-sm font-mono focus:outline-hidden focus:ring-1 focus:ring-blue-500/50"
                />
                <button
                  onClick={() => handleRemoveExtract(key)}
                  className="p-1.5 text-zinc-500 hover:text-red-400 transition-colors"
                >
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
            <button
              onClick={handleAddExtract}
              className="flex items-center gap-1 text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
            >
              <Plus className="w-3 h-3" />
              Add extraction
            </button>
          </div>

          {/* Dependencies */}
          <div>
            <h4 className="text-sm font-medium text-zinc-400 mb-2">Dependencies</h4>
            {otherSteps.length === 0 ? (
              <p className="text-xs text-zinc-500 italic">No other steps available</p>
            ) : (
              <div className="space-y-1 max-h-32 overflow-y-auto border border-zinc-700 rounded-md p-2 bg-zinc-800/50">
                {otherSteps.map((s) => (
                  <label
                    key={s.id}
                    className="flex items-center gap-2 p-1 rounded hover:bg-zinc-700/50 cursor-pointer"
                  >
                    <input
                      type="checkbox"
                      checked={dependsOnList.includes(s.id)}
                      onChange={() => handleToggleDependency(s.id)}
                      className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
                    />
                    <span className="text-sm text-zinc-300 truncate">{s.name}</span>
                  </label>
                ))}
              </div>
            )}
          </div>

          {/* Required */}
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="step_required"
              checked={required !== false}
              onChange={(e) => onUpdate({ required: e.target.checked } as Partial<UnifiedStep>)}
              className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="step_required" className="text-sm text-zinc-300">
              Required (workflow fails if this step fails)
            </label>
          </div>
        </div>
      )}
    </div>
  );
}

// =============================================================================
// Test Fields Config (used within CommandConfig when test_type or test_id is set)
// =============================================================================

function TestFieldsConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Test Type</label>
        <select
          value={step.test_type}
          onChange={(e) => onUpdate({ test_type: e.target.value as TestType })}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        >
          <option value="playwright">Playwright (Browser)</option>
          <option value="qontinui_vision">Qontinui Vision</option>
          <option value="python">Python Script</option>
          <option value="repository">Repository Test</option>
          <option value="custom_command">Custom Command</option>
        </select>
      </div>

      {/* Command for custom types */}
      {(step.test_type === "custom_command" ||
        step.test_type === "python" ||
        step.test_type === "repository") && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Command</label>
          <input
            type="text"
            value={step.command || ""}
            onChange={(e) => onUpdate({ command: e.target.value })}
            placeholder={
              step.test_type === "python"
                ? "python test_script.py"
                : step.test_type === "repository"
                  ? "npm test"
                  : "command to run"
            }
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          />
        </div>
      )}

      {/* Playwright-specific options */}
      {step.test_type === "playwright" && (
        <>
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Execution Mode</label>
            <select
              value={step.execution_mode || "independent"}
              onChange={(e) =>
                onUpdate({ execution_mode: e.target.value as PlaywrightExecutionMode })
              }
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            >
              <option value="independent">Independent (fresh session)</option>
              <option value="chained">Chained (continue after previous)</option>
            </select>
            <p className="text-xs text-zinc-500 mt-1">
              Independent starts a new browser session; Chained continues from previous test
            </p>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Fused Script ID (optional)
            </label>
            <input
              type="text"
              value={step.fused_script_id || ""}
              onChange={(e) => onUpdate({ fused_script_id: e.target.value || undefined })}
              placeholder="script-uuid"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
            <p className="text-xs text-zinc-500 mt-1">
              If set, this test will run after the specified setup script
            </p>
          </div>
        </>
      )}
    </div>
  );
}

// =============================================================================
// Check Fields Config (used within CommandConfig when check_type is set)
// =============================================================================

function CheckFieldsConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  // Filter tools by check type
  const availableTools = CHECK_TOOLS.filter((t) => t.check_type === step.check_type);
  const selectedTool = CHECK_TOOLS.find((t) => t.tool === step.tool);
  const _checkTypeInfo = CHECK_TYPE_INFO.find((t) => t.type === step.check_type);

  const isCiCd = step.check_type === "ci_cd";

  return (
    <div className="space-y-4">
      {/* Check Type */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Check Type</label>
        <select
          value={step.check_type}
          onChange={(e) => {
            const newType = e.target.value as CheckType;
            onUpdate({ check_type: newType, tool: undefined, command: undefined });
          }}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        >
          <option value="lint">Lint (code quality)</option>
          <option value="format">Format (code style)</option>
          <option value="typecheck">Type Check (static analysis)</option>
          <option value="analyze">Analyze (code metrics)</option>
          <option value="security">Security (vulnerability scan)</option>
          <option value="custom_command">Custom Command</option>
          <option value="ci_cd">CI/CD (GitHub Actions)</option>
        </select>
      </div>

      {isCiCd ? (
        <>
          {/* Repository */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Repository</label>
            <input
              type="text"
              value={step.repository || ""}
              onChange={(e) => onUpdate({ repository: e.target.value || undefined })}
              placeholder="owner/repo"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
            />
            <p className="text-xs text-zinc-500 mt-1">
              GitHub repository (e.g., jspindev/qontinui-runner). Leave blank to auto-detect from
              working directory.
            </p>
          </div>

          {/* Working Directory (shown when no explicit repo) */}
          {!step.repository && (
            <div>
              <label className="block text-sm font-medium text-zinc-400 mb-1">
                Working Directory
              </label>
              <input
                type="text"
                value={step.working_directory || ""}
                onChange={(e) => onUpdate({ working_directory: e.target.value || undefined })}
                placeholder="Path to git repo root"
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
              />
              <p className="text-xs text-zinc-500 mt-1">
                Git repo directory to auto-detect the GitHub repository from.
              </p>
            </div>
          )}

          {/* Workflow Name */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Workflow Name (optional)
            </label>
            <input
              type="text"
              value={step.workflow_name || ""}
              onChange={(e) => onUpdate({ workflow_name: e.target.value || undefined })}
              placeholder="CI"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
            <p className="text-xs text-zinc-500 mt-1">GitHub Actions workflow name to filter by.</p>
          </div>

          {/* Branch */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Branch (optional)
            </label>
            <input
              type="text"
              value={step.branch || ""}
              onChange={(e) => onUpdate({ branch: e.target.value || undefined })}
              placeholder="main"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>

          {/* Wait for completion */}
          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="wait_for_completion"
              checked={step.wait_for_completion ?? false}
              onChange={(e) => onUpdate({ wait_for_completion: e.target.checked })}
              className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="wait_for_completion" className="text-sm text-zinc-300">
              Wait for in-progress runs
            </label>
          </div>
          <p className="text-xs text-zinc-500 ml-6 -mt-2">
            Poll until the CI run finishes instead of failing immediately when in progress.
          </p>

          {/* Timeout */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Timeout (seconds)
            </label>
            <input
              type="number"
              value={step.timeout_seconds ?? 300}
              onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 300 })}
              min={15}
              max={600}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>
        </>
      ) : (
        <>
          {/* Tool Selection */}
          {step.check_type !== "custom_command" && availableTools.length > 0 && (
            <div>
              <label className="block text-sm font-medium text-zinc-400 mb-1">Tool</label>
              <select
                value={step.tool || ""}
                onChange={(e) => {
                  const tool = CHECK_TOOLS.find((t) => t.tool === e.target.value);
                  onUpdate({
                    tool: e.target.value || undefined,
                    command: tool?.default_command || undefined,
                  });
                }}
                className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
              >
                <option value="">Select a tool...</option>
                {availableTools.map((tool) => (
                  <option key={tool.tool} value={tool.tool}>
                    {tool.name} - {tool.description}
                  </option>
                ))}
              </select>
              {selectedTool && (
                <p className="text-xs text-zinc-500 mt-1">
                  Language: {selectedTool.language} | Auto-fix:{" "}
                  {selectedTool.supports_auto_fix ? "Yes" : "No"}
                </p>
              )}
            </div>
          )}

          {/* Command Override */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Command {step.check_type === "custom_command" ? "(required)" : "(optional override)"}
            </label>
            <input
              type="text"
              value={step.command || ""}
              onChange={(e) => onUpdate({ command: e.target.value || undefined })}
              placeholder={selectedTool?.default_command || "Enter command..."}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
            />
            {selectedTool?.default_command && !step.command && (
              <p className="text-xs text-zinc-500 mt-1">
                Default:{" "}
                <code className="bg-zinc-700 px-1 rounded">{selectedTool.default_command}</code>
              </p>
            )}
          </div>

          {/* Working Directory */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Working Directory (optional)
            </label>
            <input
              type="text"
              value={step.working_directory || ""}
              onChange={(e) => onUpdate({ working_directory: e.target.value || undefined })}
              placeholder="Leave empty for project root"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>

          {/* Auto-fix toggle (only for tools that support it) */}
          {(selectedTool?.supports_auto_fix || step.check_type === "format") && (
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="auto_fix"
                checked={step.auto_fix ?? false}
                onChange={(e) => onUpdate({ auto_fix: e.target.checked })}
                className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
              />
              <label htmlFor="auto_fix" className="text-sm text-zinc-300">
                Auto-fix issues (when supported)
              </label>
            </div>
          )}

          {/* Timeout */}
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Timeout (seconds)
            </label>
            <input
              type="number"
              value={step.timeout_seconds ?? 60}
              onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 60 })}
              min={5}
              max={600}
              className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
            />
          </div>
        </>
      )}
    </div>
  );
}

// =============================================================================
// Command Step Config (unified: shell commands, checks, check groups)
// =============================================================================

function CommandConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "command" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  const effectiveMode = step.mode;

  const handleModeChange = (newMode: string) => {
    // Clear fields that belong to other modes, set mode
    const cleared: Partial<typeof step> = {
      mode: newMode as "shell" | "check" | "check_group" | "test",
    };
    if (newMode !== "check") {
      cleared.check_type = undefined;
    }
    if (newMode !== "check_group") {
      cleared.check_group_id = undefined;
    }
    if (newMode !== "test") {
      cleared.test_type = undefined;
      cleared.test_id = undefined;
      cleared.code = undefined;
    }
    onUpdate(cleared);
  };

  // Mode selector
  const modeSelector = (
    <div className="mb-4">
      <label className="block text-sm font-medium text-zinc-400 mb-1">Command Mode</label>
      <select
        value={effectiveMode}
        onChange={(e) => handleModeChange(e.target.value)}
        className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
      >
        <option value="shell">Shell Command</option>
        <option value="check">Check (lint, typecheck, etc.)</option>
        <option value="check_group">Check Group</option>
        <option value="test">Test</option>
      </select>
    </div>
  );

  // Check group mode
  if (effectiveMode === "check_group") {
    return (
      <div className="space-y-4">
        {modeSelector}
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Check Group ID</label>
          <input
            type="text"
            value={step.check_group_id || ""}
            onChange={(e) => onUpdate({ check_group_id: e.target.value })}
            placeholder="check-group-uuid"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">
            The ID of a saved check group from the Check Builder
          </p>
        </div>

        {step.check_group_id && (
          <div className="p-2 bg-cyan-500/10 border border-cyan-500/30 rounded-md">
            <p className="text-xs text-cyan-400 font-mono">{step.check_group_id}</p>
          </div>
        )}
      </div>
    );
  }

  // Check mode
  if (effectiveMode === "check") {
    return (
      <div className="space-y-4">
        {modeSelector}
        <CheckFieldsConfig step={step} onUpdate={onUpdate} />
      </div>
    );
  }

  // Test mode
  if (effectiveMode === "test") {
    return (
      <div className="space-y-4">
        {modeSelector}
        <TestFieldsConfig step={step} onUpdate={onUpdate} />
      </div>
    );
  }

  // Default: shell command config
  return (
    <div className="space-y-4">
      {modeSelector}
      {/* Command */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Command</label>
        <textarea
          value={step.command || ""}
          onChange={(e) => onUpdate({ command: e.target.value })}
          placeholder="git branch backup-$(date +%Y%m%d)"
          rows={3}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 font-mono text-sm"
        />
        <p className="text-xs text-zinc-500 mt-1">
          Shell command to execute (bash on Unix, PowerShell on Windows)
        </p>
      </div>

      {/* Working Directory */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">
          Working Directory (optional)
        </label>
        <input
          type="text"
          value={step.working_directory || ""}
          onChange={(e) => onUpdate({ working_directory: e.target.value || undefined })}
          placeholder="Leave empty for project root"
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      {/* Timeout */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Timeout (seconds)</label>
        <input
          type="number"
          value={step.timeout_seconds ?? 60}
          onChange={(e) => onUpdate({ timeout_seconds: parseInt(e.target.value) || 60 })}
          min={5}
          max={600}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
        />
      </div>

      {/* Fail on Error */}
      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          id="fail_on_error"
          checked={step.fail_on_error ?? true}
          onChange={(e) => onUpdate({ fail_on_error: e.target.checked })}
          className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
        />
        <label htmlFor="fail_on_error" className="text-sm text-zinc-300">
          Fail workflow on non-zero exit code
        </label>
      </div>

      {/* Common Examples */}
      <div className="pt-2 border-t border-zinc-700">
        <p className="text-xs text-zinc-500 mb-2">Common examples:</p>
        <div className="flex flex-wrap gap-2">
          {[
            {
              label: "Git backup branch",
              cmd: "git branch backup-$(date +%Y%m%d) 2>/dev/null || true",
            },
            { label: "Git commit", cmd: "git add -A && git commit -m 'chore: automated update'" },
            { label: "npm install", cmd: "npm install" },
            { label: "poetry install", cmd: "poetry install" },
          ].map((example) => (
            <button
              key={example.label}
              onClick={() => onUpdate({ command: example.cmd })}
              className="px-2 py-1 text-xs bg-zinc-700 hover:bg-zinc-600 rounded text-zinc-300"
            >
              {example.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Prompt Step Config
// =============================================================================

function PromptConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "prompt" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-4">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Prompt Content</label>
        <textarea
          value={step.content || ""}
          onChange={(e) => onUpdate({ content: e.target.value })}
          placeholder="Enter the prompt for the AI agent..."
          rows={24}
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50 resize-y font-mono text-sm"
        />
        <p className="text-xs text-zinc-500 mt-1">
          This prompt will be sent to the AI agent during the agentic phase
        </p>
      </div>

      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">
            Provider (optional)
          </label>
          <select
            value={step.provider ?? ""}
            onChange={(e) => onUpdate({ provider: e.target.value || undefined })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          >
            <option value="">Default</option>
            <option value="claude_cli">Claude CLI</option>
            <option value="gemini_api">Gemini API</option>
          </select>
        </div>
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Model (optional)</label>
          <select
            value={step.model ?? ""}
            onChange={(e) => onUpdate({ model: e.target.value || undefined })}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          >
            <option value="">Default</option>
            <option value="claude-sonnet-4">Claude Sonnet 4</option>
            <option value="claude-opus-4">Claude Opus 4</option>
            <option value="gemini-2.5-pro">Gemini 2.5 Pro</option>
          </select>
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// UI Bridge Step Config
// =============================================================================

function UiBridgeConfig({
  step,
  onUpdate,
}: {
  step: UnifiedStep & { type: "ui_bridge" };
  onUpdate: (updates: Partial<typeof step>) => void;
}) {
  return (
    <div className="space-y-3">
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Action</label>
        <select
          value={step.action || "snapshot"}
          onChange={(e) =>
            onUpdate({
              action: e.target.value as
                | "navigate"
                | "execute"
                | "assert"
                | "snapshot"
                | "snapshot_assert",
            })
          }
          className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
        >
          <option value="navigate">Navigate</option>
          <option value="execute">Execute Instruction</option>
          <option value="assert">Assert Condition</option>
          <option value="snapshot">Take Snapshot</option>
          <option value="snapshot_assert">Snapshot Assert (Batch)</option>
        </select>
      </div>

      {/* URL for navigate action */}
      {step.action === "navigate" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">URL</label>
          <input
            type="url"
            value={step.url || ""}
            onChange={(e) => onUpdate({ url: e.target.value })}
            placeholder="https://example.com"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
          />
          <p className="text-xs text-zinc-500 mt-1">The URL to navigate to</p>
        </div>
      )}

      {/* Instruction for execute action */}
      {step.action === "execute" && (
        <div>
          <label className="block text-sm font-medium text-zinc-400 mb-1">Instruction</label>
          <textarea
            value={step.instruction || ""}
            onChange={(e) => onUpdate({ instruction: e.target.value })}
            placeholder="Click the submit button, fill in the form..."
            rows={4}
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 resize-y"
          />
          <p className="text-xs text-zinc-500 mt-1">
            Natural language instruction for the UI Bridge to execute
          </p>
        </div>
      )}

      {/* Target, assert_type, expected for assert action */}
      {step.action === "assert" && (
        <>
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Target Element</label>
            <input
              type="text"
              value={step.target || ""}
              onChange={(e) => onUpdate({ target: e.target.value })}
              placeholder='[data-testid="submit-btn"], .header-title, etc.'
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 font-mono text-sm"
            />
            <p className="text-xs text-zinc-500 mt-1">CSS selector of the target element</p>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Assert Type</label>
            <select
              value={step.assert_type || "exists"}
              onChange={(e) =>
                onUpdate({
                  assert_type: e.target.value as
                    | "exists"
                    | "text_equals"
                    | "contains"
                    | "visible"
                    | "enabled",
                })
              }
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
            >
              <option value="exists">Exists</option>
              <option value="text_equals">Text Equals</option>
              <option value="contains">Contains</option>
              <option value="visible">Visible</option>
              <option value="enabled">Enabled</option>
            </select>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Expected Value</label>
            <input
              type="text"
              value={step.expected || ""}
              onChange={(e) => onUpdate({ expected: e.target.value })}
              placeholder="Expected text or value"
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
            />
            <p className="text-xs text-zinc-500 mt-1">
              Expected value for text_equals and contains assertions
            </p>
          </div>
        </>
      )}

      {/* Snapshot Assert config */}
      {step.action === "snapshot_assert" && (
        <>
          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">
              Assertions (JSON)
            </label>
            <textarea
              value={step.target || "[]"}
              onChange={(e) => onUpdate({ target: e.target.value })}
              placeholder={`[\n  {\n    "id": "check-1",\n    "description": "Header exists",\n    "severity": "critical",\n    "assertionType": "exists",\n    "criteria": { "textContent": "Header" }\n  }\n]`}
              rows={8}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 resize-y font-mono text-xs"
            />
            <p className="text-xs text-zinc-500 mt-1">
              JSON array of assertions. Each needs: id, description, severity, assertionType
              (exists/contains/visible/not_exists), criteria (search fields like textContent,
              testId, type).
            </p>
          </div>

          <div>
            <label className="block text-sm font-medium text-zinc-400 mb-1">Snapshot Target</label>
            <select
              value={step.ui_bridge_snapshot_target || "control"}
              onChange={(e) => onUpdate({ ui_bridge_snapshot_target: e.target.value })}
              className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
            >
              <option value="control">Control (Runner UI)</option>
              <option value="sdk">SDK (Connected App)</option>
            </select>
          </div>
        </>
      )}

      {/* Timeout */}
      <div>
        <label className="block text-sm font-medium text-zinc-400 mb-1">Timeout (ms)</label>
        <input
          type="number"
          value={step.timeout_ms ?? 5000}
          onChange={(e) => onUpdate({ timeout_ms: parseInt(e.target.value) || 5000 })}
          min={1000}
          max={60000}
          step={1000}
          className="w-32 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
        />
      </div>
    </div>
  );
}

// =============================================================================
// Workflow Step Config
// =============================================================================

function WorkflowConfig({
  step,
  onChangeWorkflow,
}: {
  step: UnifiedStep & { type: "workflow" };
  onChangeWorkflow?: () => void;
}) {
  const wfStep = step as WorkflowStep;
  return (
    <div className="space-y-4">
      <div className="flex items-start gap-3 p-3 bg-zinc-800 border border-zinc-700 rounded-lg">
        <div className="p-2 rounded-md bg-blue-500/10 text-blue-400">
          <Workflow className="w-5 h-5" />
        </div>
        <div className="flex-1 min-w-0">
          <div className="font-medium text-zinc-200">
            {wfStep.workflow_name || "No workflow selected"}
          </div>
          {wfStep.workflow_id && (
            <p className="text-xs text-zinc-500 mt-0.5 font-mono truncate">{wfStep.workflow_id}</p>
          )}
        </div>
      </div>

      {onChangeWorkflow && (
        <button
          onClick={onChangeWorkflow}
          className="w-full px-3 py-2 text-sm bg-zinc-700 hover:bg-zinc-600 rounded-md text-zinc-200 transition-colors"
        >
          Change Workflow
        </button>
      )}

      {!wfStep.workflow_id && (
        <p className="text-sm text-amber-400/80">
          No workflow selected. Click &quot;Change Workflow&quot; to pick one.
        </p>
      )}
    </div>
  );
}

// =============================================================================
// Main StepConfigPanel Component
// =============================================================================

interface StepConfigPanelProps {
  onClose?: () => void;
  /** Callback to open the workflow library picker for changing the referenced workflow */
  onOpenWorkflowPicker?: () => void;
}

export function StepConfigPanel({ onClose, onOpenWorkflowPicker }: StepConfigPanelProps) {
  const { state, getSelectedStep, updateStep } = useWorkflowBuilder();
  const selectedStep = getSelectedStep();

  if (!selectedStep) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center text-zinc-500">
          <AlertCircle className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>Select a step to configure</p>
        </div>
      </div>
    );
  }

  const phase = findStepPhase(selectedStep.id, state.workflow);

  if (!phase) {
    return (
      <div className="h-full flex items-center justify-center p-4">
        <div className="text-center text-zinc-500">
          <AlertCircle className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p>Step not found in workflow</p>
        </div>
      </div>
    );
  }

  const handleUpdate = (updates: Partial<UnifiedStep>) => {
    updateStep({ ...selectedStep, ...updates } as UnifiedStep, phase);
  };

  // Get step type label
  const stepTypeLabel = (() => {
    switch (selectedStep.type) {
      case "command": {
        const cmd = selectedStep as CommandStep;
        return cmd.test_type || cmd.test_id ? "Test" : "Command";
      }
      case "prompt":
        return "AI Prompt";
      case "ui_bridge":
        return "UI Bridge";
      case "workflow":
        return "Workflow";
      default:
        return "Step";
    }
  })();

  // Collect all steps for dependency selection in data flow
  const allSteps = [
    ...state.workflow.setup_steps,
    ...state.workflow.verification_steps,
    ...state.workflow.agentic_steps,
    ...(state.workflow.completion_steps || []),
  ];

  return (
    <div className="h-full flex flex-col bg-zinc-850 border-l border-zinc-700">
      {/* Header */}
      <div className="flex items-center justify-between p-4 border-b border-zinc-700">
        <div>
          <h3 className="text-sm font-medium text-zinc-200">Configure Step</h3>
          <p className="text-xs text-zinc-500">{stepTypeLabel}</p>
        </div>
        {onClose && (
          <button
            onClick={onClose}
            className="p-1 hover:bg-zinc-700 rounded transition-colors text-zinc-400 hover:text-zinc-200"
          >
            <X className="w-4 h-4" />
          </button>
        )}
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4">
        {/* Skill Origin Badge */}
        {selectedStep.skill_origin && (
          <div className="mb-3 flex items-center justify-between gap-2 px-2.5 py-1.5 bg-zinc-800/50 border border-zinc-700/50 rounded-md">
            <span className="text-xs text-zinc-400">
              From skill:{" "}
              <span className="text-zinc-300 font-medium">
                {selectedStep.skill_origin.skill_slug}
              </span>
            </span>
            <button
              className="text-[10px] text-zinc-500 hover:text-zinc-300 transition-colors"
              onClick={() =>
                handleUpdate({ skill_origin: undefined } as unknown as Partial<UnifiedStep>)
              }
              title="Detach from skill — converts to raw step"
            >
              Detach
            </button>
          </div>
        )}

        {/* Common fields */}
        <div className="mb-4">
          <label className="block text-sm font-medium text-zinc-400 mb-1">Step Name</label>
          <input
            type="text"
            value={selectedStep.name}
            onChange={(e) => handleUpdate({ name: e.target.value })}
            placeholder="Step name"
            className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
          />
        </div>

        {/* Type-specific config */}
        {selectedStep.type === "command" && (
          <CommandConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "prompt" && (
          <PromptConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {/* TestConfig removed — test mode is now inside CommandConfig */}
        {selectedStep.type === "ui_bridge" && (
          <UiBridgeConfig step={selectedStep} onUpdate={handleUpdate} />
        )}
        {selectedStep.type === "workflow" && (
          <WorkflowConfig
            step={selectedStep as UnifiedStep & { type: "workflow" }}
            onChangeWorkflow={onOpenWorkflowPicker}
          />
        )}

        {/* Console Error Handling -- shown for all verification phase steps */}
        {phase === "verification" && (
          <div className="mt-4 pt-4 border-t border-zinc-700">
            <h4 className="text-xs font-medium text-zinc-500 uppercase tracking-wider mb-3">
              Console Errors
            </h4>
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="fail_on_console_errors"
                checked={(selectedStep as BaseStep).fail_on_console_errors ?? false}
                onChange={(e) =>
                  handleUpdate({ fail_on_console_errors: e.target.checked } as Partial<UnifiedStep>)
                }
                className="rounded bg-zinc-700 border-zinc-600 text-blue-500 focus:ring-blue-500/50"
              />
              <label htmlFor="fail_on_console_errors" className="text-sm text-zinc-300">
                Fail on console errors
              </label>
            </div>
            <p className="text-xs text-zinc-500 mt-1 ml-6">
              Step will fail if browser console errors are detected during execution, even if the
              step itself passes.
            </p>
          </div>
        )}

        {/* Data Flow section (universal for all step types) */}
        <DataFlowSection step={selectedStep} onUpdate={handleUpdate} allSteps={allSteps} />
      </div>
    </div>
  );
}
