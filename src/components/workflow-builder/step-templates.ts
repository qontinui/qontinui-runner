/**
 * Step Templates
 *
 * Pre-configured step instances for the workflow builder.
 * Each template creates a step with sensible defaults for a specific use case.
 *
 * Templates are organized by category:
 *   testing    - Test runner configurations (pytest, playwright, npm test, etc.)
 *   checks     - Code quality checks (lint, format, typecheck, security)
 *   shell      - General shell commands
 *   ui_bridge  - UI Bridge SDK interactions
 *   ai         - AI-driven prompts
 *   api        - HTTP API requests
 *   macros     - Sequential action macros
 *   exploration - State exploration
 */

import type {
  WorkflowPhase,
  UnifiedStep,
  CommandStep,
  UiBridgeStep,
  PromptStep,
} from "../../types";
import { generateStepId } from "../../types";

// =============================================================================
// Template Types
// =============================================================================

export type StepTemplateCategory =
  | "testing"
  | "checks"
  | "shell"
  | "ui_bridge"
  | "ai"
  | "api"
  | "macros"
  | "exploration";

export interface StepTemplate {
  id: string;
  name: string;
  description: string;
  category: StepTemplateCategory;
  icon: string;
  color: string;
  tags: string[];
  applicablePhases: WorkflowPhase[];
  createStep: (phase: WorkflowPhase) => UnifiedStep;
}

// =============================================================================
// Category Metadata
// =============================================================================

export const TEMPLATE_CATEGORIES: Record<
  StepTemplateCategory,
  { label: string; icon: string; color: string; description: string }
> = {
  testing: {
    label: "Testing",
    icon: "TestTube2",
    color: "green",
    description: "Test runner configurations",
  },
  checks: {
    label: "Checks",
    icon: "CheckCircle2",
    color: "blue",
    description: "Code quality checks",
  },
  shell: {
    label: "Shell",
    icon: "Terminal",
    color: "gray",
    description: "General shell commands",
  },
  ui_bridge: {
    label: "UI Bridge",
    icon: "Monitor",
    color: "emerald",
    description: "UI Bridge SDK interactions",
  },
  ai: {
    label: "AI",
    icon: "Bot",
    color: "violet",
    description: "AI-driven prompts",
  },
  api: {
    label: "API",
    icon: "Globe",
    color: "sky",
    description: "HTTP API requests",
  },
  macros: {
    label: "Macros",
    icon: "Zap",
    color: "amber",
    description: "Sequential action macros",
  },
  exploration: {
    label: "Exploration",
    icon: "Compass",
    color: "emerald",
    description: "State exploration",
  },
};

// =============================================================================
// Helpers
// =============================================================================

const NON_AGENTIC_PHASES: WorkflowPhase[] = ["setup", "verification", "completion"];

function makeCommand(phase: WorkflowPhase, overrides: Partial<CommandStep>): CommandStep {
  return {
    id: generateStepId(),
    type: "command",
    phase: phase as "setup" | "verification" | "completion",
    name: "Command",
    ...overrides,
  };
}

function makeUiBridge(phase: WorkflowPhase, overrides: Partial<UiBridgeStep>): UiBridgeStep {
  return {
    id: generateStepId(),
    type: "ui_bridge",
    phase: phase as "setup" | "verification" | "completion",
    name: "UI Bridge",
    action: "snapshot",
    ...overrides,
  };
}

function makePrompt(phase: WorkflowPhase, overrides: Partial<PromptStep>): PromptStep {
  return {
    id: generateStepId(),
    type: "prompt",
    phase: phase as "setup" | "verification" | "agentic" | "completion",
    name: "Prompt",
    content: "",
    ...overrides,
  };
}

// =============================================================================
// Built-in Templates
// =============================================================================

export const BUILT_IN_TEMPLATES: StepTemplate[] = [
  // -- Testing ----------------------------------------------------------------
  {
    id: "pytest",
    name: "Run pytest",
    description: "Run Python tests with pytest",
    category: "testing",
    icon: "TestTube2",
    color: "green",
    tags: ["python", "pytest", "unit-test"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Run pytest",
        command: "pytest",
        test_type: "repository",
      }),
  },
  {
    id: "playwright-test",
    name: "Playwright Test",
    description: "Run Playwright browser tests",
    category: "testing",
    icon: "TestTube2",
    color: "green",
    tags: ["playwright", "browser", "e2e"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Playwright Test",
        test_type: "playwright",
        execution_mode: "independent",
      }),
  },
  {
    id: "playwright-snippets",
    name: "Playwright + Snippets",
    description: "Playwright test with prompt snippet context",
    category: "testing",
    icon: "TestTube2",
    color: "green",
    tags: ["playwright", "snippets", "e2e"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Playwright + Snippets",
        test_type: "playwright",
        execution_mode: "independent",
      }),
  },
  {
    id: "npm-test",
    name: "npm test",
    description: "Run tests via npm test",
    category: "testing",
    icon: "TestTube2",
    color: "green",
    tags: ["npm", "node", "javascript"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "npm test",
        command: "npm test",
        test_type: "repository",
      }),
  },
  {
    id: "cargo-test",
    name: "cargo test",
    description: "Run Rust tests with cargo",
    category: "testing",
    icon: "TestTube2",
    color: "green",
    tags: ["rust", "cargo"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "cargo test",
        command: "cargo test -- --nocapture",
        test_type: "repository",
      }),
  },
  {
    id: "custom-test",
    name: "Custom Test",
    description: "Run any test command",
    category: "testing",
    icon: "TestTube2",
    color: "green",
    tags: ["custom"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Custom Test",
        test_type: "custom_command",
      }),
  },

  // -- Checks -----------------------------------------------------------------
  {
    id: "lint-check",
    name: "Lint Check",
    description: "Run linting tools (ESLint, Ruff, etc.)",
    category: "checks",
    icon: "CheckCircle2",
    color: "blue",
    tags: ["lint", "eslint", "ruff"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Lint Check",
        check_type: "lint",
      }),
  },
  {
    id: "format-check",
    name: "Format Check",
    description: "Check code formatting (Prettier, Black, etc.)",
    category: "checks",
    icon: "CheckCircle2",
    color: "blue",
    tags: ["format", "prettier", "black"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Format Check",
        check_type: "format",
      }),
  },
  {
    id: "type-check",
    name: "Type Check",
    description: "Run type checking (TypeScript, mypy, etc.)",
    category: "checks",
    icon: "CheckCircle2",
    color: "blue",
    tags: ["typecheck", "typescript", "mypy"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Type Check",
        check_type: "typecheck",
      }),
  },
  {
    id: "security-scan",
    name: "Security Scan",
    description: "Run security vulnerability scanner",
    category: "checks",
    icon: "ShieldCheck",
    color: "red",
    tags: ["security", "audit", "vulnerabilities"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Security Scan",
        check_type: "security",
      }),
  },
  {
    id: "check-group",
    name: "Run Check Group",
    description: "Run a named group of checks together",
    category: "checks",
    icon: "Layers",
    color: "teal",
    tags: ["group", "batch", "checks"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Run Check Group",
        check_group_id: "",
      }),
  },

  // -- Shell ------------------------------------------------------------------
  {
    id: "shell-command",
    name: "Shell Command",
    description: "Run any shell command",
    category: "shell",
    icon: "Terminal",
    color: "gray",
    tags: ["shell", "bash", "command"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "Command",
        command: "",
      }),
  },

  // -- UI Bridge --------------------------------------------------------------
  {
    id: "ui-navigate",
    name: "UI Navigate",
    description: "Navigate to a URL in the browser",
    category: "ui_bridge",
    icon: "Monitor",
    color: "emerald",
    tags: ["navigate", "browser", "url"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeUiBridge(phase, {
        name: "Navigate",
        action: "navigate",
      }),
  },
  {
    id: "ui-assert",
    name: "UI Assert",
    description: "Assert a condition on a UI element",
    category: "ui_bridge",
    icon: "Monitor",
    color: "emerald",
    tags: ["assert", "verify", "check"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeUiBridge(phase, {
        name: "UI Assert",
        action: "assert",
      }),
  },
  {
    id: "ui-execute",
    name: "UI Execute",
    description: "Execute an instruction on the UI",
    category: "ui_bridge",
    icon: "Monitor",
    color: "emerald",
    tags: ["execute", "interact", "click"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeUiBridge(phase, {
        name: "UI Execute",
        action: "execute",
      }),
  },
  {
    id: "ui-snapshot",
    name: "UI Snapshot",
    description: "Take a snapshot of the current UI state",
    category: "ui_bridge",
    icon: "Monitor",
    color: "emerald",
    tags: ["snapshot", "capture", "screenshot"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeUiBridge(phase, {
        name: "UI Snapshot",
        action: "snapshot",
      }),
  },
  {
    id: "ui-bridge-compare",
    name: "UI Bridge Compare",
    description: "Compare current app state against a reference snapshot",
    category: "ui_bridge",
    icon: "GitCompare",
    color: "pink",
    tags: ["compare", "ui-bridge", "snapshot", "diff"],
    applicablePhases: ["verification", "completion"],
    createStep: (phase) =>
      makeUiBridge(phase, {
        name: "Compare App State",
        action: "compare",
        comparison_mode: "structural",
      }),
  },

  // -- AI ---------------------------------------------------------------------
  {
    id: "ai-task",
    name: "AI Task",
    description: "General AI task instructions",
    category: "ai",
    icon: "Bot",
    color: "violet",
    tags: ["ai", "prompt", "task"],
    applicablePhases: ["setup", "verification", "agentic", "completion"],
    createStep: (phase) =>
      makePrompt(phase, {
        name: "AI Task",
        content: "",
      }),
  },
  {
    id: "ai-verification",
    name: "AI Verification",
    description: "AI-evaluated success criteria",
    category: "ai",
    icon: "Bot",
    color: "violet",
    tags: ["ai", "verification", "evaluate"],
    applicablePhases: ["verification", "completion"],
    createStep: (phase) =>
      makePrompt(phase, {
        name: "AI Verification",
        content:
          "Evaluate whether the following criteria have been met:\n\n1. \n2. \n3. \n\nRespond with PASS if all criteria are met, or FAIL with details about what is missing.",
      }),
  },
  {
    id: "ai-with-context",
    name: "AI Task with Context",
    description: "AI task using a saved context document",
    category: "ai",
    icon: "Bot",
    color: "violet",
    tags: ["ai", "context", "prompt"],
    applicablePhases: ["setup", "verification", "agentic", "completion"],
    createStep: (phase) =>
      makePrompt(phase, {
        name: "AI Task with Context",
        content:
          "# Context\nAttach a saved context to provide background information.\n\n# Task\nDescribe what the AI should do with the above context.",
      }),
  },
  {
    id: "run-task",
    name: "Run Task",
    description: "Execute a saved AI task prompt",
    category: "ai",
    icon: "FileText",
    color: "orange",
    tags: ["task", "prompt", "ai"],
    applicablePhases: ["setup", "verification", "agentic", "completion"],
    createStep: (phase) =>
      makePrompt(phase, {
        name: "AI Task",
        content: "",
      }),
  },

  // -- Exploration ------------------------------------------------------------
  {
    id: "state-exploration",
    name: "State Exploration",
    description: "Run a state exploration configuration",
    category: "exploration",
    icon: "Compass",
    color: "emerald",
    tags: ["exploration", "states", "testing"],
    applicablePhases: NON_AGENTIC_PHASES,
    createStep: (phase) =>
      makeCommand(phase, {
        name: "State Exploration",
        command: "",
      }),
  },
];

// =============================================================================
// Query Helpers
// =============================================================================

/**
 * Get all templates applicable to a given workflow phase.
 */
export function getTemplatesForPhase(phase: string): StepTemplate[] {
  return BUILT_IN_TEMPLATES.filter((t) => t.applicablePhases.includes(phase as WorkflowPhase));
}

/**
 * Get all templates grouped by category.
 * Only includes categories that have at least one template.
 */
export function getTemplatesByCategory(): Record<StepTemplateCategory, StepTemplate[]> {
  const result = {} as Record<StepTemplateCategory, StepTemplate[]>;
  for (const cat of Object.keys(TEMPLATE_CATEGORIES) as StepTemplateCategory[]) {
    const templates = BUILT_IN_TEMPLATES.filter((t) => t.category === cat);
    if (templates.length > 0) {
      result[cat] = templates;
    }
  }
  return result;
}

/**
 * Get a single template by ID.
 */
export function getTemplateById(id: string): StepTemplate | undefined {
  return BUILT_IN_TEMPLATES.find((t) => t.id === id);
}
