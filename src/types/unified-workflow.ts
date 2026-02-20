/**
 * Unified Workflow Types
 *
 * Type definitions for the unified Workflow Builder system.
 * All automation is organized into four phases: Setup, Verification, Agentic, Completion.
 *
 * Execution Order:
 *   Setup (once) -> [Verification <-> Agentic]* -> Completion (once)
 *
 * The Verification/Agentic loop continues until all required checks pass or max iterations.
 * Setup and Completion run exactly once - at the beginning and end respectively.
 *
 * Step Types (3 core types):
 *   command   - Shell commands, checks, check groups, tests (merged from shell_command, check, check_group, test, api_request, mcp_call)
 *   ui_bridge - UI Bridge SDK interactions (navigate, execute, assert, snapshot)
 *   prompt    - AI task instructions
 */

// =============================================================================
// Phases
// =============================================================================

/**
 * Workflow execution phases
 */
export type WorkflowPhase = "setup" | "verification" | "agentic" | "completion";

// =============================================================================
// Log Source Selection
// =============================================================================

/**
 * Log source selection mode for a workflow
 * - "default": Use the global default profile (from Settings)
 * - "ai": Let AI automatically select relevant sources
 * - "all": Use all enabled log sources
 * - { profile_id: string }: Use a specific profile
 */
export type LogSourceSelection = "default" | "ai" | "all" | { profile_id: string };

// =============================================================================
// Health Check Configuration
// =============================================================================

/**
 * Configuration for a health check URL
 */
export interface HealthCheckUrl {
  /** Display name for the health check (e.g., "Backend Server") */
  name: string;
  /** URL to check (e.g., "http://localhost:8000/health") */
  url: string;
  /** Expected HTTP status code (default: 200) */
  expected_status?: number;
  /** Timeout in seconds (default: 5) */
  timeout_seconds?: number;
  /** Whether failure should stop the workflow (default: true) */
  is_critical?: boolean;
}

// =============================================================================
// Step Types
// =============================================================================

/**
 * Base interface for all step types
 */
export interface BaseStep {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name for the step */
  name: string;
  /**
   * If true, step fails when console errors are captured during execution
   * (even if the step itself passes). Default: false (console errors are informational).
   */
  fail_on_console_errors?: boolean;
  /** Input mappings: name -> "step-id.field" or "step-id.output.json.path" */
  inputs?: Record<string, string>;
  /** Extract named values from this step's output: name -> JSON path */
  extract?: Record<string, string>;
  /** Step IDs that must complete before this step runs */
  depends_on?: string[];
  /** Whether this step is required for verification pass/fail. Default: true */
  required?: boolean;
  /** Retry configuration */
  retry?: { count: number; delay_ms: number };
}

// -----------------------------------------------------------------------------
// API Request Builder Types (used by the API request builder tab, not workflow steps)
// -----------------------------------------------------------------------------

/**
 * HTTP methods for API requests
 */
export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

/**
 * Content types for API request bodies
 */
export type ApiContentType =
  | "application/json"
  | "application/x-www-form-urlencoded"
  | "text/plain"
  | "none";

/**
 * Variable extraction from API response using JSON path
 */
export interface ApiVariableExtraction {
  /** Variable name to store the extracted value (use in subsequent steps as {{variable_name}}) */
  variable_name: string;
  /** JSON path to extract from response body (e.g., "$.data.id", "$.items[0].name") */
  json_path: string;
  /** Default value if extraction fails */
  default_value?: string;
}

/**
 * Assertion for API response verification
 */
export interface ApiAssertion {
  /** Type of assertion to perform */
  type: "status_code" | "json_path" | "header" | "body_contains" | "response_time";
  /** Expected value or pattern */
  expected: string | number;
  /** JSON path for json_path assertions */
  json_path?: string;
  /** Header name for header assertions */
  header_name?: string;
  /** Comparison operator (default: equals) */
  operator?: "equals" | "contains" | "matches" | "greater_than" | "less_than";
}

// -----------------------------------------------------------------------------
// Test Types (used by command steps in test mode)
// -----------------------------------------------------------------------------

/**
 * Test types supported by the verification system.
 * Used when a command step has test_type or test_id set.
 */
export type TestType =
  | "playwright"
  | "qontinui_vision"
  | "python"
  | "repository"
  | "custom_command";

/**
 * Playwright test execution mode
 */
export type PlaywrightExecutionMode = "independent" | "chained";

// -----------------------------------------------------------------------------
// Check Steps
// -----------------------------------------------------------------------------

/**
 * Check type categories
 */
export type CheckType =
  | "lint"
  | "format"
  | "typecheck"
  | "analyze"
  | "security"
  | "custom_command"
  | "http_status"
  | "ai_review"
  | "ci_cd";

// -----------------------------------------------------------------------------
// Command Steps (unified: shell_command + check + check_group)
// -----------------------------------------------------------------------------

/**
 * Command Step (Setup, Verification, or Completion)
 *
 * Unified command step that can run shell commands, code quality checks,
 * check groups, or tests. The behavior is determined by which fields are set:
 * - check_group_id set -> runs all checks in a saved check group
 * - check_type set -> runs a code quality check
 * - test_id or test_type set -> runs a verification test
 * - Otherwise -> runs a shell command
 */
export interface CommandStep extends BaseStep {
  type: "command";
  phase: "setup" | "verification" | "completion";
  /** The shell command to execute */
  command?: string;
  /** Working directory (relative to project root) */
  working_directory?: string;
  /** Timeout in seconds */
  timeout_seconds?: number;
  /** Whether to fail the workflow if command returns non-zero exit code */
  fail_on_error?: boolean;
  /** For setup steps: whether to run on subsequent iterations (default: false) */
  run_on_subsequent_iterations?: boolean;
  /** Reference to saved shell command ID */
  shell_command_id?: string;

  // Check-specific fields (when check_type is set)
  /** Type of check to run (lint, format, typecheck, etc.) */
  check_type?: CheckType;
  /** Tool name (e.g., "eslint", "ruff", "mypy") */
  tool?: string;
  /** Reference to saved check ID */
  check_id?: string;
  /** Config file path (e.g., pyproject.toml, .eslintrc) */
  config_path?: string;
  /** Run with auto-fix if supported */
  auto_fix?: boolean;
  /** Fail the step if warnings are reported */
  fail_on_warning?: boolean;
  /** CI/CD: GitHub repository in owner/repo format */
  repository?: string;
  /** CI/CD: GitHub Actions workflow name filter */
  workflow_name?: string;
  /** CI/CD: Branch filter */
  branch?: string;
  /** CI/CD: Wait for in-progress runs to complete */
  wait_for_completion?: boolean;

  // Check group fields (when check_group_id is set)
  /** Reference to saved check group ID */
  check_group_id?: string;

  // Test fields (when test_id or test_type is set)
  /** Type of test to run */
  test_type?: TestType;
  /** Reference to saved test ID */
  test_id?: string;
  /** Inline code (for playwright and python tests) */
  code?: string;
  /** Reference to saved Playwright script ID */
  script_id?: string;
  /** Target URL for Playwright tests */
  target_url?: string;
  /** Which setup script to fuse with (for Playwright tests) */
  fused_script_id?: string;
  /** Execution mode: fresh session or continue after previous test */
  execution_mode?: PlaywrightExecutionMode;
}

// -----------------------------------------------------------------------------
// Prompt Steps
// -----------------------------------------------------------------------------

/**
 * Prompt Step (Setup, Verification, Agentic, or Completion)
 *
 * AI task instructions. Can be used in any phase:
 * - Setup: AI-driven environment preparation
 * - Verification: AI-evaluated success criteria
 * - Agentic: Main AI work loop
 * - Completion: Final AI actions after loop exits
 */
export interface PromptStep extends BaseStep {
  type: "prompt";
  phase: "setup" | "verification" | "agentic" | "completion";
  /** The prompt content/instructions */
  content: string;
  /** Reference to saved prompt ID */
  prompt_id?: string;
  /** AI provider override */
  provider?: string;
  /** Model override */
  model?: string;
  /**
   * True for the auto-generated summary step (locked to last position in completion phase).
   * Summary steps cannot be moved and are always executed last.
   */
  is_summary_step?: boolean;
}

// -----------------------------------------------------------------------------
// UI Bridge Steps
// -----------------------------------------------------------------------------

/**
 * UI Bridge Step (Setup, Verification, or Completion)
 *
 * Interacts with the UI via the UI Bridge SDK. Supports navigation,
 * executing instructions, asserting conditions, and taking snapshots.
 */
export interface UiBridgeStep extends BaseStep {
  type: "ui_bridge";
  phase: "setup" | "verification" | "completion";
  /** Action: navigate to URL, execute instruction, assert condition, or take snapshot */
  action: "navigate" | "execute" | "assert" | "snapshot";
  /** URL for navigate action or UI Bridge endpoint */
  url?: string;
  /** Natural language instruction for execute action */
  instruction?: string;
  /** Target element for assert action */
  target?: string;
  /** Assertion type */
  assert_type?: "exists" | "text_equals" | "contains" | "visible" | "enabled";
  /** Expected value for assertions */
  expected?: string;
  /** Timeout in milliseconds */
  timeout_ms?: number;
}

// =============================================================================
// Step Type Names
// =============================================================================

/**
 * All valid step type names
 */
export type StepTypeName = "command" | "ui_bridge" | "prompt";

// =============================================================================
// Unified Step Type
// =============================================================================

/**
 * Union of all step types
 */
export type UnifiedStep = CommandStep | PromptStep | UiBridgeStep;

/**
 * Setup phase step types
 */
export type SetupStep = CommandStep | PromptStep | UiBridgeStep;

/**
 * Verification phase step types
 */
export type VerificationStep = CommandStep | PromptStep | UiBridgeStep;

/**
 * Agentic phase step types
 */
export type AgenticStep = PromptStep;

/**
 * Completion phase step types
 */
export type CompletionStep = CommandStep | PromptStep | UiBridgeStep;

// =============================================================================
// Workflow
// =============================================================================

/**
 * A unified workflow containing steps organized by phase
 */
export interface UnifiedWorkflow {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name */
  name: string;
  /** Description of what this workflow does */
  description: string;

  // Steps organized by phase
  /** Setup phase steps (runs once at the beginning) */
  setup_steps: SetupStep[];
  /** Verification phase steps (runs before each agentic iteration) */
  verification_steps: VerificationStep[];
  /** Agentic phase steps (AI work, iterates until verification passes) */
  agentic_steps: AgenticStep[];
  /** Completion phase steps (runs once after the loop exits) */
  completion_steps: CompletionStep[];

  // Settings (shown when relevant steps present)
  /** Maximum iterations for agentic phase (when has agentic steps) */
  max_iterations?: number;
  /**
   * Optional inactivity timeout in seconds for AI sessions.
   * - undefined/null: No timeout, runs until completion or manual stop (default)
   * - number: Kill AI session after N seconds of no output
   * Takes precedence over the global AI settings timeout.
   */
  timeout_seconds?: number | null;
  /** AI provider override */
  provider?: string;
  /** Model override */
  model?: string;

  // Log source settings
  /**
   * Log source selection for this workflow.
   * - "default": Use the global default profile (from Settings -> Log Sources)
   * - "ai": Let AI automatically select relevant sources based on context
   * - "all": Use all enabled log sources
   * - { profile_id: string }: Use a specific profile
   * Default: "default"
   */
  log_source_selection?: LogSourceSelection;

  // Context settings
  /**
   * Manually added context IDs.
   * These contexts are explicitly selected for inclusion in the AI prompt.
   */
  context_ids?: string[];
  /**
   * Disabled context IDs.
   * These contexts are excluded from auto-include even if they match triggers.
   */
  disabled_context_ids?: string[];
  /**
   * Whether to auto-include contexts based on task description triggers.
   * When enabled, contexts with matching auto-include rules are automatically added.
   * Default: true
   */
  auto_include_contexts?: boolean;

  // Summary settings
  /**
   * Skip the automatic AI summary generation at the end of workflow execution.
   * Default: false (AI summary is generated)
   * Set to true to save tokens when only deterministic summary is needed.
   * Note: Deterministic summary (test results, screenshots, etc.) is always collected.
   */
  skip_ai_summary?: boolean;

  // Log watch settings
  /**
   * Whether to automatically include a log_watch step before verification.
   * When enabled (default), a log_watch step is prepended to verification steps
   * to detect runtime errors in backend/frontend logs.
   * Default: true
   */
  log_watch_enabled?: boolean;

  // Health check settings
  /**
   * Whether to automatically include health check steps before verification.
   * When enabled (default) and health_check_urls is non-empty, health check steps
   * are prepended to verification steps to verify configured servers are running.
   * Health checks run BEFORE log_watch to catch server down before scanning logs.
   * Default: true
   */
  health_check_enabled?: boolean;

  /**
   * URLs to health check before verification (user-configurable).
   * Each entry specifies a URL to check, expected status, and timeout.
   * If empty, no health checks are performed even if health_check_enabled is true.
   */
  health_check_urls?: HealthCheckUrl[];

  // Prompt template settings
  /**
   * Custom developer prompt template for this workflow.
   * When set, this template is used instead of the global default when running the workflow.
   * The template supports variables like {{SESSION_ID}}, {{ITERATION}}, {{MAX_ITERATIONS}},
   * {{GOAL}}, {{EXECUTION_STEPS}}, {{WORKSPACE_ESCAPED}}.
   * Set to null/undefined to use the global template (which may itself be customized or default).
   */
  prompt_template?: string | null;

  // Metadata
  /** Category for organization */
  category: string;
  /** Tags for filtering */
  tags: string[];
  /** Creation timestamp (ISO 8601) */
  created_at: string;
  /** Last modification timestamp (ISO 8601) */
  modified_at: string;
}

// =============================================================================
// Feature Detection
// =============================================================================

/**
 * Features detected from workflow steps
 */
export interface WorkflowFeatures {
  /** Has any setup steps */
  hasSetup: boolean;
  /** Has any verification steps */
  hasVerification: boolean;
  /** Has any agentic steps */
  hasAgentic: boolean;
  /** Has any completion steps */
  hasCompletion: boolean;
  /** Has UI Bridge steps in any phase */
  hasUiBridge: boolean;
  /** Show iteration settings (has agentic steps) */
  showIterationSettings: boolean;
  /** Has AI prompts in any phase */
  hasAiPrompts: boolean;
}

/**
 * Detect features from workflow steps
 */
export function detectWorkflowFeatures(workflow: UnifiedWorkflow): WorkflowFeatures {
  const allSteps: UnifiedStep[] = [
    ...workflow.setup_steps,
    ...workflow.verification_steps,
    ...workflow.agentic_steps,
    ...(workflow.completion_steps ?? []),
  ];

  const hasSetup = workflow.setup_steps.length > 0;
  const hasVerification = workflow.verification_steps.length > 0;
  const hasAgentic = workflow.agentic_steps.length > 0;
  const hasCompletion = (workflow.completion_steps ?? []).length > 0;

  const hasUiBridge = allSteps.some((s) => s.type === "ui_bridge");
  const hasAiPrompts = allSteps.some((s) => s.type === "prompt");

  return {
    hasSetup,
    hasVerification,
    hasAgentic,
    hasCompletion,
    hasUiBridge,
    showIterationSettings: hasAgentic,
    hasAiPrompts,
  };
}

// =============================================================================
// Step Type Constants
// =============================================================================

/**
 * Step type display information
 */
export interface StepTypeInfo {
  type: string;
  label: string;
  description: string;
  icon: string;
  color: string;
  phase: WorkflowPhase | "setup" | "verification";
}

/**
 * All step types organized by phase.
 * 3 core types available in setup, verification, and completion.
 * Agentic phase is restricted to AI Prompt only.
 */
export const STEP_TYPES: Record<WorkflowPhase, StepTypeInfo[]> = {
  setup: [
    {
      type: "command",
      label: "Command",
      description: "Run shell commands, checks, or tests",
      icon: "Terminal",
      color: "gray",
      phase: "setup",
    },
    {
      type: "ui_bridge",
      label: "UI Bridge",
      description: "Interact with UI via UI Bridge SDK",
      icon: "Monitor",
      color: "emerald",
      phase: "setup",
    },
    {
      type: "prompt",
      label: "AI Task",
      description: "AI-driven task",
      icon: "Bot",
      color: "violet",
      phase: "setup",
    },
  ],
  verification: [
    {
      type: "command",
      label: "Command",
      description: "Run commands, checks, or tests for verification",
      icon: "Terminal",
      color: "gray",
      phase: "verification",
    },
    {
      type: "ui_bridge",
      label: "UI Bridge",
      description: "Verify UI state via UI Bridge",
      icon: "Monitor",
      color: "emerald",
      phase: "verification",
    },
    {
      type: "prompt",
      label: "AI Verification",
      description: "AI-evaluated criteria",
      icon: "Bot",
      color: "violet",
      phase: "verification",
    },
  ],
  agentic: [
    {
      type: "prompt",
      label: "Prompt",
      description: "AI task instructions",
      icon: "MessageSquare",
      color: "amber",
      phase: "agentic",
    },
  ],
  completion: [
    {
      type: "command",
      label: "Command",
      description: "Run cleanup commands or final tests",
      icon: "Terminal",
      color: "gray",
      phase: "completion",
    },
    {
      type: "ui_bridge",
      label: "UI Bridge",
      description: "Final UI interactions",
      icon: "Monitor",
      color: "emerald",
      phase: "completion",
    },
    {
      type: "prompt",
      label: "AI Completion",
      description: "Final AI actions",
      icon: "Bot",
      color: "violet",
      phase: "completion",
    },
  ],
};

/**
 * Phase display information
 */
export const PHASE_INFO: Record<
  WorkflowPhase,
  { label: string; description: string; color: string }
> = {
  setup: {
    label: "Setup",
    description: "Runs once at the beginning",
    color: "blue",
  },
  verification: {
    label: "Verification",
    description: "Checks success criteria, loops with agentic",
    color: "green",
  },
  agentic: {
    label: "Agentic",
    description: "AI work, iterates until verification passes",
    color: "amber",
  },
  completion: {
    label: "Completion",
    description: "Runs once after the loop exits",
    color: "purple",
  },
};

// =============================================================================
// Summary Step Constants and Helpers
// =============================================================================

/**
 * Default prompt content for the AI summary step.
 * This step generates a summary of all tasks completed in the workflow.
 */
export const DEFAULT_SUMMARY_PROMPT = `Write a one-paragraph summary of all the tasks completed in this workflow. Include what was accomplished, whether the stated goal was achieved, any issues encountered and how they were resolved, and remaining work if the goal was not fully achieved. Be concise but comprehensive.`;

/**
 * Create a summary step for the completion phase.
 * Summary steps are locked to the last position and cannot be moved.
 */
export function createSummaryStep(): PromptStep {
  return {
    id: crypto.randomUUID(),
    type: "prompt",
    phase: "completion",
    name: "AI Summary",
    content: DEFAULT_SUMMARY_PROMPT,
    is_summary_step: true,
  };
}

// =============================================================================
// Utility Functions
// =============================================================================

/**
 * Generate a unique ID for a step
 */
export function generateStepId(): string {
  return crypto.randomUUID();
}

/**
 * Create a default step of a given type
 */
export function createDefaultStep(type: UnifiedStep["type"], phase: WorkflowPhase): UnifiedStep {
  const id = generateStepId();

  switch (type) {
    case "command":
      return {
        id,
        type: "command",
        phase: phase as "setup" | "verification" | "completion",
        name: "Command",
        command: "",
      };
    case "ui_bridge":
      return {
        id,
        type: "ui_bridge",
        phase: phase as "setup" | "verification" | "completion",
        name: "UI Bridge",
        action: "snapshot",
      };
    case "prompt": {
      // Different default names based on phase
      const promptNames: Record<WorkflowPhase, string> = {
        setup: "AI Setup Task",
        verification: "AI Verification",
        agentic: "Prompt",
        completion: "AI Completion Task",
      };
      return {
        id,
        type: "prompt",
        phase: phase as "setup" | "verification" | "agentic" | "completion",
        name: promptNames[phase] ?? "Prompt",
        content: "",
      };
    }
    default:
      throw new Error(`Unknown step type: ${type}`);
  }
}

/**
 * Create a default empty workflow
 *
 * @param includeSummaryStep - Whether to include the AI Summary step in the completion phase.
 *                             Defaults to true. The summary step is locked to last position.
 */
export function createDefaultWorkflow(
  includeSummaryStep: boolean = true,
): Omit<UnifiedWorkflow, "id" | "created_at" | "modified_at"> {
  return {
    name: "",
    description: "",
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: includeSummaryStep ? [createSummaryStep()] : [],
    category: "general",
    tags: [],
  };
}

/**
 * Check if a workflow is empty (has no user-added steps).
 * A workflow with only a summary step is considered empty since it's the default state.
 */
export function isWorkflowEmpty(workflow: UnifiedWorkflow): boolean {
  const completionSteps = workflow.completion_steps ?? [];
  // A workflow is empty if it has no setup/verification/agentic steps
  // and completion only has the default summary step (or nothing)
  const hasOnlySummaryStep =
    completionSteps.length === 0 ||
    (completionSteps.length === 1 &&
      completionSteps[0].type === "prompt" &&
      (completionSteps[0] as PromptStep).is_summary_step === true);

  return (
    workflow.setup_steps.length === 0 &&
    workflow.verification_steps.length === 0 &&
    workflow.agentic_steps.length === 0 &&
    hasOnlySummaryStep
  );
}

/**
 * Get total step count across all phases
 */
export function getTotalStepCount(workflow: UnifiedWorkflow): number {
  return (
    workflow.setup_steps.length +
    workflow.verification_steps.length +
    workflow.agentic_steps.length +
    (workflow.completion_steps ?? []).length
  );
}

/**
 * Get the phase for a given step
 */
export function getStepPhase(step: UnifiedStep): WorkflowPhase {
  return step.phase;
}

/**
 * Check if a step type can exist in a given phase
 */
export function canStepExistInPhase(stepType: UnifiedStep["type"], phase: WorkflowPhase): boolean {
  // Agentic phase only allows prompts
  if (phase === "agentic") {
    return stepType === "prompt";
  }

  // All 3 step types are allowed in setup, verification, and completion
  switch (stepType) {
    case "command":
    case "ui_bridge":
    case "prompt":
      return true;
    default:
      return false;
  }
}

// =============================================================================
// Export/Import Types
// =============================================================================

/**
 * Manifest for exported workflow files
 */
export interface WorkflowExportManifest {
  /** Export format version */
  version: string;
  /** When the export was created (ISO 8601) */
  exported_at: string;
  /** App version that created the export */
  app_version: string;
  /** Type of content */
  content_type: "unified_workflow";
}

/**
 * A single workflow export file
 */
export interface WorkflowExport {
  /** Export manifest with version info */
  manifest: WorkflowExportManifest;
  /** The workflow data */
  workflow: UnifiedWorkflow;
}

/**
 * Result of importing a workflow
 */
export interface WorkflowImportResult {
  /** The imported workflow */
  workflow: UnifiedWorkflow;
  /** Whether an existing workflow was overwritten */
  overwritten: boolean;
  /** Original ID if it was changed */
  original_id: string | null;
}
