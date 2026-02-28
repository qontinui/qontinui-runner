// Generator Evaluation shared types

export const API_BASE = "http://localhost:9876";

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
// Benchmarks
// ============================================================================

export interface ExpectedStructure {
  has_setup?: boolean;
  has_verification?: boolean;
  has_agentic?: boolean;
  has_completion?: boolean;
  min_setup_steps?: number;
  min_verification_steps?: number;
  min_agentic_steps?: number;
  expected_step_types: string[];
  keywords: string[];
  expected_check_types: string[];
  expected_test_types: string[];
}

export interface Benchmark {
  id: string;
  name: string;
  description: string;
  category: string | null;
  tags: string[] | null;
  expected_structure: ExpectedStructure;
  created_at: string;
  updated_at: string;
  enabled: boolean;
}

export interface BenchmarkResult {
  id: string;
  benchmark_id: string;
  artifact_id: string | null;
  run_at: string;
  model_used: string | null;
  structure_score: number | null;
  content_score: number | null;
  step_type_score: number | null;
  overall_score: number | null;
  score_breakdown: unknown;
  generated_json: unknown;
  duration_ms: number | null;
  passed: boolean;
  notes: string | null;
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
// Example Library
// ============================================================================

export interface ExampleWorkflow {
  id: string;
  name: string;
  description: string;
  category: string;
  example_status: string | null;
  created_at: string;
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
  const resp = await fetch(`${API_BASE}${path}`, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  const json: ApiResponse<T> = await resp.json();
  if (!json.success) {
    throw new Error(json.error || "API request failed");
  }
  return json.data as T;
}
