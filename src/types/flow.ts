/**
 * Flow Designer types
 *
 * Types for deterministic, step-by-step workflow execution.
 * Inspired by CrewAI's Flows system.
 */

/** Merge strategy for parallel steps */
export type ParallelMerge = "wait_all" | "wait_any" | { wait_n: number };

/** Status of a flow execution */
export type FlowStatus =
  | "pending"
  | "running"
  | "waiting_for_input"
  | "completed"
  | "failed"
  | "cancelled";

/** Types of flow steps */
export type StepType =
  | {
      type: "agent";
      role: string;
      prompt: string;
      max_iterations: number | null;
      next: string;
    }
  | {
      type: "tool";
      tool_id: string;
      inputs: Record<string, unknown>;
      next: string;
    }
  | {
      type: "conditional";
      condition: Condition;
      then_step: string;
      else_step: string;
    }
  | {
      type: "parallel";
      branches: string[];
      merge_strategy: ParallelMerge;
      next: string;
    }
  | {
      type: "human_input";
      prompt: string;
      options: string[];
      next: string;
    }
  | {
      type: "transform";
      expression: string;
      output_key: string;
      next: string;
    }
  | {
      type: "wait";
      seconds: number;
      next: string;
    }
  | {
      type: "loop";
      over: string;
      body_step: string;
      next: string;
    }
  | { type: "end" }
  | { type: "fail"; error: string };

/** Condition for conditional branching (matches Rust serde with tag="type", rename_all="snake_case") */
export type Condition =
  | { type: "equals"; left: string; right: unknown }
  | { type: "not_equals"; left: string; right: unknown }
  | { type: "greater_than"; left: string; right: number }
  | { type: "less_than"; left: string; right: number }
  | { type: "is_true"; value: string }
  | { type: "is_false"; value: string }
  | { type: "is_null"; value: string }
  | { type: "is_not_null"; value: string }
  | { type: "contains"; value: string; substring: string }
  | { type: "and"; conditions: Condition[] }
  | { type: "or"; conditions: Condition[] }
  | { type: "not"; condition: Condition }
  | { type: "expression"; expr: string };

/** Input parameter for a flow */
export interface FlowInput {
  name: string;
  description: string | null;
  required: boolean;
  default_value: unknown | null;
}

/** Output definition for a flow */
export interface FlowOutput {
  name: string;
  description: string | null;
  source: string;
}

/** A step in a flow */
export interface FlowStep {
  /** Step ID */
  id: string;
  /** Step name */
  name: string;
  /** Step type */
  step_type: StepType;
  /** Description */
  description: string | null;
  /** Timeout for this step in seconds */
  timeout_secs: number | null;
  /** Whether to continue on error */
  continue_on_error: boolean;
  /** Retry configuration */
  retry_count: number;
  /** Input mappings from flow context */
  input_mappings: Record<string, string>;
  /** Condition for executing this step */
  condition: Condition | null;
}

/** A flow-based workflow definition */
export interface Flow {
  /** Unique identifier */
  id: string;
  /** Human-readable name */
  name: string;
  /** Description */
  description: string | null;
  /** Steps in the flow (keyed by step ID) */
  steps: Record<string, FlowStep>;
  /** Starting step ID */
  start_step: string | null;
  /** Global timeout in seconds */
  timeout_secs: number | null;
  /** Input parameters */
  inputs: FlowInput[];
  /** Output definitions */
  outputs: FlowOutput[];
  /** Tags */
  tags: string[];
  /** Version */
  version: string;
}

/** Record of a step execution */
export interface StepExecution {
  /** Step ID */
  step_id: string;
  /** When started */
  started_at: string;
  /** When completed */
  completed_at: string | null;
  /** Duration in milliseconds */
  duration_ms: number | null;
  /** Whether successful */
  success: boolean;
  /** Error if failed */
  error: string | null;
  /** Output values */
  outputs: Record<string, unknown>;
}

/** Runtime state during flow execution */
export interface FlowState {
  /** Flow instance ID */
  instance_id: string;
  /** Flow definition ID */
  flow_id: string;
  /** Current step ID */
  current_step: string | null;
  /** Step execution history */
  history: StepExecution[];
  /** Flow context (variables, outputs) */
  context: Record<string, unknown>;
  /** Flow status */
  status: FlowStatus;
  /** When started */
  started_at: string;
  /** When completed */
  completed_at: string | null;
  /** Error if failed */
  error: string | null;
}

/** Summary of a flow for listing */
export interface FlowSummary {
  id: string;
  name: string;
  description: string | null;
  step_count: number;
  tags: string[];
  version: string;
}

/** Summary of a flow execution */
export interface FlowExecutionSummary {
  instance_id: string;
  flow_id: string;
  status: string;
  current_step: string | null;
  started_at: string;
  completed_at: string | null;
}

/** Step type labels for UI */
export const STEP_TYPE_LABELS: Record<string, string> = {
  agent: "Agent",
  tool: "Tool",
  conditional: "Conditional",
  parallel: "Parallel",
  human_input: "Human Input",
  transform: "Transform",
  wait: "Wait",
  loop: "Loop",
  end: "End",
  fail: "Fail",
};

/** Step type colors for visualization */
export const STEP_TYPE_COLORS: Record<string, string> = {
  agent: "#22c55e", // green
  tool: "#3b82f6", // blue
  conditional: "#eab308", // yellow
  parallel: "#a855f7", // purple
  human_input: "#f97316", // orange
  transform: "#6b7280", // gray
  wait: "#06b6d4", // cyan
  loop: "#14b8a6", // teal
  end: "#10b981", // emerald
  fail: "#ef4444", // red
};

/** Helper to get step type from StepType union */
export function getStepTypeName(stepType: StepType): string {
  return stepType.type;
}

/** Helper to create a simple condition */
export function createExpressionCondition(expr: string): Condition {
  return { type: "expression", expr };
}

// ============================================================================
// Enhanced Query Types (Tag Filtering, Execution Filtering, Pagination)
// ============================================================================

/** Filter options for flow execution query */
export interface FlowExecutionFilter {
  flow_id?: string;
  status?: string;
}

/** Paginated flow execution result */
export interface PaginatedFlowExecutionResult {
  items: FlowExecutionSummary[];
  total: number;
  offset: number;
  limit: number;
}
