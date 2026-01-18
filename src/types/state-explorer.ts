/**
 * Types for State Explorer
 *
 * These types correspond to the Rust types in src-tauri/src/verification_agent/types.rs
 */

/**
 * Exploration strategy options
 */
export interface ExplorationStrategy {
  id: string;
  name: string;
  description: string;
  recommended_for: string;
}

/**
 * Configuration for an exploration task
 */
export interface ExplorationConfig {
  /** Path to the qontinui config file */
  config_path: string;

  /** Exploration strategy to use */
  strategy: "exhaustive" | "smoke_test" | "regression" | "random_walk" | "targeted";

  /** Maximum number of states to visit (0 = unlimited) */
  max_states: number;

  /** Maximum time in seconds for the exploration run (0 = unlimited) */
  max_duration_seconds: number;

  /** Specific state IDs to explore (for targeted strategy) */
  target_state_ids: string[];

  /** Specific transition IDs to explore (for targeted strategy) */
  target_transition_ids: string[];

  /** Monitor index to use for exploration */
  monitor_index?: number;

  /** Whether to capture screenshots at each state */
  capture_screenshots: boolean;

  /** Whether to capture screenshots during transitions */
  capture_transition_screenshots: boolean;

  /** Delay in milliseconds between state visits */
  state_delay_ms: number;

  /** Directory to store exploration artifacts */
  output_directory?: string;

  /** Whether to stop on first failure */
  stop_on_first_failure: boolean;

  /** Random seed for reproducible random walks */
  random_seed?: number;
}

/**
 * Exploration status for a check
 */
export type ExplorationStatus =
  | "pending"
  | "in_progress"
  | "passed"
  | "failed"
  | "skipped"
  | "error";

/**
 * Check result for a single element
 */
export interface ElementCheck {
  element: string;
  found: boolean;
  confidence: number;
  location?: [number, number, number, number]; // x, y, width, height
}

/**
 * Result of exploring a single state
 */
export interface StateExplorationResult {
  state_id: string;
  state_name: string;
  status: ExplorationStatus;
  screenshot_path?: string;
  started_at: string;
  completed_at?: string;
  duration_ms: number;
  expected_elements_found: ElementCheck[];
  unexpected_elements_found: ElementCheck[];
  detection_confidence: number;
  error?: string;
  ai_notes?: string;
}

/**
 * Result of exploring a transition
 */
export interface TransitionExplorationResult {
  transition_id: string;
  from_state_id: string;
  to_state_id: string;
  status: ExplorationStatus;
  screenshot_before?: string;
  screenshot_after?: string;
  started_at: string;
  completed_at?: string;
  duration_ms: number;
  expected_duration_ms?: number;
  duration_exceeded: boolean;
  error?: string;
  notes?: string;
}

/**
 * Overall result of an exploration run
 */
export interface ExplorationResult {
  run_id: string;
  config_path: string;
  config_name?: string;
  strategy: string;
  started_at: string;
  completed_at?: string;
  total_duration_ms: number;
  total_states: number;
  states_visited: number;
  states_passed: number;
  states_failed: number;
  states_skipped: number;
  total_transitions: number;
  transitions_verified: number;
  transitions_passed: number;
  transitions_failed: number;
  state_explorations: StateExplorationResult[];
  transition_explorations: TransitionExplorationResult[];
  overall_status: ExplorationStatus;
  summary: string;
  report_path?: string;
  ai_analysis_prompt?: string;
}

/**
 * Summary of an exploration run for history list
 */
export interface ExplorationHistoryItem {
  run_id: string;
  config_name?: string;
  strategy?: string;
  started_at?: string;
  completed_at?: string;
  summary?: string;
  report_path?: string;
}

/**
 * Exploration plan preview
 */
export interface ExplorationPlan {
  strategy: string;
  total_states_in_config: number;
  total_transitions_in_config: number;
  states_to_visit: number;
  transitions_to_verify: number;
  estimated_cost: number;
  states: string[];
  transitions: string[];
}

/**
 * Default configuration for a new exploration task
 */
export function getDefaultExplorationConfig(configPath: string): ExplorationConfig {
  return {
    config_path: configPath,
    strategy: "smoke_test",
    max_states: 0,
    max_duration_seconds: 0,
    target_state_ids: [],
    target_transition_ids: [],
    capture_screenshots: true,
    capture_transition_screenshots: false,
    state_delay_ms: 500,
    stop_on_first_failure: false,
  };
}

/**
 * A saved exploration configuration that can be reused
 */
export interface SavedExploration {
  /** Unique identifier (UUID v4) */
  id: string;

  /** User-friendly name for this exploration */
  name: string;

  /** Optional description */
  description?: string;

  /** Path to the qontinui config file (stored separately from config) */
  config_path?: string;

  /** The configuration settings */
  config: Omit<ExplorationConfig, "config_path">;

  /** Tags for organization */
  tags: string[];

  /** Creation timestamp (ISO 8601) */
  created_at: string;

  /** Last modification timestamp (ISO 8601) */
  modified_at: string;

  /** Last run timestamp (ISO 8601) */
  last_run_at?: string;

  /** Number of times this exploration has been run */
  run_count: number;
}

/**
 * Request to create a new saved exploration
 */
export interface CreateSavedExplorationRequest {
  name: string;
  description?: string;
  config_path?: string;
  config: Omit<ExplorationConfig, "config_path">;
  tags?: string[];
}

/**
 * Request to update a saved exploration
 */
export interface UpdateSavedExplorationRequest {
  name?: string;
  description?: string;
  config_path?: string;
  config?: Omit<ExplorationConfig, "config_path">;
  tags?: string[];
}

/**
 * Get default saved exploration configuration
 */
export function getDefaultSavedExploration(): Omit<
  SavedExploration,
  "id" | "created_at" | "modified_at"
> {
  return {
    name: "",
    description: "",
    config: {
      strategy: "smoke_test",
      max_states: 0,
      max_duration_seconds: 0,
      target_state_ids: [],
      target_transition_ids: [],
      capture_screenshots: true,
      capture_transition_screenshots: false,
      state_delay_ms: 500,
      stop_on_first_failure: false,
    },
    tags: [],
    run_count: 0,
  };
}
