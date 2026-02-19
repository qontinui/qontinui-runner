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
export type StepOutputType = "command" | "check" | "ui_bridge";

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
// Command Step Output
// ============================================================================

/**
 * Output from a command step (shell commands, API requests, MCP calls, checks).
 */
export interface CommandStepOutput extends BaseStepOutput {
  step_type: "command";
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
// UI Bridge Step Output
// ============================================================================

/**
 * Output from a UI Bridge step.
 */
export interface UiBridgeStepOutput extends BaseStepOutput {
  step_type: "ui_bridge";
  /** Action that was performed */
  action: "navigate" | "execute" | "assert" | "snapshot";
  /** URL that was navigated to or targeted */
  url?: string;
  /** Instruction that was executed */
  instruction?: string;
  /** Assertion result details */
  assertion_result?: {
    target: string;
    type: string;
    expected?: string;
    actual?: string;
    passed: boolean;
  };
  /** Snapshot data (element tree) */
  snapshot?: unknown;
  /** Response from the UI Bridge SDK */
  response?: unknown;
}

// ============================================================================
// Step Output Union
// ============================================================================

/**
 * Discriminated union of all step output types.
 * Use the `step_type` field to narrow the type.
 */
export type StepOutput = CommandStepOutput | CheckStepOutput | UiBridgeStepOutput;

// ============================================================================
// Type Guards
// ============================================================================

export function isCommandOutput(output: StepOutput): output is CommandStepOutput {
  return output.step_type === "command";
}

export function isCheckOutput(output: StepOutput): output is CheckStepOutput {
  return output.step_type === "check";
}

export function isUiBridgeOutput(output: StepOutput): output is UiBridgeStepOutput {
  return output.step_type === "ui_bridge";
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
