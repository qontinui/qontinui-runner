import { getApiBase, tracedFetch } from "@/lib/runner-api";

// Generator Evaluation shared types

// ============================================================================
// Dashboard
// ============================================================================

export interface DashboardMetrics {
  total_generations: number;
  successful_generations: number;
  success_rate: number;
  avg_total_duration_ms: number | null;
  avg_verification_iterations: number | null;
  first_pass_rate: number | null;
  hardener_total_processed: number;
  hardener_total_converted: number;
  total_edits: number;
  total_deletes: number;
  total_ratings: number;
  avg_rating: number | null;
}

export interface TimeSeriesPoint {
  date: string;
  total_generations: number;
  successful_generations: number;
  avg_duration_ms: number | null;
  avg_verification_iterations: number | null;
}

// ============================================================================
// Pipeline Artifacts
// ============================================================================

export interface PipelineArtifactSummary {
  id: string;
  workflow_id: string | null;
  description: string;
  category: string | null;
  created_at: string;
  total_duration_ms: number | null;
  success: boolean;
  model_used: string | null;
  verification_iteration_count: number;
  hardener_converted_count: number;
}

export interface PipelineArtifact {
  id: string;
  workflow_id: string | null;
  task_run_id: string | null;
  description: string;
  category: string | null;
  created_at: string;
  discovery_duration_ms: number | null;
  builder_duration_ms: number | null;
  autofix_duration_ms: number | null;
  verification_duration_ms: number | null;
  hardener_duration_ms: number | null;
  total_duration_ms: number | null;
  discovery_calls: unknown;
  builder_raw_output: string | null;
  builder_parsed_json: unknown;
  autofix_diff: unknown;
  verification_iterations: unknown;
  fixer_snapshots: unknown[] | null;
  hardening_summary: unknown;
  hardened_json: unknown;
  final_json: unknown;
  validation_errors: unknown;
  success: boolean;
  error_message: string | null;
  model_used: string | null;
}

// ============================================================================
// Edit Analysis
// ============================================================================

export interface EditAnalysis {
  edited_fields: [string, number][];
  type_distribution: [string, number][];
  rating_distribution: [number, number][];
  recent_feedback: RecentFeedback[];
}

export interface RecentFeedback {
  id: string;
  workflow_id: string;
  feedback_type: string;
  edited_field: string | null;
  old_value: string | null;
  new_value: string | null;
  created_at: string;
  workflow_name: string | null;
}

// ============================================================================
// Step Quality Evaluation
// ============================================================================

export type EvaluationDimension =
  | "determinism"
  | "entailment"
  | "specificity"
  | "executability"
  | "coverage"
  | "robustness";

export type ScoringTier = "deterministic" | "judge" | "prm";

export interface DimensionScore {
  dimension: EvaluationDimension;
  score: number;
  confidence: number;
  tier: ScoringTier;
  explanation?: string;
  evidence: string[];
}

export type EvaluationFlagType =
  | "unexecutable"
  | "placeholder"
  | "false_positive"
  | "no_entailment"
  | "non_deterministic";

export interface EvaluationFlag {
  flag_type: EvaluationFlagType;
  message: string;
  step_id?: string;
}

export interface StepEvaluation {
  step_id: string;
  step_name: string;
  scores: DimensionScore[];
  composite_score: number;
  min_score: number;
  flags: EvaluationFlag[];
  criterion_id?: string;
  criterion_priority?: "critical" | "important" | "optional";
}

export interface CoverageEntry {
  criterion_id: string;
  criterion_description: string;
  criterion_priority: "critical" | "important" | "optional";
  mapped_steps: {
    step_id: string;
    step_name: string;
    entailment_score: number;
    entailment_explanation?: string;
    gaps: string[];
  }[];
  best_entailment: number;
  coverage_adequate: boolean;
}

export interface UncoveredCriterion {
  criterion_id: string;
  criterion_description: string;
  priority: "critical" | "important" | "optional";
  suggested_verification: string;
}

export interface SemanticCoverageMatrix {
  entries: CoverageEntry[];
  uncovered_criteria: UncoveredCriterion[];
  unlinked_steps: string[];
  overall_coverage_score: number;
}

export type QualityGateLevel = "minimum" | "standard" | "strict";

export interface QualityGateResult {
  passed: boolean;
  gate_level: QualityGateLevel;
  failures: {
    gate_level: QualityGateLevel;
    reason: string;
    step_id?: string;
    criterion_id?: string;
  }[];
}

export interface WorkflowEvaluation {
  step_evaluations: StepEvaluation[];
  overall_score: number;
  coverage_matrix: SemanticCoverageMatrix;
  quality_gate: QualityGateResult;
  scoring_strategy: "fast_only" | "standard" | "full";
  evaluation_duration_ms: number;
}

export interface RepairInstruction {
  step_id: string;
  step_name: string;
  weakest_dimension: EvaluationDimension;
  current_score: number;
  explanation?: string;
  suggested_fix: string;
}

// ============================================================================
// API Response
// ============================================================================

export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
}

/** Typed fetch helper for generator-eval endpoints */
export async function fetchApi<T>(path: string, options?: RequestInit): Promise<T> {
  const resp = await tracedFetch(`${getApiBase()}${path}`, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  const json: ApiResponse<T> = await resp.json();
  if (!json.success) {
    throw new Error(json.error || "API request failed");
  }
  return json.data as T;
}
