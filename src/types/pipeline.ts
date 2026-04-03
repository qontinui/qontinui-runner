/**
 * TypeScript types for multi-agent pipeline and autoresearch comparison.
 *
 * Mirrors Rust types from autoresearch/agentic_verification.rs and comparison.rs.
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

export type ComparisonVariation = "same" | "multi_agent" | "model" | "context_tokens" | "custom";

export type ComparisonEntryStatus = "pending" | "running" | "completed" | "failed";
export type ComparisonStatus = "running" | "comparing" | "completed" | "failed";

export interface ComparisonEntryResult {
  success: boolean;
  verification_passed: boolean;
  iterations: number;
  duration_ms: number;
  files_changed: number;
}

export interface ComparisonEntry {
  task_run_id: string;
  branch_name: string;
  worktree_path: string;
  config_overrides: Record<string, unknown>;
  status: ComparisonEntryStatus;
  result?: ComparisonEntryResult;
}

export interface ComparisonRecommendation {
  branch_name: string;
  confidence: number;
  reasoning: string;
}

export interface ComparisonRun {
  id: string;
  workflow_id: string;
  workflow_name: string;
  source_branch: string;
  source_commit: string;
  entries: ComparisonEntry[];
  status: ComparisonStatus;
  comparison_report?: string;
  recommendation?: ComparisonRecommendation;
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
