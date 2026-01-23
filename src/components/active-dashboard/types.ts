/**
 * Active Dashboard Types
 *
 * Type definitions for the live execution dashboard components.
 * These types define the shape of data displayed during active workflow execution.
 *
 * Note: Types are expanded to include all runner data from:
 * - src/types/displayProfile.ts (ActionLogEntry, ActionLogViewData)
 * - src/types/statistics.ts (Anomaly, RunDetails, TransitionRecord, TemplateMatchRecord)
 * - src/types/execution.ts (ExecutionStats, CoverageData, ActionType enum)
 * - src/managers/logging/LogStore.ts (ImageRecognitionEntry)
 * - src/types/eventPayloads.ts (ImageRecognitionDebugData)
 */

/** Action types that can appear in the action stream - expanded to allow any string */
export type ActionType = string;

/** Common action type constants for reference */
export const ACTION_TYPES = {
  // Vision actions
  FIND: "find",
  FIND_ALL: "find_all",
  WAIT_FOR: "wait_for",
  WAIT_UNTIL_GONE: "wait_until_gone",

  // Input actions
  CLICK: "click",
  DOUBLE_CLICK: "double_click",
  RIGHT_CLICK: "right_click",
  TYPE: "type",
  PRESS_KEY: "press_key",
  HOTKEY: "hotkey",
  SCROLL: "scroll",
  DRAG: "drag",

  // State machine actions
  GO_TO_STATE: "go_to_state",
  TRANSITION: "transition",
  VERIFY_STATE: "verify_state",
  RUN_WORKFLOW: "run_workflow",

  // Control flow
  CONDITIONAL: "conditional",
  LOOP: "loop",
  PARALLEL: "parallel",
  SEQUENCE: "sequence",

  // Utility
  WAIT: "wait",
  SCREENSHOT: "screenshot",
  LOG: "log",
  ASSERT: "assert",

  // Custom/plugin
  CUSTOM: "custom",
} as const;

/** Status of an individual action */
export type ActionStatus =
  | "running"
  | "success"
  | "failed"
  | "pending"
  | "timeout"
  | "skipped"
  | "error";

/** Status of the overall execution */
export type ExecutionStatus =
  | "idle"
  | "running"
  | "paused"
  | "stopped"
  | "completed"
  | "failed"
  | "timeout"
  | "cancelled";

/** Type of anomaly detected during a run */
export type AnomalyType =
  | "timing_anomaly"
  | "confidence_anomaly"
  | "missing_element"
  | "unexpected_element"
  | "state_mismatch"
  | "unexpected_failure"
  | "slow_transition"
  | "low_confidence";

/** Severity level for anomalies */
export type AnomalySeverity = "low" | "medium" | "high";

/** Anomaly detected during a run (from statistics.ts) */
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

/** Coverage data for test runs (from execution.ts) */
export interface CoverageData {
  coverage_percentage: number;
  states_covered: number;
  total_states: number;
  transitions_covered: number;
  total_transitions: number;
  uncovered_states?: string[];
  uncovered_transitions?: string[];
  state_visit_counts?: Record<string, number>;
  transition_execution_counts?: Record<string, number>;
}

/** Image recognition debug data structure (from eventPayloads.ts) */
export interface ImageRecognitionDebugData {
  top_matches?: Array<{
    confidence: number;
    location: { x: number; y: number };
    dimensions?: { w: number; h: number };
  }>;
  [key: string]: unknown;
}

/** Individual action item in the action stream - expanded with fields from ActionLogEntry */
export interface ActionItem {
  id: string;
  action_type: ActionType;
  status: ActionStatus;
  timestamp: number;
  /** End timestamp (Unix epoch in seconds) */
  end_timestamp?: number;
  duration?: number;
  target?: string;
  result?: string;
  error?: string;
  children?: ActionItem[];
  level: number;
  /** Parent action ID for hierarchical relationships */
  parent_action_id?: string | null;
  /** Whether this action has children and can be expanded */
  is_expandable?: boolean;
  /** Additional metadata as key-value pairs */
  metadata?: Record<string, unknown>;
}

/** Result of an image recognition operation - expanded with fields from ImageRecognitionEntry */
export interface ImageRecognitionResult {
  /** Unique identifier for this recognition result */
  id?: string;
  /** Timestamp of the recognition operation */
  timestamp?: string;
  /** Node ID that triggered this recognition */
  node?: string;
  template: string;
  found: boolean;
  confidence: number;
  threshold: number;
  location?: { x: number; y: number; width: number; height: number };
  annotatedScreenshotPath?: string;
  /** Screenshot timestamp for correlation */
  screenshotTimestamp?: string;
  /** Gap value from pattern matching */
  gap?: number;
  /** Percentage off from expected match */
  percentOff?: number;
  /** String description of best match location */
  bestMatchLocation?: string;
  /** Base64 encoded screenshot (when no file path) */
  screenshotData?: string;
  /** Base64 encoded annotated screenshot with colored match boxes */
  visualDebugImage?: string;
  /** Path to the template image file */
  templatePath?: string;
  /** Base64 encoded template image */
  imageData?: string;
  /** Base64 encoded cropped region from screenshot at match location */
  matchedRegionImage?: string;
  /** Debug data with top_matches for displaying match details */
  debug?: ImageRecognitionDebugData;
  /** Which monitor was captured (null = all monitors combined) */
  monitorIndex?: number | null;
}

/** Screenshot file information */
export interface ScreenshotInfo {
  filename: string;
  path: string;
  size_bytes: number;
  modified?: string | null;
}

/** Container for different types of screenshots */
export interface ScreenshotsResult {
  annotated: ScreenshotInfo[];
  playwright: ScreenshotInfo[];
}

/** Overall execution state for the dashboard - expanded with fields from RunDetails and ExecutionStats */
export interface ExecutionState {
  status: ExecutionStatus;
  workflowName: string | null;
  currentTransition: string | null;
  currentAction: string | null;
  elapsedTime: number;
  actionsCompleted: number;
  successRate: number;
  averageConfidence: number;
  iteration?: number;
  maxIterations?: number;
  screenshots?: ScreenshotsResult;

  // Run identification (from RunDetails)
  /** Unique run identifier */
  run_id?: string;
  /** ISO timestamp of when the run started */
  started_at?: string;
  /** ISO timestamp of when the run ended (null if still running) */
  ended_at?: string | null;

  // Action statistics (from ExecutionStats)
  /** Total number of actions in the run */
  total_actions?: number;
  /** Number of failed actions */
  failed_actions?: number;
  /** Number of actions that timed out */
  timeout_actions?: number;
  /** Number of skipped actions */
  skipped_actions?: number;
  /** Average duration of actions in milliseconds */
  avg_action_duration_ms?: number;

  // Coverage and state tracking
  /** Coverage data for test runs */
  coverage?: CoverageData;
  /** List of state IDs that have been visited */
  states_visited?: string[];
  /** List of currently active state IDs */
  active_states?: string[];

  // Anomaly detection
  /** Anomalies detected during the run */
  anomalies?: Anomaly[];

  // Error information
  /** Type of error if run failed */
  error_type?: string;
  /** Error message if run failed */
  error_message?: string;
}

/** Props for ControlBar component */
export interface ControlBarProps {
  workflowName: string | null;
  status: ExecutionStatus;
  onPlayPause: () => void;
  onStop: () => void;
  onGoToExecute: () => void;
}

/** Props for LiveExecutionView component */
export interface LiveExecutionViewProps {
  actions: ActionItem[];
  status: string;
  currentAction: string | null;
}

/** Props for ScreenshotView component */
export interface ScreenshotViewProps {
  status: string;
}

/** Props for ActionStream component */
export interface ActionStreamProps {
  actions: ActionItem[];
  currentAction: string | null;
}

/** Props for StatusPanel component */
export interface StatusPanelProps {
  executionState: ExecutionState;
}

/** Props for BottomBar component */
export interface BottomBarProps {
  currentAction: string | null;
  iteration?: number;
  maxIterations?: number;
}

/** Props for IdleState component */
export interface IdleStateProps {
  onGoToExecute: () => void;
  /** Callback to navigate to Recap page and set session workflow */
  onGoToRecap?: () => void;
  /** Name of the last run workflow (if any) */
  lastRunWorkflowName?: string | null;
}
