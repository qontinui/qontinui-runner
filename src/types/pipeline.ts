/**
 * TypeScript types for multi-agent pipeline and workflow comparison.
 *
 * Mirrors Rust types from agentic_verification.rs and comparison.rs.
 */

// ─── Workflow Architecture ──────────────────────────────────────────────────

export type WorkflowArchitecture = "traditional" | "agentic_verification" | "multi_agent_pipeline";

// ─── Pipeline Agent Configuration ───────────────────────────────────────────

export interface PipelineAgentConfig {
  model?: string;
  provider?: string;
  max_tokens?: number;
  prompt_variant?: string;
  temperature?: number;
}

export interface MultiAgentPipelineConfig {
  spec_analyst?: PipelineAgentConfig;
  locator?: PipelineAgentConfig;
  implementer?: PipelineAgentConfig;
  verifier?: PipelineAgentConfig;
  max_parallel_implementers?: number;
  max_retries_per_subtree?: number;
  dag_strategy?: "strict" | "permissive" | "flat";
  level_strategy?: "level_by_level" | "greedy" | "critical_first";
  integration_verification?: boolean;
  max_total_iterations?: number;
}

// ─── Pipeline Execution Results ─────────────────────────────────────────────

export interface PipelineAgentTrace {
  agent_type: string;
  agent_id: string;
  run_id: string;
  input_snapshot: Record<string, unknown>;
  output_snapshot: Record<string, unknown>;
  config: PipelineAgentConfig;
  duration_ms: number;
  tokens_in: number;
  tokens_out: number;
  cost_usd: number;
  downstream_success?: boolean;
  output_quality_score?: number;
  schema_valid_first_attempt?: boolean;
  validation_retries?: number;
  validation_error_summary?: string | null;
}

export interface PipelineCriterionResult {
  criterion_id: string;
  passed: boolean;
  method_used: string;
  confidence: number;
  details: string;
  duration_ms: number;
}

export interface SubtreeLevelResult {
  level: number;
  implementer_trace: PipelineAgentTrace;
  verifier_trace: PipelineAgentTrace;
  retries: number;
  passed: boolean;
  criterion_results: PipelineCriterionResult[];
}

export interface SubtreeResult {
  subtree_id: string;
  level_results: SubtreeLevelResult[];
  retries_used: number;
  all_passed: boolean;
  regressions: string[];
}

// ─── DAG ────────────────────────────────────────────────────────────────────

export interface DAGNode {
  criterion_id: string;
  dependencies: string[];
  dependents: string[];
  level: number;
  subtree_id: string;
}

export interface DAGSubtree {
  id: string;
  root_criteria: string[];
  all_criteria: string[];
  max_level: number;
  estimated_complexity: string;
}

export interface ExecutionDAG {
  nodes: Record<string, DAGNode>;
  roots: string[];
  levels: string[][];
  subtrees: DAGSubtree[];
}

// ─── Pipeline Result ────────────────────────────────────────────────────────

export interface MultiAgentPipelineResult {
  total_iterations: number;
  goal_achieved: boolean;
  was_stopped: boolean;
  max_iterations_reached: boolean;
  subtree_results: SubtreeResult[];
  integration_result?: PipelineCriterionResult[];
  agent_traces: PipelineAgentTrace[];
  dag: ExecutionDAG;
  total_criteria: number;
  passed_criteria: number;
  total_tokens: number;
  total_cost_usd: number;
}

// ─── Comparison ─────────────────────────────────────────────────────────────

// These mirror the shapes the runner actually serves from
// `src-tauri/src/mcp/comparison_api.rs` — `GET /comparison/{id}` and
// `GET /comparisons` both return `ComparisonRunView`, whose `entries` are
// `ComparisonEntryJson`.
//
// No field on either Rust struct carries `skip_serializing_if`, so every key is
// ALWAYS present on the wire — the nullable ones are `T | null`, not `T?`. A
// consumer testing `'report' in run` would be misled by an optional marker.
//
// They used to mirror a `ComparisonRun` struct that has since been deleted, and
// three of its fields (`workflow_name`, `comparison_report`, `recommendation`)
// named columns `project.comparison_runs` never had in any alembic revision —
// the defect qontinui-runner#1371 repaired on the Rust side. A wire type that
// promises fields the wire never carries is a trap for the first consumer that
// believes it, so it is corrected here rather than left standing.

export type ComparisonVariation =
  | "same"
  | "architecture"
  | "multi_agent"
  | "model"
  | "context_tokens"
  | "custom";

// These three unions are narrower than the Rust side, which types all of them
// as `String` read straight out of a DB column. Every writer in the tree emits
// one of the listed tokens today, so the narrowing is true — but it is a claim
// about the writers, not a guarantee from the type.
export type ComparisonEntryStatus = "pending" | "running" | "completed" | "failed";
export type ComparisonStatus = "running" | "comparing" | "completed" | "failed";

/**
 * How a run's OBSERVED treatment axis relates to the one it declared.
 * `unknown` is a coverage gap, never an assertion of agreement.
 */
export type AxisDriftClass =
  | "none"
  | "benign_add"
  | "pending"
  | "in_place"
  | "active_negation"
  | "divergent"
  | "unknown";

export interface ComparisonEntryResult {
  success: boolean;
  iterations: number;
  duration_ms: number;
}

export interface ComparisonEntry {
  /**
   * Human-readable arm name, and the identity `recommendation_from_entries_json`
   * reports a winner by.
   *
   * On the `custom` path the caller's own blob is stored verbatim as `overrides`,
   * so `label` ALSO appears inside it there. That duplicate is why `label` is on
   * the axis-computation ignore list — a run whose arms differ only in their
   * label has no treatment axis.
   */
  label: string;
  /**
   * The per-run config overrides applied to this arm. Only the keys the run
   * endpoint applies have any effect; anything else is inert.
   *
   * Rust-side this is a bare `serde_json::Value`, so a non-object is
   * representable, though the arm builder only ever writes objects.
   */
  overrides: Record<string, unknown>;
  /** Always present on the wire; `null` until the arm has been launched. */
  task_run_id: string | null;
  status: ComparisonEntryStatus;
  /** Always present on the wire; `null` until the arm has finished. */
  result: ComparisonEntryResult | null;
}

/**
 * The winner a comparison points at.
 *
 * Derived by the runner from the arms a run actually stored
 * (`recommendation_from_entries_json`) — it is NOT a stored column, and it is
 * not part of `ComparisonRun`. The meta-optimizer bridge is what produces one.
 */
export interface ComparisonRecommendation {
  branch_name: string;
  confidence: number;
  reasoning: string;
}

export interface ComparisonRun {
  id: string;
  workflow_id: string;
  /** What the author DECLARED would vary between the arms. */
  variation_type: string;
  status: ComparisonStatus;
  entries: ComparisonEntry[];
  /**
   * Always present on the wire. `null` on every current row — nothing in the
   * runner writes `project.comparison_runs.report` yet.
   */
  report: string | null;
  created_at: string;
  /** Always present on the wire; `null` until the run completes. */
  completed_at: string | null;
  /**
   * The key paths OBSERVED to actually differ across the arms.
   *
   * `null` means the axis was never computed — it does **not** mean nothing
   * differed, which is `[]`. `axis_drift_class` says which case this is.
   *
   * Narrower than the Rust type, deliberately: the column is `jsonb` and the
   * view serves `Option<serde_json::Value>`, but `AxisFacts::computed_axis_json`
   * is its only writer and only ever emits an array of strings.
   */
  computed_axis: string[] | null;
  axis_drift_class: AxisDriftClass;
}

// ─── Research Config ────────────────────────────────────────────────────────

export type SearchDimension =
  | { type: "model"; candidates: string[] }
  | { type: "multi_agent_mode" }
  | { type: "max_iterations"; range: [number, number] }
  | { type: "context_tokens"; candidates: number[] }
  | { type: "workflow_architecture" }
  | { type: "custom"; name: string; candidates: unknown[] };

export interface ResearchConfig {
  name: string;
  benchmark_workflow_id: string;
  trials_per_experiment: number;
  control_config: Record<string, unknown>;
  search_dimensions: SearchDimension[];
  mutation_strategy: "sequential" | "random_perturbation" | "ai_guided";
  acceptance_criteria: {
    primary_metric: "pass_rate" | "mean_iterations" | "mean_duration";
    significance_threshold: number;
  };
  use_worktree: boolean;
}
