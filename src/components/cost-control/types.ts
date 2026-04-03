/**
 * Cost Control Panel Types
 *
 * TypeScript interfaces for real-time cost tracking, budget management,
 * anomaly detection, and circuit breaker status.
 */

/** Tauri event payloads use snake_case (Rust serde defaults). */
export interface CostUpdateEvent {
  task_run_id: string;
  phase: string;
  iteration: number | null;
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  cost_usd: number;
  cumulative_cost_usd: number;
  cache_hit_rate: number;
  timestamp: number;
}

export interface BudgetWarningEvent {
  task_run_id: string;
  remaining_fraction: number;
  total_cost_usd: number;
  budget_limit_usd: number;
  message: string;
  timestamp: number;
}

export interface CostAnomalyEvent {
  task_run_id: string;
  cost_usd: number;
  mean_cost_usd: number;
  std_dev: number;
  z_score: number;
  message: string;
  timestamp: number;
}

export interface CostDashboard {
  total_cost_usd: number;
  total_tokens: number;
  cache_hit_rate: number;
  cache_savings_usd: number;
  per_phase_breakdown: PhaseCostBreakdown[];
  budget_utilization: number | null;
}

export interface PhaseCostBreakdown {
  phase: string;
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  cost_usd: number;
}

export interface ActiveBudgetStatus {
  execution_id: string;
  total_cost_usd: number;
  max_cost_usd: number;
  remaining_fraction: number;
  total_tokens: number;
  max_tokens: number;
  circuit_breaker_tripped: boolean;
  anomaly_detector_sample_count: number;
  anomaly_detector_mean: number;
}
