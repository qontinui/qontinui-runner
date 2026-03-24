/**
 * LLM Observability Dashboard Types
 *
 * TypeScript interfaces for the LLM token usage and cost analytics data
 * returned by Tauri backend commands.
 */

export interface TokenUsageSummary {
  total_cost_cents: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_calls: number;
  unique_models: number;
  unique_providers: number;
  avg_cost_per_call_cents: number;
  avg_duration_ms: number | null;
}

export interface DailyCostRow {
  date: string;
  total_cost_cents: number;
  total_input_tokens: number;
  total_output_tokens: number;
  call_count: number;
}

export interface ModelCostRow {
  model_used: string;
  total_cost_cents: number;
  total_input_tokens: number;
  total_output_tokens: number;
  call_count: number;
  avg_duration_ms: number | null;
}

export interface PhaseCostRow {
  phase: string;
  total_cost_cents: number;
  total_input_tokens: number;
  total_output_tokens: number;
}

export interface ProviderLatencyRow {
  provider_used: string;
  avg_duration_ms: number;
  min_duration_ms: number;
  max_duration_ms: number;
  call_count: number;
}

export interface TaskRunCostRow {
  task_run_id: string;
  total_cost_cents: number;
  total_input_tokens: number;
  total_output_tokens: number;
  call_count: number;
  started_at: string;
}

/** Time range options for the LLM analytics dashboard */
export type LlmTimeRange = "1d" | "7d" | "30d" | "all";

export const LLM_TIME_RANGE_LABELS: Record<LlmTimeRange, string> = {
  "1d": "Last 24 hours",
  "7d": "Last 7 days",
  "30d": "Last 30 days",
  all: "All time",
};
