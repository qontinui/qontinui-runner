/**
 * Shared types for test run reporting
 *
 * @deprecated These types are part of the deprecated TestRunReportingService.
 * Use types from ExecutionReportingService for new implementations.
 */

// ============================================================================
// Input Types (sent to Rust backend)
// ============================================================================

/** Input for creating a test run */
export interface CreateTestRunInput {
  project_id: string;
  workflow_id: string;
  workflow_name: string;
  total_states?: number;
  total_transitions?: number;
  description?: string;
}

/** Input for completing a test run - matches Rust CompleteTestRunInput */
export interface CompleteTestRunInput {
  run_id: string;
  success: boolean;
  total_transitions: number;
  successful_transitions: number;
  failed_transitions: number;
  timeout_transitions?: number;
  unique_transitions_covered?: number;
  coverage_percentage?: number;
  states_covered?: number;
  total_states?: number;
  deficiencies_found?: number;
  screenshots_captured?: number;
  duration_seconds?: number;
}

/** Input for reporting image recognition batch */
export interface ReportRecognitionsInput {
  run_id: string;
  project_id: string;
  workflow_id?: number;
  test_run_id: string;
  actions: Array<ImageRecognitionData & { action_id: string; sequence_number: number }>;
}

// ============================================================================
// Response Types (received from Rust backend)
// ============================================================================

/** Response from creating a test run */
export interface CreateTestRunResponse {
  run_id: string;
  project_id: string;
  run_name: string;
  status: string;
  started_at: string;
  ended_at?: string;
  duration_seconds?: number;
}

/** Response from reporting transitions */
export interface ReportTransitionsResponse {
  recorded: number;
  run_id: string;
}

/** Response from reporting recognitions */
export interface ReportRecognitionsResponse {
  indexed: number;
  run_id: string;
}

/** Response from completing a test run */
export interface CompleteTestRunResponse {
  run_id: string;
  status: string;
  duration_seconds: number;
}

// ============================================================================
// Data Types (used in reporting)
// ============================================================================

/** Transition data for reporting - matches backend TransitionCreate schema */
export interface TransitionData {
  sequence_number: number;
  from_state: string;
  to_state: string;
  transition_name: string;
  status: "success" | "failed" | "skipped" | "timeout";
  started_at: string;
  completed_at: string;
  duration_ms: number;
  error_message?: string;
  error_type?: string;
  screenshot_id?: string;
  metadata?: Record<string, unknown>;
}

/** Image recognition data for historical storage */
export interface ImageRecognitionData {
  pattern_id: string;
  pattern_name?: string;
  action_type: string;
  active_states: string[];
  success: boolean;
  match_count?: number;
  best_match_score?: number;
  match_x?: number;
  match_y?: number;
  match_width?: number;
  match_height?: number;
  duration_ms?: number;
  result_data?: Record<string, unknown>;
}

/** Recognition data with sequence info for batching */
export interface SequencedRecognitionData extends ImageRecognitionData {
  action_id: string;
  sequence_number: number;
}

// ============================================================================
// Internal State Types
// ============================================================================

/** Execution stats tracked during a run */
export interface ExecutionStats {
  totalTransitions: number;
  successfulTransitions: number;
  failedTransitions: number;
  statesCovered: Set<string>;
  totalRecognitions: number;
  successfulRecognitions: number;
  failedRecognitions: number;
}

/** Active run state */
export interface ActiveRunState {
  runId: string | null;
  projectId: string | null;
  workflowId: number | null;
}

/** Batching configuration */
export interface BatchConfig {
  flushIntervalMs: number;
  batchSize: number;
}

/** Default batching configuration */
export const DEFAULT_BATCH_CONFIG: BatchConfig = {
  flushIntervalMs: 5000,
  batchSize: 10,
};

// ============================================================================
// Public Stats (read-only view)
// ============================================================================

/** Read-only view of execution stats for external consumers */
export interface PublicExecutionStats {
  totalTransitions: number;
  successfulTransitions: number;
  failedTransitions: number;
  statesCovered: number;
}
