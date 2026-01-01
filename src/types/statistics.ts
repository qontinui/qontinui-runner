/**
 * Type definitions for the Statistics Dashboard
 *
 * These types mirror the Rust types in src-tauri/src/tiered_info/types.rs
 * and src-tauri/src/commands/tiered_info.rs for the Tiered Information Model.
 */

// =============================================================================
// Response Wrapper
// =============================================================================

/**
 * Response wrapper for tiered info commands.
 * Matches TieredInfoResponse<T> from tiered_info.rs
 */
export interface TieredInfoResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

// =============================================================================
// Enums and Basic Types
// =============================================================================

/**
 * Run status enumeration.
 * Matches RunStatus from types.rs with #[serde(rename_all = "lowercase")]
 */
export type RunStatus = "running" | "completed" | "failed" | "timeout" | "cancelled";

/**
 * Type of anomaly detected during a run.
 * Matches AnomalyType from types.rs with #[serde(rename_all = "snake_case")]
 */
export type AnomalyType =
  | "timing_anomaly"
  | "confidence_anomaly"
  | "missing_element"
  | "unexpected_element"
  | "state_mismatch"
  | "unexpected_failure"
  | "slow_transition"
  | "low_confidence";

/**
 * Severity level for anomalies.
 * Matches AnomalySeverity from types.rs with #[serde(rename_all = "lowercase")]
 */
export type AnomalySeverity = "low" | "medium" | "high";

/**
 * Type of flaky item (for FlakyItem.item_type).
 */
export type FlakyItemType = "transition" | "template";

// =============================================================================
// Basic Structures
// =============================================================================

/**
 * Summary of actions executed in a run.
 * Matches ActionsSummary from types.rs
 */
export interface ActionsSummary {
  total: number;
  success: number;
  failed: number;
  skipped: number;
}

/**
 * Record of a transition execution.
 * Matches TransitionRecord from types.rs
 */
export interface TransitionRecord {
  from_state: string;
  to_state: string;
  action: string;
  success: boolean;
  duration_ms: number;
  error?: string;
}

/**
 * Record of template matching results.
 * Matches TemplateMatchRecord from types.rs
 */
export interface TemplateMatchRecord {
  template: string;
  count: number;
  avg_confidence: number;
  failures: number;
}

/**
 * Anomaly detected during a run.
 * Matches Anomaly from types.rs
 */
export interface Anomaly {
  anomaly_type: AnomalyType;
  description: string;
  severity: AnomalySeverity;
  /** Expected value (if applicable) */
  expected?: string;
  /** Actual value observed */
  actual?: string;
  /** When the anomaly was detected (ISO timestamp) */
  detected_at?: string;
  /** Additional context for debugging */
  context?: Record<string, unknown>;
}

// =============================================================================
// Tier 1: Run Details
// =============================================================================

/**
 * Tier 1: Detailed run information.
 * Matches RunDetails from types.rs
 */
export interface RunDetails {
  id: string;
  config_id: string;
  workflow_name?: string;
  started_at: string;
  ended_at?: string;
  duration_ms?: number;
  status: RunStatus;
  success?: boolean;
  error_type?: string;
  error_message?: string;
  actions_summary?: ActionsSummary;
  states_visited: string[];
  transitions_executed: TransitionRecord[];
  template_matches: TemplateMatchRecord[];
  anomalies: Anomaly[];
}

// =============================================================================
// Statistics Types
// =============================================================================

/**
 * Statistics for a specific transition.
 * Matches TransitionStats from types.rs
 */
export interface TransitionStats {
  total: number;
  success: number;
  failure: number;
  avg_duration_ms: number;
  /** ISO timestamp of last failure */
  last_failure?: string;
  /** Standard deviation of duration (for flakiness detection) */
  duration_stddev?: number;
}

/**
 * Statistics for a specific template.
 * Matches TemplateStats from types.rs
 */
export interface TemplateStats {
  total: number;
  matches: number;
  failures: number;
  avg_confidence: number;
  min_confidence?: number;
  max_confidence?: number;
}

/**
 * Statistics for a specific state.
 * Matches StateStats from types.rs
 */
export interface StateStats {
  visits: number;
  avg_time_in_state_ms: number;
  entry_failures: number;
}

/**
 * Error pattern tracking.
 * Matches ErrorPattern from types.rs
 */
export interface ErrorPattern {
  count: number;
  /** ISO timestamp of last occurrence */
  last_seen?: string;
  /** Recent error contexts (limited) */
  contexts: string[];
}

// =============================================================================
// Tier 4: Config Statistics
// =============================================================================

/**
 * Tier 4: Aggregated config statistics.
 * Matches ConfigStatistics from types.rs
 */
export interface ConfigStatistics {
  id: string;
  config_id: string;
  config_hash?: string;
  total_runs: number;
  successful_runs: number;
  failed_runs: number;
  timeout_runs: number;
  avg_duration_ms?: number;
  recent_success_rate?: number;
  recent_avg_duration_ms?: number;
  transition_stats: Record<string, TransitionStats>;
  template_stats: Record<string, TemplateStats>;
  state_stats: Record<string, StateStats>;
  error_patterns: Record<string, ErrorPattern>;
  flaky_transitions: string[];
  flaky_templates: string[];
  first_run_at?: string;
  last_run_at?: string;
  last_updated_at?: string;
}

// =============================================================================
// Flakiness Types
// =============================================================================

/**
 * Flaky item (transition or template) with detailed info.
 * Matches FlakyItem from types.rs
 */
export interface FlakyItem {
  name: string;
  item_type: FlakyItemType;
  success_rate: number;
  total_attempts: number;
  last_failure?: string;
  details?: Record<string, unknown>;
}

/**
 * Response containing flakiness summary for a configuration.
 * Matches FlakinessSummary from tiered_info.rs
 */
export interface FlakinessSummary {
  /** Number of flaky transitions */
  flaky_transition_count: number;
  /** Number of flaky templates */
  flaky_template_count: number;
  /** List of flaky transition IDs */
  flaky_transition_ids: string[];
  /** List of flaky template IDs */
  flaky_template_ids: string[];
  /** Whether the cache is stale (older than 5 minutes) */
  cache_is_stale: boolean;
}

// =============================================================================
// Debugging Context
// =============================================================================

/**
 * Brief summary of a failed run for debugging context.
 * Matches RunFailureSummary from types.rs
 */
export interface RunFailureSummary {
  run_id: string;
  timestamp: string;
  error_type?: string;
  error_message?: string;
  failed_transition?: string;
  failed_template?: string;
}

/**
 * Debugging context for AI prompts.
 * This is the Tier 4 summary formatted for AI consumption.
 * Matches DebuggingContext from types.rs
 */
export interface DebuggingContext {
  config_id: string;
  config_name?: string;

  // Overall health
  total_runs: number;
  success_rate: number;
  recent_success_rate?: number;
  avg_duration_ms?: number;

  // Problem areas
  flaky_transitions: FlakyItem[];
  flaky_templates: FlakyItem[];
  /** [error_type, count] tuples */
  common_errors: [string, number][];

  // Recent failures (for immediate context)
  recent_failures: RunFailureSummary[];
}

// =============================================================================
// Execution Options
// =============================================================================

/**
 * Flakiness-aware execution options for a transition or template.
 * Matches ExecutionOptions from executor/flakiness.rs
 */
export interface ExecutionOptions {
  /** Number of retry attempts for this operation */
  retry_count: number;
  /** Multiplier for timeout duration */
  timeout_multiplier: number;
  /** Confidence threshold for template matching */
  confidence_threshold: number;
  /** Whether to use an alternative path if available */
  use_alternative_path: boolean;
}

/**
 * Default execution options (matches Rust Default impl).
 */
export const DEFAULT_EXECUTION_OPTIONS: ExecutionOptions = {
  retry_count: 1,
  timeout_multiplier: 1.0,
  confidence_threshold: 0.8,
  use_alternative_path: false,
};

// =============================================================================
// Record Run Input
// =============================================================================

/**
 * Input for recording a run.
 * Matches RecordRunInput from tiered_info.rs
 */
export interface RecordRunInput {
  config_id: string;
  /** Project ID (required for discovery push) */
  project_id?: string;
  /** Config name for discovery context */
  config_name?: string;
  workflow_name?: string;
  /** "completed", "failed", "timeout", "cancelled" */
  status: string;
  success?: boolean;
  duration_ms?: number;
  error_type?: string;
  error_message?: string;
  actions_summary?: ActionsSummary;
  states_visited?: string[];
  transitions_executed?: TransitionRecord[];
  template_matches?: TemplateMatchRecord[];
  anomalies?: Anomaly[];
}
