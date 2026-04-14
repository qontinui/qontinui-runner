/**
 * DAG Schema — TypeScript types mirroring the Rust DAG types in
 * src-tauri/src/workflow/dag_schema.rs
 *
 * Used for frontend YAML validation before submitting a workflow definition
 * to the runner backend. Kept as plain TypeScript types (no Zod dependency)
 * with a lightweight hand-rolled validator.
 */

// ─────────────────────────────────────────────────────────────────────────────
// Enum literals
// ─────────────────────────────────────────────────────────────────────────────

export type TriggerRule =
  | "all_success"
  | "one_success"
  | "all_done"
  | "none_failed_min_one_success";

export const TRIGGER_RULES: readonly TriggerRule[] = [
  "all_success",
  "one_success",
  "all_done",
  "none_failed_min_one_success",
];

export type ContextMode = "fresh" | "shared";

export const CONTEXT_MODES: readonly ContextMode[] = ["fresh", "shared"];

// ─────────────────────────────────────────────────────────────────────────────
// Nested config types
// ─────────────────────────────────────────────────────────────────────────────

/** Retry configuration for a node or workflow-level defaults. */
export interface RetryConfig {
  max_attempts: number;
  delay_ms?: number;
  backoff_multiplier?: number;
}

/** Configuration for a human-approval gate node. */
export interface ApprovalConfig {
  message: string;
  on_reject?: string;
  timeout_seconds?: number;
}

/** Default values applied to nodes that do not specify the field themselves. */
export interface NodeDefaults {
  timeout_seconds?: number;
  retry?: RetryConfig;
  context?: ContextMode;
  provider?: string;
  model?: string;
}

/** Workflow-level settings controlling orchestration behaviour. */
export interface WorkflowSettings {
  max_iterations?: number;
  rollback_policy?: string;
  isolation?: string;
  approval_gate?: boolean;
  cross_workflow_learning?: boolean;
}

// ─────────────────────────────────────────────────────────────────────────────
// Node definition
// ─────────────────────────────────────────────────────────────────────────────

/**
 * A single node in the DAG.
 *
 * Exactly one "type-discriminating" field must be set:
 * `prompt`, `command`, `check_type`, `ui_bridge_action`, `a11y_action`,
 * `loop_body`, `approval`, `workflow_ref`, or `cancel_reason`.
 */
export interface DagNodeDef {
  name?: string;
  depends_on?: string[];
  when?: string;
  trigger_rule?: TriggerRule;
  retry?: RetryConfig;
  timeout_seconds?: number;
  context?: ContextMode;

  // ── PromptNode ──────────────────────────────────────────────────────────
  prompt?: string;
  prompt_mode?: string;

  // ── CommandNode ─────────────────────────────────────────────────────────
  command?: string;
  working_directory?: string;
  fail_on_error?: boolean;

  // ── CheckNode ───────────────────────────────────────────────────────────
  check_type?: string;
  check_group_id?: string;

  // ── UiBridgeNode ────────────────────────────────────────────────────────
  ui_bridge_action?: string;
  ui_bridge_url?: string;
  ui_bridge_instruction?: string;

  // ── NativeAccessibilityNode ─────────────────────────────────────────────
  a11y_action?: string;
  a11y_target?: string;

  // ── LoopNode ────────────────────────────────────────────────────────────
  loop_body?: string[];
  until?: string;
  until_bash?: string;
  max_loop_iterations?: number;
  commit_interval?: number;

  // ── ApprovalNode ────────────────────────────────────────────────────────
  approval?: ApprovalConfig;

  // ── WorkflowRefNode ─────────────────────────────────────────────────────
  workflow_ref?: string;
  workflow_inputs?: Record<string, string>;

  // ── CancelNode ──────────────────────────────────────────────────────────
  cancel_reason?: string;

  // ── Data flow ───────────────────────────────────────────────────────────
  inputs?: Record<string, string>;
  extract?: Record<string, string>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Top-level workflow definition
// ─────────────────────────────────────────────────────────────────────────────

/** Top-level YAML workflow definition, mirrors Rust `DagWorkflowDef`. */
export interface DagWorkflowDef {
  name: string;
  description?: string;
  version?: string;
  nodes: Record<string, DagNodeDef>;
  defaults?: NodeDefaults;
  settings?: WorkflowSettings;
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation
// ─────────────────────────────────────────────────────────────────────────────

/** A single validation error with a dot-path location and message. */
export interface DagValidationError {
  path: string;
  message: string;
}

/** The type-discriminating fields that identify a node's kind. */
const NODE_TYPE_FIELDS = [
  "prompt",
  "command",
  "check_type",
  "ui_bridge_action",
  "a11y_action",
  "loop_body",
  "approval",
  "workflow_ref",
  "cancel_reason",
] as const;

type NodeTypeField = (typeof NODE_TYPE_FIELDS)[number];

function validateRetryConfig(retry: unknown, path: string, errors: DagValidationError[]): void {
  if (typeof retry !== "object" || retry === null) {
    errors.push({ path, message: "must be an object" });
    return;
  }
  const r = retry as Record<string, unknown>;
  if (
    typeof r.max_attempts !== "number" ||
    !Number.isInteger(r.max_attempts) ||
    r.max_attempts < 1
  ) {
    errors.push({ path: `${path}.max_attempts`, message: "must be a positive integer" });
  }
  if (r.delay_ms !== undefined && (typeof r.delay_ms !== "number" || r.delay_ms < 0)) {
    errors.push({ path: `${path}.delay_ms`, message: "must be a non-negative number" });
  }
  if (
    r.backoff_multiplier !== undefined &&
    (typeof r.backoff_multiplier !== "number" || r.backoff_multiplier < 1)
  ) {
    errors.push({ path: `${path}.backoff_multiplier`, message: "must be a number >= 1" });
  }
}

function validateApprovalConfig(
  approval: unknown,
  path: string,
  errors: DagValidationError[],
): void {
  if (typeof approval !== "object" || approval === null) {
    errors.push({ path, message: "must be an object" });
    return;
  }
  const a = approval as Record<string, unknown>;
  if (typeof a.message !== "string" || a.message.trim() === "") {
    errors.push({ path: `${path}.message`, message: "must be a non-empty string" });
  }
  if (a.on_reject !== undefined && typeof a.on_reject !== "string") {
    errors.push({ path: `${path}.on_reject`, message: "must be a string" });
  }
  if (
    a.timeout_seconds !== undefined &&
    (typeof a.timeout_seconds !== "number" || a.timeout_seconds <= 0)
  ) {
    errors.push({ path: `${path}.timeout_seconds`, message: "must be a positive number" });
  }
}

function validateNode(nodeId: string, node: unknown, errors: DagValidationError[]): void {
  const path = `nodes.${nodeId}`;

  if (typeof node !== "object" || node === null) {
    errors.push({ path, message: "must be an object" });
    return;
  }
  const n = node as Record<string, unknown>;

  // Validate trigger_rule if present
  if (n.trigger_rule !== undefined && !TRIGGER_RULES.includes(n.trigger_rule as TriggerRule)) {
    errors.push({
      path: `${path}.trigger_rule`,
      message: `must be one of: ${TRIGGER_RULES.join(", ")}`,
    });
  }

  // Validate context mode if present
  if (n.context !== undefined && !CONTEXT_MODES.includes(n.context as ContextMode)) {
    errors.push({
      path: `${path}.context`,
      message: `must be one of: ${CONTEXT_MODES.join(", ")}`,
    });
  }

  // Validate depends_on
  if (n.depends_on !== undefined) {
    if (!Array.isArray(n.depends_on)) {
      errors.push({ path: `${path}.depends_on`, message: "must be an array of strings" });
    } else {
      n.depends_on.forEach((dep, i) => {
        if (typeof dep !== "string") {
          errors.push({ path: `${path}.depends_on[${i}]`, message: "must be a string" });
        }
      });
    }
  }

  // Validate retry if present
  if (n.retry !== undefined) {
    validateRetryConfig(n.retry, `${path}.retry`, errors);
  }

  // Validate approval if present
  if (n.approval !== undefined) {
    validateApprovalConfig(n.approval, `${path}.approval`, errors);
  }

  // Validate timeout_seconds if present
  if (
    n.timeout_seconds !== undefined &&
    (typeof n.timeout_seconds !== "number" || n.timeout_seconds <= 0)
  ) {
    errors.push({ path: `${path}.timeout_seconds`, message: "must be a positive number" });
  }

  // Ensure at least one type-discriminating field is set
  const presentTypeFields = NODE_TYPE_FIELDS.filter(
    (f) => n[f as string] !== undefined,
  ) as NodeTypeField[];

  if (presentTypeFields.length === 0) {
    errors.push({
      path,
      message: `must have at least one of: ${NODE_TYPE_FIELDS.join(", ")}`,
    });
  }

  // Validate loop_body items are strings
  if (n.loop_body !== undefined) {
    if (!Array.isArray(n.loop_body)) {
      errors.push({ path: `${path}.loop_body`, message: "must be an array of strings" });
    } else {
      n.loop_body.forEach((item, i) => {
        if (typeof item !== "string") {
          errors.push({ path: `${path}.loop_body[${i}]`, message: "must be a string" });
        }
      });
    }
  }
}

function validateNodeDefaults(defaults: unknown, errors: DagValidationError[]): void {
  if (defaults === undefined || defaults === null) return;
  if (typeof defaults !== "object") {
    errors.push({ path: "defaults", message: "must be an object" });
    return;
  }
  const d = defaults as Record<string, unknown>;
  if (d.retry !== undefined) {
    validateRetryConfig(d.retry, "defaults.retry", errors);
  }
  if (d.context !== undefined && !CONTEXT_MODES.includes(d.context as ContextMode)) {
    errors.push({
      path: "defaults.context",
      message: `must be one of: ${CONTEXT_MODES.join(", ")}`,
    });
  }
  if (
    d.timeout_seconds !== undefined &&
    (typeof d.timeout_seconds !== "number" || d.timeout_seconds <= 0)
  ) {
    errors.push({ path: "defaults.timeout_seconds", message: "must be a positive number" });
  }
}

/**
 * Validate an unknown value (typically parsed YAML) against the
 * `DagWorkflowDef` schema.
 *
 * Returns an array of errors. An empty array means the document is valid.
 *
 * @example
 * ```ts
 * import yaml from "js-yaml";
 * import { validateDagWorkflow } from "@/lib/dag-schema";
 *
 * const doc = yaml.load(yamlText);
 * const errors = validateDagWorkflow(doc);
 * if (errors.length > 0) {
 *   console.error("Invalid workflow:", errors);
 * }
 * ```
 */
export function validateDagWorkflow(value: unknown): DagValidationError[] {
  const errors: DagValidationError[] = [];

  if (typeof value !== "object" || value === null) {
    errors.push({ path: "", message: "workflow definition must be an object" });
    return errors;
  }

  const def = value as Record<string, unknown>;

  // Required top-level fields
  if (typeof def.name !== "string" || def.name.trim() === "") {
    errors.push({ path: "name", message: "must be a non-empty string" });
  }
  if (def.description !== undefined && typeof def.description !== "string") {
    errors.push({ path: "description", message: "must be a string" });
  }
  if (def.version !== undefined && typeof def.version !== "string") {
    errors.push({ path: "version", message: "must be a string" });
  }

  // Nodes map
  if (typeof def.nodes !== "object" || def.nodes === null || Array.isArray(def.nodes)) {
    errors.push({ path: "nodes", message: "must be a non-null object (node map)" });
  } else {
    for (const [nodeId, nodeDef] of Object.entries(def.nodes)) {
      validateNode(nodeId, nodeDef, errors);
    }
  }

  // Validate depends_on references after all nodes are known
  if (typeof def.nodes === "object" && def.nodes !== null && !Array.isArray(def.nodes)) {
    const nodeIds = new Set(Object.keys(def.nodes as Record<string, unknown>));
    for (const [nodeId, nodeDef] of Object.entries(def.nodes as Record<string, unknown>)) {
      if (typeof nodeDef === "object" && nodeDef !== null) {
        const n = nodeDef as Record<string, unknown>;
        if (Array.isArray(n.depends_on)) {
          n.depends_on.forEach((dep: unknown, i: number) => {
            if (typeof dep === "string" && !nodeIds.has(dep)) {
              errors.push({
                path: `nodes.${nodeId}.depends_on[${i}]`,
                message: `references unknown node "${dep}"`,
              });
            }
          });
        }
      }
    }
  }

  // Optional sections
  if (def.defaults !== undefined) {
    validateNodeDefaults(def.defaults, errors);
  }

  return errors;
}

/**
 * Type guard: returns true if `value` is a valid `DagWorkflowDef`.
 */
export function isDagWorkflowDef(value: unknown): value is DagWorkflowDef {
  return validateDagWorkflow(value).length === 0;
}
