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
 *
 * Discriminant values mirror the Python schema wire tags in
 * `qontinui-schemas/src/qontinui_schemas/generated/per_type/` — the canonical
 * form is `<snake_case>` without the trailing `_step` (e.g. `code_execution`,
 * not `code_execution_step`), matching the Rust/serde internal tagging.
 */
export type StepOutputType =
  | "command"
  | "check"
  | "ui_bridge"
  | "prompt"
  | "code_execution"
  | "pathfinding"
  | "native_accessibility"
  | "execute_playbook"
  | "shell_command";

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
// Prompt Step Output
// ============================================================================

/**
 * Phase in which a prompt step ran. Mirrors
 * `PromptStepPhase` in the Python schema.
 */
export type PromptStepPhase = "setup" | "verification" | "agentic" | "completion";

/**
 * Output from a prompt (AI task instructions) step.
 *
 * Input schema: `qontinui-schemas/.../per_type/prompt_step.py` (`PromptStep`).
 * Response schema: `.../per_type/run_prompt_response.py` (`RunPromptResponse`).
 *
 * The runner's `run_prompt` endpoint either runs synchronously (populating
 * `response_text` / `output_text`) or spawns a background AI session that
 * is later joined via `session_id` + `task_run_id`.
 */
export interface PromptStepOutput extends BaseStepOutput {
  step_type: "prompt";
  /** Prompt body (content that was sent to the model). */
  prompt: string;
  /** Saved prompt ID (when the body was a reference). */
  prompt_id?: string;
  /** AI provider override used (if any). */
  provider?: string;
  /** Model override used (if any). */
  model?: string;
  /** Phase in which the prompt ran. */
  phase?: PromptStepPhase;
  /** Final response text from the model. */
  response_text?: string;
  /** Immediate / streaming output text (synchronous runs). */
  output_text?: string;
  /** ID of the created AI session, if the prompt spawned a background run. */
  session_id?: string;
  /** ID of the created task run, if any. */
  task_run_id?: string;
  /** Path to the log file for the session. */
  log_file?: string;
  /** Variables extracted from the prompt response (mirrors `extract`). */
  extractions?: Record<string, unknown>;
  /** Acceptance criterion IDs verified by this prompt. */
  criterion_ids?: string[];
  /** Whether the prompt is marked as the end-of-completion summary step. */
  is_summary_step?: boolean;
}

// ============================================================================
// Code Execution Step Output
// ============================================================================

/**
 * Sandbox enforcement mode — mirrors the `sandbox_mode` field on the
 * Python `CodeExecutionStep` schema.
 */
export type CodeExecutionSandboxMode = "enforce" | "warn";

/**
 * Output from an inline-Python / Python-file execution step.
 *
 * Input schema: `qontinui-schemas/.../per_type/code_execution_step.py`
 * (`CodeExecutionStep`, wire tag `"code_execution"`).
 */
export interface CodeExecutionStepOutput extends BaseStepOutput {
  step_type: "code_execution";
  /** Inline Python source that was executed (if present). */
  code?: string;
  /** Path to the Python file that was executed (if any). */
  code_file?: string;
  /** Sandbox enforcement mode the runner used. */
  sandbox_mode?: CodeExecutionSandboxMode;
  /** Timeout in seconds, if configured. */
  timeout_seconds?: number;
  /** Process exit code from the Python runtime. */
  exit_code?: number;
  /** Captured standard output. */
  stdout?: string;
  /** Captured standard error. */
  stderr?: string;
  /** Structured return value parsed from stdout (e.g., printed JSON trailer). */
  return_value?: unknown;
  /** Variables extracted from the run (mirrors `extract`). */
  extractions?: Record<string, unknown>;
  /** Runtime/exception class that caused failure, if any. */
  exception_type?: string;
  /** Python traceback string, if the run raised. */
  traceback?: string;
}

// ============================================================================
// Pathfinding Step Output
// ============================================================================

/**
 * A single step on a computed path — mirrors
 * `PathfindingStep` in the Python schema.
 */
export interface PathfindingPathEntry {
  /** Logical transition ID taken on this edge. */
  transition_id: string;
  /** Display name of the transition. */
  transition_name: string;
  /** States required to be active before this edge. */
  from_states: string[];
  /** States activated by this edge. */
  activate_states: string[];
  /** States exited by this edge. */
  exit_states: string[];
  /** Cost of this edge. */
  path_cost: number;
}

/**
 * Output from a pathfinding step (state-machine path computation).
 *
 * Input schema (edge): `.../per_type/pathfinding_step.py` (`PathfindingStep`).
 * Result envelope: `.../per_type/pathfinding_result.py` (`PathfindingResult`).
 *
 * The step_output represents the resolved search: whether a path was found,
 * the ordered edges, and the total cost.
 */
export interface PathfindingStepOutput extends BaseStepOutput {
  step_type: "pathfinding";
  /** Whether a valid path was found. */
  found: boolean;
  /** Ordered edges on the computed path (empty when `found` is `false`). */
  path: PathfindingPathEntry[];
  /** Sum of `path_cost` across edges. */
  total_cost: number;
  /** Starting state(s) the search used. */
  from_states?: string[];
  /** Target state(s) the search aimed at. */
  to_states?: string[];
  /** Pathfinding-specific error message (distinct from step-level error). */
  pathfinding_error?: string;
}

// ============================================================================
// Native Accessibility Step Output
// ============================================================================

/**
 * Accessibility action kind — mirrors the Python `A11yAction` enum.
 */
export type NativeAccessibilityAction =
  | "capture"
  | "click"
  | "type"
  | "focus"
  | "query"
  | "ai_context";

/**
 * Output from a native accessibility step (UIA / AT-SPI / AX).
 *
 * Input schema: `.../per_type/native_accessibility_step.py`
 * (`NativeAccessibilityStep`, wire tag `"native_accessibility"`).
 * Snapshot shape: `.../per_type/accessibility_snapshot.py`.
 *
 * Shape depends on `action`:
 * - `capture` / `ai_context` → `snapshot` populated
 * - `query` → `matches` populated (subset of tree nodes)
 * - `click` / `type` / `focus` → `ref_id` target echoed, `snapshot` optional
 */
export interface NativeAccessibilityStepOutput extends BaseStepOutput {
  step_type: "native_accessibility";
  /** Action performed. */
  action: NativeAccessibilityAction;
  /** Connection target used (`"Desktop"`, window title, or `"pid:1234"`). */
  target?: string;
  /** Backend actually used (auto/uia/atspi/ax/cdp/none). */
  backend?: string;
  /** Element ref ID operated on (for click/type/focus). */
  ref_id?: string;
  /** Text typed (for `type` action). */
  text?: string;
  /** Label filter applied (for `query`). */
  query_label?: string;
  /** Role filter applied (for `query`). */
  query_role?: string;
  /** Full accessibility snapshot (for `capture` / `ai_context`). */
  snapshot?: unknown;
  /** Elements matched by the query, if `action === "query"`. */
  matches?: unknown[];
  /** Total nodes in the snapshot (summary stat). */
  total_nodes?: number;
  /** Interactive nodes in the snapshot (summary stat). */
  interactive_nodes?: number;
  /** Title of the captured window / page. */
  title?: string;
  /** URL of the captured page (web targets only). */
  url?: string;
}

// ============================================================================
// Execute Playbook Step Output
// ============================================================================

/**
 * Record of a single playbook transition attempt.
 */
export interface PlaybookTransitionRecord {
  /** Logical transition ID attempted. */
  transition_id: string;
  /** Display name of the transition. */
  transition_name?: string;
  /** Whether the transition succeeded. */
  success: boolean;
  /** Duration of this transition in milliseconds. */
  duration_ms?: number;
  /** Error message if this transition failed. */
  error?: string;
}

/**
 * Output from an execute_playbook step (drives a state machine through
 * recorded transitions from a playbook).
 *
 * Input schema: `.../per_type/execute_playbook_step.py`
 * (`ExecutePlaybookStep`, wire tag `"execute_playbook"`).
 */
export interface ExecutePlaybookStepOutput extends BaseStepOutput {
  step_type: "execute_playbook";
  /** Path to the playbook file, if one was used. */
  playbook_path?: string;
  /** Inline playbook markdown content, if provided. */
  content?: string;
  /** Ordered record of the transitions the playbook drove. */
  transitions: PlaybookTransitionRecord[];
  /** Number of transitions that succeeded. */
  transitions_succeeded: number;
  /** Number of transitions that failed. */
  transitions_failed: number;
  /** Final state(s) of the state machine after the run. */
  final_states?: string[];
  /** Whether the playbook ran to completion. */
  completed: boolean;
}

// ============================================================================
// Step Output Union
// ============================================================================

/**
 * Discriminated union of all step output types.
 * Use the `step_type` field to narrow the type.
 */
export type StepOutput =
  | CommandStepOutput
  | CheckStepOutput
  | UiBridgeStepOutput
  | PromptStepOutput
  | CodeExecutionStepOutput
  | PathfindingStepOutput
  | NativeAccessibilityStepOutput
  | ExecutePlaybookStepOutput;

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

export function isPromptOutput(output: StepOutput): output is PromptStepOutput {
  return output.step_type === "prompt";
}

export function isCodeExecutionOutput(output: StepOutput): output is CodeExecutionStepOutput {
  return output.step_type === "code_execution";
}

export function isPathfindingOutput(output: StepOutput): output is PathfindingStepOutput {
  return output.step_type === "pathfinding";
}

export function isNativeAccessibilityOutput(
  output: StepOutput,
): output is NativeAccessibilityStepOutput {
  return output.step_type === "native_accessibility";
}

export function isExecutePlaybookOutput(output: StepOutput): output is ExecutePlaybookStepOutput {
  return output.step_type === "execute_playbook";
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
