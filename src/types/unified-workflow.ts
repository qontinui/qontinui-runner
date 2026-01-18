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
// Step Types
// =============================================================================

/**
 * Base interface for all step types
 */
interface BaseStep {
  /** Unique identifier (UUID v4) */
  id: string;
  /** Display name for the step */
  name: string;
}

// -----------------------------------------------------------------------------
// SETUP Phase Steps
// -----------------------------------------------------------------------------

/**
 * Playwright Script Step (Setup or Completion)
 *
 * Browser automation using Playwright.
 * In setup phase: Navigates to the testing point.
 * In completion phase: Runs final browser actions.
 * Note: Both phases run once (setup at beginning, completion at end).
 */
export interface ScriptStep extends BaseStep {
  type: "script";
  phase: "setup" | "completion";
  /** Inline Playwright code */
  code?: string;
  /** Reference to saved script ID */
  script_id?: string;
  /** Starting URL for the script */
  target_url?: string;
  /** Whether to run refinement loop until script succeeds */
  refinement_enabled: boolean;
}

/**
 * State Navigation Step (Setup or Verification)
 *
 * Navigates to a stored application state using Qontinui vision.
 * Note: In setup phase, this step always runs once regardless of run_on_subsequent_iterations.
 */
export interface StateStep extends BaseStep {
  type: "state";
  phase: "setup" | "verification";
  /** Reference to stored state ID */
  state_id: string;
  /** State name (for display) */
  state_name?: string;
  /** Timeout for reaching the state */
  timeout_seconds?: number;
  /**
   * Run on subsequent iterations.
   * Note: Ignored for setup phase - setup always runs once.
   * @deprecated For setup phase steps, this field is ignored.
   */
  run_on_subsequent_iterations?: boolean;
}

/**
 * Workflow Reference Step (Setup or Verification)
 *
 * Executes another saved workflow.
 * Note: In setup phase, this step always runs once regardless of run_on_subsequent_iterations.
 */
export interface WorkflowRefStep extends BaseStep {
  type: "workflow_ref";
  phase: "setup" | "verification";
  /** Reference to another workflow ID */
  workflow_id: string;
  /** Workflow name (for display) */
  workflow_name?: string;
  /**
   * Run on subsequent iterations.
   * Note: Ignored for setup phase - setup always runs once.
   * @deprecated For setup phase steps, this field is ignored.
   */
  run_on_subsequent_iterations?: boolean;
}

// -----------------------------------------------------------------------------
// GUI Action Steps (Setup or Verification)
// -----------------------------------------------------------------------------

/**
 * GUI Action types
 */
export type GuiActionType = "click" | "double_click" | "right_click" | "type" | "hotkey" | "scroll";

/**
 * GUI Action Step (Setup or Verification)
 *
 * Mouse and keyboard actions using Qontinui vision-based automation.
 * Note: In setup phase, this step always runs once regardless of run_on_subsequent_iterations.
 */
export interface GuiActionStep extends BaseStep {
  type: "gui_action";
  phase: "setup" | "verification";
  /** Action type to perform */
  action: GuiActionType;
  /** Target image IDs for click actions */
  target_image_ids?: string[];
  /** Target image names (for display) */
  target_image_names?: string[];
  /** Text to type (for type action) */
  text_input?: string;
  /** Hotkey combination (for hotkey action) */
  hotkey?: string;
  /** Scroll direction (for scroll action) */
  scroll_direction?: "up" | "down";
  /** Pause after action in milliseconds */
  pause_after_ms?: number;
  /** Monitor index (0 = primary, undefined = all) */
  monitor_index?: number;
  /**
   * Run on subsequent iterations.
   * Note: Ignored for setup phase - setup always runs once.
   * @deprecated For setup phase steps, this field is ignored.
   */
  run_on_subsequent_iterations?: boolean;
}

// -----------------------------------------------------------------------------
// API Request Steps (Setup or Verification)
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

/**
 * API Request Step (Setup, Verification, or Completion)
 *
 * Makes HTTP requests to APIs with variable substitution, response extraction,
 * and optional assertions. Use for fetching data or verifying API state.
 *
 * Variable substitution: Use {{variable_name}} in URL, headers, or body.
 * Variables can come from previous extractions or credential storage.
 *
 * Note: In setup/completion phase, this step always runs once.
 */
export interface ApiRequestStep extends BaseStep {
  type: "api_request";
  phase: "setup" | "verification" | "completion";

  /** HTTP method */
  method: HttpMethod;
  /** URL with optional variable substitution (e.g., {{base_url}}/api/users/{{user_id}}) */
  url: string;

  /** Request headers as key-value pairs (supports {{variables}}) */
  headers?: Record<string, string>;
  /** Request body (supports {{variables}}) */
  body?: string;
  /** Content type for the request body */
  content_type?: ApiContentType;

  /** Request timeout in milliseconds (default: 30000) */
  timeout_ms?: number;
  /** Whether to follow redirects (default: true) */
  follow_redirects?: boolean;

  /** Variable extractions from response using JSON paths */
  extractions?: ApiVariableExtraction[];
  /** Assertions for response verification (step fails if any assertion fails) */
  assertions?: ApiAssertion[];

  /** Credential ID for authentication (from secure storage) */
  credential_id?: string;

  /**
   * Run on subsequent iterations.
   * Note: Ignored for setup/completion phase - these always run once.
   * @deprecated For setup/completion phase steps, this field is ignored.
   */
  run_on_subsequent_iterations?: boolean;
}

// -----------------------------------------------------------------------------
// VERIFICATION Phase Steps
// -----------------------------------------------------------------------------

/**
 * Test types supported by the verification system
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

/**
 * Test Step (Verification only)
 *
 * Runs verification checks to test target functionality.
 */
export interface TestStep extends BaseStep {
  type: "test";
  phase: "verification";
  /** Type of test to run */
  test_type: TestType;
  /** Command to run (for custom_command and repository tests) */
  command?: string;
  /** Working directory for repository tests */
  working_directory?: string;
  /** Inline code (for playwright and python tests) */
  code?: string;
  /** Reference to saved test ID */
  test_id?: string;
  /**
   * Whether failure blocks the agentic loop.
   * - true (default): Failure causes agentic phase to fix the problem
   * - false: Informative only, doesn't block completion
   * @deprecated Use is_blocking instead
   */
  is_critical: boolean;
  /**
   * Whether failure blocks the agentic loop.
   * - true (default): Failure causes agentic phase to fix the problem
   * - false: Informative only, doesn't block completion
   */
  is_blocking?: boolean;
  /** Description of what this test verifies */
  description?: string;

  // Playwright-specific configuration
  /** Which setup script to fuse with (for Playwright tests) */
  fused_script_id?: string;
  /** Execution mode: fresh session or continue after previous test */
  execution_mode?: PlaywrightExecutionMode;
}

/**
 * Screenshot Step (Verification only)
 *
 * Captures current screen state for AI analysis.
 */
export interface ScreenshotStep extends BaseStep {
  type: "screenshot";
  phase: "verification";
  /** Delay before capturing in milliseconds */
  delay_ms?: number;
  /** Which monitor to capture */
  monitor?: "all" | "primary" | "left" | "right" | number;
}

/**
 * Check type categories
 */
export type CheckType = "lint" | "format" | "typecheck" | "analyze" | "security" | "custom_command";

/**
 * Check Step (Verification only)
 *
 * Runs code quality checks like linting, formatting, and type checking.
 * Supports auto-fix for tools that can automatically correct issues.
 */
export interface CheckStep extends BaseStep {
  type: "check";
  phase: "verification";
  /** Type of check to run */
  check_type: CheckType;
  /** Reference to saved check ID (from check library) */
  check_id?: string;
  /** Command to run (for custom_command or overrides) */
  command?: string;
  /** Working directory for the check */
  working_directory?: string;
  /** Config file path (e.g., pyproject.toml, .eslintrc) */
  config_path?: string;
  /** Run with auto-fix if supported by the tool */
  auto_fix?: boolean;
  /** Fail the step if warnings are reported */
  fail_on_warning?: boolean;
  /** Timeout in seconds */
  timeout_seconds?: number;
  /**
   * Whether failure blocks the agentic loop.
   * - true (default): Failure causes agentic phase to fix the problem
   * - false: Informative only, doesn't block completion
   */
  is_blocking?: boolean;
}

// -----------------------------------------------------------------------------
// AGENTIC Phase Steps
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
   * For verification prompts: whether failure blocks the agentic loop.
   * - true (default): Failure causes agentic phase to fix the problem
   * - false: Informative only, doesn't block completion
   */
  is_blocking?: boolean;
}

/**
 * Shell Command Step (Setup or Completion)
 *
 * Runs an arbitrary shell command for environment setup or cleanup tasks.
 * Examples: git operations, file management, environment variables.
 */
export interface ShellCommandStep extends BaseStep {
  type: "shell_command";
  phase: "setup" | "completion";
  /** The shell command to execute */
  command: string;
  /** Reference to saved shell command ID */
  shell_command_id?: string;
  /** Working directory (relative to project root) */
  working_directory?: string;
  /** Timeout in seconds (default: 60) */
  timeout_seconds?: number;
  /** Whether to fail the workflow if command returns non-zero exit code */
  fail_on_error?: boolean;
  /** For setup steps: whether to run on subsequent iterations (default: false) */
  run_on_subsequent_iterations?: boolean;
}

// -----------------------------------------------------------------------------
// MCP Call Steps (Model Context Protocol)
// -----------------------------------------------------------------------------

/**
 * MCP Call Step (Setup, Verification, or Completion)
 *
 * Calls a tool on an external MCP server. MCP servers can provide
 * database access, API integrations, custom tools, and more.
 *
 * Variable substitution: Use {{variable_name}} in arguments.
 * Variables can come from previous extractions or credential storage.
 */
export interface McpCallStep extends BaseStep {
  type: "mcp_call";
  phase: "setup" | "verification" | "completion";

  /** MCP server ID (from MCP settings) */
  server_id: string;
  /** MCP server name (for display) */
  server_name?: string;
  /** Tool name to call */
  tool_name: string;

  /** Tool arguments (supports {{variables}}) */
  arguments?: Record<string, unknown>;

  /** Request timeout in seconds (default: 30) */
  timeout_seconds?: number;

  /** Variable extractions from response using JSON paths */
  extractions?: ApiVariableExtraction[];
  /** Assertions for response verification (step fails if any assertion fails) */
  assertions?: ApiAssertion[];

  /** Whether to fail the workflow if the call fails (default: true) */
  fail_on_error?: boolean;

  /**
   * Run on subsequent iterations.
   * Note: Ignored for setup/completion phase - these always run once.
   */
  run_on_subsequent_iterations?: boolean;
}

// -----------------------------------------------------------------------------
// AWAS Steps (Application Web Automation Specification)
// -----------------------------------------------------------------------------

/**
 * AWAS Discover Step (Setup phase)
 *
 * Discovers the AWAS manifest from a URL. This is typically the first
 * step when automating an AWAS-enabled application.
 */
export interface AwasDiscoverStep extends BaseStep {
  type: "awas_discover";
  phase: "setup";
  /** URL to discover AWAS manifest from */
  url: string;
  /** Timeout in seconds (default: 30) */
  timeout_seconds?: number;
}

/**
 * AWAS Execute Step (Setup or Verification)
 *
 * Executes an AWAS action on the target application.
 */
export interface AwasExecuteStep extends BaseStep {
  type: "awas_execute";
  phase: "setup" | "verification";
  /** URL of the application */
  url: string;
  /** Action ID to execute (from the AWAS manifest) */
  action_id: string;
  /** Parameters for the action */
  params?: Record<string, unknown>;
  /** Timeout in seconds (default: 30) */
  timeout_seconds?: number;
}

/**
 * AWAS Check Support Step (Setup phase)
 *
 * Checks if a URL supports AWAS.
 */
export interface AwasCheckSupportStep extends BaseStep {
  type: "awas_check_support";
  phase: "setup";
  /** URL to check for AWAS support */
  url: string;
  /** Timeout in seconds (default: 30) */
  timeout_seconds?: number;
}

/**
 * AWAS List Actions Step (Setup or Verification)
 *
 * Lists available AWAS actions from a previously discovered manifest.
 */
export interface AwasListActionsStep extends BaseStep {
  type: "awas_list_actions";
  phase: "setup" | "verification";
  /** URL to list actions for (optional, uses last discovered manifest) */
  url?: string;
  /** Timeout in seconds (default: 30) */
  timeout_seconds?: number;
}

/**
 * AWAS Extract Elements Step (Verification)
 *
 * Extracts AWAS-annotated elements from HTML content.
 */
export interface AwasExtractElementsStep extends BaseStep {
  type: "awas_extract_elements";
  phase: "verification";
  /** HTML content to extract elements from */
  html: string;
  /** Base URL for resolving relative URLs */
  base_url?: string;
}

// =============================================================================
// Unified Step Type
// =============================================================================

/**
 * Union of all step types
 */
export type UnifiedStep =
  | ScriptStep
  | StateStep
  | WorkflowRefStep
  | GuiActionStep
  | ApiRequestStep
  | McpCallStep
  | TestStep
  | CheckStep
  | ScreenshotStep
  | PromptStep
  | ShellCommandStep
  // AWAS step types
  | AwasDiscoverStep
  | AwasExecuteStep
  | AwasCheckSupportStep
  | AwasListActionsStep
  | AwasExtractElementsStep;

/**
 * AWAS step types union
 */
export type AwasStep =
  | AwasDiscoverStep
  | AwasExecuteStep
  | AwasCheckSupportStep
  | AwasListActionsStep
  | AwasExtractElementsStep;

/**
 * Setup phase step types
 * Includes PromptStep for AI-driven setup tasks
 * Includes AWAS steps for web automation
 */
export type SetupStep =
  | ScriptStep
  | StateStep
  | WorkflowRefStep
  | GuiActionStep
  | ApiRequestStep
  | McpCallStep
  | PromptStep
  | AwasDiscoverStep
  | AwasExecuteStep
  | AwasCheckSupportStep
  | AwasListActionsStep;

/**
 * Verification phase step types
 * Includes PromptStep for AI-evaluated verification criteria
 * Includes AWAS steps for web automation verification
 */
export type VerificationStep =
  | TestStep
  | CheckStep
  | ScreenshotStep
  | GuiActionStep
  | StateStep
  | WorkflowRefStep
  | ApiRequestStep
  | McpCallStep
  | PromptStep
  | AwasExecuteStep
  | AwasListActionsStep
  | AwasExtractElementsStep;

/**
 * Agentic phase step types
 */
export type AgenticStep = PromptStep;

/**
 * Completion phase step types
 * Runs once after the verification/agentic loop exits
 */
export type CompletionStep =
  | PromptStep
  | ScriptStep
  | ApiRequestStep
  | McpCallStep
  | ShellCommandStep;

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
  /** AI provider override */
  provider?: string;
  /** Model override */
  model?: string;

  // Log source settings
  /**
   * Log source selection for this workflow.
   * - "default": Use the global default profile (from Settings → Log Sources)
   * - "ai": Let AI automatically select relevant sources based on context
   * - "all": Use all enabled log sources
   * - { profile_id: string }: Use a specific profile
   * Default: "default"
   */
  log_source_selection?: LogSourceSelection;

  // Summary settings
  /**
   * Skip the automatic AI summary generation at the end of workflow execution.
   * Default: false (AI summary is generated)
   * Set to true to save tokens when only deterministic summary is needed.
   * Note: Deterministic summary (test results, screenshots, etc.) is always collected.
   */
  skip_ai_summary?: boolean;

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
  /** Has Playwright scripts (always run every iteration) */
  hasPlaywrightScripts: boolean;
  /** Has Qontinui automation (GUI actions, states, workflows) */
  hasQontinuiAutomation: boolean;
  /** Has tests that need configuration selection */
  hasPlaywrightTests: boolean;
  /** Requires config selection (has GUI-dependent steps) */
  requiresConfig: boolean;
  /** Show iteration settings (has agentic steps) */
  showIterationSettings: boolean;
  /** Has AI prompts in any phase */
  hasAiPrompts: boolean;
}

/**
 * Detect features from workflow steps
 */
export function detectWorkflowFeatures(workflow: UnifiedWorkflow): WorkflowFeatures {
  const hasSetup = workflow.setup_steps.length > 0;
  const hasVerification = workflow.verification_steps.length > 0;
  const hasAgentic = workflow.agentic_steps.length > 0;
  const hasCompletion = (workflow.completion_steps ?? []).length > 0;

  const hasPlaywrightScripts = workflow.setup_steps.some((s) => s.type === "script");
  const hasQontinuiAutomation =
    workflow.setup_steps.some(
      (s) => s.type === "state" || s.type === "workflow_ref" || s.type === "gui_action",
    ) || workflow.verification_steps.some((s) => s.type === "gui_action");

  const hasPlaywrightTests = workflow.verification_steps.some(
    (s) => s.type === "test" && s.test_type === "playwright",
  );

  const hasAiPrompts =
    workflow.setup_steps.some((s) => s.type === "prompt") ||
    workflow.verification_steps.some((s) => s.type === "prompt") ||
    workflow.agentic_steps.some((s) => s.type === "prompt") ||
    (workflow.completion_steps ?? []).some((s) => s.type === "prompt");

  const requiresConfig = hasQontinuiAutomation;

  return {
    hasSetup,
    hasVerification,
    hasAgentic,
    hasCompletion,
    hasPlaywrightScripts,
    hasQontinuiAutomation,
    hasPlaywrightTests,
    requiresConfig,
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
 * All step types organized by phase
 */
export const STEP_TYPES: Record<WorkflowPhase, StepTypeInfo[]> = {
  setup: [
    {
      type: "script",
      label: "Playwright Script",
      description: "Navigate to testing point with browser automation",
      icon: "FileCode",
      color: "emerald",
      phase: "setup",
    },
    {
      type: "state",
      label: "Navigate to State",
      description: "Go to a stored application state",
      icon: "Navigation",
      color: "blue",
      phase: "setup",
    },
    {
      type: "workflow_ref",
      label: "Run Workflow",
      description: "Execute another saved workflow",
      icon: "GitBranch",
      color: "purple",
      phase: "setup",
    },
    {
      type: "gui_action",
      label: "GUI Action",
      description: "Click, type, or press hotkeys",
      icon: "MousePointer2",
      color: "orange",
      phase: "setup",
    },
    {
      type: "api_request",
      label: "API Request",
      description: "Fetch data from APIs before automation",
      icon: "Globe",
      color: "cyan",
      phase: "setup",
    },
    {
      type: "prompt",
      label: "AI Setup Task",
      description: "AI-driven environment preparation",
      icon: "Bot",
      color: "violet",
      phase: "setup",
    },
    {
      type: "shell_command",
      label: "Shell Command",
      description: "Run shell commands (git, scripts, etc.)",
      icon: "Terminal",
      color: "gray",
      phase: "setup",
    },
    {
      type: "mcp_call",
      label: "MCP Call",
      description: "Call a tool on an MCP server",
      icon: "Plug",
      color: "indigo",
      phase: "setup",
    },
    // AWAS Setup Steps
    {
      type: "awas_discover",
      label: "AWAS Discover",
      description: "Discover AWAS manifest from a URL",
      icon: "Search",
      color: "teal",
      phase: "setup",
    },
    {
      type: "awas_check_support",
      label: "AWAS Check Support",
      description: "Check if URL supports AWAS",
      icon: "CheckCircle",
      color: "teal",
      phase: "setup",
    },
    {
      type: "awas_list_actions",
      label: "AWAS List Actions",
      description: "List available AWAS actions",
      icon: "List",
      color: "teal",
      phase: "setup",
    },
    {
      type: "awas_execute",
      label: "AWAS Execute",
      description: "Execute an AWAS action",
      icon: "Play",
      color: "teal",
      phase: "setup",
    },
  ],
  verification: [
    {
      type: "test_playwright",
      label: "Playwright Test",
      description: "Browser assertions and checks",
      icon: "TestTube2",
      color: "green",
      phase: "verification",
    },
    {
      type: "test_vision",
      label: "Qontinui Vision Test",
      description: "Visual element detection",
      icon: "Eye",
      color: "cyan",
      phase: "verification",
    },
    {
      type: "test_python",
      label: "Python Test",
      description: "White-box unit tests",
      icon: "Code",
      color: "yellow",
      phase: "verification",
    },
    {
      type: "test_repository",
      label: "Repository Test",
      description: "Run tests from your repo (pytest, jest, cargo)",
      icon: "Package",
      color: "indigo",
      phase: "verification",
    },
    {
      type: "test_custom",
      label: "Custom Command",
      description: "Any shell command for verification",
      icon: "Terminal",
      color: "gray",
      phase: "verification",
    },
    {
      type: "screenshot",
      label: "Screenshot",
      description: "Capture current screen state",
      icon: "Camera",
      color: "pink",
      phase: "verification",
    },
    {
      type: "check_lint",
      label: "Lint Check",
      description: "Run linting checks (ruff, eslint, clippy)",
      icon: "AlertTriangle",
      color: "cyan",
      phase: "verification",
    },
    {
      type: "check_format",
      label: "Format Check",
      description: "Run formatting checks (black, prettier, rustfmt)",
      icon: "AlignLeft",
      color: "cyan",
      phase: "verification",
    },
    {
      type: "check_typecheck",
      label: "Type Check",
      description: "Run type checking (mypy, tsc)",
      icon: "FileType",
      color: "cyan",
      phase: "verification",
    },
    {
      type: "check_custom",
      label: "Custom Check",
      description: "Run custom check command",
      icon: "Terminal",
      color: "cyan",
      phase: "verification",
    },
    {
      type: "check_analyze",
      label: "Code Analysis",
      description: "Run code analysis (circular deps, god class, coupling, SRP, dead code)",
      icon: "Search",
      color: "indigo",
      phase: "verification",
    },
    {
      type: "check_security",
      label: "Security Check",
      description: "Run security scans (vulnerability detection, unsafe code audit)",
      icon: "Shield",
      color: "red",
      phase: "verification",
    },
    {
      type: "state",
      label: "Navigate to State",
      description: "Go to a stored application state",
      icon: "Navigation",
      color: "blue",
      phase: "verification",
    },
    {
      type: "workflow_ref",
      label: "Run Workflow",
      description: "Execute another saved workflow",
      icon: "GitBranch",
      color: "purple",
      phase: "verification",
    },
    {
      type: "gui_action",
      label: "GUI Action",
      description: "Click, type, or press hotkeys for verification",
      icon: "MousePointer2",
      color: "orange",
      phase: "verification",
    },
    {
      type: "api_request",
      label: "API Request",
      description: "Verify API responses with assertions",
      icon: "Globe",
      color: "cyan",
      phase: "verification",
    },
    {
      type: "prompt",
      label: "AI Verification",
      description: "AI-evaluated success criteria",
      icon: "Bot",
      color: "violet",
      phase: "verification",
    },
    {
      type: "mcp_call",
      label: "MCP Call",
      description: "Call an MCP tool for verification",
      icon: "Plug",
      color: "indigo",
      phase: "verification",
    },
    // AWAS Verification Steps
    {
      type: "awas_execute",
      label: "AWAS Execute",
      description: "Execute an AWAS action for verification",
      icon: "Play",
      color: "teal",
      phase: "verification",
    },
    {
      type: "awas_list_actions",
      label: "AWAS List Actions",
      description: "List available AWAS actions",
      icon: "List",
      color: "teal",
      phase: "verification",
    },
    {
      type: "awas_extract_elements",
      label: "AWAS Extract Elements",
      description: "Extract AWAS elements from HTML",
      icon: "FileSearch",
      color: "teal",
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
      type: "prompt",
      label: "AI Completion Task",
      description: "Final AI actions after loop exits",
      icon: "Bot",
      color: "violet",
      phase: "completion",
    },
    {
      type: "script",
      label: "Playwright Script",
      description: "Final browser automation",
      icon: "FileCode",
      color: "emerald",
      phase: "completion",
    },
    {
      type: "api_request",
      label: "API Request",
      description: "Final API calls (notifications, cleanup)",
      icon: "Globe",
      color: "cyan",
      phase: "completion",
    },
    {
      type: "shell_command",
      label: "Shell Command",
      description: "Run shell commands (git commit, cleanup, etc.)",
      icon: "Terminal",
      color: "gray",
      phase: "completion",
    },
    {
      type: "mcp_call",
      label: "MCP Call",
      description: "Call an MCP tool (notifications, cleanup)",
      icon: "Plug",
      color: "indigo",
      phase: "completion",
    },
  ],
};

/**
 * GUI action sub-types
 */
export const GUI_ACTION_TYPES: {
  type: GuiActionType;
  label: string;
  icon: string;
  description: string;
}[] = [
  { type: "click", label: "Click", icon: "MousePointer2", description: "Single click on target" },
  {
    type: "double_click",
    label: "Double-Click",
    icon: "MousePointerClick",
    description: "Double-click on target",
  },
  {
    type: "right_click",
    label: "Right-Click",
    icon: "MousePointer",
    description: "Context menu click",
  },
  { type: "type", label: "Type Text", icon: "Keyboard", description: "Type text at cursor" },
  { type: "hotkey", label: "Hotkey", icon: "Command", description: "Press key combination" },
  { type: "scroll", label: "Scroll", icon: "ArrowUpDown", description: "Scroll up or down" },
];

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
    case "script":
      return {
        id,
        type: "script",
        phase: "setup",
        name: "New Script",
        refinement_enabled: true,
      };
    case "state":
      return {
        id,
        type: "state",
        phase: "setup",
        name: "Navigate to State",
        state_id: "",
      };
    case "workflow_ref":
      return {
        id,
        type: "workflow_ref",
        phase: "setup",
        name: "Run Workflow",
        workflow_id: "",
      };
    case "gui_action":
      return {
        id,
        type: "gui_action",
        phase: phase as "setup" | "verification",
        name: "GUI Action",
        action: "click",
      };
    case "test":
      return {
        id,
        type: "test",
        phase: "verification",
        name: "New Test",
        test_type: "custom_command",
        is_critical: true,
        is_blocking: true,
      };
    case "check":
      return {
        id,
        type: "check",
        phase: "verification",
        name: "New Check",
        check_type: "custom_command",
        is_blocking: true,
      };
    case "screenshot":
      return {
        id,
        type: "screenshot",
        phase: "verification",
        name: "Screenshot",
      };
    case "api_request":
      return {
        id,
        type: "api_request",
        phase: phase as "setup" | "verification",
        name: "API Request",
        method: "GET",
        url: "",
      };
    case "mcp_call":
      return {
        id,
        type: "mcp_call",
        phase: phase as "setup" | "verification" | "completion",
        name: "MCP Call",
        server_id: "",
        tool_name: "",
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
        is_blocking: phase === "verification" ? true : undefined,
      };
    }
    // AWAS step types
    case "awas_discover":
      return {
        id,
        type: "awas_discover",
        phase: "setup",
        name: "AWAS Discover",
        url: "",
      };
    case "awas_execute":
      return {
        id,
        type: "awas_execute",
        phase: phase as "setup" | "verification",
        name: "AWAS Execute",
        url: "",
        action_id: "",
      };
    case "awas_check_support":
      return {
        id,
        type: "awas_check_support",
        phase: "setup",
        name: "AWAS Check Support",
        url: "",
      };
    case "awas_list_actions":
      return {
        id,
        type: "awas_list_actions",
        phase: phase as "setup" | "verification",
        name: "AWAS List Actions",
      };
    case "awas_extract_elements":
      return {
        id,
        type: "awas_extract_elements",
        phase: "verification",
        name: "AWAS Extract Elements",
        html: "",
      };
    default:
      throw new Error(`Unknown step type: ${type}`);
  }
}

/**
 * Create a default empty workflow
 */
export function createDefaultWorkflow(): Omit<
  UnifiedWorkflow,
  "id" | "created_at" | "modified_at"
> {
  return {
    name: "",
    description: "",
    setup_steps: [],
    verification_steps: [],
    agentic_steps: [],
    completion_steps: [],
    category: "general",
    tags: [],
  };
}

/**
 * Check if a workflow is empty (has no steps)
 */
export function isWorkflowEmpty(workflow: UnifiedWorkflow): boolean {
  return (
    workflow.setup_steps.length === 0 &&
    workflow.verification_steps.length === 0 &&
    workflow.agentic_steps.length === 0 &&
    (workflow.completion_steps ?? []).length === 0
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
  switch (stepType) {
    case "script":
      return phase === "setup" || phase === "completion";
    case "state":
    case "workflow_ref":
    case "gui_action":
      return phase === "setup" || phase === "verification";
    case "api_request":
    case "mcp_call":
      return phase === "setup" || phase === "verification" || phase === "completion";
    case "test":
    case "check":
    case "screenshot":
      return phase === "verification";
    case "prompt":
      // Prompts can exist in all phases
      return true;
    // AWAS step types
    case "awas_discover":
    case "awas_check_support":
      return phase === "setup";
    case "awas_execute":
    case "awas_list_actions":
      return phase === "setup" || phase === "verification";
    case "awas_extract_elements":
      return phase === "verification";
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
