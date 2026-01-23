/**
 * Step Output Types
 *
 * Type definitions for outputs from different step types in the automation workflow.
 * These types enable a modular piping system where any step's output can be collected
 * and used for verification test generation.
 */

// ============================================================================
// Step Output Type Discriminant
// ============================================================================

/**
 * All supported step output types.
 * This union allows for type-safe discrimination of step outputs.
 */
export type StepOutputType =
  | "api_request"
  | "gui_action"
  | "shell_command"
  | "mcp_call"
  | "screenshot"
  | "workflow_ref"
  | "playwright_script"
  | "state_navigation"
  | "check";

// ============================================================================
// Base Step Output
// ============================================================================

/**
 * Common fields shared by all step outputs.
 */
export interface BaseStepOutput {
  /** Unique identifier for this output instance */
  id: string;
  /** Discriminant for the step type */
  step_type: StepOutputType;
  /** ID of the step that produced this output (from workflow config) */
  step_id?: string;
  /** Human-readable name of the step */
  step_name: string;
  /** When the step was executed */
  executed_at: string;
  /** Execution duration in milliseconds */
  duration_ms: number;
  /** Whether the step completed successfully */
  success: boolean;
  /** Error message if the step failed */
  error?: string;
}

// ============================================================================
// API Request Step Output
// ============================================================================

/**
 * Output from an API request step.
 */
export interface ApiRequestStepOutput extends BaseStepOutput {
  step_type: "api_request";
  /** HTTP method used */
  method: string;
  /** Request URL */
  url: string;
  /** Request headers */
  request_headers?: Record<string, string>;
  /** Request body (if any) */
  request_body?: string;
  /** Response status code */
  status_code?: number;
  /** Response headers */
  response_headers?: Record<string, string>;
  /** Response body (parsed or raw) */
  response: unknown;
  /** Variables extracted from the response */
  extractions?: Record<string, unknown>;
  /** Source configuration for re-executing the request */
  source_config: {
    method: string;
    url: string;
    headers?: Record<string, string>;
    body?: string;
    timeout_ms?: number;
    follow_redirects?: boolean;
  };
}

// ============================================================================
// GUI Action Step Output
// ============================================================================

/**
 * Bounding box for screen elements.
 */
export interface ScreenBoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Output from a GUI action step (click, type, scroll, etc.).
 */
export interface GuiActionStepOutput extends BaseStepOutput {
  step_type: "gui_action";
  /** Type of action performed */
  action_type: "click" | "double_click" | "right_click" | "type" | "scroll" | "drag" | "hover" | "key_press";
  /** Target location on screen */
  target_location?: {
    x: number;
    y: number;
  };
  /** Bounding box of the target element (if detected) */
  target_bounds?: ScreenBoundingBox;
  /** Target element label/name (from image recognition) */
  target_label?: string;
  /** Confidence score for target recognition (0-1) */
  confidence?: number;
  /** Text typed (for type actions) */
  typed_text?: string;
  /** Key pressed (for key_press actions) */
  key_pressed?: string;
  /** Scroll delta (for scroll actions) */
  scroll_delta?: { x: number; y: number };
  /** Screenshot before the action (base64) */
  screenshot_before?: string;
  /** Screenshot after the action (base64) */
  screenshot_after?: string;
  /** Monitor index where action was performed */
  monitor_index?: number;
}

// ============================================================================
// Shell Command Step Output
// ============================================================================

/**
 * Output from a shell command step.
 */
export interface ShellCommandStepOutput extends BaseStepOutput {
  step_type: "shell_command";
  /** The command that was executed */
  command: string;
  /** Working directory */
  working_directory?: string;
  /** Exit code from the command */
  exit_code: number;
  /** Standard output */
  stdout: string;
  /** Standard error */
  stderr: string;
  /** Environment variables used (subset of relevant ones) */
  env_vars?: Record<string, string>;
  /** Variables extracted from output */
  extractions?: Record<string, unknown>;
}

// ============================================================================
// MCP Call Step Output
// ============================================================================

/**
 * Output from an MCP (Model Context Protocol) tool call.
 */
export interface McpCallStepOutput extends BaseStepOutput {
  step_type: "mcp_call";
  /** MCP server ID */
  server_id: string;
  /** Tool name that was called */
  tool_name: string;
  /** Arguments passed to the tool */
  arguments: Record<string, unknown>;
  /** Result returned by the tool */
  result: unknown;
  /** Whether the tool returned an error */
  is_error?: boolean;
}

// ============================================================================
// Screenshot Step Output
// ============================================================================

/**
 * A detected element from vision analysis.
 */
export interface DetectedScreenElement {
  id: string;
  label: string;
  element_type: string;
  text_content?: string;
  bounding_box: ScreenBoundingBox;
  confidence: number;
}

/**
 * Output from a screenshot/capture step.
 */
export interface ScreenshotStepOutput extends BaseStepOutput {
  step_type: "screenshot";
  /** Screenshot image as base64 */
  screenshot_base64: string;
  /** Annotated screenshot with detected elements (base64) */
  annotated_screenshot_base64?: string;
  /** Image dimensions */
  dimensions: {
    width: number;
    height: number;
  };
  /** Monitor index */
  monitor_index?: number;
  /** Elements detected via vision analysis */
  detected_elements?: DetectedScreenElement[];
  /** Source of analysis */
  analysis_source?: "vision" | "ocr" | "segmentation";
}

// ============================================================================
// Workflow Reference Step Output
// ============================================================================

/**
 * Output from a workflow reference step (nested workflow execution).
 */
export interface WorkflowRefStepOutput extends BaseStepOutput {
  step_type: "workflow_ref";
  /** ID of the referenced workflow */
  workflow_id: string;
  /** Name of the referenced workflow */
  workflow_name: string;
  /** Outputs from nested steps (flattened or structured) */
  nested_outputs?: StepOutput[];
  /** Final state reached */
  final_state?: string;
  /** Variables extracted/modified by the workflow */
  output_variables?: Record<string, unknown>;
}

// ============================================================================
// Playwright Script Step Output
// ============================================================================

/**
 * Output from a Playwright script execution step.
 */
export interface PlaywrightScriptStepOutput extends BaseStepOutput {
  step_type: "playwright_script";
  /** Script that was executed */
  script_content?: string;
  /** Final URL after script execution */
  final_url?: string;
  /** Page title */
  page_title?: string;
  /** Console logs captured */
  console_logs?: Array<{
    type: "log" | "warn" | "error" | "info";
    message: string;
    timestamp: string;
  }>;
  /** Network requests made */
  network_requests?: Array<{
    url: string;
    method: string;
    status?: number;
    response_body?: unknown;
  }>;
  /** Final screenshot (base64) */
  final_screenshot?: string;
  /** Extracted data from the page */
  extracted_data?: Record<string, unknown>;
}

// ============================================================================
// State Navigation Step Output
// ============================================================================

/**
 * Output from a state navigation step.
 */
export interface StateNavigationStepOutput extends BaseStepOutput {
  step_type: "state_navigation";
  /** Target state name */
  target_state: string;
  /** Whether the target state was reached */
  reached: boolean;
  /** Path taken to reach the state */
  path_taken?: Array<{
    from_state: string;
    to_state: string;
    transition_name: string;
    action_taken?: string;
  }>;
  /** Final state reached (may differ from target if navigation failed) */
  final_state?: string;
  /** Screenshot of the final state (base64) */
  final_screenshot?: string;
}

// ============================================================================
// Check Step Output
// ============================================================================

/**
 * A single issue found by a check step.
 */
export interface CheckIssue {
  severity: "error" | "warning" | "info";
  message: string;
  location?: string;
  suggestion?: string;
}

/**
 * Output from a check/validation step.
 */
export interface CheckStepOutput extends BaseStepOutput {
  step_type: "check";
  /** Type of check performed */
  check_type: "accessibility" | "visual_regression" | "element_presence" | "text_match" | "custom";
  /** Issues found during the check */
  issues: CheckIssue[];
  /** Number of checks passed */
  checks_passed: number;
  /** Number of checks failed */
  checks_failed: number;
  /** Raw output from the check tool */
  raw_output?: string;
  /** Reference screenshot for visual regression */
  reference_screenshot?: string;
  /** Current screenshot for comparison */
  current_screenshot?: string;
}

// ============================================================================
// Step Output Union
// ============================================================================

/**
 * Discriminated union of all step output types.
 * Use the `step_type` field to narrow the type.
 */
export type StepOutput =
  | ApiRequestStepOutput
  | GuiActionStepOutput
  | ShellCommandStepOutput
  | McpCallStepOutput
  | ScreenshotStepOutput
  | WorkflowRefStepOutput
  | PlaywrightScriptStepOutput
  | StateNavigationStepOutput
  | CheckStepOutput;

// ============================================================================
// Type Guards
// ============================================================================

export function isApiRequestOutput(output: StepOutput): output is ApiRequestStepOutput {
  return output.step_type === "api_request";
}

export function isGuiActionOutput(output: StepOutput): output is GuiActionStepOutput {
  return output.step_type === "gui_action";
}

export function isShellCommandOutput(output: StepOutput): output is ShellCommandStepOutput {
  return output.step_type === "shell_command";
}

export function isMcpCallOutput(output: StepOutput): output is McpCallStepOutput {
  return output.step_type === "mcp_call";
}

export function isScreenshotOutput(output: StepOutput): output is ScreenshotStepOutput {
  return output.step_type === "screenshot";
}

export function isWorkflowRefOutput(output: StepOutput): output is WorkflowRefStepOutput {
  return output.step_type === "workflow_ref";
}

export function isPlaywrightScriptOutput(output: StepOutput): output is PlaywrightScriptStepOutput {
  return output.step_type === "playwright_script";
}

export function isStateNavigationOutput(output: StepOutput): output is StateNavigationStepOutput {
  return output.step_type === "state_navigation";
}

export function isCheckOutput(output: StepOutput): output is CheckStepOutput {
  return output.step_type === "check";
}

// ============================================================================
// Utility Types
// ============================================================================

/**
 * Extract the output type for a specific step type.
 */
export type StepOutputFor<T extends StepOutputType> = Extract<StepOutput, { step_type: T }>;

/**
 * Generate a unique ID for step outputs.
 */
export function generateStepOutputId(): string {
  return `step_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}
