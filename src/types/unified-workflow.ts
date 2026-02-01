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
 * State Navigation Step (Setup, Verification, or Completion)
 *
 * Navigates to a stored application state using Qontinui vision.
 * Returns a boolean based on whether the state was successfully reached.
 */
export interface StateStep extends BaseStep {
  type: "state";
  phase: "setup" | "verification" | "completion";
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
 * Workflow Reference Step (Setup, Verification, or Completion)
 *
 * Executes another saved workflow.
 * Note: In setup/completion phase, this step always runs once regardless of run_on_subsequent_iterations.
 */
export interface WorkflowRefStep extends BaseStep {
  type: "workflow_ref";
  phase: "setup" | "verification" | "completion";
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

/**
 * Macro Step (Setup, Verification, or Completion)
 *
 * Executes a saved macro (deterministic sequence of GUI actions).
 * Can be used in any non-agentic phase for GUI setup or cleanup.
 * Note: Macros don't return a boolean, so in verification they contribute
 * context but don't affect the loop decision.
 */
export interface MacroRefStep extends BaseStep {
  type: "macro";
  phase: "setup" | "verification" | "completion";
  /** Reference to saved macro ID */
  macro_id: string;
  /** Macro name (for display) */
  macro_name?: string;
  /** Monitor index (0 = primary, undefined = all) */
  monitor_index?: number;
}

// -----------------------------------------------------------------------------
// GUI Action Steps (Setup, Verification, or Completion)
// -----------------------------------------------------------------------------

/**
 * GUI Action types
 */
export type GuiActionType = "click" | "double_click" | "right_click" | "type" | "hotkey" | "scroll";

/**
 * GUI Action Step (Setup, Verification, or Completion)
 *
 * Mouse and keyboard actions using Qontinui vision-based automation.
 * Returns a boolean based on whether the target element was found.
 * In verification, can be used to check if elements exist or to prepare state.
 */
export interface GuiActionStep extends BaseStep {
  type: "gui_action";
  phase: "setup" | "verification" | "completion";
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
 * Test Step (Setup, Verification, or Completion)
 *
 * Runs verification checks to test target functionality.
 * Returns a boolean based on test pass/fail status.
 */
export interface TestStep extends BaseStep {
  type: "test";
  phase: "setup" | "verification" | "completion";
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
  /** Timeout in seconds */
  timeout_seconds?: number;
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
  /** Reference to saved Playwright script ID */
  script_id?: string;
  /** Inline Playwright script content */
  script_content?: string;
  /** Target URL for Playwright tests */
  target_url?: string;
  /** Which setup script to fuse with (for Playwright tests) */
  fused_script_id?: string;
  /** Execution mode: fresh session or continue after previous test */
  execution_mode?: PlaywrightExecutionMode;
}

/**
 * Screenshot Step (Setup, Verification, or Completion)
 *
 * Captures current screen state for AI analysis or logging.
 * Does not return a boolean - contributes context but doesn't affect loop decision.
 */
export interface ScreenshotStep extends BaseStep {
  type: "screenshot";
  phase: "setup" | "verification" | "completion";
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
 * Check Step (Setup, Verification, or Completion)
 *
 * Runs code quality checks like linting, formatting, and type checking.
 * Supports auto-fix for tools that can automatically correct issues.
 * Returns a boolean based on check pass/fail status.
 */
export interface CheckStep extends BaseStep {
  type: "check";
  phase: "setup" | "verification" | "completion";
  /** Type of check to run */
  check_type: CheckType;
  /** Tool name (UI helper, e.g., "eslint", "ruff", "mypy") */
  tool?: string;
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

/**
 * Check Group Step (Setup, Verification, or Completion)
 *
 * Runs all checks in a saved check group as a single step.
 * The group's stop_on_failure and run_in_parallel settings are respected.
 * Returns a boolean based on aggregate check pass/fail status.
 */
export interface CheckGroupStep extends BaseStep {
  type: "check_group";
  phase: "setup" | "verification" | "completion";
  /** Reference to saved check group ID */
  check_group_id: string;
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
  /**
   * True for the auto-generated summary step (locked to last position in completion phase).
   * Summary steps cannot be moved and are always executed last.
   */
  is_summary_step?: boolean;
}

/**
 * Shell Command Step (Setup, Verification, or Completion)
 *
 * Runs an arbitrary shell command for environment setup, verification, or cleanup tasks.
 * Returns a boolean based on exit code (0 = success).
 * Examples: git operations, file management, environment variables, verification scripts.
 */
export interface ShellCommandStep extends BaseStep {
  type: "shell_command";
  phase: "setup" | "verification" | "completion";
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
  /** Tool description (for display) */
  tool_description?: string;

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
  | MacroRefStep
  | GuiActionStep
  | ApiRequestStep
  | McpCallStep
  | TestStep
  | CheckStep
  | CheckGroupStep
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
 * All step types are allowed in setup (except agentic-only)
 */
export type SetupStep =
  | ScriptStep
  | StateStep
  | WorkflowRefStep
  | MacroRefStep
  | GuiActionStep
  | ApiRequestStep
  | McpCallStep
  | PromptStep
  | ShellCommandStep
  | TestStep
  | CheckStep
  | CheckGroupStep
  | ScreenshotStep
  | AwasDiscoverStep
  | AwasExecuteStep
  | AwasCheckSupportStep
  | AwasListActionsStep;

/**
 * Verification phase step types
 * All step types are allowed in verification (except agentic-only)
 * Steps that return boolean affect the loop; others contribute context
 */
export type VerificationStep =
  | ScriptStep
  | StateStep
  | WorkflowRefStep
  | MacroRefStep
  | GuiActionStep
  | ApiRequestStep
  | McpCallStep
  | PromptStep
  | ShellCommandStep
  | TestStep
  | CheckStep
  | CheckGroupStep
  | ScreenshotStep
  | AwasDiscoverStep
  | AwasExecuteStep
  | AwasCheckSupportStep
  | AwasListActionsStep
  | AwasExtractElementsStep;

/**
 * Agentic phase step types
 */
export type AgenticStep = PromptStep;

/**
 * Completion phase step types
 * All step types are allowed in completion (except agentic-only)
 * Runs once after the verification/agentic loop exits
 */
export type CompletionStep =
  | ScriptStep
  | StateStep
  | WorkflowRefStep
  | MacroRefStep
  | GuiActionStep
  | ApiRequestStep
  | McpCallStep
  | PromptStep
  | ShellCommandStep
  | TestStep
  | CheckStep
  | CheckGroupStep
  | ScreenshotStep;

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
   * - "default": Use the global default profile (from Settings → Log Sources)
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
 * Note: All step types are available in setup, verification, and completion.
 * Only agentic phase is restricted to AI Prompt only.
 */
export const STEP_TYPES: Record<WorkflowPhase, StepTypeInfo[]> = {
  setup: [
    {
      type: "script",
      label: "Playwright Script",
      description: "Browser automation with Playwright",
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
      type: "macro",
      label: "Run Macro",
      description: "Execute a saved macro (action sequence)",
      icon: "Layers",
      color: "pink",
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
      description: "Make HTTP requests to APIs",
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
    // Tests in setup
    {
      type: "test_playwright",
      label: "Playwright Test",
      description: "Browser assertions and checks",
      icon: "TestTube2",
      color: "green",
      phase: "setup",
    },
    {
      type: "test_vision",
      label: "Qontinui Vision Test",
      description: "Visual element detection",
      icon: "Eye",
      color: "cyan",
      phase: "setup",
    },
    {
      type: "test_python",
      label: "Python Test",
      description: "White-box unit tests",
      icon: "Code",
      color: "yellow",
      phase: "setup",
    },
    {
      type: "test_repository",
      label: "Repository Test",
      description: "Run tests from your repo (pytest, jest, cargo)",
      icon: "Package",
      color: "indigo",
      phase: "setup",
    },
    {
      type: "test_custom",
      label: "Custom Test Command",
      description: "Any shell command for testing",
      icon: "Terminal",
      color: "gray",
      phase: "setup",
    },
    // Checks in setup
    {
      type: "check_lint",
      label: "Lint Check",
      description: "Run linting checks (ruff, eslint, clippy)",
      icon: "AlertTriangle",
      color: "cyan",
      phase: "setup",
    },
    {
      type: "check_format",
      label: "Format Check",
      description: "Run formatting checks (black, prettier, rustfmt)",
      icon: "AlignLeft",
      color: "cyan",
      phase: "setup",
    },
    {
      type: "check_typecheck",
      label: "Type Check",
      description: "Run type checking (mypy, tsc)",
      icon: "FileType",
      color: "cyan",
      phase: "setup",
    },
    {
      type: "check_analyze",
      label: "Code Analysis",
      description: "Run code analysis",
      icon: "Search",
      color: "indigo",
      phase: "setup",
    },
    {
      type: "check_security",
      label: "Security Check",
      description: "Run security scans",
      icon: "Shield",
      color: "red",
      phase: "setup",
    },
    {
      type: "check_custom",
      label: "Custom Check",
      description: "Run custom check command",
      icon: "Terminal",
      color: "cyan",
      phase: "setup",
    },
    {
      type: "screenshot",
      label: "Screenshot",
      description: "Capture current screen state",
      icon: "Camera",
      color: "pink",
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
    // Tests (primary verification steps)
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
      label: "Custom Test Command",
      description: "Any shell command for testing",
      icon: "Terminal",
      color: "gray",
      phase: "verification",
    },
    // Checks (primary verification steps)
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
      type: "check_custom",
      label: "Custom Check",
      description: "Run custom check command",
      icon: "Terminal",
      color: "cyan",
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
    // Navigation and automation
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
      description: "Click, type, or press hotkeys",
      icon: "MousePointer2",
      color: "orange",
      phase: "verification",
    },
    {
      type: "macro",
      label: "Run Macro",
      description: "Execute a saved macro (action sequence)",
      icon: "Layers",
      color: "pink",
      phase: "verification",
    },
    {
      type: "script",
      label: "Playwright Script",
      description: "Browser automation with Playwright",
      icon: "FileCode",
      color: "emerald",
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
      type: "shell_command",
      label: "Shell Command",
      description: "Run shell commands for verification",
      icon: "Terminal",
      color: "gray",
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
    {
      type: "workflow_ref",
      label: "Run Workflow",
      description: "Execute another saved workflow",
      icon: "GitBranch",
      color: "purple",
      phase: "completion",
    },
    {
      type: "state",
      label: "Navigate to State",
      description: "Return to a specific application state",
      icon: "Navigation",
      color: "blue",
      phase: "completion",
    },
    {
      type: "macro",
      label: "Run Macro",
      description: "Execute a saved macro (action sequence)",
      icon: "Layers",
      color: "pink",
      phase: "completion",
    },
    {
      type: "gui_action",
      label: "GUI Action",
      description: "Click, type, or press hotkeys",
      icon: "MousePointer2",
      color: "orange",
      phase: "completion",
    },
    // Tests in completion
    {
      type: "test_playwright",
      label: "Playwright Test",
      description: "Final browser assertions",
      icon: "TestTube2",
      color: "green",
      phase: "completion",
    },
    {
      type: "test_vision",
      label: "Qontinui Vision Test",
      description: "Final visual verification",
      icon: "Eye",
      color: "cyan",
      phase: "completion",
    },
    {
      type: "test_python",
      label: "Python Test",
      description: "Final unit tests",
      icon: "Code",
      color: "yellow",
      phase: "completion",
    },
    {
      type: "test_repository",
      label: "Repository Test",
      description: "Run final tests from repo",
      icon: "Package",
      color: "indigo",
      phase: "completion",
    },
    {
      type: "test_custom",
      label: "Custom Test Command",
      description: "Any shell command for final testing",
      icon: "Terminal",
      color: "gray",
      phase: "completion",
    },
    // Checks in completion
    {
      type: "check_lint",
      label: "Lint Check",
      description: "Final linting (ruff, eslint, clippy)",
      icon: "AlertTriangle",
      color: "cyan",
      phase: "completion",
    },
    {
      type: "check_format",
      label: "Format Check",
      description: "Final formatting (black, prettier, rustfmt)",
      icon: "AlignLeft",
      color: "cyan",
      phase: "completion",
    },
    {
      type: "check_typecheck",
      label: "Type Check",
      description: "Final type checking (mypy, tsc)",
      icon: "FileType",
      color: "cyan",
      phase: "completion",
    },
    {
      type: "check_analyze",
      label: "Code Analysis",
      description: "Final code analysis",
      icon: "Search",
      color: "indigo",
      phase: "completion",
    },
    {
      type: "check_security",
      label: "Security Check",
      description: "Final security scans",
      icon: "Shield",
      color: "red",
      phase: "completion",
    },
    {
      type: "check_custom",
      label: "Custom Check",
      description: "Run custom check command",
      icon: "Terminal",
      color: "cyan",
      phase: "completion",
    },
    {
      type: "screenshot",
      label: "Screenshot",
      description: "Capture final screen state",
      icon: "Camera",
      color: "pink",
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
        phase: phase as "setup" | "verification" | "completion",
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

  // All other step types can exist in setup, verification, and completion
  switch (stepType) {
    case "script":
    case "state":
    case "gui_action":
    case "workflow_ref":
    case "macro":
    case "api_request":
    case "mcp_call":
    case "test":
    case "check":
    case "check_group":
    case "screenshot":
    case "prompt":
    case "shell_command":
      return true;
    // AWAS step types
    case "awas_discover":
    case "awas_check_support":
    case "awas_execute":
    case "awas_list_actions":
    case "awas_extract_elements":
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
